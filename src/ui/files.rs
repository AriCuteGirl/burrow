//! File view: GtkDirectoryList (async, monitored) -> filter -> sort -> one selection model
//! shown as a grid, a compact grid or a list (ColumnView). Only visible cells exist,
//! so 10k-file folders stay responsive.

use super::pane::Pane;
use crate::config::Config;
use crate::util;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

const ATTRS: &str = "standard::name,standard::display-name,standard::icon,standard::type,\
standard::is-hidden,standard::is-backup,standard::is-symlink,standard::size,standard::content-type,\
time::modified,thumbnail::path,thumbnail::is-valid,access::can-read";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Grid,
    Compact,
    List,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "list" => Mode::List,
            "compact" => Mode::Compact,
            _ => Mode::Grid,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Grid => "grid",
            Mode::Compact => "compact",
            Mode::List => "list",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Type,
}

impl SortKey {
    fn parse(s: &str) -> SortKey {
        match s {
            "size" => SortKey::Size,
            "modified" => SortKey::Modified,
            "type" => SortKey::Type,
            _ => SortKey::Name,
        }
    }
    fn name(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
            SortKey::Type => "type",
        }
    }
}

/// Shared state the cell factories read while binding.
#[derive(Default)]
struct Look {
    folder_color: RefCell<Option<String>>,
    /// folder key (see `folder_key`) -> icon string from config
    folder_icons: RefCell<HashMap<String, String>>,
    icon_size: Cell<i32>,
}

pub struct Files {
    pub widget: gtk::Stack,
    dir: gtk::DirectoryList,
    filtered: gtk::FilterListModel,
    pub selection: gtk::MultiSelection,
    grid: gtk::GridView,
    compact: gtk::GridView,
    list: gtk::ColumnView,
    mode: Cell<Mode>,
    filter: gtk::CustomFilter,
    sorter: gtk::CustomSorter,
    show_hidden: Rc<Cell<bool>>,
    folders_first: Rc<Cell<bool>>,
    sort_key: Rc<Cell<SortKey>>,
    sort_desc: Rc<Cell<bool>>,
    syncing_sort: Cell<bool>,
    look: Rc<Look>,
    filter_text: Rc<RefCell<String>>,
    pane: Rc<RefCell<Weak<Pane>>>,
    error_title: gtk::Label,
    error_text: gtk::Label,
    error_icon: gtk::Image,
    empty_title: gtk::Label,
    empty_text: gtk::Label,
    skeleton: gtk::FlowBox,
    skeleton_timer: RefCell<Option<glib::SourceId>>,
}

pub fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast::<gio::File>()
}

/// Config key for a folder: "~/x" for local paths, the URI otherwise.
pub fn folder_key(f: &gio::File) -> String {
    match f.path() {
        Some(p) if f.is_native() => util::contract_tilde(&p.to_string_lossy(), &glib::home_dir().to_string_lossy()),
        _ => f.uri().to_string(),
    }
}

