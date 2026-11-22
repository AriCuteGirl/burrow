//! UI: one `Win` per window, holding tabs; each tab holds one pane or two (split view).
//! All file system work goes through async GIO calls or gio::spawn_blocking.

mod apps;
mod batch;
mod connect;
mod archives;
mod create;
mod dialogs;
mod dirmodel;
mod drives_page;
mod files;
mod jobs;
mod network_page;
mod pane;
mod pathbar;
mod properties;
mod settings;
mod share_ui;
mod sidebar;

use crate::config::{self, Config, Item};
use crate::drives::{self, Drive};
use crate::theme::{self, Appearance, RicePaths, Tokens};
use files::Mode;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use pane::Pane;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone)]
pub enum Loc {
    Files(gio::File),
    Drives,
    Network,
}

impl Loc {
    fn same(&self, o: &Loc) -> bool {
        match (self, o) {
            (Loc::Files(a), Loc::Files(b)) => a.equal(b),
            (Loc::Drives, Loc::Drives) | (Loc::Network, Loc::Network) => true,
            _ => false,
        }
    }
}

pub struct Tab {
    container: gtk::Box,
    panes: RefCell<Vec<Rc<Pane>>>,
    active: Cell<usize>,
    button: gtk::Box,
    label: gtk::Label,
    icon: gtk::Image,
}

impl Tab {
    fn active_pane(&self) -> Rc<Pane> {
        let ps = self.panes.borrow();
        ps[self.active.get().min(ps.len() - 1)].clone()
    }
}

pub struct Win {
    pub window: gtk::ApplicationWindow,
    pub cfg: RefCell<Config>,
    /// Text we last wrote/read, so our own saves don't trigger a reload loop.
    cfg_text: RefCell<String>,
    /// The user's file failed to parse: keep running on the last good config, never overwrite it.
    cfg_broken: Cell<bool>,
    sys_icon_theme: Option<String>,
    pub tokens: RefCell<Tokens>,
    provider: gtk::CssProvider,
    paths: RicePaths,
    pub home: gio::File,

    pub drives: RefCell<Vec<Drive>>,
    volume_monitor: gio::VolumeMonitor,
    rice_monitors: RefCell<Vec<gio::FileMonitor>>,
    cfg_monitor: RefCell<Option<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    cfg_debounce: RefCell<Option<glib::SourceId>>,

    tabs: RefCell<Vec<Rc<Tab>>>,
    cur_tab: Cell<usize>,
    tab_bar: gtk::Box,
    tab_stack: gtk::Stack,

    pub sidebar: sidebar::Sidebar,
    pub settings: RefCell<Option<settings::Settings>>,
    syncing: Cell<bool>,
    hidden_btn: gtk::ToggleButton,
    split_btn: gtk::ToggleButton,
    mode_btns: [gtk::ToggleButton; 3],
    pub filter_entry: gtk::SearchEntry,
    status: gtk::Label,
    notice: gtk::Label,
    pub jobs: jobs::Jobs,
    undo: RefCell<Vec<UndoOp>>,
    pub trash_count: Cell<u64>,
    trash_monitor: RefCell<Option<gio::FileMonitor>>,
}

/// Things Ctrl+Z can take back.
enum UndoOp {
    /// Paths that went to the trash, and when (unix seconds).
    Trash { paths: Vec<String>, since: i64 },
    Transfer(Vec<crate::transfer::Done>),
    Rename { file: gio::File, old_name: String },
    NewFolder(gio::File),
    /// A file or shortcut we created: undo moves it to the trash.
    Created(gio::File),
    /// Batch rename: (renamed file, old name)
    Batch(Vec<(gio::File, String)>),
}

thread_local! {
    /// Signal handlers only hold weak refs to their window; this keeps each Win alive until
    /// its window closes.
    static WINDOWS: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
}

/// Icon theme the user set for GTK in ~/.config/gtk-4.0/settings.ini (the portal may say
/// something else, e.g. a KDE theme).
fn gtk_settings_icon_theme() -> Option<String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| glib::home_dir().join(".config"));
    let text = std::fs::read_to_string(base.join("gtk-4.0/settings.ini"))
        .or_else(|_| std::fs::read_to_string(base.join("gtk-3.0/settings.ini")))
        .ok()?;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("gtk-icon-theme-name"))
        .filter_map(|r| r.trim_start().strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_string())
        .find(|v| !v.is_empty())
}

pub fn startup() {
    // Plain GTK base theme so the rice CSS is the only look; this only affects Burrow.
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_theme_name(Some("Default"));
        s.set_gtk_application_prefer_dark_theme(true);
    }
}

