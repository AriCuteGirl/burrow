//! UI: one `Win` per window. All file system work goes through async GIO calls or
//! gio::spawn_blocking; the UI thread only builds widgets.

mod drives_page;
mod files;
mod pathbar;
mod sidebar;

use crate::config::{self, Config};
use crate::drives::{self, Drive};
use crate::theme::{self, Appearance, RicePaths, Tokens};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone)]
pub enum Loc {
    Files(gio::File),
    Drives,
}

impl Loc {
    fn same(&self, o: &Loc) -> bool {
        match (self, o) {
            (Loc::Files(a), Loc::Files(b)) => a.equal(b),
            (Loc::Drives, Loc::Drives) => true,
            _ => false,
        }
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

    pub loc: RefCell<Loc>,
    back: RefCell<Vec<Loc>>,
    fwd: RefCell<Vec<Loc>>,

    pub drives: RefCell<Vec<Drive>>,
    volume_monitor: gio::VolumeMonitor,
    rice_monitors: RefCell<Vec<gio::FileMonitor>>,
    cfg_monitor: RefCell<Option<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    cfg_debounce: RefCell<Option<glib::SourceId>>,

    pub files: files::Files,
    pub pathbar: pathbar::Pathbar,
    pub sidebar: sidebar::Sidebar,
    pub drives_page: drives_page::DrivesPage,
    content: gtk::Stack,
    back_btn: gtk::Button,
    fwd_btn: gtk::Button,
    up_btn: gtk::Button,
    hidden_btn: gtk::ToggleButton,
    pub filter_entry: gtk::SearchEntry,
    status: gtk::Label,
    notice: gtk::Label,
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

    // toolbar
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toolbar.add_css_class("toolbar");
    let mk = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(tip));
        b
    };
    let back_btn = mk("go-previous-symbolic", "Back (Alt+Left)");
    let fwd_btn = mk("go-next-symbolic", "Forward (Alt+Right)");
    let up_btn = mk("go-up-symbolic", "Parent folder (Alt+Up)");
    toolbar.append(&back_btn);
    toolbar.append(&fwd_btn);
    toolbar.append(&up_btn);
    let pathbar = pathbar::Pathbar::new();
    pathbar.widget.set_hexpand(true);
    toolbar.append(&pathbar.widget);
    let filter_entry = gtk::SearchEntry::new();
    filter_entry.add_css_class("filter-entry");
    filter_entry.set_placeholder_text(Some("Filter"));
    filter_entry.set_width_chars(16);
    toolbar.append(&filter_entry);
    let hidden_btn = gtk::ToggleButton::new();
    hidden_btn.set_icon_name("view-reveal-symbolic");
    hidden_btn.set_tooltip_text(Some("Show hidden files (Ctrl+H)"));
    toolbar.append(&hidden_btn);
    main.append(&toolbar);

    let content = gtk::Stack::new();
    content.set_vexpand(true);
    content.set_transition_type(gtk::StackTransitionType::Crossfade);
    let files = files::Files::new(&cfg);
    let drives_page = drives_page::DrivesPage::new();
    content.add_named(&files.widget, Some("files"));
    content.add_named(&drives_page.widget, Some("drives"));
    main.append(&content);

    // status bar
    let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    statusbar.add_css_class("statusbar");
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    let notice = gtk::Label::new(None);
    notice.set_hexpand(true);
    notice.set_xalign(0.0);
    notice.set_ellipsize(gtk::pango::EllipsizeMode::End);
    statusbar.append(&status);
    statusbar.append(&notice);
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
        loc: RefCell::new(Loc::Files(home.clone())),
        back: RefCell::new(vec![]),
        fwd: RefCell::new(vec![]),
        drives: RefCell::new(vec![]),
        volume_monitor: gio::VolumeMonitor::get(),
        rice_monitors: RefCell::new(vec![]),
        cfg_monitor: RefCell::new(None),
        debounce: RefCell::new(None),
        cfg_debounce: RefCell::new(None),
        files,
        pathbar,
        sidebar,
        drives_page,
        content,
        back_btn,
        fwd_btn,
        up_btn,
        hidden_btn,
        filter_entry,
        status,
        notice,
    });
    if let Some(e) = cfg_err {
        w.show_notice(&format!("config.toml has an error, using defaults (file left untouched): {e}"), true);
    }

    w.apply_theme();
    w.files.connect(&w);
    w.pathbar.connect(&w);
    w.connect_toolbar();
    w.install_actions();
    w.watch_drives();
    w.watch_rice();
    w.watch_config();
    w.reload_drives();

    let start = start
        .map(|f| match f.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
            // A file passed on the command line: show its folder.
            gio::FileType::Regular => f.parent().unwrap_or(f),
            _ => f,
        })
        .map(Loc::Files)
        .unwrap_or(Loc::Files(home));
    w.go(start, false);

    // Periodic usage refresh (free space changes while copying, downloading…)
    let weak = Rc::downgrade(&w);
    glib::timeout_add_seconds_local(30, move || {
        let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
        w.sidebar.refresh_usage();
        w.drives_page.refresh_usage();
        glib::ControlFlow::Continue
    });

    WINDOWS.with(|ws| ws.borrow_mut().push(w.clone()));
    let weak = Rc::downgrade(&w);
    w.window.connect_close_request(move |_| {
        if let Some(w) = weak.upgrade() {
            WINDOWS.with(|ws| ws.borrow_mut().retain(|x| !Rc::ptr_eq(x, &w)));
        }
        glib::Propagation::Proceed
    });
    w.window.present();
    w.files.grab_focus();
}

