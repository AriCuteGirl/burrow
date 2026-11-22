//! Small modal dialogs in Burrow's own style: text prompt, icon picker, colour picker.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub(super) fn dialog_window(parent: &gtk::Window, title: &str, width: i32) -> (gtk::Window, gtk::Box) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .decorated(false)
        .default_width(width)
        .resizable(false)
        .build();
    win.add_css_class("burrow");
    win.add_css_class("dialog");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("dialog-root");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("dialog-title");
    t.set_xalign(0.0);
    root.append(&t);
    win.set_child(Some(&root));
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
    (win, root)
}

pub(super) fn button_row(root: &gtk::Box, ok_label: &str) -> (gtk::Button, gtk::Button) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    row.set_margin_top(4);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("secondary");
    let ok = gtk::Button::with_label(ok_label);
    ok.add_css_class("primary");
    row.append(&cancel);
    row.append(&ok);
    root.append(&row);
    (cancel, ok)
}

/// Ask for a line of text. `cb` gets the trimmed text (may be empty = "reset").
pub fn prompt_text(parent: &impl IsA<gtk::Window>, title: &str, hint: &str, initial: &str, cb: impl Fn(String) + 'static) {
    let (win, root) = dialog_window(parent.upcast_ref(), title, 380);
    if !hint.is_empty() {
        let h = gtk::Label::new(Some(hint));
        h.add_css_class("dialog-hint");
        h.set_xalign(0.0);
        h.set_wrap(true);
        root.append(&h);
    }
    let entry = gtk::Entry::new();
    entry.add_css_class("path-entry");
    entry.set_text(initial);
    root.append(&entry);
    let (cancel, ok) = button_row(&root, "Save");
    let cb = Rc::new(cb);
    let done = {
        let (ww, e, cb) = (win.downgrade(), entry.clone(), cb.clone());
        move || {
            cb(e.text().trim().to_string());
            if let Some(w) = ww.upgrade() {
                w.close();
            }
        }
    };
    let d = done.clone();
    ok.connect_clicked(move |_| d());
    entry.connect_activate(move |_| done());
    let ww = win.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = ww.upgrade() {
            w.close()
        }
    });
    win.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