fn load_config() -> (Config, String, Option<String>) {
    let path = config::config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match Config::parse(&text) {
            Ok(c) => (c, text, None),
            Err(e) => (Config::default_with_places(), text, Some(e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let c = Config::default_with_places();
            let text = c.to_toml();
            if let Err(e) = config::save_atomic(&path, &text) {
                return (c, text, Some(format!("could not write {}: {e}", path.display())));
            }
            (c, text, None)
        }
        Err(e) => (Config::default_with_places(), String::new(), Some(e.to_string())),
    }
}

impl Config {
    pub fn default_with_places() -> Config {
        let home = glib::home_dir();
        let home_s = home.to_string_lossy().to_string();
        let mut places = vec![(String::from("~"), Some("user-home-symbolic".to_string()))];
        use glib::UserDirectory as U;
        for (d, icon) in [
            (U::Desktop, "user-desktop-symbolic"),
            (U::Documents, "folder-documents-symbolic"),
            (U::Downloads, "folder-download-symbolic"),
            (U::Pictures, "folder-pictures-symbolic"),
            (U::Music, "folder-music-symbolic"),
            (U::Videos, "folder-videos-symbolic"),
        ] {
            if let Some(p) = glib::user_special_dir(d) {
                if p != home {
                    let s = crate::util::contract_tilde(&p.to_string_lossy(), &home_s);
                    places.push((s, Some(icon.to_string())));
                }
            }
        }
        places.push(("trash:///".into(), Some("user-trash-symbolic".into())));
        Config::first_run(&places)
    }
}

fn tool_btn(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b
}

fn toggle_btn(icon: &str, tip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b
}

pub fn open_window(app: &gtk::Application, start: Option<gio::File>) {
    let paths = RicePaths::from_env();
    let tokens = theme::load_active(&paths);
    let (cfg, cfg_text, cfg_err) = load_config();

    let provider = gtk::CssProvider::new();
    if let Some(d) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&d, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
    }

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Burrow")
        .icon_name("system-file-manager")
        .default_width(1280)
        .default_height(800)
        .decorated(false)
        .build();
    window.add_css_class("burrow");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("burrow-root");
    root.set_overflow(gtk::Overflow::Hidden);
    window.set_child(Some(&root));

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_vexpand(true);
    paned.set_position(270);
    paned.set_shrink_start_child(false);
    paned.set_resize_start_child(false);
    root.append(&paned);

    let sidebar = sidebar::Sidebar::new();
    paned.set_start_child(Some(&sidebar.widget));

    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    paned.set_end_child(Some(&main));

    // top bar: tabs on the left, global controls on the right
    let topbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    topbar.add_css_class("topbar");
    let tab_scroll = gtk::ScrolledWindow::new();
    tab_scroll.set_vscrollbar_policy(gtk::PolicyType::Never);
    tab_scroll.set_hscrollbar_policy(gtk::PolicyType::External);
    tab_scroll.set_hexpand(true);
    tab_scroll.set_propagate_natural_height(true);
    let tab_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_bar.add_css_class("tabbar");
    tab_scroll.set_child(Some(&tab_bar));
    topbar.append(&tab_scroll);
    let new_tab = tool_btn("tab-new-symbolic", "New tab (Ctrl+T)");
    new_tab.add_css_class("flat-btn");
    new_tab.set_action_name(Some("win.new-tab"));
    topbar.append(&new_tab);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_width_request(12);
    topbar.append(&spacer);
    let filter_entry = gtk::SearchEntry::new();
    filter_entry.add_css_class("filter-entry");
    filter_entry.set_placeholder_text(Some("Filter"));
    filter_entry.set_width_chars(14);
    topbar.append(&filter_entry);
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    modes.add_css_class("segmented");
    let grid_b = toggle_btn("view-grid-symbolic", "Grid (Ctrl+1)");
    let compact_b = toggle_btn("view-compact-symbolic", "Compact (Ctrl+2)");
    let list_b = toggle_btn("view-list-symbolic", "List (Ctrl+3)");
    compact_b.set_group(Some(&grid_b));
    list_b.set_group(Some(&grid_b));
    for b in [&grid_b, &compact_b, &list_b] {
        b.add_css_class("flat-btn");
        modes.append(b);
    }
    topbar.append(&modes);
    let hidden_btn = toggle_btn("view-reveal-symbolic", "Show hidden files (Ctrl+H)");
    hidden_btn.add_css_class("flat-btn");
    topbar.append(&hidden_btn);
    let split_btn = toggle_btn("view-dual-symbolic", "Split view (F3)");
    split_btn.add_css_class("flat-btn");
    topbar.append(&split_btn);
    let menu = gio::Menu::new();
    menu.append(Some("Connect to server…"), Some("win.connect"));
    menu.append(Some("Settings"), Some("win.settings"));
    menu.append(Some("Open config file"), Some("win.open-config"));
    let sec = gio::Menu::new();
    sec.append(Some("New section in sidebar…"), Some("win.section-new"));
    menu.append_section(None, &sec);
    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Menu"));
    menu_btn.set_menu_model(Some(&menu));
    menu_btn.add_css_class("flat-btn");
    topbar.append(&menu_btn);
    main.append(&topbar);

    let tab_stack = gtk::Stack::new();
    tab_stack.set_vexpand(true);
    main.append(&tab_stack);

    // status bar
    let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    statusbar.add_css_class("statusbar");
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    let notice = gtk::Label::new(None);
    notice.set_hexpand(true);
    notice.set_xalign(0.0);
    notice.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let jobs = jobs::Jobs::new();
    statusbar.append(&status);
    statusbar.append(&notice);
    statusbar.append(&jobs.button);
    main.append(&statusbar);

    let home = gio::File::for_path(glib::home_dir());
    let w = Rc::new(Win {
        window,
        cfg: RefCell::new(cfg),
        cfg_text: RefCell::new(cfg_text),
        cfg_broken: Cell::new(cfg_err.is_some()),
        sys_icon_theme: gtk_settings_icon_theme(),
        tokens: RefCell::new(tokens),
        provider,
        paths,
        home: home.clone(),
        drives: RefCell::new(drives::list()),
        volume_monitor: gio::VolumeMonitor::get(),
        rice_monitors: RefCell::new(vec![]),
        cfg_monitor: RefCell::new(None),
        debounce: RefCell::new(None),
        cfg_debounce: RefCell::new(None),
        tabs: RefCell::new(vec![]),
        cur_tab: Cell::new(0),
        tab_bar,
        tab_stack,
        sidebar,
        settings: RefCell::new(None),
        syncing: Cell::new(false),
        hidden_btn,
        split_btn,
        mode_btns: [grid_b, compact_b, list_b],
        filter_entry,
        status,
        notice,
        jobs,
        undo: RefCell::new(vec![]),
        trash_count: Cell::new(0),
        trash_monitor: RefCell::new(None),
    });
    if let Some(e) = cfg_err {
        w.show_notice(&format!("config.toml has an error, using defaults (file left untouched): {e}"), true);
    }

    w.apply_theme();
    w.connect_toolbar();
    w.install_actions();
    w.watch_drives();
    w.watch_rice();
    w.watch_config();
    w.watch_trash();

    let start = start
        .map(|f| match f.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
            // A file passed on the command line: show its folder.
            gio::FileType::Regular => f.parent().unwrap_or(f),
            _ => f,
        })
        .map(Loc::Files)
        .unwrap_or(Loc::Files(home));
    w.new_tab(start, true);
    w.sidebar.rebuild(&w);

    // Periodic usage refresh (free space changes while copying, downloading…)
    let weak = Rc::downgrade(&w);
    glib::timeout_add_seconds_local(30, move || {
        let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
        w.sidebar.refresh_usage();
        for p in w.all_panes() {
            p.drives_page.refresh_usage();
        }
        glib::ControlFlow::Continue
    });

    WINDOWS.with(|ws| ws.borrow_mut().push(w.clone()));
    let weak = Rc::downgrade(&w);
    w.window.connect_close_request(move |_| {
        if let Some(w) = weak.upgrade() {
            if let Some(s) = w.settings.borrow_mut().take() {
                s.window.destroy();
            }
            WINDOWS.with(|ws| ws.borrow_mut().retain(|x| !Rc::ptr_eq(x, &w)));
        }
        glib::Propagation::Proceed
    });
    w.window.present();
    w.active().files.grab_focus();
}

impl Win {
    // ------------------------------------------------------------------ panes & tabs
    pub fn active_tab(&self) -> Rc<Tab> {
        let t = self.tabs.borrow();
        t[self.cur_tab.get().min(t.len() - 1)].clone()
    }

    pub fn active(&self) -> Rc<Pane> {
        self.active_tab().active_pane()
    }

    pub fn active_loc(&self) -> Loc {
        if self.tabs.borrow().is_empty() {
            return Loc::Files(self.home.clone());
        }
        self.active().loc.borrow().clone()
    }

    pub fn all_panes(&self) -> Vec<Rc<Pane>> {
        self.tabs.borrow().iter().flat_map(|t| t.panes.borrow().clone()).collect()
    }

    fn is_active(&self, p: &Pane) -> bool {
        !self.tabs.borrow().is_empty() && std::ptr::eq(Rc::as_ptr(&self.active()), p)
    }

    /// Navigate the active pane.
    pub fn go(self: &Rc<Self>, loc: Loc, record: bool) {
        self.active().go(loc, record);
    }

    pub fn new_tab(self: &Rc<Self>, loc: Loc, select: bool) {
        let pane = Pane::new(self, loc);
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        container.append(&pane.widget);

        let button = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        button.add_css_class("tab");
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.set_pixel_size(14);
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(18);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("tab-close");
        close.set_tooltip_text(Some("Close tab (Ctrl+W)"));
        button.append(&icon);
        button.append(&label);
        button.append(&close);

        let tab = Rc::new(Tab { container, panes: RefCell::new(vec![pane.clone()]), active: Cell::new(0), button, label, icon });
        let name = format!("tab-{:p}", Rc::as_ptr(&tab));
        self.tab_stack.add_named(&tab.container, Some(&name));
        self.tab_bar.append(&tab.button);

        let click = gtk::GestureClick::new();
        click.set_button(0);
        let (weak, tw) = (Rc::downgrade(self), Rc::downgrade(&tab));
        click.connect_released(move |g, _, _, _| {
            let (Some(w), Some(t)) = (weak.upgrade(), tw.upgrade()) else { return };
            let Some(i) = w.tab_index(&t) else { return };
            match g.current_button() {
                2 => w.close_tab(i),
                1 => w.select_tab(i),
                _ => {}
            }
        });
        tab.button.add_controller(click);
        let (weak, tw) = (Rc::downgrade(self), Rc::downgrade(&tab));
        close.connect_clicked(move |_| {
            let (Some(w), Some(t)) = (weak.upgrade(), tw.upgrade()) else { return };
            if let Some(i) = w.tab_index(&t) {
                w.close_tab(i);
            }
        });

        self.tabs.borrow_mut().push(tab.clone());
        self.update_tab(&tab);
        self.tab_bar.set_visible(self.tabs.borrow().len() > 1);
        if select || self.tabs.borrow().len() == 1 {
            self.select_tab(self.tabs.borrow().len() - 1);
        }
    }

    fn tab_index(&self, t: &Rc<Tab>) -> Option<usize> {
        self.tabs.borrow().iter().position(|x| Rc::ptr_eq(x, t))
    }

