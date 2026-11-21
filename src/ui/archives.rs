//! Extract here / Extract to… / Compress… and Mount disk image (.iso/.img) in the UI.

use super::{UndoOp, Win, display_name, jobs};
use crate::archive::{self, Format};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::PathBuf;
use std::rc::Rc;

pub fn is_disk_image(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".iso") || l.ends_with(".img")
}

fn local_paths(files: &[gio::File]) -> Option<Vec<PathBuf>> {
    files.iter().map(|f| f.path().filter(|_| f.is_native())).collect()
}

impl Win {
    /// `dest` None = next to each archive. `smart` = single folder stays, else wrap.
    pub fn extract(self: &Rc<Self>, files: Vec<gio::File>, dest: Option<gio::File>) {
        let archives: Vec<gio::File> = files.into_iter().filter(|f| f.basename().is_some_and(|b| archive::is_archive(&b.to_string_lossy()))).collect();
        let Some(paths) = local_paths(&archives) else {
            self.show_notice("Archives on network locations: copy them here first", true);
            return;
        };
        if paths.is_empty() {
            return;
        }
        let dest_path = match &dest {
            Some(d) => match d.path() {
                Some(p) => Some(p),
                None => {
                    self.show_notice("Can only extract into a local folder", true);
                    return;
                }
            },
            None => None,
        };
        let n = paths.len();
        let title = if n == 1 { format!("Extracting {}", paths[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("Extracting {n} archives") };
        jobs::run(
            self,
            &title,
            move |p, c| {
                p.total.store(paths.len() as u64, std::sync::atomic::Ordering::Relaxed);
                let mut made = vec![];
                for a in &paths {
                    if c.is_cancelled() {
                        break;
                    }
                    let d = dest_path.clone().or_else(|| a.parent().map(|x| x.to_path_buf())).unwrap_or_default();
                    match archive::extract(a, &d, true, p, c) {
                        Ok(out) => made.push(out),
                        Err(e) if e == "cancelled" => break,
                        Err(e) => {
                            let name = a.file_name().unwrap_or_default().to_string_lossy().to_string();
                            p.errors.lock().unwrap().push(format!("{name}: {e}"));
                        }
                    }
                }
                made
            },
            |w, made, p, cancelled| {
                let names: Vec<String> = made.iter().filter_map(|m| m.file_name().map(|n| n.to_string_lossy().to_string())).collect();
                for m in &made {
                    w.undo.borrow_mut().push(UndoOp::Created(gio::File::for_path(m)));
                }
                let text = match names.len() {
                    0 => "Nothing extracted".to_string(),
                    1 => format!("Extracted to {}", names[0]),
                    k => format!("Extracted {k} archives"),
                };
                let (msg, err) = jobs::summary(&text, p, cancelled);
                w.show_notice(&msg, err);
            },
        );
    }

    pub fn extract_to(self: &Rc<Self>, files: Vec<gio::File>) {
        let fd = gtk::FileDialog::builder().title("Extract to…").modal(true).accept_label("Extract here").build();
        if let Some(cur) = self.active().current_file() {
            fd.set_initial_folder(Some(&cur));
        }
        let weak = Rc::downgrade(self);
        fd.select_folder(Some(&self.window), gio::Cancellable::NONE, move |r| {
            if let (Some(w), Ok(dir)) = (weak.upgrade(), r) {
                w.extract(files, Some(dir));
            }
        });
    }

    pub fn compress_dialog(self: &Rc<Self>, files: Vec<gio::File>) {
        let Some(paths) = local_paths(&files) else {
            self.show_notice("Can only compress local files", true);
            return;
        };
        if paths.is_empty() {
            return;
        }
        let default_name = if paths.len() == 1 {
            archive::stem(&paths[0].file_name().unwrap_or_default().to_string_lossy())
        } else {
            paths[0].parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Archive".into())
        };
        let win = gtk::Window::builder().transient_for(&self.window).modal(true).title("Compress").decorated(false).default_width(420).resizable(false).build();
        win.add_css_class("burrow");
        win.add_css_class("dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("dialog-root");
        let t = gtk::Label::new(Some(&if paths.len() == 1 { format!("Compress “{}”", paths[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("Compress {} items", paths.len()) }));
        t.add_css_class("dialog-title");
        t.set_xalign(0.0);
        t.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        root.append(&t);
        let entry = gtk::Entry::new();
        entry.add_css_class("path-entry");
        entry.set_text(&default_name);
        root.append(&entry);
        let labels: Vec<&str> = Format::ALL.iter().map(|f| f.label()).collect();
        let fmt = gtk::DropDown::from_strings(&labels);
        fmt.add_css_class("settings-dropdown");
        root.append(&fmt);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("secondary");
        let ok = gtk::Button::with_label("Compress");
        ok.add_css_class("primary");
        row.append(&cancel);
        row.append(&ok);
        root.append(&row);
        win.set_child(Some(&root));

        let go = {
            let (weak, ww, entry, fmt) = (Rc::downgrade(self), win.downgrade(), entry.clone(), fmt.clone());
            move || {
                let Some(w) = weak.upgrade() else { return };
                let name = entry.text().trim().to_string();
                if let Some(err) = super::check_name(&name) {
                    w.show_notice(err, true);
                    return;
                }
                let f = Format::ALL[fmt.selected().min(4) as usize];
                let paths = paths.clone();
                jobs::run(&w, &format!("Compressing to {name}{}", f.ext()), move |p, c| archive::compress(&paths, &name, f, p, c), |w, r, p, cancelled| match r {
                    Ok(out) => {
                        w.undo.borrow_mut().push(UndoOp::Created(gio::File::for_path(&out)));
                        let (msg, err) = jobs::summary(&format!("Created {}", out.file_name().unwrap_or_default().to_string_lossy()), p, cancelled);
                        w.show_notice(&msg, err);
                    }
                    Err(e) if e == "cancelled" => w.show_notice("Compress cancelled", false),
                    Err(e) => w.show_notice(&format!("Couldn't compress: {e}"), true),
                });
                if let Some(win) = ww.upgrade() {
                    win.close();
                }
            }
        };
        let g = go.clone();
        ok.connect_clicked(move |_| g());
        entry.connect_activate(move |_| go());
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
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    /// Attach a disk image read-only through UDisks and mount it, then open it.
    pub fn mount_image(self: &Rc<Self>, file: gio::File) {
        let Some(path) = file.path().filter(|_| file.is_native()) else {
            self.show_notice("Only local disk images can be mounted", true);
            return;
        };
        let name = display_name(&file, &self.home);
        jobs::run(
            self,
            &format!("Mounting {name}"),
            move |_p, _c| -> Result<String, String> {
                let out = std::process::Command::new("udisksctl")
                    .args(["loop-setup", "--read-only", "-f"])
                    .arg(&path)
                    .output()
                    .map_err(|e| format!("udisksctl: {e}"))?;
                let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
                if !out.status.success() {
                    return Err(text.trim().to_string());
                }
                // "Mapped file X as /dev/loop0."
                let dev = text.split_whitespace().find(|w| w.starts_with("/dev/loop")).map(|d| d.trim_end_matches('.').to_string()).ok_or_else(|| text.trim().to_string())?;
                // desktops often auto-mount it; give that a moment, then mount ourselves if needed
                std::thread::sleep(std::time::Duration::from_millis(700));
                let candidates = [dev.clone(), format!("{dev}p1")];
                let mut last = String::new();
                for d in candidates {
                    let o = std::process::Command::new("udisksctl").args(["mount", "-b", &d]).output();
                    if let Ok(o) = o {
                        let t = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
                        // "Mounted /dev/loop0 at /run/media/ari/LABEL"
                        if let Some(at) = t.split(" at ").nth(1) {
                            if o.status.success() {
                                return Ok(at.trim().trim_end_matches('.').to_string());
                            }
                        }
                        if t.contains("AlreadyMounted") {
                            if let Some(mp) = t.split('`').nth(1) {
                                return Ok(mp.to_string());
                            }
                        }
                        last = t;
                    }
                }
                Err(last.trim().to_string())
            },
            move |w, r, _p, _cancelled| match r {
                Ok(mp) => {
                    w.show_notice(&format!("Mounted {name}. Eject it from the sidebar when done"), false);
                    w.reload_drives();
                    w.go(super::Loc::Files(gio::File::for_path(mp)), true);
                }
                Err(e) => w.show_notice(&format!("Couldn't mount {name}: {e}"), true),
            },
        );
    }
}
