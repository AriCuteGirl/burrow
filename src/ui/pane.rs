//! A pane: one browsing area with its own location, history, path bar and view.
//! A tab holds one pane, or two side by side in split view.

use super::drives_page::DrivesPage;
use super::files::{self, Files, Mode};
use super::pathbar::Pathbar;
use super::{Loc, Win, display_name};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub struct Pane {
    win: Weak<Win>,
    pub widget: gtk::Box,
    pub files: Files,
    pub pathbar: Pathbar,
    pub drives_page: DrivesPage,
    content: gtk::Stack,
    trash_bar: gtk::Revealer,
    trash_text: gtk::Label,
    empty_btn: gtk::Button,
    pub loc: RefCell<Loc>,
    back: RefCell<Vec<Loc>>,
    fwd: RefCell<Vec<Loc>>,
    back_btn: gtk::Button,
    fwd_btn: gtk::Button,
    up_btn: gtk::Button,
}

impl Pane {
    pub fn new(w: &Rc<Win>, start: Loc) -> Rc<Pane> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("pane");
        widget.set_hexpand(true);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("toolbar");
        let mk = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.set_action_name(Some(action));
            b
        };
        let back_btn = mk("go-previous-symbolic", "Back (Alt+Left)", "win.back");
        let fwd_btn = mk("go-next-symbolic", "Forward (Alt+Right)", "win.forward");
        let up_btn = mk("go-up-symbolic", "Parent folder (Alt+Up)", "win.up");
        // Per-pane buttons: don't use the win actions (they act on the active pane, which
        // this pane becomes on click anyway, but sensitivity must be per pane).
        for b in [&back_btn, &fwd_btn, &up_btn] {
            b.set_action_name(None);
        }
        header.append(&back_btn);
        header.append(&fwd_btn);
        header.append(&up_btn);
        let pathbar = Pathbar::new();
        pathbar.widget.set_hexpand(true);
        header.append(&pathbar.widget);
        widget.append(&header);

        // shown while browsing the trash
        let trash_bar = gtk::Revealer::new();
        let tb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        tb.add_css_class("trash-bar");
        let ti = gtk::Image::from_icon_name("user-trash-symbolic");
        let trash_text = gtk::Label::new(None);
        trash_text.set_xalign(0.0);
        trash_text.set_hexpand(true);
        trash_text.add_css_class("settings-sub");
        let restore_btn = gtk::Button::with_label("Restore selected");
        restore_btn.add_css_class("secondary");
        restore_btn.set_action_name(Some("win.restore"));
        let empty_btn = gtk::Button::with_label("Empty Trash");
        empty_btn.add_css_class("danger");
        empty_btn.set_action_name(Some("win.empty-trash"));
        tb.append(&ti);
        tb.append(&trash_text);
        tb.append(&restore_btn);
        tb.append(&empty_btn);
        trash_bar.set_child(Some(&tb));
        widget.append(&trash_bar);

        let content = gtk::Stack::new();
        content.set_vexpand(true);
        content.set_transition_type(gtk::StackTransitionType::Crossfade);
        let files = Files::new(&w.cfg.borrow());
        let drives_page = DrivesPage::new();
        content.add_named(&files.widget, Some("files"));
        content.add_named(&drives_page.widget, Some("drives"));
        widget.append(&content);

        let p = Rc::new(Pane {
            win: Rc::downgrade(w),
            widget,
            files,
            pathbar,
            drives_page,
            content,
            trash_bar,
            trash_text,
            empty_btn,
            loc: RefCell::new(start.clone()),
            back: RefCell::new(vec![]),
            fwd: RefCell::new(vec![]),
            back_btn,
            fwd_btn,
            up_btn,
        });
        p.files.connect(&p);
        p.pathbar.connect(&p);
        let weak = Rc::downgrade(&p);
        p.back_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.go_back()
            }
        });
        let weak = Rc::downgrade(&p);
        p.fwd_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.go_forward()
            }
        });
        let weak = Rc::downgrade(&p);
        p.up_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.go_up()
            }
        });

        // Clicking or focusing anything in the pane makes it the active one.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&p);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(p) = weak.upgrade() {
                if let Some(w) = p.win() {
                    w.set_active_pane(&p);
                }
            }
        });
        p.widget.add_controller(click);
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&p);
        focus.connect_enter(move |_| {
            if let Some(p) = weak.upgrade() {
                if let Some(w) = p.win() {
                    w.set_active_pane(&p);
                }
            }
        });
        p.widget.add_controller(focus);

        p.apply_look(w);
        p.drives_page.rebuild(w);
        p.go(start, false);
        p
    }

    pub fn win(&self) -> Option<Rc<Win>> {
        self.win.upgrade()
    }

    pub fn reduced_motion(&self) -> bool {
        self.win().is_some_and(|w| w.reduced_motion())
    }

    /// Theme-dependent bits that live in each pane.
    pub fn apply_look(&self, w: &Win) {
        self.files.set_folder_color(w.folder_color());
        self.content.set_transition_duration(if w.reduced_motion() { 0 } else { 140 });
    }

    pub fn title(&self) -> String {
        match &*self.loc.borrow() {
            Loc::Files(f) => display_name(f, &gio::File::for_path(glib::home_dir())),
            Loc::Drives => "Drives".into(),
        }
    }

    // ------------------------------------------------------------------ navigation
    pub fn go(self: &Rc<Self>, loc: Loc, record: bool) {
        let Some(w) = self.win() else { return };
        let prev = self.loc.borrow().clone();
        if record && !prev.same(&loc) {
            self.back.borrow_mut().push(prev);
            self.fwd.borrow_mut().clear();
            if self.back.borrow().len() > 200 {
                self.back.borrow_mut().remove(0);
            }
        }
        *self.loc.borrow_mut() = loc.clone();
        match &loc {
            Loc::Files(f) => {
                // per-folder view, else the default
                let mode = {
                    let cfg = w.cfg.borrow();
                    cfg.folders
                        .get(&files::folder_key(f))
                        .and_then(|c| c.view.as_deref())
                        .map(Mode::parse)
                        .unwrap_or_else(|| Mode::parse(&cfg.view.default_mode))
                };
                self.files.set_mode(mode);
                self.content.set_visible_child_name("files");
                self.files.open(f);
                self.pathbar.set_file(f, &w.home);
            }
            Loc::Drives => {
                self.content.set_visible_child_name("drives");
                self.pathbar.set_drives();
                self.drives_page.refresh_usage();
            }
        }
        self.update_nav();
        self.trash_bar.set_reveal_child(self.in_trash());
        self.update_trash_bar(w.trash_count.get());
        w.pane_navigated(self);
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
    pub fn reload(self: &Rc<Self>) {
        match &*self.loc.borrow() {
            Loc::Files(_) => self.files.reload(),
            Loc::Drives => self.drives_page.refresh_usage(),
        }
    }

    pub fn in_trash(&self) -> bool {
        matches!(&*self.loc.borrow(), Loc::Files(f) if f.uri_scheme().as_deref() == Some("trash"))
    }

    pub fn update_trash_bar(&self, n: u64) {
        self.trash_text.set_text(&if n == 0 {
            "The Trash is empty".to_string()
        } else {
            format!("{n} item{} in the Trash · Delete removes for good here", if n == 1 { "" } else { "s" })
        });
        self.empty_btn.set_sensitive(n > 0);
    }

    pub fn selected_files(&self) -> Vec<gio::File> {
        self.files.selected_infos().iter().filter_map(files::info_file).collect()
    }

    pub fn current_file(&self) -> Option<gio::File> {
        match &*self.loc.borrow() {
            Loc::Files(f) => Some(f.clone()),
            Loc::Drives => None,
        }
    }

    // ------------------------------------------------------------------ view
    /// Change the view mode; remembered for this folder unless it equals the default.
    pub fn set_mode(self: &Rc<Self>, m: Mode) {
        let Some(w) = self.win() else { return };
        self.files.set_mode(m);
        if let Some(f) = self.current_file() {
            let key = files::folder_key(&f);
            let mut cfg = w.cfg.borrow_mut();
            let default = cfg.view.default_mode.clone();
            let entry = cfg.folders.entry(key.clone()).or_default();
            entry.view = (m.name() != default).then(|| m.name().to_string());
            if entry.view.is_none() && entry.icon.is_none() {
                cfg.folders.remove(&key);
            }
        }
        w.save_config();
        w.sync_toolbar();
    }

    pub fn set_sort(self: &Rc<Self>, key: &str, desc: bool) {
        let Some(w) = self.win() else { return };
        {
            let mut cfg = w.cfg.borrow_mut();
            if cfg.view.sort == key && cfg.view.sort_desc == desc {
                return;
            }
            cfg.view.sort = key.to_string();
            cfg.view.sort_desc = desc;
        }
        w.save_config();
        w.config_changed();
    }

    pub fn status_changed(&self) {
        if let Some(w) = self.win() {
            w.update_status();
        }
    }

    pub fn status_text(&self) -> String {
        match &*self.loc.borrow() {
            Loc::Drives => {
                let n = self.drives_page.count.get();
                format!("{n} drive{}", if n == 1 { "" } else { "s" })
            }
            Loc::Files(_) => self.files.status_text(),
        }
    }

    pub fn start_filter(&self, c: char) {
        if let Some(w) = self.win() {
            w.filter_entry.grab_focus();
            w.filter_entry.set_text(&format!("{}{c}", w.filter_entry.text()));
            w.filter_entry.set_position(-1);
        }
    }

    // ------------------------------------------------------------------ items
    pub fn activate_item(self: &Rc<Self>, pos: u32) {
        let Some(info) = self.files.item(pos) else { return };
        let Some(f) = files::info_file(&info) else { return };
        if matches!(info.file_type(), gio::FileType::Directory | gio::FileType::Mountable) {
            // a shortcut (symlink) to a folder opens the real folder, like Windows
            let f = symlink_target(&info, &f).unwrap_or(f);
            self.go(Loc::Files(f), true);
        } else if let Some(w) = self.win() {
            w.launch(&f);
        }
    }

    pub fn open_item_in_new_tab(self: &Rc<Self>, pos: u32) {
        let Some(info) = self.files.item(pos) else { return };
        let (Some(f), Some(w)) = (files::info_file(&info), self.win()) else { return };
        if info.file_type() == gio::FileType::Directory {
            w.new_tab(Loc::Files(f), false);
        }
    }

    /// Keyboard menu key: menu for the selection at the view's centre.
    pub fn menu_for_selection(self: &Rc<Self>) {
        let v = self.files.widget.visible_child().unwrap_or_else(|| self.files.widget.clone().upcast());
        let (x, y) = (v.width() as f64 / 2.0, v.height() as f64 / 3.0);
        if self.files.selected_infos().is_empty() {
            self.background_menu(&v, x, y);
        } else {
            self.item_menu(&v, x, y);
        }
    }

    pub fn item_menu(self: &Rc<Self>, at: &gtk::Widget, x: f64, y: f64) {
        let Some(w) = self.win() else { return };
        let sel = self.files.selected_infos();
        let Some(first) = sel.first() else { return };
        let Some(f) = files::info_file(first) else { return };
        let uri = f.uri().to_string();
        let is_dir = first.file_type() == gio::FileType::Directory;
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append(Some("Open"), Some(&action("win.open-uri", &uri)));
        if is_dir && sel.len() == 1 {
            open.append(Some("Open in new tab"), Some(&action("win.open-tab", &uri)));
            open.append(Some("Open in split view"), Some(&action("win.open-split", &uri)));
        }
        if !is_dir && !self.in_trash() {
            if let Some(ct) = first.content_type() {
                let with = gio::Menu::new();
                let default = gio::AppInfo::default_for_type(&ct, false).and_then(|a| a.id());
                for a in gio::AppInfo::recommended_for_type(&ct).into_iter().take(8) {
                    let Some(id) = a.id() else { continue };
                    let label = if Some(&id) == default.as_ref() { format!("{} (default)", a.name()) } else { a.name().to_string() };
                    let item = gio::MenuItem::new(Some(&label), Some(&action("win.open-with", &id)));
                    if let Some(ic) = a.icon() {
                        item.set_icon(&ic);
                    }
                    with.append_item(&item);
                }
                let other = gio::Menu::new();
                other.append(Some("Choose application…"), Some(&action("win.choose-app", &ct)));
                with.append_section(None, &other);
                open.append_submenu(Some("Open with"), &with);
            }
        }
        if is_dir && sel.len() == 1 && f.is_native() {
            open.append(Some("Open terminal here"), Some(&action("win.terminal", &uri)));
        }
        menu.append_section(None, &open);
        if !self.in_trash() && f.is_native() {
            let arc = gio::Menu::new();
            let names: Vec<String> = sel.iter().map(|i| i.name().to_string_lossy().to_string()).collect();
            if names.iter().any(|n| crate::archive::is_archive(n)) {
                arc.append(Some("Extract here"), Some("win.extract-here"));
                arc.append(Some("Extract to…"), Some("win.extract-to"));
            }
            arc.append(Some("Compress…"), Some("win.compress"));
            menu.append_section(None, &arc);
        }
        if is_dir && sel.len() == 1 {
            let side = gio::Menu::new();
            let pin = gio::Menu::new();
            for s in w.cfg.borrow().sections.iter() {
                pin.append(Some(&s.name), Some(&format!("win.pin(('{}','{}'))", esc(&uri), esc(&s.id))));
            }
            side.append_submenu(Some("Pin to sidebar"), &pin);
            side.append(Some("Change folder icon…"), Some(&action("win.folder-icon", &uri)));
            if w.cfg.borrow().folders.get(&files::folder_key(&f)).is_some_and(|c| c.icon.is_some()) {
                side.append(Some("Reset folder icon"), Some(&action("win.folder-icon-reset", &uri)));
            }
            menu.append_section(None, &side);
        }
        let edit = gio::Menu::new();
        if !self.in_trash() {
            edit.append(Some("Cut"), Some("win.cut"));
        }
        edit.append(Some("Copy"), Some("win.copy"));
        if is_dir && sel.len() == 1 && !self.in_trash() {
            edit.append(Some("Paste into folder"), Some(&action("win.paste-into", &uri)));
        }
        if !self.in_trash() {
            edit.append(Some(if sel.len() > 1 { "Rename all…" } else { "Rename…" }), Some("win.rename"));
        }
        menu.append_section(None, &edit);
        let misc = gio::Menu::new();
        if sel.len() == 1 && f.is_native() && !self.in_trash() {
            misc.append(Some("Create shortcut…"), Some(&action("win.shortcut", &uri)));
        }
        misc.append(Some("Copy path"), Some(&action("win.copy-path", &uri)));
        misc.append(Some("Properties"), Some("win.properties"));
        menu.append_section(None, &misc);
        let danger = gio::Menu::new();
        if self.in_trash() {
            danger.append(Some("Restore"), Some("win.restore"));
            danger.append(Some("Delete for good…"), Some("win.delete"));
        } else {
            danger.append(Some("Move to Trash"), Some("win.trash"));
            danger.append(Some("Delete for good…"), Some("win.delete"));
        }
        menu.append_section(None, &danger);
        popup_menu(at, &menu, x, y);
    }

    pub fn background_menu(self: &Rc<Self>, at: &gtk::Widget, x: f64, y: f64) {
        let Some(w) = self.win() else { return };
        let Some(f) = self.current_file() else { return };
        let uri = f.uri().to_string();
        let menu = gio::Menu::new();
        if !self.in_trash() {
            let edit = gio::Menu::new();
            let new = gio::Menu::new();
            new.append(Some("Folder…"), Some("win.new-folder"));
            let docs = gio::Menu::new();
            docs.append(Some("Text document (.txt)"), Some(&action("win.new-file", "txt")));
            docs.append(Some("Shell script (.sh)"), Some(&action("win.new-file", "sh")));
            docs.append(Some("Empty file"), Some(&action("win.new-file", "empty")));
            new.append_section(None, &docs);
            let tpl = gio::Menu::new();
            for t in super::create::templates() {
                tpl.append(Some(&display_name(&t, &w.home)), Some(&action("win.new-template", &t.uri())));
            }
            if tpl.n_items() > 0 {
                new.append_section(Some("Templates"), &tpl);
            }
            edit.append_submenu(Some("New"), &new);
            edit.append(Some("Paste"), Some("win.paste"));
            if f.is_native() {
                edit.append(Some("Open terminal here"), Some(&action("win.terminal", &uri)));
            }
            menu.append_section(None, &edit);
        }
        let side = gio::Menu::new();
        let pin = gio::Menu::new();
        for s in w.cfg.borrow().sections.iter() {
            pin.append(Some(&s.name), Some(&format!("win.pin(('{}','{}'))", esc(&uri), esc(&s.id))));
        }
        side.append_submenu(Some("Pin this folder to sidebar"), &pin);
        side.append(Some("Open in new tab"), Some(&action("win.open-tab", &uri)));
        menu.append_section(None, &side);
        let misc = gio::Menu::new();
        misc.append(Some(if w.cfg.borrow().view.show_hidden { "Hide hidden files" } else { "Show hidden files" }), Some("win.hidden"));
        misc.append(Some("Copy path"), Some(&action("win.copy-path", &uri)));
        misc.append(Some("Reload"), Some("win.reload"));
        misc.append(Some("Properties"), Some("win.properties"));
        menu.append_section(None, &misc);
        if self.in_trash() {
            let t = gio::Menu::new();
            t.append(Some("Empty Trash…"), Some("win.empty-trash"));
            menu.append_section(None, &t);
        }
        popup_menu(at, &menu, x, y);
    }
}