    pub fn select_tab(self: &Rc<Self>, i: usize) {
        let Some(tab) = self.tabs.borrow().get(i).cloned() else { return };
        self.cur_tab.set(i);
        self.tab_stack.set_visible_child(&tab.container);
        for (j, t) in self.tabs.borrow().iter().enumerate() {
            if j == i {
                t.button.add_css_class("active");
            } else {
                t.button.remove_css_class("active");
            }
        }
        self.active_changed();
        tab.active_pane().files.grab_focus();
    }

    pub fn close_tab(self: &Rc<Self>, i: usize) {
        if self.tabs.borrow().len() <= 1 {
            self.window.close();
            return;
        }
        let tab = self.tabs.borrow_mut().remove(i);
        self.tab_stack.remove(&tab.container);
        self.tab_bar.remove(&tab.button);
        self.tab_bar.set_visible(self.tabs.borrow().len() > 1);
        let cur = self.cur_tab.get();
        let next = if cur > i || cur >= self.tabs.borrow().len() { cur.saturating_sub(1) } else { cur };
        self.select_tab(next);
    }

    fn cycle_tab(self: &Rc<Self>, d: isize) {
        let n = self.tabs.borrow().len() as isize;
        if n > 1 {
            self.select_tab(((self.cur_tab.get() as isize + d).rem_euclid(n)) as usize);
        }
    }

    fn update_tab(&self, tab: &Tab) {
        let titles: Vec<String> = tab.panes.borrow().iter().map(|p| p.title()).collect();
        tab.label.set_text(&titles.join(" | "));
        tab.button.set_tooltip_text(Some(&titles.join("  |  ")));
        let split = titles.len() > 1;
        tab.icon.set_icon_name(Some(if split { "view-dual-symbolic" } else { "folder-symbolic" }));
    }

