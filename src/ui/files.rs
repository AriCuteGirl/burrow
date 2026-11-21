//! File grid: GtkDirectoryList (async, monitored) -> filter -> sort -> GridView.
//! Only visible cells exist, so 10k-file folders stay responsive.

use super::{Loc, Win};
use crate::config::Config;
use crate::util;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const ATTRS: &str = "standard::name,standard::display-name,standard::icon,standard::type,\
standard::is-hidden,standard::is-backup,standard::is-symlink,standard::size,standard::content-type,\
time::modified,thumbnail::path,thumbnail::is-valid,access::can-read";

pub struct Files {
    pub widget: gtk::Stack,
    dir: gtk::DirectoryList,
    filtered: gtk::FilterListModel,
    selection: gtk::MultiSelection,
    grid: gtk::GridView,
    filter: gtk::CustomFilter,
    sorter: gtk::CustomSorter,
    show_hidden: Rc<Cell<bool>>,
    folders_first: Rc<Cell<bool>>,
    icon_size: Rc<Cell<i32>>,
    filter_text: Rc<RefCell<String>>,
    folder_color: Rc<RefCell<Option<String>>>,
    error_title: gtk::Label,
    error_text: gtk::Label,
    error_icon: gtk::Image,
    empty_title: gtk::Label,
    empty_text: gtk::Label,
    skeleton: gtk::FlowBox,
    skeleton_timer: RefCell<Option<glib::SourceId>>,
}

fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast::<gio::File>()
}