/// Escape a string for a GVariant text literal inside single quotes.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

pub fn action(name: &str, arg: &str) -> String {
    format!("{name}('{}')", esc(arg))
}

/// Pop a menu at (x, y) of `at`; unparents itself when closed.
pub fn popup_menu(at: &gtk::Widget, model: &gio::Menu, x: f64, y: f64) {
    let pop = gtk::PopoverMenu::from_model_full(model, gtk::PopoverMenuFlags::NESTED);
    // GTK puts the menu in a scroll area that can end up shorter than the menu; make it
    // always take the menu's full height instead of showing a scrollbar.
    no_scroll(pop.upcast_ref());
    // GTK only shows popovers whose parent positions its popups (boxes, rows…); containers
    // like GtkStack and GtkScrolledWindow don't. Climb to the nearest box and convert the
    // click position into its coordinates.
    let (at, x, y) = popover_host(at, x, y);
    let at = &at;
    pop.set_parent(at);
    pop.set_has_arrow(false);
    pop.set_halign(gtk::Align::Start);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    // Open upwards when clicked in the lower half, so the compositor doesn't squeeze the
    // menu into a scrolling list.
    if let Some(root) = at.root() {
        let rw = root.upcast_ref::<gtk::Widget>();
        if let Some(pt) = at.compute_point(rw, &gtk::graphene::Point::new(x as f32, y as f32)) {
            if pt.y() > rw.height() as f32 / 2.0 {
                pop.set_position(gtk::PositionType::Top);
            }
        }
    }
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

fn popover_host(at: &gtk::Widget, x: f64, y: f64) -> (gtk::Widget, f64, f64) {
    let ok = |w: &gtk::Widget| w.is::<gtk::Box>() || w.is::<gtk::ListBoxRow>() || w.is::<gtk::Button>();
    let mut host = at.clone();
    while !ok(&host) {
        match host.parent() {
            Some(p) => host = p,
            None => return (at.clone(), x, y),
        }
    }
    if &host == at {
        return (host, x, y);
    }
    match at.compute_point(&host, &gtk::graphene::Point::new(x as f32, y as f32)) {
        Some(pt) => (host, pt.x() as f64, pt.y() as f64),
        None => (at.clone(), x, y),
    }
}

fn no_scroll(w: &gtk::Widget) {
    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
        sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        sw.set_propagate_natural_height(true);
        sw.set_propagate_natural_width(true);
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        no_scroll(&ch);
        c = ch.next_sibling();
    }
}

/// Where a symlink points, resolved against its folder (None if not a symlink).
pub fn symlink_target(info: &gio::FileInfo, f: &gio::File) -> Option<gio::File> {
    if !info.is_symlink() {
        return None;
    }
    let t = info.symlink_target()?;
    let t = t.to_string_lossy();
    if t.starts_with('/') {
        Some(gio::File::for_path(t.as_ref()))
    } else {
        f.parent().map(|p| p.resolve_relative_path(t.as_ref()))
    }
}