impl Win {
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

    fn apply_theme(&self) {
        let t = self.tokens.borrow();
        let a = self.appearance();
        self.provider.load_from_string(&theme::css(&t, &a));
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
        let fc = self.cfg.borrow().appearance.folder_color.trim().to_lowercase();
        self.files.set_folder_color(match fc.as_str() {
            "none" | "" => None,
            "rice" => Some(theme::papirus_folder_color(&t).to_string()),
            c if theme::is_papirus_color(c) => Some(c.to_string()),
            _ => None,
        });
        self.content.set_transition_duration(if a.reduced_motion { 0 } else { 140 });
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

    /// Re-apply everything that depends on the config.
    pub fn config_changed(self: &Rc<Self>) {
        self.apply_theme();
        self.files.config_changed(&self.cfg.borrow());
        self.hidden_btn.set_active(self.cfg.borrow().view.show_hidden);
        self.sidebar.rebuild(self);
        self.drives_page.rebuild(self);
    }

    /// Save after a change made in the UI. Atomic, off the UI thread.
    pub fn save_config(&self) {
        if self.cfg_broken.get() {
            self.show_notice("Not saving: fix the error in config.toml first (your file is untouched)", true);
            return;
        }
        let text = self.cfg.borrow().to_toml();
        *self.cfg_text.borrow_mut() = text.clone();
        let weak_win = self.window.downgrade();
        let notice = self.notice.downgrade();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || config::save_atomic(&config::config_path(), &text)).await;
            if let (Ok(Err(e)), Some(n), Some(_)) = (res, notice.upgrade(), weak_win.upgrade()) {
                n.set_text(&format!("Could not save config: {e}"));
                n.add_css_class("error");
            }
        });
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
            // If we were inside the removed drive, leave it instead of showing errors.
            let root = mount.root();
            let inside = matches!(&*w.loc.borrow(), Loc::Files(f) if f.equal(&root) || f.has_prefix(&root));
            w.reload_drives();
            if inside {
                w.show_notice(&format!("{} was removed", mount.name()), true);
                w.go(Loc::Files(w.home.clone()), true);
            }
        });
    }

    pub fn reload_drives(self: &Rc<Self>) {
        *self.drives.borrow_mut() = drives::list();
        self.sidebar.rebuild(self);
        self.drives_page.rebuild(self);
    }

    pub fn drive_display_name(&self, d: &Drive) -> String {
        self.cfg.borrow().drive(&d.key).name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| d.name.clone())
    }

    pub fn drive_icon(&self, d: &Drive) -> gio::Icon {
        self.cfg.borrow().drive(&d.key).icon.as_deref().map(icon_from_str).unwrap_or_else(|| d.icon.clone())
    }

    /// Open a drive: mount first if needed.
    pub fn open_drive(self: &Rc<Self>, key: &str) {
        let Some(d) = self.drives.borrow().iter().find(|d| d.key == key).cloned() else { return };
        if let Some(root) = &d.root {
            self.go(Loc::Files(root.clone()), true);
            return;
        }
        let Some(v) = d.volume.clone() else { return };
        let op = gtk::MountOperation::new(Some(&self.window));
        let weak = Rc::downgrade(self);
        let name = self.drive_display_name(&d);
        self.show_notice(&format!("Mounting {name}…"), false);
        v.clone().mount(gio::MountMountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |res| {
            let Some(w) = weak.upgrade() else { return };
            match res {
                Ok(()) => {
                    if let Some(root) = v.get_mount().map(|m| m.root()) {
                        w.go(Loc::Files(root), true);
                    }
                }
                Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(e) => w.show_notice(&format!("Could not mount {name}: {}", e.message()), true),
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
        // Leave the drive first so our own directory monitor doesn't keep it busy.
        if let (Some(root), Loc::Files(f)) = (&d.root, &*self.loc.borrow()) {
            if f.equal(root) || f.has_prefix(root) {
                let w = self.clone();
                glib::idle_add_local_once(move || w.go(Loc::Files(w.home.clone()), true));
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

    // ------------------------------------------------------------------ navigation
    pub fn go(self: &Rc<Self>, loc: Loc, record: bool) {
        let prev = self.loc.borrow().clone();
        if record && !prev.same(&loc) {
            self.back.borrow_mut().push(prev);
            self.fwd.borrow_mut().clear();
        }
        *self.loc.borrow_mut() = loc.clone();
        self.filter_entry.set_text("");
        match &loc {
            Loc::Files(f) => {
                self.content.set_visible_child_name("files");
                self.files.open(f);
                self.pathbar.set_file(f, &self.home);
                self.window.set_title(Some(&format!("{} — Burrow", display_name(f, &self.home))));
            }
            Loc::Drives => {
                self.content.set_visible_child_name("drives");
                self.pathbar.set_drives();
                self.drives_page.refresh_usage();
                self.window.set_title(Some("Drives — Burrow"));
            }
        }
        self.sidebar.sync_selection(&loc);
        self.update_nav();
        self.update_status();
    }

    fn update_nav(&self) {
        self.back_btn.set_sensitive(!self.back.borrow().is_empty());
        self.fwd_btn.set_sensitive(!self.fwd.borrow().is_empty());
        let up = matches!(&*self.loc.borrow(), Loc::Files(f) if f.parent().is_some());
        self.up_btn.set_sensitive(up);
    }

    pub fn go_back(self: &Rc<Self>) {
        let Some(l) = self.back.borrow_mut().pop() else { return };
        self.fwd.borrow_mut().push(self.loc.borrow().clone());
        self.go(l, false);
    }
    pub fn go_forward(self: &Rc<Self>) {
        let Some(l) = self.fwd.borrow_mut().pop() else { return };
        self.back.borrow_mut().push(self.loc.borrow().clone());
        self.go(l, false);
    }
    pub fn go_up(self: &Rc<Self>) {
        let p = match &*self.loc.borrow() {
            Loc::Files(f) => f.parent(),
            Loc::Drives => None,
        };
        if let Some(p) = p {
            self.go(Loc::Files(p), true);
        }
    }

    pub fn update_status(&self) {
        let text = match &*self.loc.borrow() {
            Loc::Drives => {
                let n = self.drives_page.count.get();
                format!("{n} drive{}", if n == 1 { "" } else { "s" })
            }
            Loc::Files(_) => self.files.status_text(),
        };
        self.status.set_text(&text);
    }

    // ------------------------------------------------------------------ toolbar / actions
    fn connect_toolbar(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.back_btn.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.go_back()
            }
        });
        let weak = Rc::downgrade(self);
        self.fwd_btn.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.go_forward()
            }
        });
        let weak = Rc::downgrade(self);
        self.up_btn.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.go_up()
            }
        });
        self.hidden_btn.set_active(self.cfg.borrow().view.show_hidden);
        let weak = Rc::downgrade(self);
        self.hidden_btn.connect_toggled(move |b| {
            let Some(w) = weak.upgrade() else { return };
            if w.cfg.borrow().view.show_hidden == b.is_active() {
                return;
            }
            w.cfg.borrow_mut().view.show_hidden = b.is_active();
            w.files.config_changed(&w.cfg.borrow());
            w.save_config();
        });
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_search_changed(move |e| {
            if let Some(w) = weak.upgrade() {
                w.files.set_filter(&e.text());
            }
        });
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_stop_search(move |e| {
            e.set_text("");
            if let Some(w) = weak.upgrade() {
                w.files.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        self.filter_entry.connect_activate(move |_| {
            if let Some(w) = weak.upgrade() {
                w.files.grab_focus();
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
            app.set_accels_for_action(&format!("win.{name}"), accels);
        };
        add("back", &["<Alt>Left", "<Alt>KP_Left", "Back"], Box::new(|w| w.go_back()));
        add("forward", &["<Alt>Right", "<Alt>KP_Right", "Forward"], Box::new(|w| w.go_forward()));
        add("up", &["<Alt>Up"], Box::new(|w| w.go_up()));
        add("home", &["<Alt>Home"], Box::new(|w| w.go(Loc::Files(w.home.clone()), true)));
        add("location", &["<Ctrl>l", "F6"], Box::new(|w| w.pathbar.start_editing()));
        add("filter", &["<Ctrl>f"], Box::new(|w| { w.filter_entry.grab_focus(); }));
        add("hidden", &["<Ctrl>h"], Box::new(|w| w.hidden_btn.set_active(!w.hidden_btn.is_active())));
        add("reload", &["F5", "<Ctrl>r"], Box::new(|w| {
            let l = w.loc.borrow().clone();
            w.go(l, false);
            w.reload_drives();
        }));
        add("drives", &["<Ctrl>d"], Box::new(|w| w.go(Loc::Drives, true)));
        add("close", &["<Ctrl>w", "<Ctrl>q"], Box::new(|w| w.window.close()));

        let goto = gio::SimpleAction::new("goto-uri", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        goto.connect_activate(move |_, p| {
            let (Some(w), Some(uri)) = (weak.upgrade(), p.and_then(|p| p.get::<String>())) else { return };
            w.go(Loc::Files(gio::File::for_uri(&uri)), true);
        });
        self.window.add_action(&goto);

        // Mouse back/forward buttons
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, _, _| {
            let Some(w) = weak.upgrade() else { return };
            match g.current_button() {
                8 => w.go_back(),
                9 => w.go_forward(),
                _ => {}
            }
        });
        self.window.add_controller(click);
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
                f.uri().to_string()
            }
        }
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
