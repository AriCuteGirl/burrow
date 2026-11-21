//! "New ▸" (folder, text document, shell script, empty file, templates) and shortcuts.
//! Nothing here ever overwrites: files are created with "fail if exists".

use super::{UndoOp, Win, check_name, dialogs, display_name};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

/// What "New ▸" can make.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NewKind {
    Text,
    Script,
    Empty,
}

impl NewKind {
    pub fn parse(s: &str) -> Option<NewKind> {
        match s {
            "txt" => Some(NewKind::Text),
            "sh" => Some(NewKind::Script),
            "empty" => Some(NewKind::Empty),
            _ => None,
        }
    }
    fn default_name(self) -> &'static str {
        match self {
            NewKind::Text => "New text document.txt",
            NewKind::Script => "new-script.sh",
            NewKind::Empty => "New file",
        }
    }
    fn content(self) -> &'static [u8] {
        match self {
            NewKind::Script => b"#!/usr/bin/env bash\nset -euo pipefail\n\n",
            _ => b"",
        }
    }
    fn title(self) -> &'static str {
        match self {
            NewKind::Text => "New text document",
            NewKind::Script => "New shell script",
            NewKind::Empty => "New empty file",
        }
    }
}

/// Files in the XDG templates folder (~/Templates, ~/Šablony…), for "New ▸".
pub fn templates() -> Vec<gio::File> {
    let Some(dir) = glib::user_special_dir(glib::UserDirectory::Templates) else { return vec![] };
    if dir == glib::home_dir() {
        return vec![]; // unset XDG dirs point at home: never list home as templates
    }
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut v: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    v.sort();
    v.into_iter().take(30).map(gio::File::for_path).collect()
}

impl Win {
    pub fn new_file(self: &Rc<Self>, kind: NewKind) {
        let Some(dir) = self.active().current_file() else { return };
        let weak = Rc::downgrade(self);
        dialogs::prompt_text(&self.window, kind.title(), "", kind.default_name(), move |name| {
            let Some(w) = weak.upgrade() else { return };
            if let Some(err) = check_name(&name) {
                w.show_notice(err, true);
                return;
            }
            let f = dir.child(&name);
            let weak = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let res = async {
                    let out = f.create_future(gio::FileCreateFlags::NONE, glib::Priority::DEFAULT).await?;
                    let body = kind.content();
                    if !body.is_empty() {
                        out.write_all_future(body, glib::Priority::DEFAULT).await.map_err(|(_, e)| e)?;
                    }
                    out.close_future(glib::Priority::DEFAULT).await?;
                    if kind == NewKind::Script {
                        // rwxr-xr-x so it runs straight away
                        let info = gio::FileInfo::new();
                        info.set_attribute_uint32("unix::mode", 0o755);
                        f.set_attributes_future(&info, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT).await?;
                    }
                    Ok::<(), glib::Error>(())
                }
                .await;
                let Some(w) = weak.upgrade() else { return };
                match res {
                    Ok(()) => {
                        w.undo.borrow_mut().push(UndoOp::Created(f));
                        w.show_notice(&format!("Created {name}"), false);
                    }
                    Err(e) if e.matches(gio::IOErrorEnum::Exists) => w.show_notice(&format!("“{name}” already exists here"), true),
                    Err(e) => w.show_notice(&format!("Couldn't create {name}: {}", e.message()), true),
                }
            });
        });
    }

    pub fn new_from_template(self: &Rc<Self>, template: gio::File) {
        let Some(dir) = self.active().current_file() else { return };
        let initial = display_name(&template, &self.home);
        let weak = Rc::downgrade(self);
        dialogs::prompt_text(&self.window, "New from template", "", &initial, move |name| {
            let Some(w) = weak.upgrade() else { return };
            if let Some(err) = check_name(&name) {
                w.show_notice(err, true);
                return;
            }
            let dest = dir.child(&name);
            let weak = Rc::downgrade(&w);
            let d2 = dest.clone();
            // no OVERWRITE flag: fails if the name is taken
            template.copy_async(&dest, gio::FileCopyFlags::NONE, glib::Priority::DEFAULT, gio::Cancellable::NONE, None, move |r| {
                let Some(w) = weak.upgrade() else { return };
                match r {
                    Ok(()) => {
                        w.undo.borrow_mut().push(UndoOp::Created(d2));
                        w.show_notice(&format!("Created {name}"), false);
                    }
                    Err(e) if e.matches(gio::IOErrorEnum::Exists) => w.show_notice(&format!("“{name}” already exists here"), true),
                    Err(e) => w.show_notice(&format!("Couldn't create {name}: {}", e.message()), true),
                }
            });
        });
    }

    /// Ask where to put it, then make a shortcut (symbolic link) to `target` there.
    pub fn create_shortcut(self: &Rc<Self>, target: gio::File) {
        let Some(tpath) = target.path().filter(|_| target.is_native()) else {
            self.show_notice("Shortcuts can only point at local files and folders", true);
            return;
        };
        let fd = gtk::FileDialog::builder().title("Put the shortcut in…").modal(true).accept_label("Put it here").build();
        if let Some(cur) = self.active().current_file() {
            fd.set_initial_folder(Some(&cur));
        }
        let weak = Rc::downgrade(self);
        fd.select_folder(Some(&self.window), gio::Cancellable::NONE, move |r| {
            let (Some(w), Ok(dir)) = (weak.upgrade(), r) else { return };
            let Some(dpath) = dir.path() else {
                w.show_notice("Shortcuts can only be put in local folders", true);
                return;
            };
            let name = tpath.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Shortcut".into());
            let weak = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || {
                    let free = crate::ops::free_name(&name, |n| dpath.join(n).symlink_metadata().is_ok());
                    let link = dpath.join(&free);
                    std::os::unix::fs::symlink(&tpath, &link).map(|_| link)
                })
                .await;
                let Some(w) = weak.upgrade() else { return };
                match res {
                    Ok(Ok(link)) => {
                        let f = gio::File::for_path(&link);
                        w.show_notice(&format!("Shortcut created in {}", display_name(&dir, &w.home)), false);
                        w.undo.borrow_mut().push(UndoOp::Created(f));
                    }
                    Ok(Err(e)) => w.show_notice(&format!("Couldn't create the shortcut: {e}"), true),
                    Err(_) => w.show_notice("Couldn't create the shortcut", true),
                }
            });
        });
    }
}
