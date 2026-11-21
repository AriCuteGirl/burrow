//! Open terminal here, Open with…, and the "Choose application" dialog (which can also set
//! the default app for a file type, like xdg-mime).

use super::{Win, display_name};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

/// Terminals we know how to start in a folder, in order of preference for "auto".
const TERMINALS: &[&str] = &["kitty", "foot", "alacritty", "wezterm", "ghostty", "konsole", "gnome-terminal", "xfce4-terminal", "xterm"];

/// An installed app by its desktop id ("org.gnome.TextEditor.desktop").
fn app_by_id(id: &str) -> Option<gio::AppInfo> {
    gio::AppInfo::all().into_iter().find(|a| a.id().as_deref() == Some(id))
}

fn in_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// The terminal to use: config, else $TERMINAL, else the first known one installed.
pub fn pick_terminal(cfg: &str) -> Option<String> {
    let c = cfg.trim();
    if !c.is_empty() {
        return Some(c.to_string());
    }
    if let Some(t) = std::env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty()) {
        return Some(t);
    }
    TERMINALS.iter().find(|t| in_path(t)).map(|t| t.to_string())
}

impl Win {
    pub fn open_terminal(self: &Rc<Self>, dir: gio::File) {
        let Some(path) = dir.path().filter(|_| dir.is_native()) else {
            self.show_notice("A terminal can only open in a local folder", true);
            return;
        };
        let cfg = self.cfg.borrow().apps.terminal.clone();
        let Some(term) = pick_terminal(&cfg) else {
            self.show_notice("No terminal found. Set one under [apps] terminal in config.toml", true);
            return;
        };
        // the command line may carry arguments ("kitty --single-instance")
        let mut parts = term.split_whitespace();
        let Some(prog) = parts.next() else { return };
        // the working directory is enough for every terminal, no per-terminal flags needed
        match std::process::Command::new(prog).args(parts).current_dir(&path).spawn() {
            Ok(mut child) => {
                // reap it when it exits so it never lingers as a zombie
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => self.show_notice(&format!("Couldn't start {prog}: {e}"), true),
        }
    }

    /// Launch `files` with a specific app (by desktop id).
    pub fn open_with(self: &Rc<Self>, app_id: &str, files: Vec<gio::File>) {
        let Some(app) = app_by_id(app_id) else {
            self.show_notice("That application isn't installed any more", true);
            return;
        };
        let ctx = WidgetExt::display(&self.window).app_launch_context();
        let weak = Rc::downgrade(self);
        let name = app.name().to_string();
        let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        let refs: Vec<&str> = uris.iter().map(|s| s.as_str()).collect();
        app.launch_uris_async(&refs, Some(&ctx), gio::Cancellable::NONE, move |r| {
            if let (Err(e), Some(w)) = (r, weak.upgrade()) {
                w.show_notice(&format!("Couldn't open with {name}: {}", e.message()), true);
            }
        });
    }

    /// Searchable list of every installed app; optionally make the choice the default.
    pub fn choose_app(self: &Rc<Self>, files: Vec<gio::File>, content_type: String) {
        let desc = gio::content_type_get_description(&content_type).to_string();
        let what = if files.len() == 1 { display_name(&files[0], &self.home) } else { format!("{} files", files.len()) };
        let win = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Open with")
            .decorated(false)
            .default_width(460)
            .default_height(560)
            .build();
        win.add_css_class("burrow");
        win.add_css_class("dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("dialog-root");
        let t = gtk::Label::new(Some(&format!("Open “{what}” with")));
        t.add_css_class("dialog-title");
        t.set_xalign(0.0);
        t.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        root.append(&t);
        let search = gtk::SearchEntry::new();
        search.add_css_class("filter-entry");
        search.set_placeholder_text(Some("Search apps"));
        root.append(&search);

        // recommended apps first, then everything else
        let mut apps: Vec<gio::AppInfo> = gio::AppInfo::recommended_for_type(&content_type);
        let rec = apps.len();
        for a in gio::AppInfo::all() {
            if a.should_show() && !apps.iter().any(|x| x.id() == a.id()) {
                apps.push(a);
            }
        }
        apps[rec..].sort_by_key(|a| a.name().to_lowercase());
        let default_id = gio::AppInfo::default_for_type(&content_type, false).and_then(|a| a.id());

        let list = gtk::ListBox::new();
        list.add_css_class("side-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let ids: Rc<Vec<String>> = Rc::new(apps.iter().filter_map(|a| a.id().map(|s| s.to_string())).collect());
        for (i, a) in apps.iter().enumerate() {
            let hb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let img = match a.icon() {
                Some(ic) => gtk::Image::from_gicon(&ic),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            img.set_pixel_size(24);
            hb.append(&img);
            let l = gtk::Label::new(Some(&a.name()));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            l.add_css_class("side-label");
            hb.append(&l);
            if a.id().is_some() && a.id() == default_id {
                let d = gtk::Label::new(Some("default"));
                d.add_css_class("pill");
                hb.append(&d);
            } else if i < rec {
                let d = gtk::Label::new(Some("suggested"));
                d.add_css_class("drive-free");
                hb.append(&d);
            }
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&hb));
            row.set_tooltip_text(Some(&a.name()));
            list.append(&row);
        }
        let names: Rc<Vec<String>> = Rc::new(apps.iter().map(|a| a.name().to_lowercase()).collect());
        let q = Rc::new(RefCell::new(String::new()));
        let (q2, n2) = (q.clone(), names.clone());
        list.set_filter_func(move |r| {
            let q = q2.borrow();
            q.is_empty() || n2.get(r.index() as usize).is_some_and(|n| n.contains(q.as_str()))
        });
        let (q2, l2) = (q.clone(), list.clone());
        search.connect_search_changed(move |e| {
            *q2.borrow_mut() = e.text().trim().to_lowercase();
            l2.invalidate_filter();
        });
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&list));
        scroll.set_vexpand(true);
        root.append(&scroll);

        let always = gtk::CheckButton::with_label(&format!("Always use this app for {desc}"));
        root.append(&always);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("secondary");
        let open = gtk::Button::with_label("Open");
        open.add_css_class("primary");
        row.append(&cancel);
        row.append(&open);
        root.append(&row);
        win.set_child(Some(&root));

        let go = {
            let (weak, ww, list, always, ids, ct) = (Rc::downgrade(self), win.downgrade(), list.clone(), always.clone(), ids.clone(), content_type.clone());
            let files = files.clone();
            move || {
                let (Some(w), Some(r)) = (weak.upgrade(), list.selected_row()) else { return };
                let Some(id) = ids.get(r.index() as usize).cloned() else { return };
                if always.is_active() {
                    if let Some(app) = app_by_id(&id) {
                        match app.set_as_default_for_type(&ct) {
                            Ok(()) => w.show_notice(&format!("{} is now the default for {}", app.name(), gio::content_type_get_description(&ct)), false),
                            Err(e) => w.show_notice(&format!("Couldn't set the default: {}", e.message()), true),
                        }
                    }
                }
                w.open_with(&id, files.clone());
                if let Some(win) = ww.upgrade() {
                    win.close();
                }
            }
        };
        let g = go.clone();
        open.connect_clicked(move |_| g());
        let g = go.clone();
        list.connect_row_activated(move |_, _| g());
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
        search.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn config_terminal_wins() {
        assert_eq!(super::pick_terminal("foot --server").as_deref(), Some("foot --server"));
    }
}