    pub fn toggle_split(self: &Rc<Self>) {
        let tab = self.active_tab();
        let n = tab.panes.borrow().len();
        if n == 1 {
            let loc = tab.active_pane().loc.borrow().clone();
            self.split_with(&tab, loc);
        } else {
            // close the inactive pane, keep the one you're in
            let keep = tab.active_pane();
            for p in tab.panes.borrow().iter() {
                if let Some(parent) = p.widget.parent() {
                    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
                        paned.set_start_child(None::<&gtk::Widget>);
                        paned.set_end_child(None::<&gtk::Widget>);
                    }
                }
            }
            while let Some(c) = tab.container.first_child() {
                tab.container.remove(&c);
            }
            tab.container.append(&keep.widget);
            *tab.panes.borrow_mut() = vec![keep];
            tab.active.set(0);
            self.update_tab(&tab);
            self.active_changed();
        }
    }

    fn split_with(self: &Rc<Self>, tab: &Rc<Tab>, loc: Loc) {
        if tab.panes.borrow().len() > 1 {
            // already split: just show it in the other pane
            let other = 1 - tab.active.get().min(1);
            let p = tab.panes.borrow()[other].clone();
            p.go(loc, true);
            return;
        }
        let first = tab.panes.borrow()[0].clone();
        let second = Pane::new(self, loc);
        tab.container.remove(&first.widget);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_hexpand(true);
        paned.set_start_child(Some(&first.widget));
        paned.set_end_child(Some(&second.widget));
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        tab.container.append(&paned);
        // equal halves once allocated
        let pw = paned.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(p) = pw.upgrade() {
                p.set_position(p.width() / 2);
            }
        });
        tab.panes.borrow_mut().push(second.clone());
        tab.active.set(1);
        self.update_tab(tab);
        self.active_changed();
        second.files.grab_focus();
    }

    pub fn set_active_pane(self: &Rc<Self>, p: &Rc<Pane>) {
        let tab = self.active_tab();
        let Some(i) = tab.panes.borrow().iter().position(|x| Rc::ptr_eq(x, p)) else { return };
        if tab.active.get() != i {
            tab.active.set(i);
            self.active_changed();
        }
    }

    /// Refresh everything that follows the active pane.
    fn active_changed(self: &Rc<Self>) {
        let tab = self.active_tab();
        let split = tab.panes.borrow().len() > 1;
        for (i, p) in tab.panes.borrow().iter().enumerate() {
            if split && i == tab.active.get() {
                p.widget.add_css_class("active-pane");
            } else {
                p.widget.remove_css_class("active-pane");
            }
        }
        let p = tab.active_pane();
        self.syncing.set(true);
        self.filter_entry.set_text(&p.files.filter_text());
        self.split_btn.set_active(split);
        self.syncing.set(false);
        self.sync_toolbar();
        self.sidebar.sync_selection(&p.loc.borrow());
        self.update_title();
        self.update_status();
    }

    /// Called by a pane after it navigated.
    pub fn pane_navigated(self: &Rc<Self>, p: &Pane) {
        let tabs = self.tabs.borrow().clone();
        if let Some(t) = tabs.iter().find(|t| t.panes.borrow().iter().any(|x| std::ptr::eq(Rc::as_ptr(x), p))) {
            self.update_tab(t);
        }
        if self.is_active(p) {
            self.syncing.set(true);
            self.filter_entry.set_text("");
            self.syncing.set(false);
            self.sync_toolbar();
            self.sidebar.sync_selection(&p.loc.borrow());
            self.update_title();
            self.update_status();
        }
    }

    fn update_title(&self) {
        let t = self.active().title();
        self.window.set_title(Some(&format!("{t} — Burrow")));
    }

    /// View mode buttons and hidden toggle follow the active pane / config.
    pub fn sync_toolbar(&self) {
        if self.tabs.borrow().is_empty() {
            return;
        }
        self.syncing.set(true);
        let p = self.active();
        let is_files = matches!(&*p.loc.borrow(), Loc::Files(_));
        let idx = match p.files.mode() {
            Mode::Grid => 0,
            Mode::Compact => 1,
            Mode::List => 2,
        };
        self.mode_btns[idx].set_active(true);
        for b in &self.mode_btns {
            b.set_sensitive(is_files);
        }
        self.hidden_btn.set_active(self.cfg.borrow().view.show_hidden);
        self.syncing.set(false);
    }

    pub fn update_status(&self) {
        if self.tabs.borrow().is_empty() {
            return;
        }
        self.status.set_text(&self.active().status_text());
    }

    pub fn launch(self: &Rc<Self>, f: &gio::File) {
        let launcher = gtk::FileLauncher::new(Some(f));
        let weak = Rc::downgrade(self);
        let name = display_name(f, &self.home);
        launcher.launch(Some(&self.window), gio::Cancellable::NONE, move |r| {
            if let (Err(e), Some(w)) = (r, weak.upgrade()) {
                if !e.matches(gtk::DialogError::Dismissed) && !e.matches(gtk::DialogError::Cancelled) {
                    w.show_notice(&format!("Could not open {name}: {}", e.message()), true);
                }
            }
        });
    }

    // ------------------------------------------------------------------ theme
    pub fn appearance(&self) -> Appearance {
        let c = self.cfg.borrow();
        let animations = gtk::Settings::default().map(|s| s.is_gtk_enable_animations()).unwrap_or(true);
        Appearance {
            radius: c.appearance.radius_px(),
            opacity: c.appearance.opacity,
            font: c.appearance.font.clone(),
            reduced_motion: c.appearance.reduced_motion || !animations,
        }
    }

    pub fn reduced_motion(&self) -> bool {
        self.appearance().reduced_motion
    }

    pub fn folder_color(&self) -> Option<String> {
        let fc = self.cfg.borrow().appearance.folder_color.trim().to_lowercase();
        match fc.as_str() {
            "none" | "" => None,
            "rice" => Some(theme::papirus_folder_color(&self.tokens.borrow()).to_string()),
            c if theme::is_papirus_color(c) => Some(c.to_string()),
            _ => None,
        }
    }

    fn apply_theme(&self) {
        let a = self.appearance();
        self.provider.load_from_string(&theme::css(&self.tokens.borrow(), &a));
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_font_name(Some(&format!("{} 11", a.font)));
            let cfg_theme = self.cfg.borrow().appearance.icon_theme.trim().to_string();
            let theme = if cfg_theme.is_empty() { self.sys_icon_theme.clone() } else { Some(cfg_theme) };
            if let Some(t) = theme {
                if s.gtk_icon_theme_name().as_deref() != Some(t.as_str()) {
                    s.set_gtk_icon_theme_name(Some(&t));
                }
            }
        }
        for p in self.all_panes() {
            p.apply_look(self);
        }
    }

    /// Watch the switcher's `current` file and the active rice's files; re-theme on change.
    fn watch_rice(self: &Rc<Self>) {
        let mut files = vec![self.paths.current_file()];
        if let Some(id) = theme::active_id(&self.paths) {
            if let Some(d) = theme::rice_dir(&self.paths, &id) {
                files.push(d.join("rice.json"));
                files.push(d.join("hypr/look.lua"));
            }
        }
        files.push(self.paths.noctalia.clone());
        let mut mons = vec![];
        for p in files {
            let f = gio::File::for_path(&p);
            let m = if p.is_dir() {
                f.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            } else {
                f.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            };
            let Ok(m) = m else { continue };
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, ev| {
                if matches!(ev, gio::FileMonitorEvent::AttributeChanged | gio::FileMonitorEvent::PreUnmount) {
                    return;
                }
                if let Some(w) = weak.upgrade() {
                    w.schedule_retheme();
                }
            });
            mons.push(m);
        }
        *self.rice_monitors.borrow_mut() = mons;
    }

    fn schedule_retheme(self: &Rc<Self>) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(w) = weak.upgrade() else { return };
            *w.debounce.borrow_mut() = None;
            let paths = w.paths.clone();
            let weak = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let Ok(t) = gio::spawn_blocking(move || theme::load_active(&paths)).await else { return };
                let Some(w) = weak.upgrade() else { return };
                let changed_rice = w.tokens.borrow().rice_id != t.rice_id;
                if *w.tokens.borrow() != t {
                    *w.tokens.borrow_mut() = t;
                    w.apply_theme();
                    w.sidebar.rebuild(&w);
                }
                if changed_rice {
                    w.watch_rice();
                }
            });
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    // ------------------------------------------------------------------ config
    fn watch_config(self: &Rc<Self>) {
        let f = gio::File::for_path(config::config_path());
        let Ok(m) = f.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) else { return };
        let weak = Rc::downgrade(self);
        m.connect_changed(move |_, _, _, ev| {
            if matches!(ev, gio::FileMonitorEvent::AttributeChanged) {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            if let Some(id) = w.cfg_debounce.borrow_mut().take() {
                id.remove();
            }
            let weak = Rc::downgrade(&w);
            let id = glib::timeout_add_local_once(Duration::from_millis(200), move || {
                let Some(w) = weak.upgrade() else { return };
                *w.cfg_debounce.borrow_mut() = None;
                w.reload_config();
            });
            *w.cfg_debounce.borrow_mut() = Some(id);
        });
        *self.cfg_monitor.borrow_mut() = Some(m);
    }

    fn reload_config(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(text) = gio::spawn_blocking(|| std::fs::read_to_string(config::config_path())).await else { return };
            let Ok(text) = text else { return }; // deleted mid-edit: keep what we have
            let Some(w) = weak.upgrade() else { return };
            if *w.cfg_text.borrow() == text {
                return;
            }
            match Config::parse(&text) {
                Ok(c) => {
                    *w.cfg_text.borrow_mut() = text;
                    *w.cfg.borrow_mut() = c;
                    w.cfg_broken.set(false);
                    w.show_notice("config.toml reloaded", false);
                    w.config_changed();
                }
                Err(e) => {
                    w.cfg_broken.set(true);
                    w.show_notice(&format!("config.toml: {e} (keeping the last good config)"), true);
                }
            }
        });
    }

    /// Re-apply everything that depends on the config (after a file edit or a UI change).
    pub fn config_changed(self: &Rc<Self>) {
        self.apply_theme();
        let cfg = self.cfg.borrow().clone();
        for p in self.all_panes() {
            p.files.config_changed(&cfg);
            p.drives_page.rebuild(self);
            if matches!(&*p.loc.borrow(), Loc::Network) {
                p.network_page.rebuild(self);
            }
        }
        self.sync_toolbar();
        self.sidebar.rebuild(self);
        if let Some(s) = self.settings.borrow().as_ref() {
            s.sync(self);
        }
    }

    /// Save after a change made in the UI. Atomic, off the UI thread.
    pub fn save_config(&self) {
        if self.cfg_broken.get() {
            self.show_notice("Not saving: fix the error in config.toml first (your file is untouched)", true);
            return;
        }
        let text = self.cfg.borrow().to_toml();
        *self.cfg_text.borrow_mut() = text.clone();
        let notice = self.notice.downgrade();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || config::save_atomic(&config::config_path(), &text)).await;
            if let (Ok(Err(e)), Some(n)) = (res, notice.upgrade()) {
                n.set_text(&format!("Could not save config: {e}"));
            }
        });
    }

    /// Change the config from the UI: mutate, save, re-apply.
    pub fn edit_config(self: &Rc<Self>, f: impl FnOnce(&mut Config)) {
        {
            let mut c = self.cfg.borrow_mut();
            f(&mut c);
            c.sanitize();
        }
        self.save_config();
        self.config_changed();
    }

    pub fn show_notice(&self, text: &str, error: bool) {
        self.notice.set_text(text);
        if error {
            self.notice.set_markup(&format!(
                "<span foreground=\"{}\">{}</span>",
                self.tokens.borrow().danger.hex(),
                glib::markup_escape_text(text)
            ));
        } else {
            let n = self.notice.downgrade();
            let t = text.to_string();
            glib::timeout_add_seconds_local_once(4, move || {
                if let Some(n) = n.upgrade() {
                    if n.text() == t {
                        n.set_text("");
                    }
                }
            });
        }
    }

    // ------------------------------------------------------------------ drives
    fn watch_drives(self: &Rc<Self>) {
        let m = &self.volume_monitor;
        macro_rules! on {
            ($sig:ident) => {{
                let weak = Rc::downgrade(self);
                m.$sig(move |_, _| {
                    if let Some(w) = weak.upgrade() {
                        w.reload_drives();
                    }
                });
            }};
        }
        on!(connect_volume_added);
        on!(connect_volume_removed);
        on!(connect_volume_changed);
        on!(connect_mount_added);
        on!(connect_mount_changed);
        let weak = Rc::downgrade(self);
        m.connect_mount_removed(move |_, mount| {
            let Some(w) = weak.upgrade() else { return };
            // Panes inside the removed drive leave it instead of showing errors.
            let root = mount.root();
            w.reload_drives();
            for p in w.all_panes() {
                let inside = matches!(&*p.loc.borrow(), Loc::Files(f) if f.equal(&root) || f.has_prefix(&root));
                if inside {
                    w.show_notice(&format!("{} was removed", mount.name()), true);
                    p.go(Loc::Files(w.home.clone()), true);
                }
            }
        });
    }

    pub fn reload_drives(self: &Rc<Self>) {
        *self.drives.borrow_mut() = drives::list();
        self.sidebar.rebuild(self);
        for p in self.all_panes() {
            p.drives_page.rebuild(self);
            if matches!(&*p.loc.borrow(), Loc::Network) {
                p.network_page.rebuild(self);
            }
        }
    }

    pub fn drive_display_name(&self, d: &Drive) -> String {
        self.cfg.borrow().drive(&d.key).name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| d.name.clone())
    }

    pub fn drive_icon(&self, d: &Drive) -> gio::Icon {
        self.cfg.borrow().drive(&d.key).icon.as_deref().map(icon_from_str).unwrap_or_else(|| d.icon.clone())
    }

    /// Open a drive in the active pane: mount first if needed.
    pub fn open_drive(self: &Rc<Self>, key: &str) {
        let Some(d) = self.drives.borrow().iter().find(|d| d.key == key).cloned() else { return };
        if let Some(root) = &d.root {
            self.go(Loc::Files(root.clone()), true);
            return;
        }
        self.mount_drive(&d, true);
    }

    fn mount_drive(self: &Rc<Self>, d: &Drive, open: bool) {
        let Some(v) = d.volume.clone() else { return };
        let op = gtk::MountOperation::new(Some(&self.window));
        let weak = Rc::downgrade(self);
        let name = self.drive_display_name(d);
        self.show_notice(&format!("Mounting {name}…"), false);
        v.clone().mount(gio::MountMountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |res| {
            let Some(w) = weak.upgrade() else { return };
            match res {
                Ok(()) => {
                    if let (true, Some(root)) = (open, v.get_mount().map(|m| m.root())) {
                        w.go(Loc::Files(root), true);
                    }
                }
                Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(e) => w.show_notice(&format!("Could not mount {name}: {}", friendly_auth_error(e.message())), true),
            }
        });
    }

    pub fn eject_drive(self: &Rc<Self>, key: &str) {
        let Some(d) = self.drives.borrow().iter().find(|d| d.key == key).cloned() else { return };
        let op = gtk::MountOperation::new(Some(&self.window));
        let name = self.drive_display_name(&d);
        let weak = Rc::downgrade(self);
        let done = move |res: Result<(), glib::Error>, verb: &str| {
            let Some(w) = weak.upgrade() else { return };
            match res {
                Ok(()) => w.show_notice(&format!("{name} {verb}, safe to remove"), false),
                Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(e) => w.show_notice(&format!("Could not {verb} {name}: {}", e.message()), true),
            }
        };
        // Leave the drive first so our own directory monitors don't keep it busy.
        if let Some(root) = &d.root {
            for p in self.all_panes() {
                let inside = matches!(&*p.loc.borrow(), Loc::Files(f) if f.equal(root) || f.has_prefix(root));
                if inside {
                    p.go(Loc::Files(self.home.clone()), true);
                }
            }
        }
        if let Some(m) = d.mount.clone().filter(|m| m.can_eject()) {
            m.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |r| done(r, "ejected"));
        } else if let Some(v) = d.volume.clone().filter(|v| v.can_eject()) {
            v.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |r| done(r, "ejected"));
        } else if let Some(m) = d.mount.clone().filter(|m| m.can_unmount()) {
            m.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |r| done(r, "unmounted"));
        }
    }

    // ------------------------------------------------------------------ toolbar / actions
    fn connect_toolbar(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.hidden_btn.connect_toggled(move |b| {
            let Some(w) = weak.upgrade() else { return };
            if w.syncing.get() || w.cfg.borrow().view.show_hidden == b.is_active() {
                return;
            }
            w.edit_config(|c| c.view.show_hidden = b.is_active());
        });
        let weak = Rc::downgrade(self);
        self.split_btn.connect_toggled(move |b| {
            let Some(w) = weak.upgrade() else { return };
            if w.syncing.get() {
                return;
            }
            let split = w.active_tab().panes.borrow().len() > 1;
            if split != b.is_active() {
                w.toggle_split();
            }
        });
        for (i, b) in self.mode_btns.iter().enumerate() {
            let weak = Rc::downgrade(self);
            b.connect_toggled(move |b| {
                let Some(w) = weak.upgrade() else { return };
                if w.syncing.get() || !b.is_active() {
                    return;
                }
                w.active().set_mode([Mode::Grid, Mode::Compact, Mode::List][i]);
            });
        }
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_search_changed(move |e| {
            let Some(w) = weak.upgrade() else { return };
            if !w.syncing.get() {
                w.active().files.set_filter(&e.text());
            }
        });
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_stop_search(move |e| {
            e.set_text("");
            if let Some(w) = weak.upgrade() {
                w.active().files.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_activate(move |_| {
            if let Some(w) = weak.upgrade() {
                w.active().files.grab_focus();
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let app = self.window.application().expect("window has an app");
        let add = |name: &str, accels: &[&str], f: Box<dyn Fn(&Rc<Win>)>| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w)
                }
            });
            self.window.add_action(&a);
            if !accels.is_empty() {
                app.set_accels_for_action(&format!("win.{name}"), accels);
            }
        };
        add("back", &["<Alt>Left", "<Alt>KP_Left", "Back"], Box::new(|w| w.active().go_back()));
        add("forward", &["<Alt>Right", "<Alt>KP_Right", "Forward"], Box::new(|w| w.active().go_forward()));
        add("up", &["<Alt>Up"], Box::new(|w| w.active().go_up()));
        add("home", &["<Alt>Home"], Box::new(|w| w.go(Loc::Files(w.home.clone()), true)));
        add("location", &["<Ctrl>l", "F6"], Box::new(|w| w.active().pathbar.start_editing()));
        add("filter", &["<Ctrl>f"], Box::new(|w| {
            w.filter_entry.grab_focus();
        }));
        add("hidden", &["<Ctrl>h"], Box::new(|w| w.hidden_btn.set_active(!w.hidden_btn.is_active())));
        add("reload", &["F5", "<Ctrl>r"], Box::new(|w| {
            w.active().reload();
            w.reload_drives();
        }));
        add("drives", &["<Ctrl>d"], Box::new(|w| w.go(Loc::Drives, true)));
        add("network", &["<Ctrl><Shift>d"], Box::new(|w| w.go(Loc::Network, true)));
        add("connect", &["<Ctrl><Shift>k"], Box::new(|w| w.connect_dialog()));
        add("cancel-connect", &[], Box::new(|w| w.active().stop_connecting()));
        add("new-tab", &["<Ctrl>t"], Box::new(|w| {
            let loc = w.active_loc();
            w.new_tab(loc, true);
        }));
        add("close-tab", &["<Ctrl>w"], Box::new(|w| w.close_tab(w.cur_tab.get())));
        add("next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"], Box::new(|w| w.cycle_tab(1)));
        add("prev-tab", &["<Ctrl><Shift>Tab", "<Ctrl>Page_Up", "<Ctrl><Shift>ISO_Left_Tab"], Box::new(|w| w.cycle_tab(-1)));
        add("split", &["F3"], Box::new(|w| w.toggle_split()));
        add("other-pane", &["<Ctrl>grave"], Box::new(|w| {
            let t = w.active_tab();
            if t.panes.borrow().len() > 1 {
                let o = 1 - t.active.get().min(1);
                let p = t.panes.borrow()[o].clone();
                w.set_active_pane(&p);
                p.files.grab_focus();
            }
        }));
        add("mode-grid", &["<Ctrl>1"], Box::new(|w| w.active().set_mode(Mode::Grid)));
        add("mode-compact", &["<Ctrl>2"], Box::new(|w| w.active().set_mode(Mode::Compact)));
        add("mode-list", &["<Ctrl>3"], Box::new(|w| w.active().set_mode(Mode::List)));
        add("quit", &["<Ctrl>q"], Box::new(|w| w.window.close()));
        add("settings", &["<Ctrl>comma"], Box::new(|w| settings::Settings::present(w)));
        add("open-config", &[], Box::new(|w| w.launch(&gio::File::for_path(config::config_path()))));
        add("section-new", &[], Box::new(|w| w.sidebar.new_section(w)));
        add("trash", &[], Box::new(|w| {
            let files = w.active().selected_files();
            w.trash_files(files);
        }));
        add("delete", &[], Box::new(|w| {
            let files = w.active().selected_files();
            w.delete_files(files);
        }));
        add("restore", &[], Box::new(|w| w.restore_selection()));
        add("extract-here", &[], Box::new(|w| {
            let files = w.active().selected_files();
            w.extract(files, None);
        }));
        add("extract-to", &[], Box::new(|w| {
            let files = w.active().selected_files();
            w.extract_to(files);
        }));
        add("compress", &[], Box::new(|w| {
            let files = w.active().selected_files();
            w.compress_dialog(files);
        }));
        add("empty-trash", &[], Box::new(|w| w.empty_trash()));
        add("undo", &["<Ctrl>z"], Box::new(|w| w.undo()));
        add("properties", &["<Alt>Return"], Box::new(|w| {
            let p = w.active();
            let mut files = p.selected_files();
            if files.is_empty() {
                files.extend(p.current_file());
            }
            w.properties(files);
        }));
        // Ctrl+C/X/V/A and F2 are handled by the file views' key controller, so text
        // fields keep their own copy/paste.
        add("copy", &[], Box::new(|w| w.clipboard_set(w.active().selected_files(), false)));
        add("cut", &[], Box::new(|w| w.clipboard_set(w.active().selected_files(), true)));
        add("paste", &[], Box::new(|w| {
            if let Some(d) = w.active().current_file() {
                w.paste_into(d);
            }
        }));
        add("new-folder", &["<Ctrl><Shift>n"], Box::new(|w| w.new_folder()));
        add("rename", &[], Box::new(|w| w.rename_selection()));
        add("select-all", &[], Box::new(|w| {
            w.active().files.selection.select_all();
        }));
        for i in 1..=8u32 {
            let a = gio::SimpleAction::new(&format!("tab-{i}"), None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    w.select_tab(i as usize - 1);
                }
            });
            self.window.add_action(&a);
            app.set_accels_for_action(&format!("win.tab-{i}"), &[&format!("<Alt>{i}")]);
        }

        let add_s = |name: &str, f: Box<dyn Fn(&Rc<Win>, String)>| {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, p| {
                if let (Some(w), Some(s)) = (weak.upgrade(), p.and_then(|p| p.get::<String>())) {
                    f(&w, s)
                }
            });
            self.window.add_action(&a);
        };
        add_s("goto-uri", Box::new(|w, uri| w.go(Loc::Files(gio::File::for_uri(&uri)), true)));
        add_s("open-uri", Box::new(|w, uri| {
            let f = gio::File::for_uri(&uri);
            let weak = Rc::downgrade(w);
            // directory -> navigate, file -> default app (type query is async)
            f.clone().query_info_async(
                "standard::type",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |r| {
                    let Some(w) = weak.upgrade() else { return };
                    match r.map(|i| i.file_type()) {
                        Ok(gio::FileType::Directory) => w.go(Loc::Files(f), true),
                        _ => w.launch(&f),
                    }
                },
            );
        }));
        add_s("open-tab", Box::new(|w, uri| w.new_tab(Loc::Files(gio::File::for_uri(&uri)), false)));
        add_s("open-split", Box::new(|w, uri| {
            let tab = w.active_tab();
            w.split_with(&tab, Loc::Files(gio::File::for_uri(&uri)));
        }));
        add_s("copy-path", Box::new(|w, uri| {
            let f = gio::File::for_uri(&uri);
            w.window.clipboard().set_text(&f.parse_name());
            w.show_notice("Path copied", false);
        }));
        add_s("new-file", Box::new(|w, kind| {
            if let Some(k) = create::NewKind::parse(&kind) {
                w.new_file(k);
            }
        }));
        add_s("new-template", Box::new(|w, uri| w.new_from_template(gio::File::for_uri(&uri))));
        add_s("shortcut", Box::new(|w, uri| w.create_shortcut(gio::File::for_uri(&uri))));
        add_s("terminal", Box::new(|w, uri| w.open_terminal(gio::File::for_uri(&uri))));
        add_s("open-with", Box::new(|w, app| {
            let files = w.active().selected_files();
            w.open_with(&app, files);
        }));
        add_s("choose-app", Box::new(|w, ctype| {
            let files = w.active().selected_files();
            if !files.is_empty() {
                w.choose_app(files, ctype);
            }
        }));
        add_s("share", Box::new(|w, uri| w.share_dialog(gio::File::for_uri(&uri))));
        add_s("paste-into", Box::new(|w, uri| w.paste_into(gio::File::for_uri(&uri))));
        add_s("view-mode", Box::new(|w, m| w.active().set_mode(Mode::parse(&m))));
        add_s("folder-icon", Box::new(|w, uri| {
            let key = files::folder_key(&gio::File::for_uri(&uri));
            let weak = Rc::downgrade(w);
            dialogs::pick_icon(&w.window, "Folder icon", false, move |icon| {
                let Some(w) = weak.upgrade() else { return };
                w.edit_config(|c| c.folders.entry(key.clone()).or_default().icon = Some(icon));
            });
        }));
        add_s("folder-icon-reset", Box::new(|w, uri| {
            let key = files::folder_key(&gio::File::for_uri(&uri));
            w.edit_config(|c| {
                if let Some(e) = c.folders.get_mut(&key) {
                    e.icon = None;
                }
            });
        }));

        // pin(uri, section id)
        let pin = gio::SimpleAction::new("pin", Some(&glib::VariantType::new("(ss)").unwrap()));
        let weak = Rc::downgrade(self);
        pin.connect_activate(move |_, p| {
            let (Some(w), Some((uri, sid))) = (weak.upgrade(), p.and_then(|p| p.get::<(String, String)>())) else { return };
            w.pin_folder(&gio::File::for_uri(&uri), &sid, None);
        });
        self.window.add_action(&pin);

        self.sidebar.install_actions(self);

        // Mouse back/forward buttons
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, _, _| {
            let Some(w) = weak.upgrade() else { return };
            match g.current_button() {
                8 => w.active().go_back(),
                9 => w.active().go_forward(),
                _ => {}
            }
        });
        self.window.add_controller(click);
    }

    /// Add a folder to a sidebar section (at `index`, or the end). No duplicates per section.
    pub fn pin_folder(self: &Rc<Self>, f: &gio::File, section_id: &str, index: Option<usize>) {
        let path = config_path_for(f);
        let name = display_name(f, &self.home);
        let mut dup = false;
        self.edit_config(|c| {
            let Some(s) = c.sections.iter_mut().find(|s| s.id == section_id) else { return };
            if s.items.iter().any(|i| matches!(i, Item::Folder { path: p, .. } if *p == path)) {
                dup = true;
                return;
            }
            let it = Item::Folder { path, label: None, icon: None };
            match index {
                Some(i) if i <= s.items.len() => s.items.insert(i, it),
                _ => s.items.push(it),
            }
        });
        if dup {
            self.show_notice(&format!("{name} is already in that section"), false);
        } else {
            self.show_notice(&format!("Pinned {name}"), false);
        }
    }
}