impl Files {
    pub fn new(cfg: &Config) -> Files {
        let show_hidden = Rc::new(Cell::new(cfg.view.show_hidden));
        let folders_first = Rc::new(Cell::new(cfg.view.folders_first));
        let sort_key = Rc::new(Cell::new(SortKey::parse(&cfg.view.sort)));
        let sort_desc = Rc::new(Cell::new(cfg.view.sort_desc));
        let filter_text = Rc::new(RefCell::new(String::new()));
        let look = Rc::new(Look::default());
        look.icon_size.set(cfg.view.icon_size as i32);
        let pane: Rc<RefCell<Weak<Pane>>> = Rc::new(RefCell::new(Weak::new()));

        let dir = gtk::DirectoryList::new(Some(ATTRS), None::<&gio::File>);
        dir.set_monitored(true);

        let (sh, ft) = (show_hidden.clone(), filter_text.clone());
        let filter = gtk::CustomFilter::new(move |o| {
            let Some(i) = o.downcast_ref::<gio::FileInfo>() else { return false };
            if !sh.get() && (i.is_hidden() || i.is_backup()) {
                return false;
            }
            let q = ft.borrow();
            q.is_empty() || i.display_name().to_lowercase().contains(q.as_str())
        });
        let (ff, sk, sd) = (folders_first.clone(), sort_key.clone(), sort_desc.clone());
        let sorter = gtk::CustomSorter::new(move |a, b| {
            let (Some(a), Some(b)) = (a.downcast_ref::<gio::FileInfo>(), b.downcast_ref::<gio::FileInfo>()) else {
                return gtk::Ordering::Equal;
            };
            let is_dir = |i: &gio::FileInfo| i.file_type() == gio::FileType::Directory;
            let first = if ff.get() { is_dir(b).cmp(&is_dir(a)) } else { std::cmp::Ordering::Equal };
            let name = || util::natural_cmp(&a.display_name(), &b.display_name());
            let key = match sk.get() {
                SortKey::Name => name(),
                SortKey::Size => a.size().cmp(&b.size()).then_with(name),
                SortKey::Modified => a
                    .modification_date_time()
                    .map(|d| d.to_unix())
                    .cmp(&b.modification_date_time().map(|d| d.to_unix()))
                    .then_with(name),
                SortKey::Type => a.content_type().cmp(&b.content_type()).then_with(name),
            };
            let key = if sd.get() { key.reverse() } else { key };
            first.then(key).into()
        });
        let filtered = gtk::FilterListModel::new(Some(dir.clone()), Some(filter.clone()));
        filtered.set_incremental(true);
        let sorted = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
        sorted.set_incremental(true);
        let selection = gtk::MultiSelection::new(Some(sorted));

        // ---------------- grid
        let grid = gtk::GridView::new(Some(selection.clone()), Some(grid_factory(&look, &pane)));
        grid.add_css_class("files");
        grid.set_max_columns(64);
        grid.set_enable_rubberband(true);
        // ---------------- compact
        let compact = gtk::GridView::new(Some(selection.clone()), Some(compact_factory(&look, &pane)));
        compact.add_css_class("files");
        compact.add_css_class("compact");
        compact.set_max_columns(64);
        compact.set_enable_rubberband(true);
        // ---------------- list
        let list = gtk::ColumnView::new(Some(selection.clone()));
        list.add_css_class("files");
        list.set_enable_rubberband(true);
        list.set_show_row_separators(false);
        list.set_reorderable(false);
        for (id, title, expand, width) in
            [("name", "Name", true, 320), ("size", "Size", false, 100), ("type", "Type", false, 180), ("modified", "Modified", false, 170)]
        {
            let col = gtk::ColumnViewColumn::new(Some(title), Some(list_factory(id, &look, &pane)));
            col.set_id(Some(id));
            col.set_expand(expand);
            col.set_resizable(true);
            col.set_fixed_width(width);
            // a sorter makes the header clickable; the real sorting is ours (folders first etc.)
            col.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
            list.append_column(&col);
        }

        let widget = gtk::Stack::new();
        widget.set_transition_type(gtk::StackTransitionType::Crossfade);
        widget.set_transition_duration(100);
        for (name, child) in [("grid", grid.clone().upcast::<gtk::Widget>()), ("compact", compact.clone().upcast()), ("list", list.clone().upcast())] {
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_child(Some(&child));
            scroll.set_vexpand(true);
            widget.add_named(&scroll, Some(name));
        }

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

        let (empty, _, empty_title, empty_text) = state_page("folder-open-symbolic");
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

        let f = Files {
            widget,
            dir,
            filtered,
            selection,
            grid,
            compact,
            list,
            mode: Cell::new(Mode::parse(&cfg.view.default_mode)),
            filter,
            sorter,
            show_hidden,
            folders_first,
            sort_key,
            sort_desc,
            syncing_sort: Cell::new(false),
            look,
            filter_text,
            pane,
            error_title,
            error_text,
            error_icon,
            empty_title,
            empty_text,
            skeleton,
            skeleton_timer: RefCell::new(None),
        };
        f.set_folder_icons(cfg);
        f.sync_list_sort_indicator();
        f
    }

