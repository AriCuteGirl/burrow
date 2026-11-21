//! Sidebar: title, Drives overview entry, then the user's sections (favorites + drives).
//! Rebuilt from config + drive list on change; usage bars update in place.

use super::{Loc, Win, file_from_config_path, icon_from_str};
use crate::config::Item;
use crate::drives::{self, Drive};
use crate::util;
use gtk::prelude::*;
use gtk::gio;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
enum Target {
    DrivesPage,
    Folder(gio::File),
    Drive { key: String, root: Option<gio::File> },
}

pub struct Sidebar {
    pub widget: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    body: gtk::Box,
    lists: RefCell<Vec<(gtk::ListBox, Rc<Vec<Target>>)>>,
    usage: RefCell<Vec<(gio::File, gtk::ProgressBar, gtk::Label)>>,
}

impl Sidebar {
    pub fn new() -> Sidebar {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("sidebar");
        widget.set_size_request(220, -1);
        let title = gtk::Label::new(Some("Burrow"));
        title.add_css_class("app-title");
        title.set_xalign(0.0);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("app-subtitle");
        subtitle.set_xalign(0.0);
        widget.append(&title);
        widget.append(&subtitle);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_vexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
        scroll.set_child(Some(&body));
        widget.append(&scroll);
        Sidebar { widget, title, subtitle, body, lists: RefCell::new(vec![]), usage: RefCell::new(vec![]) }
    }

    pub fn rebuild(&self, w: &Rc<Win>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        self.lists.borrow_mut().clear();
        self.usage.borrow_mut().clear();
        let t = w.tokens.borrow().clone();
        self.title.set_text("Burrow");
        self.subtitle.set_text(&format!("{} rice", t.rice_name));

        let cfg = w.cfg.borrow().clone();
        let drives = w.drives.borrow().clone();

        if cfg.view.drives_page {
            let (list, targets) = (self.new_list(w), vec![Target::DrivesPage]);
            list.append(&row(&gio::ThemedIcon::new("computer-symbolic").upcast(), "Drives", None));
            self.add_list(list, targets);
        }

        for (si, s) in cfg.sections.iter().enumerate() {
            let list = self.new_list(w);
            let mut targets = vec![];
            for it in &s.items {
                match it {
                    Item::Folder { path, label, icon } => {
                        let f = file_from_config_path(path);
                        let name = label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| super::display_name(&f, &w.home));
                        let icon = icon.as_deref().map(icon_from_str).unwrap_or_else(|| gio::ThemedIcon::new("folder-symbolic").upcast());
                        let r = row(&icon, &name, None);
                        r.set_tooltip_text(Some(&f.parse_name()));
                        list.append(&r);
                        targets.push(Target::Folder(f));
                    }
                    Item::Drive { key } => {
                        if let Some(d) = drives.iter().find(|d| &d.key == key) {
                            if !cfg.drive(key).hidden {
                                list.append(&self.drive_row(w, d));
                                targets.push(Target::Drive { key: d.key.clone(), root: d.root.clone() });
                            }
                        }
                    }
                }
            }
            if s.auto_drives {
                for d in drives.iter().filter(|d| cfg.section_for_drive(&d.key) == Some(s.id.as_str())) {
                    let placed = s.items.iter().any(|i| matches!(i, Item::Drive { key } if *key == d.key));
                    if placed || cfg.drive(&d.key).hidden {
                        continue;
                    }
                    list.append(&self.drive_row(w, d));
                    targets.push(Target::Drive { key: d.key.clone(), root: d.root.clone() });
                }
            }

            // header
            let accent = s.accent.as_deref().and_then(crate::theme::Rgb::parse).unwrap_or(t.accent_hot);
            let header = gtk::Button::new();
            header.add_css_class("section-header");
            let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("section-dot");
            dot.set_valign(gtk::Align::Center);
            set_bg(&dot, &accent.hex());
            if let Some(i) = &s.icon {
                let img = gtk::Image::from_gicon(&icon_from_str(i));
                img.add_css_class("section-chevron");
                hb.append(&img);
            } else {
                hb.append(&dot);
            }
            let lbl = gtk::Label::new(Some(&s.name.to_uppercase()));
            lbl.add_css_class("section-title");
            lbl.set_hexpand(true);
            lbl.set_xalign(0.0);
            hb.append(&lbl);
            let chev = gtk::Image::from_icon_name(if s.collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" });
            chev.add_css_class("section-chevron");
            hb.append(&chev);
            header.set_child(Some(&hb));
            header.set_tooltip_text(Some(if s.collapsed { "Expand" } else { "Collapse" }));

            let rev = gtk::Revealer::new();
            rev.set_transition_duration(if w.reduced_motion() { 0 } else { 140 });
            rev.set_reveal_child(!s.collapsed);
            rev.set_child(Some(&list));
            let weak = Rc::downgrade(w);
            let rev2 = rev.clone();
            header.connect_clicked(move |h| {
                let Some(w) = weak.upgrade() else { return };
                let collapsed = rev2.reveals_child();
                rev2.set_reveal_child(!collapsed);
                chev.set_icon_name(Some(if collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" }));
                h.set_tooltip_text(Some(if collapsed { "Expand" } else { "Collapse" }));
                if let Some(s) = w.cfg.borrow_mut().sections.get_mut(si) {
                    s.collapsed = collapsed;
                }
                w.save_config();
            });
            self.body.append(&header);
            self.body.append(&rev);
            self.lists.borrow_mut().push((list, Rc::new(targets)));
        }
        self.refresh_usage();
        self.sync_selection(&w.loc.borrow());
    }

    fn new_list(&self, w: &Rc<Win>) -> gtk::ListBox {
        let list = gtk::ListBox::new();
        list.add_css_class("side-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        let weak = Rc::downgrade(w);
        list.connect_row_activated(move |l, r| {
            let Some(w) = weak.upgrade() else { return };
            let Some(targets) = w.sidebar.lists.borrow().iter().find(|(x, _)| x == l).map(|(_, t)| t.clone()) else { return };
            match targets.get(r.index() as usize) {
                Some(Target::DrivesPage) => w.go(Loc::Drives, true),
                Some(Target::Folder(f)) => w.go(Loc::Files(f.clone()), true),
                Some(Target::Drive { key, .. }) => w.open_drive(key),
                None => {}
            }
        });
        list
    }

    fn add_list(&self, list: gtk::ListBox, targets: Vec<Target>) {
        self.body.append(&list);
        self.lists.borrow_mut().push((list, Rc::new(targets)));
    }

    fn drive_row(&self, w: &Rc<Win>, d: &Drive) -> gtk::ListBoxRow {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("usage");
        let free = gtk::Label::new(Some(if d.mounted() { " " } else { "Not mounted" }));
        free.add_css_class("drive-free");
        free.set_xalign(0.0);
        let extra = gtk::Box::new(gtk::Orientation::Vertical, 3);
        if d.mounted() {
            extra.append(&bar);
        }
        extra.append(&free);
        let r = row(&w.drive_icon(d), &w.drive_display_name(d), Some(&extra));
        if let Some(root) = &d.root {
            self.usage.borrow_mut().push((root.clone(), bar, free));
            r.set_tooltip_text(Some(&root.parse_name()));
        }
        if d.can_eject() || d.can_unmount() {
            let b = gtk::Button::from_icon_name("media-eject-symbolic");
            b.add_css_class("eject");
            b.set_valign(gtk::Align::Center);
            b.set_tooltip_text(Some(if d.can_eject() { "Eject" } else { "Unmount" }));
            let weak = Rc::downgrade(w);
            let key = d.key.clone();
            b.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.eject_drive(&key)
                }
            });
            if let Some(hb) = r.child().and_downcast::<gtk::Box>() {
                hb.append(&b);
            }
        }
        r
    }