/// How a folder is written into the config: "~/x" for local, URI otherwise.
pub fn config_path_for(f: &gio::File) -> String {
    match f.path() {
        Some(p) if f.is_native() => crate::util::contract_tilde(&p.to_string_lossy(), &glib::home_dir().to_string_lossy()),
        _ => f.uri().to_string(),
    }
}

/// Icon from a config string: an icon-theme name, or a path to an image file.
pub fn icon_from_str(s: &str) -> gio::Icon {
    let s = s.trim();
    if s.contains('/') {
        let home = glib::home_dir();
        let p = crate::util::expand_tilde(s, &home.to_string_lossy());
        gio::FileIcon::new(&gio::File::for_path(p)).upcast()
    } else {
        gio::ThemedIcon::new(s).upcast()
    }
}

/// Human name for a location without any I/O.
pub fn display_name(f: &gio::File, home: &gio::File) -> String {
    if f.equal(home) {
        return "Home".into();
    }
    if f.uri_scheme().as_deref() == Some("trash") {
        return "Trash".into();
    }
    match f.basename() {
        Some(b) if b.as_os_str() != "/" => b.to_string_lossy().to_string(),
        _ => {
            if f.is_native() {
                "File System".into()
            } else {
                net_name(&f.uri())
            }
        }
    }
}

