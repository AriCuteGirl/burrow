//! Batch rename dialog: find/replace (text or regex), template with numbering, case,
//! live preview with problems marked; applied as one job, undoable.

use super::{UndoOp, Win, files, jobs};
use crate::rename::{self, Case, Rules};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::collections::HashSet;
use std::rc::Rc;

fn field(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("settings-sub");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(w);
    b.set_hexpand(true);
    b
}

fn entry(text: &str, placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.add_css_class("path-entry");
    e.set_text(text);
    e.set_placeholder_text(Some(placeholder));
    e
}

impl Win {
    pub fn batch_rename(self: &Rc<Self>, infos: Vec<gio::FileInfo>) {
        let items: Vec<(gio::File, String)> = infos.iter().filter_map(|i| Some((files::info_file(i)?, i.name().to_string_lossy().to_string()))).collect();
        if items.len() < 2 {
            return;
        }
        let renamed: HashSet<String> = items.iter().map(|(_, n)| n.clone()).collect();
        let others: Rc<HashSet<String>> = Rc::new(self.active().files.all_names().into_iter().filter(|n| !renamed.contains(n)).collect());
        let names: Rc<Vec<String>> = Rc::new(items.iter().map(|(_, n)| n.clone()).collect());
        let items = Rc::new(items);

        let win = gtk::Window::builder().transient_for(&self.window).modal(true).title("Rename").decorated(false).default_width(620).default_height(640).build();
        win.add_css_class("burrow");
        win.add_css_class("dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("dialog-root");
        let t = gtk::Label::new(Some(&format!("Rename {} items", names.len())));
        t.add_css_class("dialog-title");
        t.set_xalign(0.0);
        root.append(&t);

        let find = entry("", "text to find");
        let replace = entry("", "replace with");
        let r1 = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        r1.append(&field("Find", &find));
        r1.append(&field("Replace with", &replace));
        root.append(&r1);
        let r2 = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let regex = gtk::CheckButton::with_label("Regular expression (\\1 for groups)");
        let mcase = gtk::CheckButton::with_label("Match case");
        let keep_ext = gtk::CheckButton::with_label("Keep extension");
        keep_ext.set_active(true);
        r2.append(&regex);
        r2.append(&mcase);
        r2.append(&keep_ext);
        root.append(&r2);
        let template = entry("{name}", "{name} {n} {ext}");
        let start = gtk::SpinButton::with_range(0.0, 1_000_000.0, 1.0);
        start.set_value(1.0);
        start.add_css_class("settings-spin");
        let pad = gtk::SpinButton::with_range(1.0, 8.0, 1.0);
        pad.set_value(2.0);
        pad.add_css_class("settings-spin");
        let case = gtk::DropDown::from_strings(&["Keep case", "lowercase", "UPPERCASE", "Title Case"]);
        case.add_css_class("settings-dropdown");
        let r3 = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let tf = field("Name ({name} = current name, {n} = number, {ext} = extension)", &template);
        tf.set_hexpand(true);
        r3.append(&tf);
        let sf = field("Start at", &start);
        sf.set_hexpand(false);
        let pf = field("Digits", &pad);
        pf.set_hexpand(false);
        let cf = field("Case", &case);
        cf.set_hexpand(false);
        r3.append(&sf);
        r3.append(&pf);
        r3.append(&cf);
        root.append(&r3);

        let preview = gtk::ListBox::new();
        preview.add_css_class("side-list");
        preview.set_selection_mode(gtk::SelectionMode::None);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&preview));
        scroll.set_vexpand(true);
        scroll.set_min_content_height(220);
        let pv = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pv.add_css_class("settings-group");
        pv.append(&scroll);
        root.append(&pv);
        let status = gtk::Label::new(None);
        status.add_css_class("settings-sub");
        status.set_xalign(0.0);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status.set_hexpand(true);
        row.append(&status);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("secondary");
        let ok = gtk::Button::with_label("Rename");
        ok.add_css_class("primary");
        row.append(&cancel);
        row.append(&ok);
        root.append(&row);
        win.set_child(Some(&root));

