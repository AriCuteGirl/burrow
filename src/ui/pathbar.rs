//! Breadcrumb path bar; click empty space or Ctrl+L to type a path or URI.

use super::Loc;
use super::pane::Pane;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;

pub struct Pathbar {
    pub widget: gtk::Stack,
    crumbs: gtk::Box,
    frame: gtk::Box,
    entry: gtk::Entry,
}

const MAX_CRUMBS: usize = 7;

impl Pathbar {
    pub fn new() -> Pathbar {
        let widget = gtk::Stack::new();
        widget.set_hhomogeneous(false);
        widget.set_interpolate_size(true);
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class("pathbar");
        let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        crumbs.set_valign(gtk::Align::Center);
        frame.append(&crumbs);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        frame.append(&spacer);
        frame.set_tooltip_text(Some("Click to type a path (Ctrl+L)"));
        let entry = gtk::Entry::new();
        entry.add_css_class("path-entry");
        entry.set_placeholder_text(Some("Path or URI: ~/Projects, /mnt/games, smb://host/share"));
        widget.add_named(&frame, Some("crumbs"));
        widget.add_named(&entry, Some("entry"));
        Pathbar { widget, crumbs, frame, entry }
    }

    pub fn connect(&self, w: &Rc<Pane>) {
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(w);
        click.connect_released(move |g, _, x, y| {
            let Some(w) = weak.upgrade() else { return };
            // only empty space; crumb buttons handle their own clicks
            if let Some(widget) = g.widget() {
                if let Some(hit) = widget.pick(x, y, gtk::PickFlags::DEFAULT) {
                    if hit.ancestor(gtk::Button::static_type()).is_some() {
                        return;
                    }
                }
            }
            w.pathbar.start_editing();
        });
        self.frame.add_controller(click);

        let weak = Rc::downgrade(w);
        self.entry.connect_activate(move |e| {
            let Some(w) = weak.upgrade() else { return };
            let text = e.text().trim().to_string();
            w.pathbar.stop_editing();
            if text.is_empty() {
                return;
            }
            match parse_location(&text) {
                Some(f) => w.go(Loc::Files(f), true),
                None => {
                    if let Some(win) = w.win() {
                        win.show_notice("That doesn't look like a path or URI", true)
                    }
                }
            }
            w.files.grab_focus();
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(w);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(w) = weak.upgrade() {
                    w.pathbar.stop_editing();
                    w.files.grab_focus();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(w);
        focus.connect_leave(move |_| {
            if let Some(w) = weak.upgrade() {
                // defer: leaving can happen while the entry is being activated
                glib::idle_add_local_once(move || w.pathbar.stop_editing());
            }
        });
        self.entry.add_controller(focus);
    }

    pub fn start_editing(&self) {
        self.widget.set_visible_child_name("entry");
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    pub fn stop_editing(&self) {
        self.widget.set_visible_child_name("crumbs");
    }

    pub fn set_drives(&self) {
        self.clear();
        let b = crumb("Drives", Some("computer-symbolic"), true);
        self.crumbs.append(&b);
        self.entry.set_text("");
    }

    fn clear(&self) {
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
    }

    pub fn set_file(&self, file: &gio::File, home: &gio::File) {
        self.clear();
        self.entry.set_text(&file.parse_name());
        // walk up to the root, stopping at Home
        let mut chain = vec![];
        let mut cur = Some(file.clone());
        while let Some(f) = cur {
            let at_home = f.equal(home);
            chain.push(f.clone());
            if at_home {
                break;
            }
            cur = f.parent();
        }
        chain.reverse();
        let skip = chain.len().saturating_sub(MAX_CRUMBS);
        if skip > 0 {
            let l = gtk::Label::new(Some("…"));
            l.add_css_class("crumb-sep");
            l.set_margin_start(6);
            self.crumbs.append(&l);
        }
        let last = chain.len() - 1;
        for (i, f) in chain.iter().enumerate().skip(skip) {
            if i > skip || skip > 0 {
                let sep = gtk::Image::from_icon_name("go-next-symbolic");
                sep.add_css_class("crumb-sep");
                sep.set_pixel_size(12);
                self.crumbs.append(&sep);
            }
            let name = super::display_name(f, home);
            let icon = if f.equal(home) {
                Some("user-home-symbolic")
            } else if f.parent().is_none() && f.uri_scheme().as_deref() == Some("trash") {
                Some("user-trash-symbolic")
            } else if f.parent().is_none() {
                Some(if f.is_native() { "drive-harddisk-symbolic" } else { "folder-remote-symbolic" })
            } else {
                None
            };
            let b = crumb(&name, icon, i == last);
            let uri = f.uri().to_string();
            b.connect_clicked(move |b| {
                let _ = b.activate_action("win.goto-uri", Some(&uri.to_variant()));
            });
            self.crumbs.append(&b);
        }
    }
}

fn crumb(name: &str, icon: Option<&str>, current: bool) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("crumb");
    if current {
        b.add_css_class("current");
    }
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(i) = icon {
        let img = gtk::Image::from_icon_name(i);
        img.set_pixel_size(14);
        hb.append(&img);
    }
    let l = gtk::Label::new(Some(name));
    l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    l.set_max_width_chars(28);
    hb.append(&l);
    b.set_child(Some(&hb));
    b
}

/// Typed text -> location. Accepts ~, absolute paths and URIs (smb://, sftp://, trash:///…).
pub fn parse_location(text: &str) -> Option<gio::File> {
    let t = text.trim();
    if t.is_empty() || t.contains('\0') {
        return None;
    }
    let home = glib::home_dir();
    let t = crate::util::expand_tilde(t, &home.to_string_lossy());
    if t.starts_with('/') {
        return Some(gio::File::for_path(t));
    }
    if let Some((scheme, _)) = t.split_once(':') {
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            return Some(gio::File::for_uri(&t));
        }
    }
    None
}