impl Files {
    pub fn new(cfg: &Config) -> Files {
        let show_hidden = Rc::new(Cell::new(cfg.view.show_hidden));
        let folders_first = Rc::new(Cell::new(cfg.view.folders_first));
        let icon_size = Rc::new(Cell::new(cfg.view.icon_size as i32));
        let filter_text = Rc::new(RefCell::new(String::new()));
        let folder_color: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        let dir = gtk::DirectoryList::new(Some(ATTRS), None::<&gio::File>);
        dir.set_monitored(true);
        dir.set_io_priority(glib::Priority::DEFAULT);

        let (sh, ft) = (show_hidden.clone(), filter_text.clone());
        let filter = gtk::CustomFilter::new(move |o| {
            let Some(i) = o.downcast_ref::<gio::FileInfo>() else { return false };
            if !sh.get() && (i.is_hidden() || i.is_backup()) {
                return false;
            }
            let q = ft.borrow();
            q.is_empty() || i.display_name().to_lowercase().contains(q.as_str())
        });
        let ff = folders_first.clone();
        let sorter = gtk::CustomSorter::new(move |a, b| {
            let (Some(a), Some(b)) = (a.downcast_ref::<gio::FileInfo>(), b.downcast_ref::<gio::FileInfo>()) else {
                return gtk::Ordering::Equal;
            };
            let dir = |i: &gio::FileInfo| i.file_type() == gio::FileType::Directory;
            let o = if ff.get() { dir(b).cmp(&dir(a)) } else { std::cmp::Ordering::Equal };
            o.then_with(|| util::natural_cmp(&a.display_name(), &b.display_name())).into()
        });
        let filtered = gtk::FilterListModel::new(Some(dir.clone()), Some(filter.clone()));
        filtered.set_incremental(true);
        let sorted = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
        sorted.set_incremental(true);
        let selection = gtk::MultiSelection::new(Some(sorted));

        let factory = gtk::SignalListItemFactory::new();
        let isz = icon_size.clone();
        factory.connect_setup(move |_, o| {
            let li = o.downcast_ref::<gtk::ListItem>().unwrap();
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let s = isz.get();
            let img = gtk::Image::new();
            img.set_pixel_size(s);
            let pic = gtk::Picture::new();
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_size_request(s, s);
            pic.set_can_shrink(true);
            pic.add_css_class("file-thumb");
            pic.set_visible(false);
            let lbl = gtk::Label::new(None);
            lbl.add_css_class("file-name");
            lbl.set_wrap(true);
            lbl.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            lbl.set_lines(2);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            lbl.set_justify(gtk::Justification::Center);
            lbl.set_max_width_chars(14);
            lbl.set_width_chars(10);
            vb.append(&img);
            vb.append(&pic);
            vb.append(&lbl);
            li.set_child(Some(&vb));
        });
        let fc = folder_color.clone();
        factory.connect_bind(move |_, o| {
            let li = o.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = li.item().and_downcast::<gio::FileInfo>() else { return };
            let Some(vb) = li.child().and_downcast::<gtk::Box>() else { return };
            let img = vb.first_child().and_downcast::<gtk::Image>().unwrap();
            let pic = img.next_sibling().and_downcast::<gtk::Picture>().unwrap();
            let lbl = pic.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let name = info.display_name();
            lbl.set_text(&name);
            vb.set_tooltip_text(Some(&name));
            if info.is_hidden() || info.is_backup() {
                vb.add_css_class("file-hidden");
            } else {
                vb.remove_css_class("file-hidden");
            }
            match info.icon() {
                Some(i) => match fc.borrow().as_deref() {
                    Some(c) if info.file_type() == gio::FileType::Directory => img.set_from_gicon(&tint_folder(&i, c)),
                    _ => img.set_from_gicon(&i),
                },
                None => img.set_icon_name(Some("text-x-generic")),
            }
            img.set_visible(true);
            pic.set_visible(false);
            pic.set_paintable(None::<&gdk::Paintable>);
            // Thumbnail from the XDG cache, if one already exists. Read off the UI thread.
            let token = info.name().to_string_lossy().to_string();
            vb.set_widget_name(&token);
            let thumb = info
                .attribute_byte_string("thumbnail::path")
                .filter(|_| !info.has_attribute("thumbnail::is-valid") || info.boolean("thumbnail::is-valid"));
            if let Some(path) = thumb {
                let path = path.to_string();
                let vbw = vb.downgrade();
                glib::spawn_future_local(async move {
                    let Ok(Ok(bytes)) = gio::spawn_blocking(move || std::fs::read(path)).await else { return };
                    let Some(vb) = vbw.upgrade() else { return };
                    if vb.widget_name() != token {
                        return; // cell was recycled for another file
                    }
                    if let Ok(tex) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                        let img = vb.first_child().unwrap();
                        let pic = img.next_sibling().and_downcast::<gtk::Picture>().unwrap();
                        pic.set_paintable(Some(&tex));
                        pic.set_visible(true);
                        img.set_visible(false);
                    }
                });
            }
        });

