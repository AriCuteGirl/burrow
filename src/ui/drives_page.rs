//! "Drives" overview, like Windows' This PC: a tile per drive with a big usage bar
//! and "X free of Y".

use super::Win;
use super::sidebar::set_usage;
use crate::drives::Kind;
use gtk::gio;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct DrivesPage {
    pub widget: gtk::ScrolledWindow,
    body: gtk::Box,
    usage: RefCell<Vec<(gio::File, gtk::ProgressBar, gtk::Label)>>,
    pub count: Cell<usize>,
}

impl DrivesPage {
    pub fn new() -> DrivesPage {
        let widget = gtk::ScrolledWindow::new();
        widget.set_hscrollbar_policy(gtk::PolicyType::Never);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_margin_top(24);
        body.set_margin_bottom(24);
        body.set_margin_start(28);
        body.set_margin_end(28);
        widget.set_child(Some(&body));
        DrivesPage { widget, body, usage: RefCell::new(vec![]), count: Cell::new(0) }
    }

    pub fn rebuild(&self, w: &Rc<Win>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        self.usage.borrow_mut().clear();
        let title = gtk::Label::new(Some("Drives"));
        title.add_css_class("page-title");
        title.set_xalign(0.0);
        let sub = gtk::Label::new(Some("Everything plugged in, mounted or on the network"));
        sub.add_css_class("page-subtitle");
        sub.set_xalign(0.0);
        self.body.append(&title);
        self.body.append(&sub);

        let cfg = w.cfg.borrow().clone();
        let drives: Vec<_> = w.drives.borrow().iter().filter(|d| !cfg.drive(&d.key).hidden).cloned().collect();
        self.count.set(drives.len());
        let groups = [
            ("DEVICES AND DRIVES", vec![Kind::System, Kind::Internal, Kind::Removable]),
            ("NETWORK LOCATIONS", vec![Kind::Network]),
        ];
        for (name, kinds) in groups {
            let ds: Vec<_> = drives.iter().filter(|d| kinds.contains(&d.kind)).collect();
            if ds.is_empty() {
                continue;
            }
            let g = gtk::Label::new(Some(name));
            g.add_css_class("group-title");
            g.set_xalign(0.0);
            self.body.append(&g);
            let flow = gtk::FlowBox::new();
            flow.add_css_class("drives");
            flow.set_selection_mode(gtk::SelectionMode::None);
            flow.set_homogeneous(true);
            flow.set_max_children_per_line(4);
            flow.set_min_children_per_line(1);
            flow.set_row_spacing(6);
            flow.set_column_spacing(6);
            flow.set_valign(gtk::Align::Start);
            for d in ds {
                let card = gtk::Button::new();
                card.add_css_class("drive-card");
                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 14);
                let icon = gtk::Image::from_gicon(&w.drive_icon(d));
                icon.set_pixel_size(40);
                icon.add_css_class("card-icon");
                icon.set_valign(gtk::Align::Start);
                hb.append(&icon);
                let vb = gtk::Box::new(gtk::Orientation::Vertical, 6);
                vb.set_hexpand(true);
                let t = gtk::Label::new(Some(&w.drive_display_name(d)));
                t.add_css_class("card-title");
                t.set_xalign(0.0);
                t.set_ellipsize(gtk::pango::EllipsizeMode::End);
                vb.append(&t);
                let bar = gtk::ProgressBar::new();
                bar.add_css_class("usage");
                bar.add_css_class("big");
                let free = gtk::Label::new(Some(if d.mounted() { " " } else { "Not mounted · click to mount" }));
                free.add_css_class("card-sub");
                free.set_xalign(0.0);
                if let Some(root) = &d.root {
                    vb.append(&bar);
                    self.usage.borrow_mut().push((root.clone(), bar, free.clone()));
                    card.set_tooltip_text(Some(&root.parse_name()));
                }
                vb.append(&free);
                hb.append(&vb);
                if d.can_eject() || d.can_unmount() {
                    let b = gtk::Button::from_icon_name("media-eject-symbolic");
                    b.add_css_class("eject");
                    b.set_valign(gtk::Align::Start);
                    b.set_tooltip_text(Some(if d.can_eject() { "Eject" } else { "Unmount" }));
                    let (weak, key) = (Rc::downgrade(w), d.key.clone());
                    b.connect_clicked(move |_| {
                        if let Some(w) = weak.upgrade() {
                            w.eject_drive(&key)
                        }
                    });
                    hb.append(&b);
                }
                card.set_child(Some(&hb));
                let (weak, key) = (Rc::downgrade(w), d.key.clone());
                card.connect_clicked(move |_| {
                    if let Some(w) = weak.upgrade() {
                        w.open_drive(&key)
                    }
                });
                flow.append(&card);
            }
            self.body.append(&flow);
        }
        self.refresh_usage();
    }

    pub fn refresh_usage(&self) {
        for (root, bar, label) in self.usage.borrow().iter() {
            set_usage(root, bar, label);
        }
    }
}
