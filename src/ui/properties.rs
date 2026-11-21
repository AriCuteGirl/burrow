//! Properties dialog: what it is, where, how big (folders counted in the background),
//! dates, owner, and permissions you can change (if you own it).

use super::{Win, display_name};
use crate::util;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::Cell;
use std::rc::Rc;

const ATTRS: &str = "standard::*,time::*,unix::*,owner::*,access::*";

fn fmt_time(t: Option<glib::DateTime>) -> String {
    t.and_then(|d| d.to_local().ok())
        .and_then(|d| d.format("%-d %B %Y, %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".into())
}

fn unix_attr(info: &gio::FileInfo, attr: &str) -> Option<glib::DateTime> {
    info.has_attribute(attr).then(|| info.attribute_uint64(attr)).and_then(|t| glib::DateTime::from_unix_local(t as i64).ok())
}

fn value_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_selectable(true);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.add_css_class("settings-title");
    l
}

fn key_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_valign(gtk::Align::Start);
    l.add_css_class("settings-sub");
    l
}

impl Win {
    pub fn properties(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        let win = gtk::Window::builder()
            .transient_for(&self.window)
            .title("Properties")
            .decorated(false)
            .default_width(460)
            .build();
        win.add_css_class("burrow");
        win.add_css_class("dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("dialog-root");
        win.set_child(Some(&root));

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icon = gtk::Image::from_icon_name("text-x-generic");
        icon.set_pixel_size(48);
        let title = gtk::Label::new(None);
        title.add_css_class("dialog-title");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_selectable(true);
        head.append(&icon);
        head.append(&title);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat-btn");
        close.set_valign(gtk::Align::Start);
        head.append(&close);
        root.append(&head);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(8);
        grid.set_column_spacing(16);
        grid.add_css_class("settings-group");
        grid.set_margin_top(4);
        let inner_pad = 12;
        grid.set_margin_start(0);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("settings-group");
        grid.remove_css_class("settings-group");
        grid.set_margin_top(inner_pad);
        grid.set_margin_bottom(inner_pad);
        grid.set_margin_start(inner_pad);
        grid.set_margin_end(inner_pad);
        card.append(&grid);
        root.append(&card);
        let row = Rc::new(Cell::new(0));
        let add = {
            let (grid, row) = (grid.clone(), row.clone());
            move |k: &str, v: &gtk::Widget| {
                let r = row.get();
                grid.attach(&key_label(k), 0, r, 1, 1);
                v.set_hexpand(true);
                grid.attach(v, 1, r, 1, 1);
                row.set(r + 1);
            }
        };

        let size_label = value_label("Counting…");
        if files.len() > 1 {
            title.set_text(&format!("{} items", files.len()));
            icon.set_icon_name(Some("edit-select-all-symbolic"));
            let parent = files[0].parent().map(|p| p.parse_name().to_string()).unwrap_or_default();
            add("Location", value_label(&parent).upcast_ref());
            add("Total size", size_label.upcast_ref());
        } else {
            let f = files[0].clone();
            title.set_text(&display_name(&f, &self.home));
            let weak_win = win.downgrade();
            let (icon2, add2, home, w2) = (icon.clone(), add.clone(), self.home.clone(), Rc::downgrade(self));
            let size2 = size_label.clone();
            let root2 = root.clone();
            f.clone().query_info_async(ATTRS, gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                let (Some(_win), Some(w)) = (weak_win.upgrade(), w2.upgrade()) else { return };
                let info = match r {
                    Ok(i) => i,
                    Err(e) => {
                        add2("Error", value_label(e.message()).upcast_ref());
                        return;
                    }
                };
                if let Some(ic) = info.icon() {
                    icon2.set_from_gicon(&ic);
                }
                let is_dir = info.file_type() == gio::FileType::Directory;
                let ctype = info.content_type().map(|c| c.to_string()).unwrap_or_default();
                let desc = if is_dir { "Folder".to_string() } else { format!("{} ({ctype})", gio::content_type_get_description(&ctype)) };
                add2("Type", value_label(&desc).upcast_ref());
                if info.is_symlink() {
                    let t = info.symlink_target().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    add2("Shortcut to", value_label(&t).upcast_ref());
                }
                let loc = f.parent().map(|p| p.parse_name().to_string()).unwrap_or_else(|| f.parse_name().to_string());
                add2("Location", value_label(&loc).upcast_ref());
                if is_dir {
                    add2("Contents", size2.upcast_ref());
                } else {
                    let n = info.size().max(0) as u64;
                    size2.set_text(&format!("{} ({n} bytes)", util::human_size(n)));
                    add2("Size", size2.upcast_ref());
                }
                add2("Modified", value_label(&fmt_time(info.modification_date_time())).upcast_ref());
                add2("Accessed", value_label(&fmt_time(unix_attr(&info, "time::access"))).upcast_ref());
                if info.has_attribute("time::created") {
                    add2("Created", value_label(&fmt_time(unix_attr(&info, "time::created"))).upcast_ref());
                }
                let owner = info.attribute_string("owner::user").map(|s| s.to_string()).unwrap_or_default();
                let group = info.attribute_string("owner::group").map(|s| s.to_string()).unwrap_or_default();
                if !owner.is_empty() {
                    add2("Owner", value_label(&format!("{owner} · group {group}")).upcast_ref());
                }
                if info.has_attribute("unix::mode") {
                    let mine = info.attribute_uint32("unix::uid") == nix_uid();
                    permissions(&w, &root2, &f, info.attribute_uint32("unix::mode"), is_dir, mine);
                }
                let _ = home;
            });
        }

        // sizes of folders / multiple items: counted on a worker thread
        {
            let cancel = gio::Cancellable::new();
            let c2 = cancel.clone();
            win.connect_close_request(move |_| {
                c2.cancel();
                glib::Propagation::Proceed
            });
            let fl = files.clone();
            let single = files.len() == 1;
            let sl = size_label.downgrade();
            glib::spawn_future_local(async move {
                let c = cancel.clone();
                let Ok((n, bytes)) = gio::spawn_blocking(move || crate::transfer::tree_stats(&fl, &c)).await else { return };
                if let Some(l) = sl.upgrade() {
                    // a single plain file: its size is already shown
                    if !cancel.is_cancelled() && !(single && n <= 1) {
                        let items = n.saturating_sub(if single { 1 } else { 0 });
                        l.set_text(&format!("{} · {items} item{}", util::human_size(bytes), if items == 1 { "" } else { "s" }));
                    }
                }
            });
        }

        let ww = win.downgrade();
        close.connect_clicked(move |_| {
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
    }
}

fn nix_uid() -> u32 {
    // the uid of our own process, from /proc (no libc dependency)
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()))
        .unwrap_or(u32::MAX)
}