    pub fn refresh_usage(&self) {
        for (root, bar, label) in self.usage.borrow().iter() {
            set_usage(root, bar, label);
        }
    }

    pub fn sync_selection(&self, loc: &Loc) {
        for (list, targets) in self.lists.borrow().iter() {
            let hit = targets.iter().position(|t| match (t, loc) {
                (Target::DrivesPage, Loc::Drives) => true,
                (Target::Folder(f), Loc::Files(l)) => f.equal(l),
                (Target::Drive { root: Some(r), .. }, Loc::Files(l)) => r.equal(l),
                _ => false,
            });
            match hit.and_then(|i| list.row_at_index(i as i32)) {
                Some(r) => list.select_row(Some(&r)),
                None => list.unselect_all(),
            }
        }
    }
}

/// Fills a usage bar + "X free of Y" label asynchronously (Windows-style).
pub fn set_usage(root: &gio::File, bar: &gtk::ProgressBar, label: &gtk::Label) {
    let bar = bar.downgrade();
    let label = label.downgrade();
    drives::query_usage(root, move |u| {
        let (Some(bar), Some(label)) = (bar.upgrade(), label.upgrade()) else { return };
        match u.and_then(|(size, free)| util::usage(size, free)) {
            Some(u) => {
                bar.set_fraction(u.fraction);
                bar.set_visible(true);
                if u.full {
                    bar.add_css_class("full");
                } else {
                    bar.remove_css_class("full");
                }
                label.set_text(&u.text);
                bar.set_tooltip_text(Some(&format!("{:.0}% used", u.fraction * 100.0)));
            }
            None => {
                bar.set_visible(false);
                label.set_text("");
                label.set_visible(false);
            }
        }
    });
}

fn row(icon: &gio::Icon, name: &str, extra: Option<&gtk::Box>) -> gtk::ListBoxRow {
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let img = gtk::Image::from_gicon(icon);
    img.set_pixel_size(16);
    img.set_valign(if extra.is_some() { gtk::Align::Start } else { gtk::Align::Center });
    if extra.is_some() {
        img.set_margin_top(1);
    }
    hb.append(&img);
    let vb = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vb.set_hexpand(true);
    let l = gtk::Label::new(Some(name));
    l.add_css_class("side-label");
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    vb.append(&l);
    if let Some(e) = extra {
        vb.append(e);
    }
    hb.append(&vb);
    let r = gtk::ListBoxRow::new();
    r.set_child(Some(&hb));
    r
}

/// Per-widget background colour (section dots use the section's own accent).
fn set_bg(w: &gtk::Box, hex: &str) {
    let p = gtk::CssProvider::new();
    p.load_from_string(&format!("box {{ background: {hex}; }}"));
    #[allow(deprecated)]
    w.style_context().add_provider(&p, gtk::STYLE_PROVIDER_PRIORITY_USER + 2);
}
