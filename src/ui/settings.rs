//! Settings window on top of config.toml. Every control writes the config right away;
//! editing the file by hand updates the controls (Win::config_changed -> sync).

use super::Win;
use crate::config::{self, RadiusCfg};
use crate::theme;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

const MODES: [&str; 3] = ["grid", "compact", "list"];
const SORTS: [&str; 4] = ["name", "size", "modified", "type"];

pub struct Settings {
    pub window: gtk::Window,
    syncing: Rc<Cell<bool>>,
    radius_mode: gtk::DropDown,
    radius_px: gtk::SpinButton,
    opacity: gtk::Scale,
    font: gtk::FontDialogButton,
    icon_theme: gtk::DropDown,
    icon_themes: Rc<RefCell<Vec<String>>>,
    folder_color: gtk::DropDown,
    reduced_motion: gtk::Switch,
    default_mode: gtk::DropDown,
    show_hidden: gtk::Switch,
    folders_first: gtk::Switch,
    icon_size: gtk::Scale,
    drives_page: gtk::Switch,
    network_page: gtk::Switch,
    thumbnails: gtk::Switch,
    preview_pane: gtk::Switch,
    vim_keys: gtk::Switch,
    extras: super::extras::Extras,
    sort: gtk::DropDown,
    sort_desc: gtk::Switch,
}

fn folder_colors() -> Vec<String> {
    let mut v = vec!["rice".to_string(), "none".to_string()];
    v.extend(theme::papirus_color_names().iter().map(|s| s.to_string()));
    v
}

fn folder_color_label(c: &str) -> String {
    match c {
        "rice" => "Follow the rice".into(),
        "none" => "Icon theme default".into(),
        c => {
            let mut s = c.to_string();
            if let Some(f) = s.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            s
        }
    }
}

pub(super) fn row(group: &gtk::Box, title: &str, subtitle: &str, control: &impl IsA<gtk::Widget>) {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    r.add_css_class("settings-row");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("settings-title");
    t.set_xalign(0.0);
    text.append(&t);
    if !subtitle.is_empty() {
        let s = gtk::Label::new(Some(subtitle));
        s.add_css_class("settings-sub");
        s.set_xalign(0.0);
        s.set_wrap(true);
        text.append(&s);
    }
    r.append(&text);
    control.set_valign(gtk::Align::Center);
    r.append(control);
    group.append(&r);
}

fn group(root: &gtk::Box, title: &str) -> gtk::Box {
    let l = gtk::Label::new(Some(title));
    l.add_css_class("group-title");
    l.set_xalign(0.0);
    root.append(&l);
    let g = gtk::Box::new(gtk::Orientation::Vertical, 0);
    g.add_css_class("settings-group");
    root.append(&g);
    g
}

fn dropdown(items: &[&str]) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(items);
    d.add_css_class("settings-dropdown");
    d
}

impl Settings {
    pub fn present(w: &Rc<Win>) {
        if let Some(s) = w.settings.borrow().as_ref() {
            s.window.present();
            return;
        }
        let s = Settings::build(w);
        s.sync(w);
        s.window.present();
        *w.settings.borrow_mut() = Some(s);
    }

    fn build(w: &Rc<Win>) -> Settings {
        let window = gtk::Window::builder()
            .transient_for(&w.window)
            .title("Burrow settings")
            .default_width(620)
            .default_height(720)
            .decorated(false)
            .build();
        window.add_css_class("burrow");
        window.add_css_class("dialog");
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.add_css_class("dialog-root");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("page-title");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        head.append(&title);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat-btn");
        close.set_tooltip_text(Some("Close (Esc)"));
        head.append(&close);
        outer.append(&head);
        let sub = gtk::Label::new(Some("Saved to ~/.config/burrow/config.toml as you change things. Editing that file updates this window too."));
        sub.add_css_class("page-subtitle");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        outer.append(&sub);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_margin_end(8);
        scroll.set_child(Some(&root));
        outer.append(&scroll);

        // ---------------- appearance
        let g = group(&root, "APPEARANCE");
        let rbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let radius_mode = dropdown(&["Follow the rice", "Custom"]);
        let radius_px = gtk::SpinButton::with_range(0.0, 32.0, 1.0);
        radius_px.add_css_class("settings-spin");
        rbox.append(&radius_mode);
        rbox.append(&radius_px);
        row(&g, "Corner radius", "Follow your Hyprland rounding, or pick pixels", &rbox);
        let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.3, 1.0, 0.01);
        opacity.set_width_request(200);
        opacity.set_draw_value(true);
        opacity.set_digits(2);
        row(&g, "Background opacity", "Lower lets Hyprland blur show through", &opacity);
        let font_dialog = gtk::FontDialog::new();
        let font = gtk::FontDialogButton::new(Some(font_dialog));
        font.set_level(gtk::FontLevel::Family);
        row(&g, "Font", "Used for the whole window", &font);
        let icon_theme = dropdown(&["From GTK settings"]);
        row(&g, "Icon theme", "", &icon_theme);
        let fc: Vec<String> = folder_colors().iter().map(|c| folder_color_label(c)).collect();
        let fc_refs: Vec<&str> = fc.iter().map(|s| s.as_str()).collect();
        let folder_color = dropdown(&fc_refs);
        row(&g, "Folder colour", "Papirus folder colours; “Follow the rice” picks the closest to your accent", &folder_color);
        let reduced_motion = gtk::Switch::new();
        row(&g, "Reduce motion", "Turns off transitions and the loading pulse", &reduced_motion);