/// Owner/Group/Others × Read/Write/Execute toggles, applied right away.
fn permissions(w: &Rc<Win>, root: &gtk::Box, f: &gio::File, mode: u32, is_dir: bool, mine: bool) {
    let l = gtk::Label::new(Some("PERMISSIONS"));
    l.add_css_class("group-title");
    l.set_xalign(0.0);
    root.append(&l);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("settings-group");
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(18);
    grid.set_margin_top(12);
    grid.set_margin_bottom(12);
    grid.set_margin_start(12);
    grid.set_margin_end(12);
    let exec_word = if is_dir { "Open" } else { "Run" };
    for (c, h) in ["Read", "Write", exec_word].iter().enumerate() {
        grid.attach(&key_label(h), c as i32 + 1, 0, 1, 1);
    }
    let summary = gtk::Label::new(None);
    summary.add_css_class("settings-sub");
    summary.set_xalign(0.0);
    let current = Rc::new(Cell::new(mode & 0o7777));
    let set_summary = {
        let (s, cur) = (summary.clone(), current.clone());
        move || s.set_text(&format!("{:o} · {}", cur.get() & 0o777, util::mode_string(cur.get())))
    };
    set_summary();
    let mut checks = vec![];
    for (r, who) in ["Owner", "Group", "Others"].iter().enumerate() {
        grid.attach(&key_label(who), 0, r as i32 + 1, 1, 1);
        for c in 0..3 {
            let bit = 1u32 << ((2 - r) * 3 + (2 - c));
            let cb = gtk::CheckButton::new();
            cb.set_active(mode & bit != 0);
            cb.set_sensitive(mine);
            grid.attach(&cb, c as i32 + 1, r as i32 + 1, 1, 1);
            checks.push((cb, bit));
        }
    }
    card.append(&grid);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    foot.set_margin_start(12);
    foot.set_margin_end(12);
    foot.set_margin_bottom(12);
    summary.set_hexpand(true);
    foot.append(&summary);
    let exec = gtk::CheckButton::with_label("Allow running as a program");
    if !is_dir {
        exec.set_active(mode & 0o111 != 0);
        exec.set_sensitive(mine);
        foot.append(&exec);
    }
    card.append(&foot);
    if !mine {
        let n = gtk::Label::new(Some("You don't own this, so you can't change its permissions."));
        n.add_css_class("settings-sub");
        n.set_margin_start(12);
        n.set_margin_bottom(10);
        n.set_xalign(0.0);
        card.append(&n);
    }
    root.append(&card);

    let syncing = Rc::new(Cell::new(false));
    let checks = Rc::new(checks);
    // write the mode, then reflect the result in every control
    let apply: Rc<dyn Fn(u32)> = {
        let (f, w, cur, checks, exec, syncing) = (f.clone(), Rc::downgrade(w), current.clone(), checks.clone(), exec.clone(), syncing.clone());
        let set_summary = Rc::new(set_summary);
        Rc::new(move |new: u32| {
            let old = cur.get();
            if new == old {
                return;
            }
            let info = gio::FileInfo::new();
            info.set_attribute_uint32("unix::mode", new);
            let (w, cur, checks, exec, syncing, ss) = (w.clone(), cur.clone(), checks.clone(), exec.clone(), syncing.clone(), set_summary.clone());
            let sync_ui = move |m: u32| {
                syncing.set(true);
                for (cb, bit) in checks.iter() {
                    cb.set_active(m & bit != 0);
                }
                exec.set_active(m & 0o111 != 0);
                syncing.set(false);
            };
            cur.set(new);
            ss();
            let ss2 = ss.clone();
            let cur2 = cur.clone();
            f.set_attributes_async(&info, gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                match r {
                    Ok(_) => sync_ui(new),
                    Err(e) => {
                        cur2.set(old);
                        ss2();
                        sync_ui(old);
                        if let Some(w) = w.upgrade() {
                            w.show_notice(&format!("Couldn't change permissions: {}", e.message()), true);
                        }
                    }
                }
            });
        })
    };
    for (cb, bit) in checks.iter() {
        let (bit, cur, apply, syncing) = (*bit, current.clone(), apply.clone(), syncing.clone());
        cb.connect_toggled(move |cb| {
            if syncing.get() {
                return;
            }
            let m = if cb.is_active() { cur.get() | bit } else { cur.get() & !bit };
            apply(m);
        });
    }
    let (cur, apply2, syncing2) = (current.clone(), apply.clone(), syncing.clone());
    exec.connect_toggled(move |cb| {
        if !syncing2.get() {
            apply2(util::with_exec(cur.get(), cb.is_active()));
        }
    });
}
