//! "Network" overview: connected and saved servers, servers found on the LAN, and what
//! this computer shares.

use super::{Loc, Win, file_from_config_path, icon_from_str};
use crate::config::Item;
use crate::drives::Kind;
use crate::net::{self, Found};
use crate::share;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Outcome of the last look around the LAN (shared by rebuilds, so they don't rescan).
#[derive(Default)]
enum Scan {
    #[default]
    Never,
    Running,
    Done(Vec<Found>),
    Failed(String),
}

pub struct NetworkPage {
    pub widget: gtk::ScrolledWindow,
    body: gtk::Box,
    scan: RefCell<Scan>,
    shares: RefCell<Vec<share::Share>>,
    pub count: Cell<usize>,
}

fn group_label(text: &str) -> gtk::Label {
    let g = gtk::Label::new(Some(text));
    g.add_css_class("group-title");
    g.set_xalign(0.0);
    g
}

fn hint(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("state-text");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l
}

fn card(icon: &gio::Icon, title: &str, sub: &str, extra: Option<&gtk::Button>) -> gtk::Button {
    let c = gtk::Button::new();
    c.add_css_class("drive-card");
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let img = gtk::Image::from_gicon(icon);
    img.set_pixel_size(32);
    img.add_css_class("card-icon");
    hb.append(&img);
    let vb = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vb.set_hexpand(true);
    vb.set_valign(gtk::Align::Center);
    for (text, class) in [(title, "card-title"), (sub, "card-sub")] {
        let l = gtk::Label::new(Some(text));
        l.add_css_class(class);
        l.set_xalign(0.0);
        l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        vb.append(&l);
    }
    hb.append(&vb);
    if let Some(b) = extra {
        hb.append(b);
    }
    c.set_child(Some(&hb));
    c.set_tooltip_text(Some(sub));
    c
}

fn flow(parent: &gtk::Box) -> gtk::FlowBox {
    let f = gtk::FlowBox::new();
    f.add_css_class("drives");
    f.set_selection_mode(gtk::SelectionMode::None);
    f.set_homogeneous(true);
    f.set_max_children_per_line(4);
    f.set_row_spacing(6);
    f.set_column_spacing(6);
    f.set_valign(gtk::Align::Start);
    parent.append(&f);
    f
}

impl NetworkPage {
    pub fn new() -> NetworkPage {
        let widget = gtk::ScrolledWindow::new();
        widget.set_hscrollbar_policy(gtk::PolicyType::Never);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_margin_top(24);
        body.set_margin_bottom(24);
        body.set_margin_start(28);
        body.set_margin_end(28);
        widget.set_child(Some(&body));
        NetworkPage { widget, body, scan: RefCell::default(), shares: RefCell::new(vec![]), count: Cell::new(0) }
    }

    /// Look around the LAN and list our own shares, off the UI thread.
    pub fn refresh(self: &Rc<Self>, w: &Rc<Win>) {
        if matches!(*self.scan.borrow(), Scan::Running) {
            return;
        }
        *self.scan.borrow_mut() = Scan::Running;
        self.rebuild(w);
        let (me, weak) = (Rc::downgrade(self), Rc::downgrade(w));
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(|| (net::discover(Duration::from_secs(6)), share::list())).await;
            let (Some(me), Some(w)) = (me.upgrade(), weak.upgrade()) else { return };
            match res {
                Ok((found, shares)) => {
                    *me.shares.borrow_mut() = shares;
                    *me.scan.borrow_mut() = match found {
                        Ok(f) => Scan::Done(f),
                        Err(e) => Scan::Failed(e),
                    };
                }
                Err(_) => *me.scan.borrow_mut() = Scan::Failed("the search stopped unexpectedly".into()),
            }
            me.rebuild(&w);
            w.update_status();
        });
    }

    pub fn rebuild(self: &Rc<Self>, w: &Rc<Win>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_hexpand(true);
        for (text, class) in [("Network", "page-title"), ("Servers, other computers, and what you share", "page-subtitle")] {
            let l = gtk::Label::new(Some(text));
            l.add_css_class(class);
            l.set_xalign(0.0);
            titles.append(&l);
        }
        head.append(&titles);
        let again = gtk::Button::with_label("Search again");
        again.add_css_class("secondary");
        again.set_valign(gtk::Align::Center);
        again.set_sensitive(!matches!(*self.scan.borrow(), Scan::Running));
        let (me, weak) = (Rc::downgrade(self), Rc::downgrade(w));
        again.connect_clicked(move |_| {
            if let (Some(me), Some(w)) = (me.upgrade(), weak.upgrade()) {
                me.refresh(&w);
            }
        });
        head.append(&again);
        let connect = gtk::Button::with_label("Connect to server…");
        connect.add_css_class("primary");
        connect.set_valign(gtk::Align::Center);
        connect.set_action_name(Some("win.connect"));
        head.append(&connect);
        self.body.append(&head);

        let mut total = 0;
        let go = |uri: String| {
            let weak = Rc::downgrade(w);
            move |_: &gtk::Button| {
                if let Some(w) = weak.upgrade() {
                    w.go(Loc::Files(gio::File::for_uri(&uri)), true);
                }
            }
        };
        let remote: gio::Icon = gio::ThemedIcon::new("folder-remote-symbolic").upcast();

        // ---------------- connected now
        let mounted: Vec<_> = w.drives.borrow().iter().filter(|d| d.kind == Kind::Network && d.root.is_some()).cloned().collect();
        if !mounted.is_empty() {
            self.body.append(&group_label("CONNECTED"));
            let f = flow(&self.body);
            for d in &mounted {
                total += 1;
                let root = d.root.clone().unwrap();
                let off = gtk::Button::from_icon_name("media-eject-symbolic");
                off.add_css_class("eject");
                off.set_valign(gtk::Align::Center);
                off.set_tooltip_text(Some("Disconnect"));
                let (weak, key) = (Rc::downgrade(w), d.key.clone());
                off.connect_clicked(move |_| {
                    if let Some(w) = weak.upgrade() {
                        w.eject_drive(&key);
                    }
                });
                let c = card(&w.drive_icon(d), &w.drive_display_name(d), &root.uri(), Some(&off));
                c.connect_clicked(go(root.uri().to_string()));
                f.append(&c);
            }
        }

        // ---------------- saved in the sidebar
        let mut saved = vec![];
        for s in &w.cfg.borrow().sections {
            for i in &s.items {
                if let Item::Folder { path, label, icon } = i {
                    if net::is_network_uri(path) && !mounted.iter().any(|d| d.root.as_ref().is_some_and(|r| r.equal(&file_from_config_path(path)))) {
                        saved.push((path.clone(), label.clone(), icon.clone()));
                    }
                }
            }
        }
        if !saved.is_empty() {
            self.body.append(&group_label("SAVED"));
            let f = flow(&self.body);
            for (path, label, icon) in saved {
                total += 1;
                let ic = icon.as_deref().map(icon_from_str).unwrap_or_else(|| remote.clone());
                let file = file_from_config_path(&path);
                let name = label.unwrap_or_else(|| super::display_name(&file, &w.home));
                let c = card(&ic, &name, &path, None);
                c.connect_clicked(go(path));
                f.append(&c);
            }
        }

        // ---------------- found on the LAN
        self.body.append(&group_label("FOUND ON YOUR NETWORK"));
        match &*self.scan.borrow() {
            Scan::Never | Scan::Running => {
                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let sp = gtk::Spinner::new();
                sp.start();
                hb.append(&sp);
                hb.append(&hint("Looking around…"));
                self.body.append(&hb);
            }
            Scan::Failed(e) => self.body.append(&hint(&format!("Couldn't search the network: {e}. You can still use Connect to server."))),
            Scan::Done(list) if list.is_empty() => self.body.append(&hint(
                "Nothing announced itself. Many Windows PCs and NAS boxes don't: use Connect to server with their name or IP address.",
            )),
            Scan::Done(list) => {
                let f = flow(&self.body);
                for x in list {
                    total += 1;
                    let ok = x.proto.supported();
                    let sub = if ok { format!("{} · {}", x.proto.label(), x.host) } else { format!("{} · not supported by this computer's GVFS", x.proto.label()) };
                    let c = card(&gio::ThemedIcon::new("network-server-symbolic").upcast(), &x.name, &sub, None);
                    c.set_sensitive(ok);
                    c.connect_clicked(go(x.uri.clone()));
                    f.append(&c);
                }
            }
        }

        // ---------------- shared by this computer
        let shares = self.shares.borrow().clone();
        if !shares.is_empty() {
            self.body.append(&group_label("SHARED BY THIS COMPUTER"));
            let f = flow(&self.body);
            for s in shares {
                total += 1;
                let stop = gtk::Button::from_icon_name("window-close-symbolic");
                stop.add_css_class("eject");
                stop.set_valign(gtk::Align::Center);
                stop.set_tooltip_text(Some("Stop sharing"));
                let (weak, name) = (Rc::downgrade(w), s.name.clone());
                stop.connect_clicked(move |_| {
                    if let Some(w) = weak.upgrade() {
                        w.stop_sharing(&name);
                    }
                });
                let who = if s.guests { "anyone, no login" } else { "login needed" };
                let what = if s.writable { "can change files" } else { "read only" };
                let c = card(&gio::ThemedIcon::new("folder-publicshare-symbolic").upcast(), &s.name, &format!("{} · {who} · {what}", s.path), Some(&stop));
                c.connect_clicked(go(gio::File::for_path(&s.path).uri().to_string()));
                f.append(&c);
            }
        }
        self.count.set(total);
    }
}