        let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
        grid.add_css_class("files");
        grid.set_max_columns(64);
        grid.set_min_columns(1);
        grid.set_enable_rubberband(true);
        grid.set_single_click_activate(false);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&grid));
        scroll.set_vexpand(true);

        let widget = gtk::Stack::new();
        widget.set_transition_type(gtk::StackTransitionType::Crossfade);
        widget.set_transition_duration(100);
        widget.add_named(&scroll, Some("grid"));

        // loading skeleton
        let skeleton = gtk::FlowBox::new();
        skeleton.set_selection_mode(gtk::SelectionMode::None);
        skeleton.set_margin_top(16);
        skeleton.set_margin_start(16);
        skeleton.set_margin_end(16);
        skeleton.set_valign(gtk::Align::Start);
        skeleton.set_column_spacing(12);
        skeleton.set_row_spacing(12);
        skeleton.set_max_children_per_line(64);
        skeleton.set_homogeneous(true);
        for _ in 0..24 {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let a = gtk::Box::new(gtk::Orientation::Vertical, 0);
            a.set_size_request(64, 64);
            a.add_css_class("skeleton");
            a.set_halign(gtk::Align::Center);
            let l = gtk::Box::new(gtk::Orientation::Vertical, 0);
            l.set_size_request(80, 12);
            l.add_css_class("skeleton");
            b.append(&a);
            b.append(&l);
            b.set_margin_top(10);
            skeleton.append(&b);
        }
        widget.add_named(&skeleton, Some("loading"));

        let (empty, empty_icon, empty_title, empty_text) = state_page("folder-open-symbolic");
        let _ = empty_icon;
        widget.add_named(&empty, Some("empty"));
        let (error, error_icon, error_title, error_text) = state_page("dialog-error-symbolic");
        error.add_css_class("error");
        let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btns.set_halign(gtk::Align::Center);
        btns.set_margin_top(8);
        let back = gtk::Button::with_label("Go back");
        back.add_css_class("secondary");
        back.set_action_name(Some("win.back"));
        let retry = gtk::Button::with_label("Try again");
        retry.add_css_class("primary");
        retry.set_action_name(Some("win.reload"));
        btns.append(&back);
        btns.append(&retry);
        error.append(&btns);
        widget.add_named(&error, Some("error"));

        Files {
            widget,
            dir,
            filtered,
            selection,
            grid,
            filter,
            sorter,
            show_hidden,
            folders_first,
            icon_size,
            filter_text,
            folder_color,
            error_title,
            error_text,
            error_icon,
            empty_title,
            empty_text,
            skeleton,
            skeleton_timer: RefCell::new(None),
        }
    }

    pub fn connect(&self, w: &Rc<Win>) {
        let weak = Rc::downgrade(w);
        self.grid.connect_activate(move |g, pos| {
            let Some(w) = weak.upgrade() else { return };
            let Some(info) = g.model().and_then(|m| m.item(pos)).and_downcast::<gio::FileInfo>() else { return };
            let Some(f) = info_file(&info) else { return };
            if info.file_type() == gio::FileType::Directory || info.file_type() == gio::FileType::Mountable {
                w.go(Loc::Files(f), true);
            } else {
                let launcher = gtk::FileLauncher::new(Some(&f));
                let weak = Rc::downgrade(&w);
                launcher.launch(Some(&w.window), gio::Cancellable::NONE, move |r| {
                    if let (Err(e), Some(w)) = (r, weak.upgrade()) {
                        if !e.matches(gtk::DialogError::Dismissed) && !e.matches(gtk::DialogError::Cancelled) {
                            w.show_notice(&format!("Could not open {}: {}", info.display_name(), e.message()), true);
                        }
                    }
                });
            }
        });

        // State switch is cheap; the status line counts items, so debounce it while a big
        // folder streams in.
        let pending = Rc::new(Cell::new(false));
        let refresh = {
            let weak = Rc::downgrade(w);
            move || {
                let Some(w) = weak.upgrade() else { return };
                w.files.update_state(&w);
                if pending.replace(true) {
                    return;
                }
                let (weak, pending) = (Rc::downgrade(&w), pending.clone());
                glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                    pending.set(false);
                    if let Some(w) = weak.upgrade() {
                        w.update_status();
                    }
                });
            }
        };
        let r = refresh.clone();
        self.dir.connect_loading_notify(move |_| r());
        let r = refresh.clone();
        self.dir.connect_error_notify(move |_| r());
        let r = refresh.clone();
        self.filtered.connect_items_changed(move |_, _, _, _| r());
        let r = refresh.clone();
        self.selection.connect_selection_changed(move |_, _, _| r());

        // Backspace = parent, typing = filter-as-you-type
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(w);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::BackSpace && mods.is_empty() {
                w.go_up();
                return glib::Propagation::Stop;
            }
            let plain = !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
            if let Some(c) = key.to_unicode().filter(|c| plain && !c.is_control() && !c.is_whitespace()) {
                w.filter_entry.grab_focus();
                w.filter_entry.set_text(&format!("{}{c}", w.filter_entry.text()));
                w.filter_entry.set_position(-1);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.grid.add_controller(keys);
    }

    pub fn open(&self, f: &gio::File) {
        self.filter_text.borrow_mut().clear();
        self.filter.changed(gtk::FilterChange::LessStrict);
        self.dir.set_file(Some(f));
        if self.grid.model().is_some_and(|m| m.n_items() > 0) {
            self.grid.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
    }

    pub fn grab_focus(&self) {
        self.grid.grab_focus();
    }

    pub fn set_filter(&self, text: &str) {
        let t = text.trim().to_lowercase();
        let old = std::mem::replace(&mut *self.filter_text.borrow_mut(), t.clone());
        let change = if t.starts_with(&old) {
            gtk::FilterChange::MoreStrict
        } else if old.starts_with(&t) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        self.filter.changed(change);
    }

    /// Papirus colour for folder icons (None = theme default). Rebinds visible cells.
    pub fn set_folder_color(&self, c: Option<String>) {
        if *self.folder_color.borrow() == c {
            return;
        }
        *self.folder_color.borrow_mut() = c;
        let m = self.grid.model();
        self.grid.set_model(None::<&gtk::SelectionModel>);
        self.grid.set_model(m.as_ref());
    }

    pub fn config_changed(&self, cfg: &Config) {
        if self.show_hidden.replace(cfg.view.show_hidden) != cfg.view.show_hidden {
            self.filter.changed(gtk::FilterChange::Different);
        }
        if self.folders_first.replace(cfg.view.folders_first) != cfg.view.folders_first {
            self.sorter.changed(gtk::SorterChange::Different);
        }
        let s = cfg.view.icon_size as i32;
        if self.icon_size.replace(s) != s {
            // rebuild cells with the new size
            let m = self.grid.model();
            self.grid.set_model(None::<&gtk::SelectionModel>);
            self.grid.set_model(m.as_ref());
        }
    }

    fn update_state(&self, w: &Rc<Win>) {
        if let Some(id) = self.skeleton_timer.borrow_mut().take() {
            id.remove();
        }
        let n = self.filtered.n_items();
        if let Some(e) = self.dir.error() {
            let (icon, title, text) = describe_error(&e);
            self.error_icon.set_icon_name(Some(icon));
            self.error_title.set_text(title);
            self.error_text.set_text(&text);
            self.widget.set_visible_child_name("error");
        } else if self.dir.is_loading() && n == 0 {
            // show the skeleton only if loading takes a moment (no flicker on fast folders)
            self.widget.set_visible_child_name("grid");
            let weak = Rc::downgrade(w);
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                let Some(w) = weak.upgrade() else { return };
                *w.files.skeleton_timer.borrow_mut() = None;
                if w.files.dir.is_loading() && w.files.filtered.n_items() == 0 {
                    for c in w.files.skeleton_children() {
                        if w.reduced_motion() {
                            c.add_css_class("reduced");
                        } else {
                            c.remove_css_class("reduced");
                        }
                    }
                    w.files.widget.set_visible_child_name("loading");
                }
            });
            *self.skeleton_timer.borrow_mut() = Some(id);
        } else if n == 0 {
            let q = self.filter_text.borrow();
            if !q.is_empty() {
                self.empty_title.set_text("No matches");
                self.empty_text.set_text(&format!("Nothing here matches “{q}”. Press Esc to clear the filter."));
            } else if self.dir.n_items() > 0 {
                self.empty_title.set_text("Only hidden files");
                self.empty_text.set_text("Press Ctrl+H to show them.");
            } else {
                self.empty_title.set_text("This folder is empty");
                self.empty_text.set_text("Drop files here or create something new.");
            }
            self.widget.set_visible_child_name("empty");
        } else {
            self.widget.set_visible_child_name("grid");
        }
    }

    fn skeleton_children(&self) -> Vec<gtk::Widget> {
        let mut v = vec![];
        let mut c = self.skeleton.first_child();
        while let Some(x) = c {
            if let Some(b) = x.first_child() {
                let mut d = b.first_child();
                while let Some(y) = d {
                    d = y.next_sibling();
                    v.push(y);
                }
            }
            c = x.next_sibling();
        }
        v
    }

    pub fn status_text(&self) -> String {
        let n = self.filtered.n_items();
        let sel = self.selection.selection();
        let s = sel.size();
        let mut folders = 0u32;
        for i in 0..n.min(20_000) {
            if let Some(info) = self.filtered.item(i).and_downcast::<gio::FileInfo>() {
                if info.file_type() == gio::FileType::Directory {
                    folders += 1;
                }
            }
        }
        let files = n - folders;
        let mut out = format!(
            "{folders} folder{} · {files} file{}",
            if folders == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" }
        );
        if self.dir.is_loading() {
            out.push_str(" · loading…");
        }
        if s > 0 {
            let mut bytes = 0u64;
            if let Some((iter, first)) = gtk::BitsetIter::init_first(&sel) {
                for pos in std::iter::once(first).chain(iter).take(50_000) {
                    if let Some(info) = self.selection.item(pos).and_downcast::<gio::FileInfo>() {
                        if info.file_type() != gio::FileType::Directory {
                            bytes += info.size().max(0) as u64;
                        }
                    }
                }
            }
            out.push_str(&format!(" · {s} selected ({})", util::human_size(bytes)));
        }
        out
    }
}