/// "smb://ari@nas/media/" -> "media on nas", "sftp://pi/" -> "pi"
pub fn net_name(uri: &str) -> String {
    let Some((_, rest)) = uri.split_once("://") else { return uri.to_string() };
    let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = auth.rsplit('@').next().unwrap_or(auth);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let last = glib::Uri::unescape_string(last, None).map(|s| s.to_string()).unwrap_or_else(|| last.to_string());
    match (host.is_empty(), last.is_empty()) {
        (false, true) => host.to_string(),
        (false, false) => format!("{last} on {host}"),
        _ => uri.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_titles() {
        assert_eq!(super::net_name("smb://ari@nas/My%20Films/"), "My Films on nas");
        assert_eq!(super::net_name("sftp://pi:2222/"), "pi:2222");
        assert_eq!(super::net_name("network:///"), "network:///");
    }
}

pub fn file_from_config_path(p: &str) -> gio::File {
    let home = glib::home_dir();
    let p = crate::util::expand_tilde(p.trim(), &home.to_string_lossy());
    if p.contains("://") || p.starts_with("trash:") {
        gio::File::for_uri(&p)
    } else {
        gio::File::for_path(p)
    }
}

impl Win {
    // ------------------------------------------------------------------ trash & delete
    pub fn trash_files(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        let n = files.len();
        let title = if n == 1 { format!("Moving {} to Trash", display_name(&files[0], &self.home)) } else { format!("Moving {n} items to Trash") };
        jobs::run(self, &title, move |p, c| crate::ops::trash(&files, p, c), |w, (ok, since), p, cancelled| {
            let paths: Vec<String> = ok.iter().filter_map(|f| f.path()).map(|p| p.to_string_lossy().to_string()).collect();
            if !paths.is_empty() {
                w.undo.borrow_mut().push(UndoOp::Trash { paths, since });
            }
            let k = ok.len();
            let text = if k == 1 { format!("Moved {} to Trash · Ctrl+Z to undo", display_name(&ok[0], &w.home)) } else { format!("Moved {k} items to Trash · Ctrl+Z to undo") };
            let (msg, err) = jobs::summary(&text, p, cancelled);
            w.show_notice(&msg, err);
            w.update_trash_count();
        });
    }

    /// Delete for good, after asking.
    pub fn delete_files(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        let n = files.len();
        let what = if n == 1 { format!("“{}”", display_name(&files[0], &self.home)) } else { format!("{n} items") };
        let weak = Rc::downgrade(self);
        dialogs::confirm(
            &self.window,
            &format!("Delete {what} for good?"),
            "This can't be undone. Folders are deleted with everything inside them.",
            "Delete",
            move || {
                let Some(w) = weak.upgrade() else { return };
                let title = format!("Deleting {what}");
                jobs::run(&w, &title, move |p, c| crate::ops::delete(&files, p, c), move |w, (), p, cancelled| {
                    let (msg, err) = jobs::summary(&format!("Deleted {n} item{}", if n == 1 { "" } else { "s" }), p, cancelled);
                    w.show_notice(&msg, err);
                    w.update_trash_count();
                });
            },
        );
    }

    fn restore_selection(self: &Rc<Self>) {
        let items: Vec<crate::ops::Trashed> = self
            .active()
            .files
            .selected_infos()
            .iter()
            .filter_map(|i| {
                let item = files::info_file(i)?;
                let orig = i.attribute_byte_string("trash::orig-path")?.to_string();
                Some(crate::ops::Trashed { item, orig, deleted: 0 })
            })
            .collect();
        self.restore_items(items);
    }

    fn restore_items(self: &Rc<Self>, items: Vec<crate::ops::Trashed>) {
        if items.is_empty() {
            return;
        }
        let n = items.len();
        jobs::run(self, &format!("Restoring {n} item{}", if n == 1 { "" } else { "s" }), move |p, c| crate::ops::restore(&items, p, c), |w, k, p, cancelled| {
            let (msg, err) = jobs::summary(&format!("Restored {k} item{}", if k == 1 { "" } else { "s" }), p, cancelled);
            w.show_notice(&msg, err);
            w.update_trash_count();
        });
    }

    fn empty_trash(self: &Rc<Self>) {
        let n = self.trash_count.get();
        let weak = Rc::downgrade(self);
        dialogs::confirm(
            &self.window,
            "Empty the Trash?",
            &format!("{n} item{} will be deleted for good. This can't be undone.", if n == 1 { "" } else { "s" }),
            "Empty Trash",
            move || {
                let Some(w) = weak.upgrade() else { return };
                jobs::run(&w, "Emptying Trash", crate::ops::empty_trash, |w, (), p, cancelled| {
                    let (msg, err) = jobs::summary("Trash emptied", p, cancelled);
                    w.show_notice(&msg, err);
                    w.update_trash_count();
                });
            },
        );
    }

    fn undo(self: &Rc<Self>) {
        let Some(op) = self.undo.borrow_mut().pop() else {
            self.show_notice("Nothing to undo", false);
            return;
        };
        match op {
            UndoOp::Transfer(items) => {
                let n = items.len();
                jobs::run(self, &format!("Undo: {n} item{}", if n == 1 { "" } else { "s" }), move |p, c| crate::transfer::undo(&items, p, c), |w, k, p, cancelled| {
                    let (msg, err) = jobs::summary(&format!("Undone ({k} item{})", if k == 1 { "" } else { "s" }), p, cancelled);
                    w.show_notice(&msg, err);
                });
            }
            UndoOp::Rename { file, old_name } => {
                let weak = Rc::downgrade(self);
                let target = old_name.clone();
                file.set_display_name_async(&target, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                    if let Some(w) = weak.upgrade() {
                        match r {
                            Ok(_) => w.show_notice(&format!("Undo: renamed back to {old_name}"), false),
                            Err(e) => w.show_notice(&format!("Couldn't undo the rename: {}", e.message()), true),
                        }
                    }
                });
            }
            UndoOp::Batch(items) => {
                let back: Vec<(gio::File, String)> = items;
                let n = back.len();
                jobs::run(self, &format!("Undo: renaming {n} items back"), move |p, c| crate::rename::rename_all(&back, p, c), |w, done, p, cancelled| {
                    let (msg, err) = jobs::summary(&format!("Undo: {} names restored", done.len()), p, cancelled);
                    w.show_notice(&msg, err);
                });
            }
            UndoOp::Created(f) => {
                let weak = Rc::downgrade(self);
                f.trash_async(glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                    if let Some(w) = weak.upgrade() {
                        match r {
                            Ok(()) => w.show_notice("Undo: moved it to the Trash", false),
                            Err(e) => w.show_notice(&format!("Couldn't undo: {}", e.message()), true),
                        }
                    }
                });
            }
            UndoOp::NewFolder(f) => {
                let weak = Rc::downgrade(self);
                // only removes it while still empty
                f.delete_async(glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                    if let Some(w) = weak.upgrade() {
                        match r {
                            Ok(()) => w.show_notice("Undo: folder removed", false),
                            Err(_) => w.show_notice("Not undone: the new folder isn't empty any more", true),
                        }
                    }
                });
            }
            UndoOp::Trash { paths, since } => {
                let n = paths.len();
                jobs::run(
                    self,
                    &format!("Undo: restoring {n} item{}", if n == 1 { "" } else { "s" }),
                    move |p, c| {
                        let all = crate::ops::list_trash(c);
                        let items = crate::ops::match_trashed(&all, &paths, since);
                        let missing = paths.len() - items.len();
                        (crate::ops::restore(&items, p, c), missing)
                    },
                    |w, (k, missing), p, cancelled| {
                        let mut text = format!("Undo: restored {k} item{}", if k == 1 { "" } else { "s" });
                        if missing > 0 {
                            text.push_str(&format!(" ({missing} no longer in the Trash)"));
                        }
                        let (msg, err) = jobs::summary(&text, p, cancelled);
                        w.show_notice(&msg, err || missing > 0);
                        w.update_trash_count();
                    },
                );
            }
        }
    }

    fn watch_trash(self: &Rc<Self>) {
        let t = gio::File::for_uri("trash:///");
        if let Ok(m) = t.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, _| {
                if let Some(w) = weak.upgrade() {
                    w.update_trash_count();
                }
            });
            *self.trash_monitor.borrow_mut() = Some(m);
        }
        self.update_trash_count();
    }

    pub fn update_trash_count(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gio::File::for_uri("trash:///").query_info_async(
            "trash::item-count",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::LOW,
            gio::Cancellable::NONE,
            move |r| {
                let (Some(w), Ok(i)) = (weak.upgrade(), r) else { return };
                let n = i.attribute_uint32("trash::item-count") as u64;
                if w.trash_count.replace(n) != n {
                    w.sidebar.rebuild(&w);
                    for p in w.all_panes() {
                        p.update_trash_bar(n);
                    }
                }
            },
        );
    }
}

