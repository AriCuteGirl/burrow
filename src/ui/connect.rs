//! Logging in to servers, and the "Connect to server" dialog.
//!
//! The password goes straight from the entry to GVFS, which keeps it in the system keyring
//! if asked to. Burrow never stores, logs or puts it in an address.

use super::dialogs::{button_row, dialog_window};
use super::{Loc, Win};
use crate::config::Item;
use crate::net::{self, Proto};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

fn small(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dialog-hint");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_max_width_chars(52);
    l
}

fn labelled(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("settings-sub");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(w);
    w.set_hexpand(true);
    b.set_hexpand(true);
    b
}

fn entry(placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.add_css_class("path-entry");
    e.set_placeholder_text(Some(placeholder));
    e
}

/// GIO asks through this when a location needs a login or a decision.
pub fn mount_op(w: &Rc<Win>) -> gio::MountOperation {
    let op = gio::MountOperation::new();
    let open: Rc<RefCell<Option<gtk::Window>>> = Rc::new(RefCell::new(None));

    let (weak, slot) = (Rc::downgrade(w), open.clone());
    op.connect_ask_password(move |op, message, user, domain, flags| {
        let Some(w) = weak.upgrade() else { return };
        // without this GIO's default handler answers "unhandled" before the dialog does
        op.stop_signal_emission_by_name("ask-password");
        let win = password_dialog(&w, op, message, user, domain, flags);
        *slot.borrow_mut() = Some(win);
    });

    // e.g. "the host key is unknown / has changed": always the user's decision
    let (weak, slot) = (Rc::downgrade(w), open.clone());
    op.connect_local("ask-question", false, move |v| {
        let op = v[0].get::<gio::MountOperation>().ok()?;
        let w = weak.upgrade()?;
        op.stop_signal_emission_by_name("ask-question");
        let message = v[1].get::<String>().unwrap_or_default();
        let choices = v[2].get::<Vec<String>>().unwrap_or_default();
        *slot.borrow_mut() = Some(question_dialog(&w, &op, &message, &choices));
        None
    });

    op.connect_aborted(move |_| {
        if let Some(win) = open.borrow_mut().take() {
            win.destroy();
        }
    });
    op
}

fn password_dialog(w: &Rc<Win>, op: &gio::MountOperation, message: &str, user: &str, domain: &str, flags: gio::AskPasswordFlags) -> gtk::Window {
    let (win, root) = dialog_window(w.window.upcast_ref(), "Log in", 420);
    root.append(&small(message));
    let anon = gtk::CheckButton::with_label("Connect as a guest, without a login");
    if flags.contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED) {
        root.append(&anon);
    }
    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.append(&form);
    let user_e = entry("");
    user_e.set_text(user);
    let need_user = flags.contains(gio::AskPasswordFlags::NEED_USERNAME);
    if need_user {
        form.append(&labelled("User name", &user_e));
    }
    let dom_e = entry("");
    dom_e.set_text(domain);
    let need_domain = flags.contains(gio::AskPasswordFlags::NEED_DOMAIN);
    if need_domain {
        form.append(&labelled("Domain or workgroup", &dom_e));
    }
    let pass = gtk::PasswordEntry::new();
    pass.add_css_class("path-entry");
    pass.set_show_peek_icon(true);
    pass.set_activates_default(true);
    if flags.contains(gio::AskPasswordFlags::NEED_PASSWORD) {
        form.append(&labelled("Password", &pass));
    }
    let save = gtk::CheckButton::with_label("Remember it in the keyring");
    let can_save = flags.contains(gio::AskPasswordFlags::SAVING_SUPPORTED);
    if can_save {
        form.append(&save);
    }
    let f2 = form.clone();
    anon.connect_toggled(move |a| f2.set_sensitive(!a.is_active()));
    let (cancel, ok) = button_row(&root, "Connect");
    win.set_default_widget(Some(&ok));
    user_e.set_activates_default(true);
    dom_e.set_activates_default(true);

    // answered exactly once: by Connect, or by closing the window any other way
    let pending = Rc::new(RefCell::new(Some(op.clone())));
    let (p, ww, ue, de, pe) = (pending.clone(), win.downgrade(), user_e.clone(), dom_e.clone(), pass.clone());
    ok.connect_clicked(move |_| {
        if let Some(op) = p.borrow_mut().take() {
            if anon.is_active() {
                op.set_anonymous(true);
            } else {
                op.set_anonymous(false);
                if need_user {
                    op.set_username(Some(&ue.text()));
                }
                if need_domain {
                    op.set_domain(Some(&de.text()));
                }
                op.set_password(Some(&pe.text()));
                op.set_password_save(if can_save && save.is_active() { gio::PasswordSave::Permanently } else { gio::PasswordSave::Never });
            }
            op.reply(gio::MountOperationResult::Handled);
        }
        pe.set_text("");
        if let Some(w) = ww.upgrade() {
            w.close();
        }
    });
    win.connect_close_request(move |_| {
        if let Some(op) = pending.borrow_mut().take() {
            op.reply(gio::MountOperationResult::Aborted);
        }
        glib::Propagation::Proceed
    });
    let ww = win.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = ww.upgrade() {
            w.close()
        }
    });
    win.present();
    if need_user && user.is_empty() {
        user_e.grab_focus();
    } else {
        pass.grab_focus();
    }
    win
}

