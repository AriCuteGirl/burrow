//! Settings for shortcuts and custom actions, and running those actions.

use super::dialogs::{button_row, dialog_window};
use super::{Win, files};
use crate::actions::{Action, Target};
use crate::keys::{self, KEYS, Overrides};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const MODS: gdk::ModifierType =
    gdk::ModifierType::CONTROL_MASK.union(gdk::ModifierType::SHIFT_MASK).union(gdk::ModifierType::ALT_MASK).union(gdk::ModifierType::SUPER_MASK);

fn norm(a: &str) -> String {
    match gtk::accelerator_parse(a) {
        Some((k, m)) => gtk::accelerator_name(k.to_lower(), m).to_string(),
        None => a.to_string(),
    }
}

fn pretty(a: &str) -> String {
    gtk::accelerator_parse(a).map(|(k, m)| gtk::accelerator_get_label(k, m).to_string()).unwrap_or_else(|| a.to_string())
}

impl Win {
    /// Put the shortcuts from the config in place.
    pub fn apply_keys(&self) {
        let over = self.cfg.borrow().keys.clone();
        let Some(app) = self.window.application() else { return };
        let mut view = vec![];
        for key in KEYS {
            let acc: Vec<String> = keys::accels(&over, key).into_iter().filter(|a| gtk::accelerator_parse(a).is_some()).collect();
            match key.scope {
                keys::Scope::Window => {
                    let refs: Vec<&str> = acc.iter().map(String::as_str).collect();
                    app.set_accels_for_action(&format!("win.{}", key.id), &refs);
                }
                keys::Scope::View => {
                    app.set_accels_for_action(&format!("win.{}", key.id), &[]);
                    view.extend(acc.iter().filter_map(|a| gtk::accelerator_parse(a)).map(|(k, m)| (k.to_lower(), m, key.id)));
                }
            }
        }
        *self.view_keys.borrow_mut() = view;
    }

    /// The file view shortcut for a key press, if any.
    pub fn view_key(&self, key: gdk::Key, mods: gdk::ModifierType) -> Option<&'static str> {
        let (k, m) = (key.to_lower(), mods & MODS);
        self.view_keys.borrow().iter().find(|(a, b, _)| *a == k && *b == m).map(|(_, _, id)| *id)
    }

    // ------------------------------------------------------------------ custom actions
    pub fn targets(&self) -> Vec<Target> {
        let mut out = vec![];
        for i in self.active().files.selected_infos() {
            let Some(f) = files::info_file(&i) else { continue };
            let Some(path) = f.path() else { continue };
            out.push(Target { path, uri: f.uri().to_string(), mime: i.content_type().map(|c| c.to_string()).unwrap_or_default(), folder: files::is_folder(&i) });
        }
        out
    }

    pub fn run_action(self: &Rc<Self>, index: usize) {
        let Some(a) = self.cfg.borrow().actions.get(index).cloned() else { return };
        let sel = self.targets();
        if !a.applies(&sel) {
            return;
        }
        let cmds = match a.commands(&sel) {
            Ok(c) => c,
            Err(e) => return self.show_notice(&format!("{}: {e}", a.name), true),
        };
        let dir = sel[0].path.parent().map(|p| p.to_path_buf()).unwrap_or_else(glib::home_dir);
        let (weak, name) = (Rc::downgrade(self), a.name.clone());
        let n = cmds.len();
        let first = cmds[0].join(" ");
        let go = move || {
            let Some(w) = weak.upgrade() else { return };
            super::jobs::run(
                &w,
                &name.clone(),
                move |p, c| run_all(&cmds, &dir, p, c),
                move |w, (), p, cancelled| {
                    let (msg, err) = super::jobs::summary(&format!("{name}: done"), p, cancelled);
                    w.show_notice(&msg, err);
                },
            );
        };
        if a.confirm {
            let what = if n == 1 { first } else { format!("{first}\n… and {} more", n - 1) };
            super::dialogs::confirm(&self.window, &format!("Run “{}”?", a.name), &what, "Run", go);
        } else {
            go();
        }
    }
}

fn run_all(cmds: &[Vec<String>], dir: &std::path::Path, p: &crate::ops::Progress, c: &gio::Cancellable) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;
    p.total.store(cmds.len() as u64, Ordering::Relaxed);
    for argv in cmds {
        if !crate::ops::checkpoint(p, c) {
            return;
        }
        p.set_current(argv.last().map(String::as_str).unwrap_or(""));
        let fail = |m: String| {
            if let Ok(mut v) = p.errors.lock() {
                v.push(m);
            }
        };
        let mut child = match Command::new(&argv[0]).args(&argv[1..]).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() {
            Ok(ch) => ch,
            Err(e) => {
                fail(format!("couldn't start {}: {e}", argv[0]));
                return;
            }
        };
        let mut err = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(e) = err.as_mut() {
                let _ = e.take(64 * 1024).read_to_end(&mut s);
                let _ = std::io::copy(e, &mut std::io::sink());
            }
            String::from_utf8_lossy(&s).trim().lines().last().unwrap_or("").to_string()
        });
        loop {
            if c.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            match child.try_wait() {
                Ok(Some(st)) => {
                    let msg = reader.join().unwrap_or_default();
                    if !st.success() {
                        fail(if msg.is_empty() { format!("{} failed ({st})", argv[0]) } else { msg });
                    }
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(60)),
                Err(e) => {
                    fail(e.to_string());
                    break;
                }
            }
        }
        p.tick();
    }
}