/// Pick an icon from the icon theme (searchable) or an image file. `cb` gets an icon name
/// or an absolute file path (the same strings the config uses).
pub fn pick_icon(parent: &impl IsA<gtk::Window>, title: &str, symbolic_first: bool, cb: impl Fn(String) + 'static) {
    let (win, root) = dialog_window(parent.upcast_ref(), title, 560);
    win.set_default_height(520);
    win.set_resizable(true);
    let search = gtk::SearchEntry::new();
    search.add_css_class("filter-entry");
    search.set_placeholder_text(Some("Search icons (folder, game, music, code…)"));
    root.append(&search);

    let theme = gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d));
    let mut names: Vec<String> = theme.as_ref().map(|t| t.icon_names().iter().map(|s| s.to_string()).collect()).unwrap_or_default();
    // folder/place icons first, then symbolic or colourful ones depending on use
    names.sort_by_key(|n| {
        (
            !(n.starts_with("folder") || n.starts_with("user-") || n.starts_with("drive-")),
            n.ends_with("-symbolic") != symbolic_first,
            n.clone(),
        )
    });
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let model = gtk::StringList::new(&refs);
    let query = Rc::new(RefCell::new(String::new()));
    let q = query.clone();
    let filter = gtk::CustomFilter::new(move |o| {
        let q = q.borrow();
        q.is_empty() || o.downcast_ref::<gtk::StringObject>().is_some_and(|s| s.string().contains(q.as_str()))
    });
    let filtered = gtk::FilterListModel::new(Some(model), Some(filter.clone()));
    let sel = gtk::SingleSelection::new(Some(filtered));
    sel.set_autoselect(false);
    sel.set_can_unselect(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let img = gtk::Image::new();
        img.set_pixel_size(32);
        img.set_margin_top(6);
        img.set_margin_bottom(6);
        li.set_child(Some(&img));
    });
    factory.connect_bind(|_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(img), Some(s)) = (li.child().and_downcast::<gtk::Image>(), li.item().and_downcast::<gtk::StringObject>()) else { return };
        img.set_icon_name(Some(&s.string()));
        img.set_tooltip_text(Some(&s.string()));
    });
    let grid = gtk::GridView::new(Some(sel.clone()), Some(factory));
    grid.add_css_class("files");
    grid.set_max_columns(12);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&grid));
    scroll.set_vexpand(true);
    scroll.set_min_content_height(320);
    root.append(&scroll);
    let chosen = gtk::Label::new(Some("Nothing selected"));
    chosen.add_css_class("dialog-hint");
    chosen.set_xalign(0.0);
    root.append(&chosen);

    let (q, f) = (query.clone(), filter.clone());
    search.connect_search_changed(move |e| {
        *q.borrow_mut() = e.text().trim().to_lowercase();
        f.changed(gtk::FilterChange::Different);
    });
    let ch = chosen.clone();
    sel.connect_selection_changed(move |s, _, _| {
        let name = s.selected_item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string());
        ch.set_text(&name.unwrap_or_else(|| "Nothing selected".into()));
    });

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let file_btn = gtk::Button::with_label("Use an image file…");
    file_btn.add_css_class("secondary");
    row.append(&file_btn);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("secondary");
    let ok = gtk::Button::with_label("Use icon");
    ok.add_css_class("primary");
    row.append(&cancel);
    row.append(&ok);
    root.append(&row);

    let cb = Rc::new(cb);
    let finish = {
        let (ww, cb) = (win.downgrade(), cb.clone());
        move |v: String| {
            cb(v);
            if let Some(w) = ww.upgrade() {
                w.close();
            }
        }
    };
    let (s2, fin) = (sel.clone(), finish.clone());
    ok.connect_clicked(move |_| {
        if let Some(n) = s2.selected_item().and_downcast::<gtk::StringObject>() {
            fin(n.string().to_string());
        }
    });
    let (s2, fin) = (sel.clone(), finish.clone());
    grid.connect_activate(move |_, pos| {
        if let Some(n) = s2.model().and_then(|m| m.item(pos)).and_downcast::<gtk::StringObject>() {
            fin(n.string().to_string());
        }
    });
    let ww = win.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = ww.upgrade() {
            w.close()
        }
    });
    let (ww, fin) = (win.downgrade(), finish);
    file_btn.connect_clicked(move |_| {
        let Some(w) = ww.upgrade() else { return };
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let fd = gtk::FileDialog::builder().title("Choose an image").modal(true).filters(&filters).build();
        let fin = fin.clone();
        fd.open(Some(&w), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r {
                if let Some(p) = f.path() {
                    fin(p.to_string_lossy().to_string());
                }
            }
        });
    });
    win.present();
    search.grab_focus();
}