        // ---------------- view
        let g = group(&root, "BROWSING");
        let default_mode = dropdown(&["Grid", "Compact", "List"]);
        row(&g, "Default view", "Folders you switch remember their own view", &default_mode);
        let icon_size = gtk::Scale::with_range(gtk::Orientation::Horizontal, 32.0, 256.0, 8.0);
        icon_size.set_width_request(200);
        icon_size.set_draw_value(true);
        icon_size.set_digits(0);
        row(&g, "Grid icon size", "", &icon_size);
        let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let sort = dropdown(&["Name", "Size", "Modified", "Type"]);
        let sort_desc = gtk::Switch::new();
        sort_desc.set_tooltip_text(Some("Descending"));
        let desc_l = gtk::Label::new(Some("Descending"));
        desc_l.add_css_class("settings-sub");
        sbox.append(&sort);
        sbox.append(&desc_l);
        sbox.append(&sort_desc);
        row(&g, "Sort by", "", &sbox);
        let folders_first = gtk::Switch::new();
        row(&g, "Folders first", "", &folders_first);
        let show_hidden = gtk::Switch::new();
        row(&g, "Show hidden files", "Ctrl+H toggles it anywhere", &show_hidden);
        let drives_page = gtk::Switch::new();
        row(&g, "Drives page in the sidebar", "The overview with a usage bar per drive", &drives_page);
        let network_page = gtk::Switch::new();
        row(&g, "Network page in the sidebar", "Servers, other computers, and what you share", &network_page);
        let thumbnails = gtk::Switch::new();
        row(&g, "Thumbnails", "For images, videos, PDFs, fonts and more", &thumbnails);
        let preview_pane = gtk::Switch::new();
        row(&g, "Preview pane", "Space shows a quick look any time", &preview_pane);
        let vim_keys = gtk::Switch::new();
        row(&g, "Vim keys", "hjkl, gg, G, yy, dd, p, / … in the file view; typing then doesn't filter", &vim_keys);
        let extras = super::extras::Extras::new(&root);

        // ---------------- footer
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        foot.set_margin_top(12);
        let open = gtk::Button::with_label("Open config.toml");
        open.add_css_class("secondary");
        foot.append(&open);
        let sp = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sp.set_hexpand(true);
        foot.append(&sp);
        let done = gtk::Button::with_label("Done");
        done.add_css_class("primary");
        foot.append(&done);
        outer.append(&foot);
        window.set_child(Some(&outer));