impl Win {
    // ------------------------------------------------------------------ copy / move / paste
    /// Copy or move into `dest`. Auto = move on the same drive, copy across drives.
    pub fn transfer(self: &Rc<Self>, sources: Vec<gio::File>, dest: gio::File, mode: crate::transfer::Mode) {
        use crate::transfer::Mode;
        // dropping something onto itself does nothing
        let sources: Vec<gio::File> = sources.into_iter().filter(|s| !s.equal(&dest)).collect();
        if sources.is_empty() {
            return;
        }
        let n = sources.len();
        let what = if n == 1 { format!("“{}”", display_name(&sources[0], &self.home)) } else { format!("{n} items") };
        let verb = match mode {
            Mode::Copy => "Copying",
            Mode::Move => "Moving",
            Mode::Auto => "Moving",
        };
        let dname = display_name(&dest, &self.home);
        let title = format!("{verb} {what} to {dname}");
        jobs::run(self, &title, move |p, c| crate::transfer::transfer(&sources, &dest, mode, p, c), move |w, done, p, cancelled| {
            let k = done.len();
            let moved = done.iter().filter(|d| d.moved).count();
            let verb = if moved == k && k > 0 { "Moved" } else if moved == 0 { "Copied" } else { "Copied/moved" };
            if k > 0 {
                w.undo.borrow_mut().push(UndoOp::Transfer(done));
            }
            let text = format!("{verb} {k} item{} to {dname}{}", if k == 1 { "" } else { "s" }, if k > 0 { " · Ctrl+Z to undo" } else { "" });
            let (msg, err) = jobs::summary(&text, p, cancelled);
            w.show_notice(&msg, err);
            w.sidebar.refresh_usage();
        });
    }