/// "folder-documents" -> "folder-carmine-documents" (else "folder-carmine"), if the icon theme has it.
/// GTK searches a child theme fully before its parents, so a list of names with fallbacks
/// would pick the child's plain "folder"; resolve the coloured name ourselves instead.
fn tint_folder(icon: &gio::Icon, color: &str) -> gio::Icon {
    let Some(t) = icon.downcast_ref::<gio::ThemedIcon>() else { return icon.clone() };
    let Some(theme) = gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d)) else { return icon.clone() };
    let mut candidates = vec![];
    for n in t.names() {
        let n = n.as_str();
        if let Some(rest) = n.strip_prefix("folder-").filter(|r| !r.ends_with("symbolic")) {
            candidates.push(format!("folder-{color}-{rest}"));
        }
    }
    candidates.push(format!("folder-{color}"));
    match candidates.into_iter().find(|c| theme.has_icon(c)) {
        Some(name) => gio::ThemedIcon::new(&name).upcast(),
        None => icon.clone(),
    }
}

fn describe_error(e: &glib::Error) -> (&'static str, &'static str, String) {
    use gio::IOErrorEnum as E;
    if e.matches(E::PermissionDenied) {
        ("action-unavailable-symbolic", "Permission denied", "You don't have permission to open this folder.".into())
    } else if e.matches(E::NotFound) {
        ("dialog-question-symbolic", "Folder not found", "It may have been moved, deleted, or its drive was removed.".into())
    } else if e.matches(E::NotMounted) || e.matches(E::HostNotFound) || e.matches(E::TimedOut) || e.matches(E::ConnectionRefused) || e.matches(E::NetworkUnreachable) {
        ("network-offline-symbolic", "Location unavailable", format!("The network location can't be reached. {}", e.message()))
    } else if e.matches(E::NotDirectory) {
        ("text-x-generic-symbolic", "Not a folder", "This location is a file, not a folder.".into())
    } else {
        ("dialog-error-symbolic", "Couldn't open this folder", e.message().to_string())
    }
}

fn state_page(icon: &str) -> (gtk::Box, gtk::Image, gtk::Label, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("state-box");
    b.set_valign(gtk::Align::Center);
    b.set_halign(gtk::Align::Center);
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(64);
    img.add_css_class("state-icon");
    let t = gtk::Label::new(None);
    t.add_css_class("state-title");
    let s = gtk::Label::new(None);
    s.add_css_class("state-text");
    s.set_wrap(true);
    s.set_max_width_chars(48);
    s.set_justify(gtk::Justification::Center);
    b.append(&img);
    b.append(&t);
    b.append(&s);
    (b, img, t, s)
}
