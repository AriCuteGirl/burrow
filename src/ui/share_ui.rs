//! "Share on the network…": a Samba user share for a folder, with the safe choices as the
//! defaults and a confirmation that spells out who will get access.

use super::dialogs::{self, button_row, dialog_window};
use super::{Win, display_name};
use crate::share::{self, Options, Reach, Risk, Share, Status};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SETUP: &str = "sudo mkdir -p /var/lib/samba/usershares
sudo groupadd -rf sambashare
sudo chown root:sambashare /var/lib/samba/usershares
sudo chmod 1770 /var/lib/samba/usershares
sudo gpasswd -a \"$USER\" sambashare";

const SETUP_CONF: &str = "usershare path = /var/lib/samba/usershares
usershare max shares = 50
usershare owner only = yes";

fn para(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dialog-hint");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_max_width_chars(62);
    l
}

fn code(w: &Rc<Win>, root: &gtk::Box, text: &'static str) {
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hb.add_css_class("code-block");
    let l = gtk::Label::new(Some(text));
    l.add_css_class("code-text");
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.set_selectable(true);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::Char);
    let b = gtk::Button::from_icon_name("edit-copy-symbolic");
    b.add_css_class("eject");
    b.set_valign(gtk::Align::Start);
    b.set_tooltip_text(Some("Copy"));
    let cb = w.window.clipboard();
    b.connect_clicked(move |_| cb.set_text(text));
    hb.append(&l);
    hb.append(&b);
    root.append(&hb);
}

fn warning(text: &str) -> gtk::Label {
    let l = para(text);
    l.remove_css_class("dialog-hint");
    l.add_css_class("form-error");
    l
}

fn close_button(root: &gtk::Box, win: &gtk::Window, label: &str) {
    let b = gtk::Button::with_label(label);
    b.add_css_class("secondary");
    b.set_halign(gtk::Align::End);
    let ww = win.downgrade();
    b.connect_clicked(move |_| {
        if let Some(w) = ww.upgrade() {
            w.close()
        }
    });
    root.append(&b);
}