    /// Put files on the clipboard in the formats other file managers use too.
    pub fn clipboard_set(self: &Rc<Self>, files: Vec<gio::File>, cut: bool) {
        if files.is_empty() {
            return;
        }
        let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
        let urilist = format!("{}\r\n", uris.join("\r\n"));
        let list = gdk::FileList::from_array(&files);
        let providers = [
            gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &glib::Bytes::from_owned(gnome.into_bytes())),
            gdk::ContentProvider::for_bytes("application/x-kde-cutselection", &glib::Bytes::from_owned(if cut { b"1".to_vec() } else { b"0".to_vec() })),
            gdk::ContentProvider::for_value(&list.to_value()),
            gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned(urilist.into_bytes())),
        ];
        let union = gdk::ContentProvider::new_union(&providers);
        let _ = self.window.clipboard().set_content(Some(&union));
        let n = files.len();
        self.show_notice(&format!("{} {n} item{} · paste with Ctrl+V", if cut { "Cut" } else { "Copied" }, if n == 1 { "" } else { "s" }), false);
    }

    /// Paste the clipboard into `dest` (copy, or move if it was cut).
    pub fn paste_into(self: &Rc<Self>, dest: gio::File) {
        let cb = self.window.clipboard();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // GNOME/Nemo/Thunar format first: it says copy or cut
            let mut parsed: Option<(bool, Vec<gio::File>)> = None;
            if let Ok((stream, _)) = cb.read_future(&["x-special/gnome-copied-files"], glib::Priority::DEFAULT).await {
                let out = gio::MemoryOutputStream::new_resizable();
                if out.splice_future(&stream, gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET, glib::Priority::DEFAULT).await.is_ok() {
                    let text = String::from_utf8_lossy(&out.steal_as_bytes()).to_string();
                    let mut lines = text.lines();
                    let cut = lines.next().map(str::trim) == Some("cut");
                    let files: Vec<gio::File> = lines.map(str::trim).filter(|l| !l.is_empty()).map(gio::File::for_uri).collect();
                    if !files.is_empty() {
                        parsed = Some((cut, files));
                    }
                }
            }
            if parsed.is_none() {
                if let Ok(v) = cb.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await {
                    if let Ok(list) = v.get::<gdk::FileList>() {
                        parsed = Some((false, list.files()));
                    }
                }
            }
            let Some(w) = weak.upgrade() else { return };
            match parsed {
                Some((cut, files)) => {
                    w.transfer(files, dest, if cut { crate::transfer::Mode::Move } else { crate::transfer::Mode::Copy });
                    if cut {
                        // a cut is pasted once
                        let _ = w.window.clipboard().set_content(None::<&gdk::ContentProvider>);
                    }
                }
                None => w.show_notice("Nothing to paste", false),
            }
        });
    }

    fn new_folder(self: &Rc<Self>) {
        let Some(dir) = self.active().current_file() else { return };
        let weak = Rc::downgrade(self);
        dialogs::prompt_text(&self.window, "New folder", "", "New Folder", move |name| {
            let Some(w) = weak.upgrade() else { return };
            if let Some(err) = check_name(&name) {
                w.show_notice(err, true);
                return;
            }
            let f = dir.child(&name);
            let weak = Rc::downgrade(&w);
            f.clone().make_directory_async(glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                let Some(w) = weak.upgrade() else { return };
                match r {
                    Ok(()) => {
                        w.undo.borrow_mut().push(UndoOp::NewFolder(f));
                        w.show_notice(&format!("Created {name}"), false);
                    }
                    Err(e) if e.matches(gio::IOErrorEnum::Exists) => w.show_notice(&format!("“{name}” already exists here"), true),
                    Err(e) => w.show_notice(&format!("Couldn't create the folder: {}", e.message()), true),
                }
            });
        });
    }

    fn rename_selection(self: &Rc<Self>) {
        let sel = self.active().files.selected_infos();
        if sel.len() > 1 {
            self.batch_rename(sel);
            return;
        }
        if sel.len() != 1 {
            return;
        }
        let info = sel[0].clone();
        let Some(file) = files::info_file(&info) else { return };
        let old = info.display_name().to_string();
        let weak = Rc::downgrade(self);
        let initial = old.clone();
        dialogs::prompt_text(&self.window, "Rename", "", &initial, move |name| {
            let Some(w) = weak.upgrade() else { return };
            if name == old {
                return;
            }
            if let Some(err) = check_name(&name) {
                w.show_notice(err, true);
                return;
            }
            if file.parent().is_some_and(|p| p.child(&name).query_exists(gio::Cancellable::NONE)) {
                w.show_notice(&format!("“{name}” already exists here"), true);
                return;
            }
            let weak = Rc::downgrade(&w);
            let old = old.clone();
            let new_name = name.clone();
            file.set_display_name_async(&new_name, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |r| {
                let Some(w) = weak.upgrade() else { return };
                match r {
                    Ok(newf) => {
                        w.undo.borrow_mut().push(UndoOp::Rename { file: newf, old_name: old });
                        w.show_notice(&format!("Renamed to {name} · Ctrl+Z to undo"), false);
                    }
                    Err(e) => w.show_notice(&format!("Couldn't rename: {}", e.message()), true),
                }
            });
        });
    }
}

/// Polkit refusals become a plain explanation instead of a D-Bus error dump.
pub(crate) fn friendly_auth_error(msg: &str) -> String {
    if msg.contains("NotAuthorizedCanObtain") || msg.contains("Not authorized") || msg.contains("authentication agent") {
        "this needs your password, but no password prompt (polkit agent) is running in your session".into()
    } else {
        msg.trim().lines().last().unwrap_or(msg).to_string()
    }
}

/// Reject names the file system or the user would regret.
pub(crate) fn check_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name == "." || name == ".." {
        Some("That name can't be used")
    } else if name.contains('/') || name.contains('\0') {
        Some("Names can't contain “/”")
    } else if name.len() > 255 {
        Some("That name is too long")
    } else {
        None
    }
}