        let danger = self.tokens.borrow().danger.hex();
        let rules = {
            let (find, replace, regex, mcase, keep_ext, template, start, pad, case) =
                (find.clone(), replace.clone(), regex.clone(), mcase.clone(), keep_ext.clone(), template.clone(), start.clone(), pad.clone(), case.clone());
            move || Rules {
                find: find.text().to_string(),
                replace: replace.text().to_string(),
                regex: regex.is_active(),
                match_case: mcase.is_active(),
                template: template.text().to_string(),
                start: start.value() as u32,
                pad: pad.value() as usize,
                case: [Case::Keep, Case::Lower, Case::Upper, Case::Title][case.selected().min(3) as usize],
                keep_ext: keep_ext.is_active(),
            }
        };
        // returns the new names when they can be applied
        let refresh: Rc<dyn Fn() -> Option<Vec<String>>> = {
            let (preview, status, ok, names, others, rules) = (preview.clone(), status.clone(), ok.clone(), names.clone(), others.clone(), rules.clone());
            Rc::new(move || {
                while let Some(c) = preview.first_child() {
                    preview.remove(&c);
                }
                let new = match rename::apply(&rules(), &names) {
                    Ok(n) => n,
                    Err(e) => {
                        status.set_markup(&format!("<span foreground=\"{danger}\">Pattern error: {}</span>", glib::markup_escape_text(&e)));
                        ok.set_sensitive(false);
                        return None;
                    }
                };
                let probs = rename::problems(&new, &others);
                let mut changes = 0;
                let mut bad = 0;
                for ((old, nw), pr) in names.iter().zip(&new).zip(&probs) {
                    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    let a = gtk::Label::new(Some(old));
                    a.add_css_class("file-meta");
                    a.set_xalign(0.0);
                    a.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    a.set_hexpand(true);
                    a.set_width_chars(20);
                    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
                    arrow.set_pixel_size(12);
                    let b = gtk::Label::new(None);
                    b.set_xalign(0.0);
                    b.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    b.set_hexpand(true);
                    b.set_width_chars(20);
                    match pr {
                        Some(why) => {
                            bad += 1;
                            b.set_markup(&format!("<span foreground=\"{danger}\">{} · {why}</span>", glib::markup_escape_text(nw)));
                        }
                        None => {
                            b.set_text(nw);
                            b.add_css_class(if nw != old { "settings-title" } else { "file-meta" });
                        }
                    }
                    if nw != old {
                        changes += 1;
                    }
                    hb.append(&a);
                    hb.append(&arrow);
                    hb.append(&b);
                    preview.append(&hb);
                }
                if bad > 0 {
                    status.set_markup(&format!("<span foreground=\"{danger}\">{bad} problem{} to fix first</span>", if bad == 1 { "" } else { "s" }));
                } else {
                    status.set_text(&format!("{changes} of {} will be renamed", names.len()));
                }
                ok.set_sensitive(bad == 0 && changes > 0);
                (bad == 0 && changes > 0).then_some(new)
            })
        };
        refresh();
        for e in [&find, &replace, &template] {
            let r = refresh.clone();
            e.connect_changed(move |_| {
                r();
            });
        }
        for c in [&regex, &mcase, &keep_ext] {
            let r = refresh.clone();
            c.connect_toggled(move |_| {
                r();
            });
        }
        for s in [&start, &pad] {
            let r = refresh.clone();
            s.connect_value_changed(move |_| {
                r();
            });
        }
        let r = refresh.clone();
        case.connect_selected_notify(move |_| {
            r();
        });

        let (weak, ww, r) = (Rc::downgrade(self), win.downgrade(), refresh.clone());
        ok.connect_clicked(move |_| {
            let (Some(w), Some(new)) = (weak.upgrade(), r()) else { return };
            let todo: Vec<(gio::File, String)> = items.iter().zip(new).map(|((f, _), n)| (f.clone(), n)).collect();
            let n = todo.len();
            jobs::run(&w, &format!("Renaming {n} items"), move |p, c| rename::rename_all(&todo, p, c), |w, done, p, cancelled| {
                let k = done.len();
                if k > 0 {
                    w.undo.borrow_mut().push(UndoOp::Batch(done));
                }
                let (msg, err) = jobs::summary(&format!("Renamed {k} item{} · Ctrl+Z to undo", if k == 1 { "" } else { "s" }), p, cancelled);
                w.show_notice(&msg, err);
            });
            if let Some(win) = ww.upgrade() {
                win.close();
            }
        });
        let ww = win.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = ww.upgrade() {
                w.close()
            }
        });
        let keys = gtk::EventControllerKey::new();
        let ww = win.downgrade();
        keys.connect_key_pressed(move |_, k, _, _| {
            if k == gdk::Key::Escape {
                if let Some(w) = ww.upgrade() {
                    w.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win.add_controller(keys);
        win.present();
        find.grab_focus();
    }
}