// ---------------------------------------------------------------- settings groups

pub struct Extras {
    keys_box: gtk::Box,
    actions_box: gtk::Box,
    note: gtk::Label,
    shown: RefCell<Option<(Overrides, Vec<Action>)>>,
}

fn title(root: &gtk::Box, t: &str) -> gtk::Box {
    let l = gtk::Label::new(Some(t));
    l.add_css_class("group-title");
    l.set_xalign(0.0);
    root.append(&l);
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("settings-group");
    root.append(&b);
    b
}

impl Extras {
    pub fn new(root: &gtk::Box) -> Extras {
        let actions_box = title(root, "RIGHT-CLICK ACTIONS");
        let keys_box = title(root, "SHORTCUTS");
        let note = gtk::Label::new(None);
        note.add_css_class("settings-sub");
        note.set_xalign(0.0);
        note.set_wrap(true);
        note.set_margin_top(6);
        root.append(&note);
        Extras { keys_box, actions_box, note, shown: RefCell::new(None) }
    }

    pub fn sync(&self, w: &Rc<Win>) {
        let now = (w.cfg.borrow().keys.clone(), w.cfg.borrow().actions.clone());
        if self.shown.borrow().as_ref() == Some(&now) {
            return;
        }
        for b in [&self.keys_box, &self.actions_box] {
            while let Some(c) = b.first_child() {
                b.remove(&c);
            }
        }
        for key in KEYS {
            self.key_row(w, key, &now.0);
        }
        self.note.set_text("Click a shortcut and press the new keys. Esc cancels, Backspace turns it off.");
        for (i, a) in now.1.iter().enumerate() {
            let edit = gtk::Button::with_label("Edit…");
            edit.add_css_class("secondary");
            let weak = Rc::downgrade(w);
            edit.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    action_dialog(&w, Some(i));
                }
            });
            row(&self.actions_box, &a.name, &a.command, &edit);
        }
        let add = gtk::Button::with_label("Add…");
        add.add_css_class("secondary");
        let weak = Rc::downgrade(w);
        add.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                action_dialog(&w, None);
            }
        });
        row(&self.actions_box, "New action", "Run your own command on the selected files", &add);
        *self.shown.borrow_mut() = Some(now);
    }

    fn key_row(&self, w: &Rc<Win>, key: &'static keys::Key, over: &Overrides) {
        let acc = keys::accels(over, key);
        let shown: Vec<String> = acc.iter().take(2).map(|a| pretty(a)).collect();
        let text = if shown.is_empty() { "Off".to_string() } else { shown.join("  or  ") };
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if over.contains_key(key.id) {
            let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
            reset.add_css_class("eject");
            reset.set_tooltip_text(Some("Back to the default"));
            let weak = Rc::downgrade(w);
            reset.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.edit_config(|c| {
                        c.keys.remove(key.id);
                    });
                }
            });
            hb.append(&reset);
        }
        let b = gtk::Button::with_label(&text);
        b.add_css_class("secondary");
        hb.append(&b);
        let capturing = Rc::new(std::cell::Cell::new(false));
        let (cap, t2) = (capturing.clone(), text.clone());
        b.connect_clicked(move |b| {
            cap.set(true);
            b.set_label("Press keys…");
        });
        let focus = gtk::EventControllerFocus::new();
        let (cap, bw) = (capturing.clone(), b.downgrade());
        focus.connect_leave(move |_| {
            if cap.replace(false) {
                if let Some(b) = bw.upgrade() {
                    b.set_label(&t2);
                }
            }
        });
        b.add_controller(focus);
        let ctl = gtk::EventControllerKey::new();
        ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (weak, bw, note, cap) = (Rc::downgrade(w), b.downgrade(), self.note.clone(), capturing);
        ctl.connect_key_pressed(move |_, k, _, mods| {
            let (Some(w), Some(b)) = (weak.upgrade(), bw.upgrade()) else { return glib::Propagation::Proceed };
            if !cap.get() {
                return glib::Propagation::Proceed;
            }
            let m = mods & MODS;
            // wait for a real key, not just Ctrl/Shift/…
            if gdk::Key::name(&k).is_some_and(|n| ["Control", "Shift", "Alt", "Super", "Meta", "Hyper", "ISO_Level", "Caps_Lock"].iter().any(|p| n.starts_with(p))) && k != gdk::Key::ISO_Left_Tab {
                return glib::Propagation::Stop;
            }
            cap.set(false);
            if k == gdk::Key::Escape && m.is_empty() {
                b.set_label(&text);
                return glib::Propagation::Stop;
            }
            let new = if k == gdk::Key::BackSpace && m.is_empty() { vec![] } else { vec![gtk::accelerator_name(k.to_lower(), m).to_string()] };
            if let Some(a) = new.first() {
                let typed = k.to_unicode().is_some_and(|c| !c.is_control() && !c.is_whitespace());
                if key.scope == keys::Scope::Window && typed && !m.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
                    note.set_text("That would get in the way of typing: use it with Ctrl, Alt or Super.");
                    b.set_label(&text);
                    return glib::Propagation::Stop;
                }
                let taken = keys::clash(&w.cfg.borrow().keys, key.id, a, norm);
                if let Some(other) = taken {
                    note.set_text(&format!("{} is already used for “{}”. Change that one first.", pretty(a), other.label));
                    b.set_label(&text);
                    return glib::Propagation::Stop;
                }
            }
            w.edit_config(|c| {
                c.keys.insert(key.id.to_string(), new);
            });
            glib::Propagation::Stop
        });
        b.add_controller(ctl);
        row(&self.keys_box, key.label, "", &hb);
    }
}