impl Win {
    pub fn share_dialog(self: &Rc<Self>, dir: gio::File) {
        let Some(path) = dir.path().filter(|_| dir.is_native()) else {
            self.show_notice("Only folders on this computer can be shared", true);
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let looked = gio::spawn_blocking(move || {
                let real = path.canonicalize().map_err(|e| e.to_string())?;
                let risk = share::risk(&real, &glib::home_dir());
                let existing = share::list().into_iter().find(|s| Path::new(&s.path) == real);
                Ok::<_, String>((share::status(), share::reach(), risk, existing, real))
            })
            .await;
            let Some(w) = weak.upgrade() else { return };
            match looked {
                Ok(Ok((status, reach, risk, existing, real))) => w.share_window(status, reach, risk, existing, real),
                Ok(Err(e)) => w.show_notice(&format!("Can't share this: {e}"), true),
                Err(_) => w.show_notice("Couldn't check the sharing setup", true),
            }
        });
    }

    fn share_window(self: &Rc<Self>, status: Status, reach: Reach, risk: Risk, existing: Option<Share>, real: PathBuf) {
        let name = display_name(&gio::File::for_path(&real), &self.home);
        let (win, root) = dialog_window(self.window.upcast_ref(), &format!("Share “{name}”"), 560);

        if let Risk::Refuse(why) = &risk {
            root.append(&warning(&format!("Burrow won't share this folder. {why}")));
            close_button(&root, &win, "Close");
            win.present();
            return;
        }
        match status {
            Status::NoSamba => {
                root.append(&para("Sharing needs Samba, and its “net” tool isn't installed."));
                close_button(&root, &win, "Close");
                win.present();
                return;
            }
            Status::Disabled(e) => {
                root.append(&para(&format!(
                    "Samba is installed, but folder sharing by users is turned off ({e}). Burrow doesn't change system settings. If you want it on, this is the one-time setup:"
                )));
                code(self, &root, SETUP);
                root.append(&para("Then add these lines to the [global] part of /etc/samba/smb.conf, restart Samba, and log out and in again:"));
                code(self, &root, SETUP_CONF);
                if let Some(x) = reach.explain() {
                    root.append(&warning(&x));
                }
                close_button(&root, &win, "Close");
                win.present();
                return;
            }
            Status::Ready => {}
        }
        if let Some(s) = existing {
            let who = if s.guests { "anyone on the network, without a login" } else { "people with a Samba login on this computer" };
            let what = if s.writable { "open and change" } else { "open" };
            root.append(&para(&format!("Already shared as “{}”. It lets {who} {what} the files in it.", s.name)));
            if let Some(x) = reach.explain() {
                root.append(&warning(&x));
            }
            let (cancel, stop) = button_row(&root, "Stop sharing");
            stop.remove_css_class("primary");
            stop.add_css_class("danger");
            cancel.set_label("Close");
            let (weak, ww, n) = (Rc::downgrade(self), win.downgrade(), s.name.clone());
            stop.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.stop_sharing(&n);
                }
                if let Some(w) = ww.upgrade() {
                    w.close();
                }
            });
            let ww = win.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(w) = ww.upgrade() {
                    w.close()
                }
            });
            win.present();
            return;
        }

        root.append(&para(&real.to_string_lossy()));
        let entry = gtk::Entry::new();
        entry.add_css_class("path-entry");
        entry.set_text(&name);
        let l = gtk::Label::new(Some("Share name"));
        l.add_css_class("settings-sub");
        l.set_xalign(0.0);
        root.append(&l);
        root.append(&entry);
        let write = gtk::CheckButton::with_label("Let others add, change and delete files");
        let guests = gtk::CheckButton::with_label("Let anyone in without a login (guests)");
        root.append(&write);
        root.append(&guests);
        let guest_note = warning("Guests means every device on the network, including visitors' phones.");
        guest_note.set_visible(false);
        root.append(&guest_note);
        if reach.guests_blocked {
            guests.set_sensitive(false);
            guests.set_tooltip_text(Some("Your smb.conf doesn't allow guests (map to guest)"));
        }
        let gn = guest_note.clone();
        guests.connect_toggled(move |g| gn.set_visible(g.is_active()));
        if let Some(x) = reach.explain() {
            root.append(&warning(&x));
        }
        root.append(&para("Without guests, people log in with your user name and your Samba password (set it once with smbpasswd in a terminal)."));
        let problem = warning("");
        problem.set_visible(false);
        root.append(&problem);
        let (cancel, ok) = button_row(&root, "Share…");
        let ww = win.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = ww.upgrade() {
                w.close()
            }
        });
        let weak = Rc::downgrade(self);
        let ww = win.downgrade();
        let e2 = entry.clone();
        ok.connect_clicked(move |_| {
            let Some(w) = weak.upgrade() else { return };
            let n = e2.text().trim().to_string();
            if let Some(err) = share::check_name(&n) {
                problem.set_text(err);
                problem.set_visible(true);
                return;
            }
            let o = Options { writable: write.is_active(), guests: guests.is_active() };
            // spell out exactly what this exposes, and to whom
            let who = if o.guests { "ANYONE on your network, without a login," } else { "Anyone with your Samba login" };
            let what = if o.writable { "open, change and delete everything" } else { "open everything" };
            let mut text = format!("{who} will be able to {what} inside {}, including hidden files and subfolders, until you stop sharing.", real.display());
            if let Risk::Warn(why) = &risk {
                text.push_str(&format!("\n\nCareful: {why}"));
            }
            let (weak, real2) = (Rc::downgrade(&w), real.clone());
            if let Some(win) = ww.upgrade() {
                win.close();
            }
            dialogs::confirm(&w.window, &format!("Share as “{n}”?"), &text, "Share it", move || {
                let Some(w) = weak.upgrade() else { return };
                let weak = Rc::downgrade(&w);
                glib::spawn_future_local(async move {
                    let n2 = n.clone();
                    let r = gio::spawn_blocking(move || share::add(&n2, &real2, "", o)).await;
                    let Some(w) = weak.upgrade() else { return };
                    match r {
                        Ok(Ok(())) => {
                            w.show_notice(&format!("“{n}” is shared. Stop it from the Network page or the folder's menu"), false);
                            w.refresh_network();
                        }
                        Ok(Err(e)) => w.show_notice(&format!("Couldn't share: {e}"), true),
                        Err(_) => w.show_notice("Couldn't share the folder", true),
                    }
                });
            });
        });
        entry.connect_activate(move |_| ok.emit_clicked());
        win.present();
        entry.grab_focus();
    }

    pub fn stop_sharing(self: &Rc<Self>, name: &str) {
        let (weak, name) = (Rc::downgrade(self), name.to_string());
        glib::spawn_future_local(async move {
            let n = name.clone();
            let r = gio::spawn_blocking(move || share::remove(&n)).await;
            let Some(w) = weak.upgrade() else { return };
            match r {
                Ok(Ok(())) => w.show_notice(&format!("“{name}” is no longer shared"), false),
                Ok(Err(e)) => w.show_notice(&format!("Couldn't stop sharing: {e}"), true),
                Err(_) => w.show_notice("Couldn't stop sharing", true),
            }
            w.refresh_network();
        });
    }

    pub fn refresh_network(self: &Rc<Self>) {
        for p in self.all_panes() {
            if matches!(&*p.loc.borrow(), super::Loc::Network) {
                p.network_page.refresh(self);
            }
        }
    }
}