        let s = Settings {
            window,
            syncing: Rc::new(Cell::new(false)),
            radius_mode,
            radius_px,
            opacity,
            font,
            icon_theme,
            icon_themes: Rc::new(RefCell::new(vec![])),
            folder_color,
            reduced_motion,
            default_mode,
            show_hidden,
            folders_first,
            icon_size,
            drives_page,
            network_page,
            thumbnails,
            preview_pane,
            vim_keys,
            extras,
            sort,
            sort_desc,
        };
        s.connect(w, &close, &done, &open);
        s.load_icon_themes(w);
        s
    }

    fn connect(&self, w: &Rc<Win>, close: &gtk::Button, done: &gtk::Button, open: &gtk::Button) {
        let weak = Rc::downgrade(w);
        let sync = self.syncing.clone();
        // apply now; `debounce` for sliders so dragging doesn't write the file 60 times a second
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let edit = {
            let weak = weak.clone();
            let sync = sync.clone();
            move |f: Box<dyn FnOnce(&mut config::Config)>| {
                if sync.get() {
                    return;
                }
                if let Some(w) = weak.upgrade() {
                    w.edit_config(f);
                }
            }
        };
        let edit_slow = {
            let weak = weak.clone();
            let sync = sync.clone();
            let pending = pending.clone();
            move |f: Box<dyn Fn(&mut config::Config)>| {
                if sync.get() {
                    return;
                }
                if let Some(id) = pending.borrow_mut().take() {
                    id.remove();
                }
                let (weak, pending2) = (weak.clone(), pending.clone());
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                    *pending2.borrow_mut() = None;
                    if let Some(w) = weak.upgrade() {
                        w.edit_config(|c| f(c));
                    }
                });
                *pending.borrow_mut() = Some(id);
            }
        };

        let (e, px) = (edit.clone(), self.radius_px.clone());
        self.radius_mode.connect_selected_notify(move |d| {
            let custom = d.selected() == 1;
            px.set_sensitive(custom);
            let v = px.value() as u32;
            e(Box::new(move |c| c.appearance.radius = if custom { RadiusCfg::Px(v) } else { RadiusCfg::Mode("rice".into()) }));
        });
        let (e, rm) = (edit_slow.clone(), self.radius_mode.clone());
        self.radius_px.connect_value_changed(move |s| {
            if rm.selected() == 1 {
                let v = s.value() as u32;
                e(Box::new(move |c| c.appearance.radius = RadiusCfg::Px(v)));
            }
        });
        let e = edit_slow.clone();
        self.opacity.connect_value_changed(move |s| {
            let v = (s.value() * 100.0).round() / 100.0;
            e(Box::new(move |c| c.appearance.opacity = v));
        });
        let e = edit.clone();
        self.font.connect_font_desc_notify(move |b| {
            if let Some(fam) = b.font_desc().and_then(|d| d.family()) {
                let fam = fam.to_string();
                e(Box::new(move |c| c.appearance.font = fam));
            }
        });
        let (e, themes) = (edit.clone(), self.icon_themes.clone());
        self.icon_theme.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            let v = if i == 0 { String::new() } else { themes.borrow().get(i - 1).cloned().unwrap_or_default() };
            e(Box::new(move |c| c.appearance.icon_theme = v));
        });
        let e = edit.clone();
        self.folder_color.connect_selected_notify(move |d| {
            let v = folder_colors().get(d.selected() as usize).cloned().unwrap_or_else(|| "rice".into());
            e(Box::new(move |c| c.appearance.folder_color = v));
        });
        let e = edit.clone();
        self.reduced_motion.connect_active_notify(move |s| {
            let v = s.is_active();
            e(Box::new(move |c| c.appearance.reduced_motion = v));
        });
        let e = edit.clone();
        self.default_mode.connect_selected_notify(move |d| {
            let v = MODES[d.selected().min(2) as usize].to_string();
            e(Box::new(move |c| c.view.default_mode = v));
        });
        let e = edit_slow.clone();
        self.icon_size.connect_value_changed(move |s| {
            let v = s.value() as u32;
            e(Box::new(move |c| c.view.icon_size = v));
        });
        let e = edit.clone();
        self.sort.connect_selected_notify(move |d| {
            let v = SORTS[d.selected().min(3) as usize].to_string();
            e(Box::new(move |c| c.view.sort = v));
        });
        for (sw, f) in [
            (&self.sort_desc, (|c: &mut config::Config, v| c.view.sort_desc = v) as fn(&mut config::Config, bool)),
            (&self.folders_first, |c, v| c.view.folders_first = v),
            (&self.show_hidden, |c, v| c.view.show_hidden = v),
            (&self.drives_page, |c, v| c.view.drives_page = v),
            (&self.network_page, |c, v| c.view.network_page = v),
            (&self.thumbnails, |c, v| c.view.thumbnails = v),
            (&self.preview_pane, |c, v| c.view.preview_pane = v),
            (&self.vim_keys, |c, v| c.view.vim_keys = v),
        ] {
            let e = edit.clone();
            sw.connect_active_notify(move |s| {
                let v = s.is_active();
                e(Box::new(move |c| f(c, v)));
            });
        }

        let close_fn = {
            let weak: Weak<Win> = weak.clone();
            move || {
                if let Some(w) = weak.upgrade() {
                    if let Some(s) = w.settings.borrow_mut().take() {
                        s.window.destroy();
                    }
                }
            }
        };
        let c = close_fn.clone();
        close.connect_clicked(move |_| c());
        let c = close_fn.clone();
        done.connect_clicked(move |_| c());
        let c = close_fn.clone();
        self.window.connect_close_request(move |_| {
            c();
            glib::Propagation::Proceed
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, k, _, _| {
            if k == gtk::gdk::Key::Escape {
                close_fn();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let weak2 = weak.clone();
        open.connect_clicked(move |_| {
            if let Some(w) = weak2.upgrade() {
                w.launch(&gio::File::for_path(config::config_path()));
            }
        });
    }

    /// Installed icon themes (folders with an index.theme that has icon directories).
    fn load_icon_themes(&self, w: &Rc<Win>) {
        let weak = Rc::downgrade(w);
        glib::spawn_future_local(async move {
            let Ok(names) = gio::spawn_blocking(scan_icon_themes).await else { return };
            let Some(w) = weak.upgrade() else { return };
            let Some(s) = w.settings.borrow().as_ref().map(|s| (s.icon_theme.clone(), s.icon_themes.clone(), s.syncing.clone())) else { return };
            let (dd, store, syncing) = s;
            *store.borrow_mut() = names.clone();
            let mut labels = vec!["From GTK settings".to_string()];
            labels.extend(names);
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            syncing.set(true);
            dd.set_model(Some(&gtk::StringList::new(&refs)));
            syncing.set(false);
            if let Some(s) = w.settings.borrow().as_ref() {
                s.sync(&w);
            }
        });
    }

    /// Controls follow the config (called after any config change).
    pub fn sync(&self, w: &Rc<Win>) {
        let c = w.cfg.borrow().clone();
        self.syncing.set(true);
        match c.appearance.radius {
            RadiusCfg::Px(p) => {
                self.radius_mode.set_selected(1);
                self.radius_px.set_value(p as f64);
                self.radius_px.set_sensitive(true);
            }
            RadiusCfg::Mode(_) => {
                self.radius_mode.set_selected(0);
                self.radius_px.set_value(w.tokens.borrow().radius as f64);
                self.radius_px.set_sensitive(false);
            }
        }
        if (self.opacity.value() - c.appearance.opacity).abs() > 0.004 {
            self.opacity.set_value(c.appearance.opacity);
        }
        let cur = self.font.font_desc().and_then(|d| d.family()).map(|f| f.to_string());
        if cur.as_deref() != Some(c.appearance.font.as_str()) {
            self.font.set_font_desc(&gtk::pango::FontDescription::from_string(&c.appearance.font));
        }
        let it = c.appearance.icon_theme.trim();
        let idx = if it.is_empty() { 0 } else { self.icon_themes.borrow().iter().position(|t| t == it).map(|i| i + 1).unwrap_or(0) };
        self.icon_theme.set_selected(idx as u32);
        let fc = folder_colors().iter().position(|x| *x == c.appearance.folder_color.trim().to_lowercase()).unwrap_or(0);
        self.folder_color.set_selected(fc as u32);
        self.reduced_motion.set_active(c.appearance.reduced_motion);
        self.default_mode.set_selected(MODES.iter().position(|m| *m == c.view.default_mode).unwrap_or(0) as u32);
        if (self.icon_size.value() as u32) != c.view.icon_size {
            self.icon_size.set_value(c.view.icon_size as f64);
        }
        self.sort.set_selected(SORTS.iter().position(|m| *m == c.view.sort).unwrap_or(0) as u32);
        self.sort_desc.set_active(c.view.sort_desc);
        self.folders_first.set_active(c.view.folders_first);
        self.show_hidden.set_active(c.view.show_hidden);
        self.drives_page.set_active(c.view.drives_page);
        self.network_page.set_active(c.view.network_page);
        self.thumbnails.set_active(c.view.thumbnails);
        self.preview_pane.set_active(c.view.preview_pane);
        self.vim_keys.set_active(c.view.vim_keys);
        self.syncing.set(false);
        self.extras.sync(w);
    }
}

fn scan_icon_themes() -> Vec<String> {
    let mut dirs = vec![glib::home_dir().join(".icons"), glib::user_data_dir().join("icons")];
    dirs.extend(glib::system_data_dirs().into_iter().map(|d| d.join("icons")));
    let mut out = std::collections::BTreeSet::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let idx = e.path().join("index.theme");
            let Ok(text) = std::fs::read_to_string(&idx) else { continue };
            // cursor-only themes have no icon Directories
            let has_dirs = text.lines().any(|l| l.trim_start().starts_with("Directories=") && l.trim().len() > "Directories=".len());
            let hidden = text.lines().any(|l| l.trim() == "Hidden=true");
            if has_dirs && !hidden {
                if let Some(n) = e.file_name().to_str() {
                    if n != "default" && n != "hicolor" {
                        out.insert(n.to_string());
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}