fn row(g: &gtk::Box, t: &str, sub: &str, control: &impl IsA<gtk::Widget>) {
    super::settings::row(g, t, sub, control);
}

fn action_dialog(w: &Rc<Win>, index: Option<usize>) {
    let cur = index.and_then(|i| w.cfg.borrow().actions.get(i).cloned()).unwrap_or_default();
    let parent: gtk::Window = w.settings.borrow().as_ref().map(|s| s.window.clone()).unwrap_or_else(|| w.window.clone().upcast());
    let (win, root) = dialog_window(&parent, if index.is_some() { "Edit action" } else { "New action" }, 520);
    let field = |label: &str, text: &str, ph: &str| {
        let l = gtk::Label::new(Some(label));
        l.add_css_class("settings-sub");
        l.set_xalign(0.0);
        let e = gtk::Entry::new();
        e.add_css_class("path-entry");
        e.set_text(text);
        e.set_placeholder_text(Some(ph));
        root.append(&l);
        root.append(&e);
        e
    };
    let name = field("Name in the menu", &cur.name, "Resize to half");
    let command = field("Command", &cur.command, "magick %f -resize 50% %d/small-%n");
    let types = field("Only for (empty = everything)", &cur.types.join(", "), "image/*, .png, folder, file");
    let help = gtk::Label::new(Some("%f file · %F all files · %d its folder · %n name · %s name without extension · %u address. Runs the program directly, not through a shell."));
    help.add_css_class("dialog-hint");
    help.set_xalign(0.0);
    help.set_wrap(true);
    root.append(&help);
    let ask = gtk::CheckButton::with_label("Ask before running");
    ask.set_active(cur.confirm);
    root.append(&ask);
    let problem = gtk::Label::new(None);
    problem.add_css_class("form-error");
    problem.set_xalign(0.0);
    problem.set_wrap(true);
    problem.set_visible(false);
    root.append(&problem);
    if let Some(i) = index {
        let del = gtk::Button::with_label("Delete this action");
        del.add_css_class("secondary");
        del.set_halign(gtk::Align::Start);
        let (weak, ww) = (Rc::downgrade(w), win.downgrade());
        del.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.edit_config(|c| {
                    if i < c.actions.len() {
                        c.actions.remove(i);
                    }
                });
            }
            if let Some(w) = ww.upgrade() {
                w.close();
            }
        });
        root.append(&del);
    }
    let (cancel, ok) = button_row(&root, "Save");
    let ww = win.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = ww.upgrade() {
            w.close()
        }
    });
    let (weak, ww) = (Rc::downgrade(w), win.downgrade());
    let n2 = name.clone();
    ok.connect_clicked(move |_| {
        let a = Action {
            name: n2.text().trim().to_string(),
            command: command.text().trim().to_string(),
            types: types.text().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            confirm: ask.is_active(),
        };
        if let Err(e) = a.valid() {
            problem.set_text(&e);
            problem.set_visible(true);
            return;
        }
        if let Some(w) = weak.upgrade() {
            w.edit_config(|c| match index {
                Some(i) if i < c.actions.len() => c.actions[i] = a,
                _ => c.actions.push(a),
            });
        }
        if let Some(w) = ww.upgrade() {
            w.close();
        }
    });
    win.present();
    name.grab_focus();
}