fn question_dialog(w: &Rc<Win>, op: &gio::MountOperation, message: &str, choices: &[String]) -> gtk::Window {
    let mut lines = message.splitn(2, '\n');
    let (win, root) = dialog_window(w.window.upcast_ref(), lines.next().unwrap_or("Question"), 460);
    if let Some(rest) = lines.next().filter(|r| !r.trim().is_empty()) {
        root.append(&small(rest.trim()));
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    root.append(&row);
    let pending = Rc::new(RefCell::new(Some(op.clone())));
    let mut last = None;
    for (i, c) in choices.iter().enumerate() {
        let b = gtk::Button::with_label(c);
        b.add_css_class("secondary");
        let (p, ww) = (pending.clone(), win.downgrade());
        b.connect_clicked(move |_| {
            if let Some(op) = p.borrow_mut().take() {
                op.set_choice(i as i32);
                op.reply(gio::MountOperationResult::Handled);
            }
            if let Some(w) = ww.upgrade() {
                w.close();
            }
        });
        row.append(&b);
        last = Some(b);
    }
    win.connect_close_request(move |_| {
        if let Some(op) = pending.borrow_mut().take() {
            op.reply(gio::MountOperationResult::Aborted);
        }
        glib::Propagation::Proceed
    });
    win.present();
    // the last choice is the cautious one ("Cancel"): Enter must not accept a changed key
    if let Some(b) = last {
        b.grab_focus();
    }
    win
}

impl Win {
    pub fn connect_dialog(self: &Rc<Self>) {
        let (win, root) = dialog_window(self.window.upcast_ref(), "Connect to server", 520);
        let supported: Vec<bool> = Proto::ALL.iter().map(|p| p.supported()).collect();
        let labels: Vec<String> =
            Proto::ALL.iter().zip(&supported).map(|(p, s)| if *s { p.label().to_string() } else { format!("{} · not installed", p.label()) }).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let kind = gtk::DropDown::from_strings(&refs);
        kind.add_css_class("settings-dropdown");
        root.append(&labelled("Kind of server", &kind));

        let host = entry("nas, 192.168.1.20, or a whole address like smb://nas/Media");
        let port = entry("auto");
        port.set_width_chars(6);
        port.set_max_width_chars(6);
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        r.append(&labelled("Server", &host));
        let pb = labelled("Port", &port);
        pb.set_hexpand(false);
        port.set_hexpand(false);
        r.append(&pb);
        root.append(&r);
        let path = entry("Media/Films (optional)");
        let user = entry("optional");
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        r.append(&labelled("Share or folder", &path));
        r.append(&labelled("User name", &user));
        root.append(&r);

        let keep = gtk::CheckButton::with_label("Keep it in the sidebar");
        root.append(&keep);
        let sections: Vec<(String, String)> = self.cfg.borrow().sections.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
        let names: Vec<&str> = sections.iter().map(|s| s.1.as_str()).collect();
        let section = gtk::DropDown::from_strings(&names);
        section.add_css_class("settings-dropdown");
        let label = entry("Name in the sidebar (optional)");
        let keep_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        keep_row.append(&labelled("Name", &label));
        keep_row.append(&labelled("Section", &section));
        keep_row.set_visible(false);
        root.append(&keep_row);
        let kr = keep_row.clone();
        keep.connect_toggled(move |k| kr.set_visible(k.is_active()));

        let note = small("");
        note.set_visible(false);
        root.append(&note);
        let problem = gtk::Label::new(None);
        problem.add_css_class("form-error");
        problem.set_xalign(0.0);
        problem.set_wrap(true);
        problem.set_visible(false);
        root.append(&problem);
        let (cancel, ok) = button_row(&root, "Connect");

        let say = {
            let (note, ok) = (note.clone(), ok.clone());
            move |i: usize| {
                let p = Proto::ALL[i];
                let text = if !supported[i] {
                    "The network layer on this computer (GVFS) has no support for this kind of server."
                } else if p.insecure() {
                    "Not encrypted: anyone on the network can read your password and files. Prefer sftp, ftps or davs."
                } else {
                    ""
                };
                note.set_text(text);
                note.set_visible(!text.is_empty());
                ok.set_sensitive(supported[i]);
            }
        };
        say(0);
        kind.connect_selected_notify(move |k| say((k.selected() as usize).min(Proto::ALL.len() - 1)));

        let go = {
            let (weak, ww) = (Rc::downgrade(self), win.downgrade());
            let (kind, host, port, path, user, keep, label, section, problem) =
                (kind.clone(), host.clone(), port.clone(), path.clone(), user.clone(), keep.clone(), label.clone(), section.clone(), problem.clone());
            move || {
                let Some(w) = weak.upgrade() else { return };
                let typed = host.text().to_string();
                let res = if typed.contains("://") || typed.trim().starts_with("\\\\") {
                    net::parse_address(&typed)
                } else {
                    let p = port.text().trim().to_string();
                    match p.parse::<u16>() {
                        _ if p.is_empty() => Ok(None),
                        Ok(n) if n > 0 => Ok(Some(n)),
                        _ => Err("The port is a number from 1 to 65535".to_string()),
                    }
                    .and_then(|port| net::build_uri(Proto::ALL[kind.selected().min(6) as usize], &typed, port, &user.text(), &path.text()))
                    .map(|u| (u, false))
                };
                let (uri, had_password) = match res {
                    Ok(x) => x,
                    Err(e) => {
                        problem.set_text(&e);
                        problem.set_visible(true);
                        return;
                    }
                };
                if keep.is_active() {
                    if let Some((sid, _)) = sections.get(section.selected() as usize) {
                        w.save_server(&uri, &label.text(), sid);
                    }
                }
                if had_password {
                    w.show_notice("The password in that address was dropped: you'll be asked for it instead", false);
                }
                w.go(Loc::Files(gio::File::for_uri(&uri)), true);
                if let Some(win) = ww.upgrade() {
                    win.close();
                }
            }
        };
        let g = go.clone();
        ok.connect_clicked(move |_| g());
        for e in [&host, &port, &path, &user, &label] {
            let (g, ok) = (go.clone(), ok.clone());
            e.connect_activate(move |_| {
                if ok.is_sensitive() {
                    g()
                }
            });
        }
        let ww = win.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = ww.upgrade() {
                w.close()
            }
        });
        win.present();
        host.grab_focus();
    }

    /// Saved servers are ordinary sidebar entries with an address for a path.
    pub fn save_server(self: &Rc<Self>, uri: &str, label: &str, section: &str) {
        let (uri, _) = net::strip_password(uri);
        let label = Some(label.trim().to_string()).filter(|l| !l.is_empty());
        self.edit_config(|c| {
            let Some(s) = c.sections.iter_mut().find(|s| s.id == section) else { return };
            if !s.items.iter().any(|i| matches!(i, Item::Folder { path, .. } if *path == uri)) {
                s.items.push(Item::Folder { path: uri, label, icon: Some("folder-remote-symbolic".into()) });
            }
        });
    }
}