    pub fn connect(&self, pane: &Rc<Pane>) {
        *self.pane.borrow_mut() = Rc::downgrade(pane);
        let activate = {
            let weak = Rc::downgrade(pane);
            move |pos: u32| {
                if let Some(p) = weak.upgrade() {
                    p.activate_item(pos);
                }
            }
        };
        let a = activate.clone();
        self.grid.connect_activate(move |_, pos| a(pos));
        let a = activate.clone();
        self.compact.connect_activate(move |_, pos| a(pos));
        self.list.connect_activate(move |_, pos| activate(pos));

        // State switch is cheap; the status line counts items, so debounce it while a big
        // folder streams in.
        let pending = Rc::new(Cell::new(false));
        let refresh = {
            let weak = Rc::downgrade(pane);
            move || {
                let Some(p) = weak.upgrade() else { return };
                p.files.update_state(&p);
                if pending.replace(true) {
                    return;
                }
                let (weak, pending) = (Rc::downgrade(&p), pending.clone());
                glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                    pending.set(false);
                    if let Some(p) = weak.upgrade() {
                        p.status_changed();
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

        // List header click -> our sort key
        let weak = Rc::downgrade(pane);
        if let Some(s) = self.list.sorter().and_downcast::<gtk::ColumnViewSorter>() {
            s.connect_changed(move |s, _| {
                let Some(p) = weak.upgrade() else { return };
                if p.files.syncing_sort.get() {
                    return;
                }
                let Some(col) = s.primary_sort_column() else { return };
                let key = SortKey::parse(col.id().as_deref().unwrap_or("name"));
                let desc = s.primary_sort_order() == gtk::SortType::Descending;
                p.set_sort(key.name(), desc);
            });
        }

        for view in [self.grid.clone().upcast::<gtk::Widget>(), self.compact.clone().upcast(), self.list.clone().upcast()] {
            // Backspace = parent, typing = filter-as-you-type
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(pane);
            keys.connect_key_pressed(move |_, key, _, mods| {
                let Some(p) = weak.upgrade() else { return glib::Propagation::Proceed };
                if key == gdk::Key::BackSpace && mods.is_empty() {
                    p.go_up();
                    return glib::Propagation::Stop;
                }
                if key == gdk::Key::Menu || (key == gdk::Key::F10 && mods == gdk::ModifierType::SHIFT_MASK) {
                    p.menu_for_selection();
                    return glib::Propagation::Stop;
                }
                let plain = !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
                if let Some(c) = key.to_unicode().filter(|c| plain && !c.is_control() && !c.is_whitespace()) {
                    p.start_filter(c);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            view.add_controller(keys);

            // right click on empty space
            let click = gtk::GestureClick::new();
            click.set_button(3);
            let weak = Rc::downgrade(pane);
            let v = view.clone();
            click.connect_pressed(move |g, _, x, y| {
                let Some(p) = weak.upgrade() else { return };
                g.set_state(gtk::EventSequenceState::Claimed);
                p.files.selection.unselect_all();
                p.background_menu(&v, x, y);
            });
            view.add_controller(click);
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode.get()
    }

    pub fn set_mode(&self, m: Mode) {
        self.mode.set(m);
        if matches!(self.widget.visible_child_name().as_deref(), Some("grid" | "compact" | "list")) {
            self.widget.set_visible_child_name(m.name());
        }
    }

    pub fn open(&self, f: &gio::File) {
        self.filter_text.borrow_mut().clear();
        self.filter.changed(gtk::FilterChange::LessStrict);
        self.selection.unselect_all();
        self.dir.set_file(Some(f));
        if self.selection.n_items() > 0 {
            self.grid.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
    }

    pub fn reload(&self) {
        let f = self.dir.file();
        self.dir.set_file(None::<&gio::File>);
        self.dir.set_file(f.as_ref());
    }

    pub fn grab_focus(&self) {
        match self.mode.get() {
            Mode::Grid => self.grid.grab_focus(),
            Mode::Compact => self.compact.grab_focus(),
            Mode::List => self.list.grab_focus(),
        };
    }

    pub fn filter_text(&self) -> String {
        self.filter_text.borrow().clone()
    }

    pub fn set_filter(&self, text: &str) {
        let t = text.trim().to_lowercase();
        let old = std::mem::replace(&mut *self.filter_text.borrow_mut(), t.clone());
        if old == t {
            return;
        }
        let change = if t.starts_with(&old) {
            gtk::FilterChange::MoreStrict
        } else if old.starts_with(&t) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        self.filter.changed(change);
    }

    fn rebind(&self) {
        // Re-create visible cells (size or icon look changed).
        for v in [&self.grid, &self.compact] {
            v.set_model(None::<&gtk::SelectionModel>);
            v.set_model(Some(&self.selection));
        }
        self.list.set_model(None::<&gtk::SelectionModel>);
        self.list.set_model(Some(&self.selection));
    }

    /// Papirus colour for folder icons (None = theme default).
    pub fn set_folder_color(&self, c: Option<String>) {
        if *self.look.folder_color.borrow() == c {
            return;
        }
        *self.look.folder_color.borrow_mut() = c;
        self.rebind();
    }

    fn set_folder_icons(&self, cfg: &Config) -> bool {
        let map: HashMap<String, String> =
            cfg.folders.iter().filter_map(|(k, v)| v.icon.clone().map(|i| (k.clone(), i))).collect();
        if *self.look.folder_icons.borrow() == map {
            return false;
        }
        *self.look.folder_icons.borrow_mut() = map;
        true
    }

    fn sync_list_sort_indicator(&self) {
        let id = self.sort_key.get().name();
        let order = if self.sort_desc.get() { gtk::SortType::Descending } else { gtk::SortType::Ascending };
        let cols = self.list.columns();
        for i in 0..cols.n_items() {
            if let Some(c) = cols.item(i).and_downcast::<gtk::ColumnViewColumn>() {
                if c.id().as_deref() == Some(id) {
                    self.syncing_sort.set(true);
                    self.list.sort_by_column(Some(&c), order);
                    self.syncing_sort.set(false);
                }
            }
        }
    }

    pub fn config_changed(&self, cfg: &Config) {
        if self.show_hidden.replace(cfg.view.show_hidden) != cfg.view.show_hidden {
            self.filter.changed(gtk::FilterChange::Different);
        }
        let key = SortKey::parse(&cfg.view.sort);
        let resort = self.folders_first.replace(cfg.view.folders_first) != cfg.view.folders_first
            | (self.sort_key.replace(key) != key)
            | (self.sort_desc.replace(cfg.view.sort_desc) != cfg.view.sort_desc);
        if resort {
            self.sorter.changed(gtk::SorterChange::Different);
            self.sync_list_sort_indicator();
        }
        let s = cfg.view.icon_size as i32;
        let resize = self.look.icon_size.replace(s) != s;
        if self.set_folder_icons(cfg) | resize {
            self.rebind();
        }
    }

    pub fn update_state(&self, p: &Rc<Pane>) {
        if let Some(id) = self.skeleton_timer.borrow_mut().take() {
            id.remove();
        }
        let n = self.filtered.n_items();
        let view = self.mode.get().name();
        if let Some(e) = self.dir.error() {
            let (icon, title, text) = describe_error(&e);
            self.error_icon.set_icon_name(Some(icon));
            self.error_title.set_text(title);
            self.error_text.set_text(&text);
            self.widget.set_visible_child_name("error");
        } else if self.dir.is_loading() && n == 0 {
            // show the skeleton only if loading takes a moment (no flicker on fast folders)
            self.widget.set_visible_child_name(view);
            let weak = Rc::downgrade(p);
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                let Some(p) = weak.upgrade() else { return };
                let f = &p.files;
                *f.skeleton_timer.borrow_mut() = None;
                if f.dir.is_loading() && f.filtered.n_items() == 0 {
                    let reduced = p.reduced_motion();
                    for c in f.skeleton_children() {
                        if reduced {
                            c.add_css_class("reduced");
                        } else {
                            c.remove_css_class("reduced");
                        }
                    }
                    f.widget.set_visible_child_name("loading");
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
            self.widget.set_visible_child_name(view);
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

    /// Selected items (capped), in view order.
    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        let sel = self.selection.selection();
        let mut out = vec![];
        if let Some((iter, first)) = gtk::BitsetIter::init_first(&sel) {
            for pos in std::iter::once(first).chain(iter).take(50_000) {
                if let Some(i) = self.selection.item(pos).and_downcast::<gio::FileInfo>() {
                    out.push(i);
                }
            }
        }
        out
    }

    pub fn item(&self, pos: u32) -> Option<gio::FileInfo> {
        self.selection.item(pos).and_downcast::<gio::FileInfo>()
    }

    pub fn status_text(&self) -> String {
        let n = self.filtered.n_items();
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
        let sel = self.selected_infos();
        if !sel.is_empty() {
            let bytes: u64 = sel
                .iter()
                .filter(|i| i.file_type() != gio::FileType::Directory)
                .map(|i| i.size().max(0) as u64)
                .sum();
            out.push_str(&format!(" · {} selected ({})", sel.len(), util::human_size(bytes)));
        }
        out
    }
}

// ---------------------------------------------------------------- cells

fn item_icon(info: &gio::FileInfo, look: &Look) -> gio::Icon {
    let is_dir = info.file_type() == gio::FileType::Directory;
    if is_dir && !look.folder_icons.borrow().is_empty() {
        if let Some(f) = info_file(info) {
            if let Some(i) = look.folder_icons.borrow().get(&folder_key(&f)) {
                return super::icon_from_str(i);
            }
        }
    }
    let Some(icon) = info.icon() else { return gio::ThemedIcon::new("text-x-generic").upcast() };
    match look.folder_color.borrow().as_deref() {
        Some(c) if is_dir => tint_folder(&icon, c),
        _ => icon,
    }
}

/// Right-click menu, middle-click (open in new tab) and drag source for one cell.
fn attach_cell(w: &gtk::Widget, li: &gtk::ListItem, pane: &Rc<RefCell<Weak<Pane>>>) {
    let click = gtk::GestureClick::new();
    click.set_button(0);
    let (liw, pw) = (li.downgrade(), pane.clone());
    click.connect_pressed(move |g, n, x, y| {
        let (Some(li), Some(p)) = (liw.upgrade(), pw.borrow().upgrade()) else { return };
        let pos = li.position();
        match g.current_button() {
            3 => {
                g.set_state(gtk::EventSequenceState::Claimed);
                if !li.is_selected() {
                    p.files.selection.select_item(pos, true);
                }
                if let Some(w) = g.widget() {
                    p.item_menu(&w, x, y);
                }
            }
            2 if n == 1 => {
                g.set_state(gtk::EventSequenceState::Claimed);
                p.open_item_in_new_tab(pos);
            }
            _ => {}
        }
    });
    w.add_controller(click);

    // Drag out (to the sidebar to pin, or to other apps)
    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
    let (liw, pw) = (li.downgrade(), pane.clone());
    drag.connect_prepare(move |_, _, _| {
        let (li, p) = (liw.upgrade()?, pw.borrow().upgrade()?);
        let files: Vec<gio::File> = if li.is_selected() {
            p.files.selected_infos().iter().filter_map(info_file).collect()
        } else {
            li.item().and_downcast::<gio::FileInfo>().and_then(|i| info_file(&i)).into_iter().collect()
        };
        if files.is_empty() {
            return None;
        }
        let list = gdk::FileList::from_array(&files);
        Some(gdk::ContentProvider::for_value(&list.to_value()))
    });
    let ww = w.downgrade();
    drag.connect_drag_begin(move |src, _| {
        if let Some(w) = ww.upgrade() {
            src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&w))), 0, 0);
        }
    });
    w.add_controller(drag);
}

fn grid_factory(look: &Rc<Look>, pane: &Rc<RefCell<Weak<Pane>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let (lk, pn) = (look.clone(), pane.clone());
    factory.connect_setup(move |_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let s = lk.icon_size.get();
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
        attach_cell(vb.upcast_ref(), li, &pn);
        li.set_child(Some(&vb));
    });
    let lk = look.clone();
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
        set_hidden_class(vb.upcast_ref(), &info);
        img.set_from_gicon(&item_icon(&info, &lk));
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
    factory
}

fn compact_factory(look: &Rc<Look>, pane: &Rc<RefCell<Weak<Pane>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let pn = pane.clone();
    factory.connect_setup(move |_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let img = gtk::Image::new();
        img.set_pixel_size(24);
        let lbl = gtk::Label::new(None);
        lbl.add_css_class("file-name");
        lbl.set_xalign(0.0);
        lbl.set_hexpand(true);
        lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        lbl.set_width_chars(18);
        lbl.set_max_width_chars(24);
        hb.append(&img);
        hb.append(&lbl);
        attach_cell(hb.upcast_ref(), li, &pn);
        li.set_child(Some(&hb));
    });
    let lk = look.clone();
    factory.connect_bind(move |_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(info) = li.item().and_downcast::<gio::FileInfo>() else { return };
        let Some(hb) = li.child().and_downcast::<gtk::Box>() else { return };
        let img = hb.first_child().and_downcast::<gtk::Image>().unwrap();
        let lbl = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
        img.set_from_gicon(&item_icon(&info, &lk));
        lbl.set_text(&info.display_name());
        hb.set_tooltip_text(Some(&info.display_name()));
        set_hidden_class(hb.upcast_ref(), &info);
    });
    factory
}

fn list_factory(col: &'static str, look: &Rc<Look>, pane: &Rc<RefCell<Weak<Pane>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let pn = pane.clone();
    factory.connect_setup(move |_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hb.set_hexpand(true);
        if col == "name" {
            let img = gtk::Image::new();
            img.set_pixel_size(20);
            hb.append(&img);
        }
        let lbl = gtk::Label::new(None);
        lbl.set_xalign(if col == "size" { 1.0 } else { 0.0 });
        lbl.set_hexpand(true);
        lbl.set_ellipsize(if col == "name" { gtk::pango::EllipsizeMode::Middle } else { gtk::pango::EllipsizeMode::End });
        lbl.add_css_class(if col == "name" { "file-name" } else { "file-meta" });
        hb.append(&lbl);
        attach_cell(hb.upcast_ref(), li, &pn);
        li.set_child(Some(&hb));
    });
    let lk = look.clone();
    factory.connect_bind(move |_, o| {
        let li = o.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(info) = li.item().and_downcast::<gio::FileInfo>() else { return };
        let Some(hb) = li.child().and_downcast::<gtk::Box>() else { return };
        let lbl = hb.last_child().and_downcast::<gtk::Label>().unwrap();
        let is_dir = info.file_type() == gio::FileType::Directory;
        let text = match col {
            "name" => {
                if let Some(img) = hb.first_child().and_downcast::<gtk::Image>() {
                    img.set_from_gicon(&item_icon(&info, &lk));
                }
                info.display_name().to_string()
            }
            "size" => if is_dir { "—".into() } else { util::human_size(info.size().max(0) as u64) },
            "type" => {
                if is_dir {
                    "Folder".into()
                } else {
                    info.content_type().map(|t| gio::content_type_get_description(&t).to_string()).unwrap_or_default()
                }
            }
            _ => info
                .modification_date_time()
                .and_then(|d| d.to_local().ok())
                .and_then(|d| d.format("%-d %b %Y %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
        };
        lbl.set_text(&text);
        set_hidden_class(hb.upcast_ref(), &info);
    });
    factory
}

fn set_hidden_class(w: &gtk::Widget, info: &gio::FileInfo) {
    if info.is_hidden() || info.is_backup() {
        w.add_css_class("file-hidden");
    } else {
        w.remove_css_class("file-hidden");
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
    } else if e.matches(E::NotMounted)
        || e.matches(E::HostNotFound)
        || e.matches(E::TimedOut)
        || e.matches(E::ConnectionRefused)
        || e.matches(E::NetworkUnreachable)
    {
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