/// Ask before something destructive. The danger button is not the default: Enter cancels.
pub fn confirm(parent: &impl IsA<gtk::Window>, title: &str, text: &str, danger: &str, on_yes: impl FnOnce() + 'static) {
    let on_yes = RefCell::new(Some(on_yes));
    let (win, root) = dialog_window(parent.upcast_ref(), title, 420);
    let t = gtk::Label::new(Some(text));
    t.add_css_class("dialog-hint");
    t.set_xalign(0.0);
    t.set_wrap(true);
    t.set_max_width_chars(48);
    root.append(&t);
    let (cancel, ok) = button_row(&root, danger);
    ok.remove_css_class("primary");
    ok.add_css_class("danger");
    let ww = win.downgrade();
    ok.connect_clicked(move |_| {
        if let Some(f) = on_yes.borrow_mut().take() {
            f();
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
    cancel.grab_focus();
}

/// "f.txt already exists": Skip / Keep both / Replace (or Merge for folders), optionally for all.
pub fn conflict(parent: &impl IsA<gtk::Window>, c: &crate::transfer::Conflict, cb: impl FnOnce(crate::transfer::Choice, bool) + 'static) {
    use crate::transfer::Choice;
    let kind = if c.dest_is_dir { "A folder" } else { "A file" };
    let (win, root) = dialog_window(parent.upcast_ref(), &format!("“{}” already exists", c.name), 480);
    let where_ = gtk::Label::new(Some(&format!("{kind} with this name is already in {}.", c.dest_dir)));
    where_.add_css_class("dialog-hint");
    where_.set_xalign(0.0);
    where_.set_wrap(true);
    root.append(&where_);
    let when = |t: i64| {
        glib::DateTime::from_unix_local(t).ok().and_then(|d| d.format("%-d %b %Y %H:%M").ok()).map(|s| s.to_string()).unwrap_or_default()
    };
    let info = |label: &str, dir: bool, size: u64, mtime: i64| {
        let what = if dir { "folder".to_string() } else { crate::util::human_size(size) };
        let l = gtk::Label::new(Some(&format!("{label}: {what} · modified {}", when(mtime))));
        l.add_css_class("settings-sub");
        l.set_xalign(0.0);
        l
    };
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("settings-group");
    card.set_margin_top(2);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.append(&info("Already there", c.dest_is_dir, c.dest_size, c.dest_mtime));
    inner.append(&info("Incoming", c.src_is_dir, c.src_size, c.src_mtime));
    card.append(&inner);
    root.append(&card);
    let all = gtk::CheckButton::with_label("Do this for every clash in this job");
    root.append(&all);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(4);
    let cancel = gtk::Button::with_label("Stop job");
    cancel.add_css_class("secondary");
    row.append(&cancel);
    let sp = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sp.set_hexpand(true);
    row.append(&sp);
    let skip = gtk::Button::with_label("Skip");
    skip.add_css_class("secondary");
    let both = gtk::Button::with_label("Keep both");
    both.add_css_class("secondary");
    row.append(&skip);
    row.append(&both);
    let third = if c.src_is_dir && c.dest_is_dir {
        let b = gtk::Button::with_label("Merge");
        b.add_css_class("primary");
        Some((b, Choice::Merge))
    } else if c.src_is_dir == c.dest_is_dir {
        let b = gtk::Button::with_label("Replace");
        b.add_css_class("danger");
        Some((b, Choice::Replace))
    } else {
        None // file vs folder: never replace one with the other
    };
    if let Some((b, _)) = &third {
        row.append(b);
    }
    root.append(&row);

    let cb: Rc<RefCell<Option<Box<dyn FnOnce(Choice, bool)>>>> = Rc::new(RefCell::new(Some(Box::new(cb))));
    let answer = {
        let (cb, ww, all) = (cb.clone(), win.downgrade(), all.clone());
        move |ch: Choice| {
            if let Some(f) = cb.borrow_mut().take() {
                f(ch, all.is_active());
            }
            if let Some(w) = ww.upgrade() {
                w.close();
            }
        }
    };
    for (b, ch) in [(&skip, Choice::Skip), (&both, Choice::KeepBoth), (&cancel, Choice::Cancel)] {
        let a = answer.clone();
        b.connect_clicked(move |_| a(ch));
    }
    if let Some((b, ch)) = third {
        let a = answer.clone();
        b.connect_clicked(move |_| a(ch));
    }
    // closing the dialog any other way (Esc) = Skip, the safe answer
    win.connect_close_request(move |_| {
        if let Some(f) = cb.borrow_mut().take() {
            f(Choice::Skip, false);
        }
        glib::Propagation::Proceed
    });
    win.present();
    skip.grab_focus();
}

/// Pick a colour; `cb` gets "#rrggbb".
pub fn pick_color(parent: &impl IsA<gtk::Window>, initial: Option<&str>, cb: impl Fn(String) + 'static) {
    let d = gtk::ColorDialog::builder().title("Section colour").modal(true).with_alpha(false).build();
    let init = initial.and_then(|s| gdk::RGBA::parse(s).ok());
    d.choose_rgba(Some(parent.upcast_ref()), init.as_ref(), gio::Cancellable::NONE, move |r| {
        if let Ok(c) = r {
            let h = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            cb(format!("#{:02x}{:02x}{:02x}", h(c.red()), h(c.green()), h(c.blue())));
        }
    });
}
