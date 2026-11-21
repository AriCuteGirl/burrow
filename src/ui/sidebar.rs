//! Sidebar: title, Drives overview entry, then the user's sections (favorites + drives).
//! Everything is editable: right-click menus, drag to reorder (within and between sections),
//! drop folders from the file view to pin them. Rebuilt from config + drive list on change;
//! usage bars update in place.

use super::pane::{action, popup_menu};
use super::{Loc, Win, file_from_config_path, icon_from_str};
use crate::config::{Config, Item, Section};
use crate::drives::{self, Drive};
use crate::util;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const DND_PREFIX: &str = "burrow-side:";

#[derive(Clone)]
enum Target {
    DrivesPage,
    Folder { file: gio::File, section: usize, item: usize },
    Drive { key: String, root: Option<gio::File>, section: usize },
}

impl Target {
    /// Stable id used in action parameters and drag payloads.
    fn id(&self) -> String {
        match self {
            Target::DrivesPage => "p".into(),
            Target::Folder { section, item, .. } => format!("f:{section}:{item}"),
            Target::Drive { key, .. } => format!("d:{key}"),
        }
    }
}

/// Parsed action / drag id.
enum Id {
    Folder(usize, usize),
    Drive(String),
    Section(usize),
}

fn parse_id(s: &str) -> Option<Id> {
    if let Some(k) = s.strip_prefix("d:") {
        return Some(Id::Drive(k.to_string()));
    }
    if let Some(r) = s.strip_prefix("s:") {
        return r.parse().ok().map(Id::Section);
    }
    let r = s.strip_prefix("f:")?;
    let (a, b) = r.split_once(':')?;
    Some(Id::Folder(a.parse().ok()?, b.parse().ok()?))
}

pub struct Sidebar {
    pub widget: gtk::Box,
    body: gtk::Box,
    lists: RefCell<Vec<(gtk::ListBox, Rc<Vec<Target>>)>>,
    usage: RefCell<Vec<(gio::File, gtk::ProgressBar, gtk::Label)>>,
}

impl Sidebar {
    pub fn new() -> Sidebar {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("sidebar");
        widget.set_size_request(220, -1);
        let title = gtk::Label::new(Some("Burrow"));
        title.add_css_class("app-title");
        title.set_xalign(0.0);
        title.set_margin_bottom(8);
        widget.append(&title);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_vexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
        scroll.set_child(Some(&body));
        widget.append(&scroll);
        Sidebar { widget, body, lists: RefCell::new(vec![]), usage: RefCell::new(vec![]) }
    }

    pub fn rebuild(&self, w: &Rc<Win>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        self.lists.borrow_mut().clear();
        self.usage.borrow_mut().clear();
        let t = w.tokens.borrow().clone();
        let cfg = w.cfg.borrow().clone();
        let drives = w.drives.borrow().clone();

        if cfg.view.drives_page {
            let list = self.new_list(w);
            list.append(&row(&gio::ThemedIcon::new("computer-symbolic").upcast(), "Drives", None));
            self.body.append(&list);
            self.lists.borrow_mut().push((list, Rc::new(vec![Target::DrivesPage])));
        }

        for (si, s) in cfg.sections.iter().enumerate() {
            let list = self.new_list(w);
            let mut targets = vec![];
            for d in visible_rows(&cfg, si, &drives) {
                match d {
                    VRow::Folder(ii) => {
                        let Item::Folder { path, label, icon } = &s.items[ii] else { continue };
                        let f = file_from_config_path(path);
                        let name = label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| super::display_name(&f, &w.home));
                        let icon = icon.as_deref().map(icon_from_str).unwrap_or_else(|| gio::ThemedIcon::new("folder-symbolic").upcast());
                        let r = row(&icon, &name, None);
                        r.set_tooltip_text(Some(&f.parse_name()));
                        let tgt = Target::Folder { file: f, section: si, item: ii };
                        self.attach_row(w, &r, &tgt);
                        list.append(&r);
                        targets.push(tgt);
                    }
                    VRow::Drive(d) => {
                        let r = self.drive_row(w, &d);
                        let tgt = Target::Drive { key: d.key.clone(), root: d.root.clone(), section: si };
                        self.attach_row(w, &r, &tgt);
                        list.append(&r);
                        targets.push(tgt);
                    }
                }
            }
            if targets.is_empty() {
                let hint = gtk::Label::new(Some("Drop folders here"));
                hint.add_css_class("drive-free");
                hint.set_xalign(0.0);
                hint.set_margin_start(34);
                hint.set_margin_top(4);
                hint.set_margin_bottom(4);
                list.set_placeholder(Some(&hint));
                // the placeholder area accepts drops too
                let drop = drop_target();
                let weak = Rc::downgrade(w);
                drop.connect_drop(move |_, v, _, _| weak.upgrade().is_some_and(|w| handle_drop(&w, v, si, None)));
                list.add_controller(drop);
            }
            let header = self.section_header(w, si, s, &t);
            let rev = gtk::Revealer::new();
            rev.set_transition_duration(if w.reduced_motion() { 0 } else { 140 });
            rev.set_reveal_child(!s.collapsed);
            rev.set_child(Some(&list));
            let weak = Rc::downgrade(w);
            header.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.edit_config(|c| {
                        if let Some(s) = c.sections.get_mut(si) {
                            s.collapsed = !s.collapsed;
                        }
                    });
                }
            });
            self.body.append(&header);
            self.body.append(&rev);
            self.lists.borrow_mut().push((list, Rc::new(targets)));
        }

        // right-click on empty sidebar space
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        spacer.set_size_request(-1, 40);
        let click = gtk::GestureClick::new();
        click.set_button(3);
        let weak = Rc::downgrade(w);
        let sp = spacer.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(w) = weak.upgrade() {
                let menu = background_menu(&w);
                popup_menu(sp.upcast_ref(), &menu, x, y);
            }
        });
        spacer.add_controller(click);
        self.body.append(&spacer);

        self.refresh_usage();
        self.sync_selection(&w.active_loc());
    }

    fn section_header(&self, w: &Rc<Win>, si: usize, s: &Section, t: &crate::theme::Tokens) -> gtk::Button {
        let accent = s.accent.as_deref().and_then(crate::theme::Rgb::parse).unwrap_or(t.accent_hot);
        let header = gtk::Button::new();
        header.add_css_class("section-header");
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        if let Some(i) = &s.icon {
            let img = gtk::Image::from_gicon(&icon_from_str(i));
            img.set_pixel_size(14);
            set_color(img.upcast_ref(), &accent.hex());
            hb.append(&img);
        } else {
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("section-dot");
            dot.set_valign(gtk::Align::Center);
            set_bg(dot.upcast_ref(), &accent.hex());
            hb.append(&dot);
        }
        let lbl = gtk::Label::new(Some(&s.name.to_uppercase()));
        lbl.add_css_class("section-title");
        lbl.set_hexpand(true);
        lbl.set_xalign(0.0);
        lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if s.accent.is_some() {
            set_color(lbl.upcast_ref(), &accent.hex());
        }
        hb.append(&lbl);
        let chev = gtk::Image::from_icon_name(if s.collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" });
        chev.add_css_class("section-chevron");
        hb.append(&chev);
        header.set_child(Some(&hb));
        header.set_tooltip_text(Some(if s.collapsed { "Expand · right-click to edit" } else { "Collapse · right-click to edit" }));

        let click = gtk::GestureClick::new();
        click.set_button(3);
        let weak = Rc::downgrade(w);
        let h = header.clone();
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(w) = weak.upgrade() else { return };
            let n = w.cfg.borrow().sections.len();
            let has_accent = w.cfg.borrow().sections.get(si).is_some_and(|s| s.accent.is_some());
            let id = format!("s:{si}");
            let m = gio::Menu::new();
            let a = gio::Menu::new();
            a.append(Some("Rename section…"), Some(&action("win.side-rename", &id)));
            a.append(Some("Colour…"), Some(&action("win.side-color", &id)));
            if has_accent {
                a.append(Some("Use the rice colour"), Some(&action("win.side-color-reset", &id)));
            }
            a.append(Some("Icon…"), Some(&action("win.side-icon", &id)));
            a.append(Some("Use a dot instead of an icon"), Some(&action("win.side-icon-reset", &id)));
            m.append_section(None, &a);
            let b = gio::Menu::new();
            if si > 0 {
                b.append(Some("Move section up"), Some(&action("win.side-up", &id)));
            }
            if si + 1 < n {
                b.append(Some("Move section down"), Some(&action("win.side-down", &id)));
            }
            b.append(Some("New section…"), Some("win.section-new"));
            m.append_section(None, &b);
            if n > 1 {
                let c = gio::Menu::new();
                c.append(Some("Delete section"), Some(&action("win.side-remove", &id)));
                m.append_section(None, &c);
            }
            popup_menu(h.upcast_ref(), &m, x, y);
        });
        header.add_controller(click);

        // drop onto the header = append to this section
        let drop = drop_target();
        let hw = header.downgrade();
        drop.connect_enter(move |_, _, _| {
            if let Some(h) = hw.upgrade() {
                h.add_css_class("drop-into");
            }
            gdk::DragAction::MOVE
        });
        let hw = header.downgrade();
        drop.connect_leave(move |_| {
            if let Some(h) = hw.upgrade() {
                h.remove_css_class("drop-into");
            }
        });
        let weak = Rc::downgrade(w);
        drop.connect_drop(move |d, v, _, _| {
            if let Some(h) = d.widget() {
                h.remove_css_class("drop-into");
            }
            let Some(w) = weak.upgrade() else { return false };
            handle_drop(&w, v, si, None)
        });
        header.add_controller(drop);
        header
    }

    fn new_list(&self, w: &Rc<Win>) -> gtk::ListBox {
        let list = gtk::ListBox::new();
        list.add_css_class("side-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        let weak = Rc::downgrade(w);
        list.connect_row_activated(move |l, r| {
            let Some(w) = weak.upgrade() else { return };
            let Some(t) = w.sidebar.target_of(l, r) else { return };
            match t {
                Target::DrivesPage => w.go(Loc::Drives, true),
                Target::Folder { file, .. } => w.go(Loc::Files(file), true),
                Target::Drive { key, .. } => w.open_drive(&key),
            }
        });
        list
    }

    fn target_of(&self, l: &gtk::ListBox, r: &gtk::ListBoxRow) -> Option<Target> {
        let lists = self.lists.borrow();
        let (_, targets) = lists.iter().find(|(x, _)| x == l)?;
        targets.get(r.index() as usize).cloned()
    }

    /// Context menu, middle click, drag source and drop target for a section row.
    fn attach_row(&self, w: &Rc<Win>, r: &gtk::ListBoxRow, tgt: &Target) {
        let id = tgt.id();
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let (weak, t) = (Rc::downgrade(w), tgt.clone());
        let rw = r.downgrade();
        click.connect_pressed(move |g, _, x, y| {
            let (Some(w), Some(r)) = (weak.upgrade(), rw.upgrade()) else { return };
            match g.current_button() {
                3 => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let m = row_menu(&w, &t);
                    popup_menu(r.upcast_ref(), &m, x, y);
                }
                2 => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    match &t {
                        Target::Folder { file, .. } => w.new_tab(Loc::Files(file.clone()), false),
                        Target::Drive { root: Some(root), .. } => w.new_tab(Loc::Files(root.clone()), false),
                        _ => {}
                    }
                }
                _ => {}
            }
        });
        r.add_controller(click);

        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        let payload = format!("{DND_PREFIX}{id}");
        drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
        let rw = r.downgrade();
        drag.connect_drag_begin(move |src, _| {
            if let Some(r) = rw.upgrade() {
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&r))), 12, 12);
            }
        });
        r.add_controller(drag);

        // drop on a row: insert before it (top half) or after it (bottom half)
        let section = match tgt {
            Target::Folder { section, .. } | Target::Drive { section, .. } => *section,
            Target::DrivesPage => return,
        };
        let drop = drop_target();
        let rw = r.downgrade();
        drop.connect_motion(move |_, _, y| {
            if let Some(r) = rw.upgrade() {
                let after = y > r.height() as f64 / 2.0;
                r.remove_css_class(if after { "drop-before" } else { "drop-after" });
                r.add_css_class(if after { "drop-after" } else { "drop-before" });
            }
            gdk::DragAction::MOVE
        });
        let rw = r.downgrade();
        drop.connect_leave(move |_| {
            if let Some(r) = rw.upgrade() {
                r.remove_css_class("drop-before");
                r.remove_css_class("drop-after");
            }
        });
        let weak = Rc::downgrade(w);
        let rw = r.downgrade();
        drop.connect_drop(move |_, v, _, y| {
            let (Some(w), Some(r)) = (weak.upgrade(), rw.upgrade()) else { return false };
            r.remove_css_class("drop-before");
            r.remove_css_class("drop-after");
            let after = y > r.height() as f64 / 2.0;
            let pos = r.index() as usize + after as usize;
            handle_drop(&w, v, section, Some(pos))
        });
        r.add_controller(drop);
    }

    fn drive_row(&self, w: &Rc<Win>, d: &Drive) -> gtk::ListBoxRow {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("usage");
        let free = gtk::Label::new(Some(if d.mounted() { " " } else { "Not mounted" }));
        free.add_css_class("drive-free");
        free.set_xalign(0.0);
        let extra = gtk::Box::new(gtk::Orientation::Vertical, 3);
        if d.mounted() {
            extra.append(&bar);
        }
        extra.append(&free);
        let r = row(&w.drive_icon(d), &w.drive_display_name(d), Some(&extra));
        if let Some(root) = &d.root {
            self.usage.borrow_mut().push((root.clone(), bar, free));
            r.set_tooltip_text(Some(&root.parse_name()));
        }
        if d.can_eject() || d.can_unmount() {
            let b = gtk::Button::from_icon_name("media-eject-symbolic");
            b.add_css_class("eject");
            b.set_valign(gtk::Align::Center);
            b.set_tooltip_text(Some(if d.can_eject() { "Eject" } else { "Unmount" }));
            let weak = Rc::downgrade(w);
            let key = d.key.clone();
            b.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.eject_drive(&key)
                }
            });
            if let Some(hb) = r.child().and_downcast::<gtk::Box>() {
                hb.append(&b);
            }
        }
        r
    }

    pub fn refresh_usage(&self) {
        for (root, bar, label) in self.usage.borrow().iter() {
            set_usage(root, bar, label);
        }
    }

    pub fn sync_selection(&self, loc: &Loc) {
        for (list, targets) in self.lists.borrow().iter() {
            let hit = targets.iter().position(|t| match (t, loc) {
                (Target::DrivesPage, Loc::Drives) => true,
                (Target::Folder { file, .. }, Loc::Files(l)) => file.equal(l),
                (Target::Drive { root: Some(r), .. }, Loc::Files(l)) => r.equal(l),
                _ => false,
            });
            match hit.and_then(|i| list.row_at_index(i as i32)) {
                Some(r) => list.select_row(Some(&r)),
                None => list.unselect_all(),
            }
        }
    }

    pub fn new_section(&self, w: &Rc<Win>) {
        let weak = Rc::downgrade(w);
        super::dialogs::prompt_text(&w.window, "New section", "e.g. Projects, Media, Games", "", move |name| {
            let Some(w) = weak.upgrade() else { return };
            if name.is_empty() {
                return;
            }
            w.edit_config(|c| {
                let base = slug(&name);
                let mut id = base.clone();
                let mut n = 2;
                while c.sections.iter().any(|s| s.id == id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                c.sections.push(Section { id, name, ..Default::default() });
            });
        });
    }

    /// Sidebar actions. Parameters are row ids: "f:<section>:<item>", "d:<drive key>", "s:<section>".
    pub fn install_actions(&self, w: &Rc<Win>) {
        let add = |name: &str, f: fn(&Rc<Win>, Id)| {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(w);
            a.connect_activate(move |_, p| {
                let Some(w) = weak.upgrade() else { return };
                if let Some(id) = p.and_then(|p| p.get::<String>()).as_deref().and_then(parse_id) {
                    f(&w, id)
                }
            });
            w.window.add_action(&a);
        };
        add("side-rename", rename);
        add("side-icon", change_icon);
        add("side-icon-reset", |w, id| {
            w.edit_config(|c| match id {
                Id::Folder(s, i) => {
                    if let Some(Item::Folder { icon, .. }) = c.sections.get_mut(s).and_then(|s| s.items.get_mut(i)) {
                        *icon = None;
                    }
                }
                Id::Drive(k) => c.drives.entry(k).or_default().icon = None,
                Id::Section(s) => {
                    if let Some(s) = c.sections.get_mut(s) {
                        s.icon = None;
                    }
                }
            })
        });
        add("side-color", |w, id| {
            let Id::Section(si) = id else { return };
            let cur = w.cfg.borrow().sections.get(si).and_then(|s| s.accent.clone());
            let weak = Rc::downgrade(w);
            super::dialogs::pick_color(&w.window, cur.as_deref(), move |hex| {
                if let Some(w) = weak.upgrade() {
                    w.edit_config(|c| {
                        if let Some(s) = c.sections.get_mut(si) {
                            s.accent = Some(hex);
                        }
                    });
                }
            });
        });
        add("side-color-reset", |w, id| {
            let Id::Section(si) = id else { return };
            w.edit_config(|c| {
                if let Some(s) = c.sections.get_mut(si) {
                    s.accent = None;
                }
            });
        });
        add("side-up", |w, id| shift(w, id, -1));
        add("side-down", |w, id| shift(w, id, 1));
        add("side-remove", |w, id| {
            w.edit_config(|c| match id {
                Id::Folder(s, i) => {
                    if let Some(s) = c.sections.get_mut(s) {
                        if i < s.items.len() {
                            s.items.remove(i);
                        }
                    }
                }
                Id::Drive(k) => {
                    // back to the automatic section
                    for s in &mut c.sections {
                        s.items.retain(|it| !matches!(it, Item::Drive { key } if *key == k));
                    }
                }
                Id::Section(si) => {
                    if c.sections.len() < 2 || si >= c.sections.len() {
                        return;
                    }
                    let removed = c.sections.remove(si);
                    // keep pinned folders: move them to the neighbouring section
                    let to = si.saturating_sub(1).min(c.sections.len() - 1);
                    let folders = removed.items.into_iter().filter(|i| matches!(i, Item::Folder { .. }));
                    c.sections[to].items.extend(folders);
                    if removed.auto_drives {
                        c.sections[to].auto_drives = true;
                    }
                }
            });
        });
        add("side-hide", |w, id| {
            if let Id::Drive(k) = id {
                w.edit_config(|c| c.drives.entry(k).or_default().hidden = true);
                w.show_notice("Drive hidden. Right-click empty sidebar space to show it again.", false);
            }
        });
        add("side-unhide", |w, id| {
            if let Id::Drive(k) = id {
                w.edit_config(|c| {
                    let e = c.drives.entry(k.clone()).or_default();
                    e.hidden = false;
                    if *e == Default::default() {
                        c.drives.remove(&k);
                    }
                });
            }
        });
        add("side-mount", |w, id| {
            if let Id::Drive(k) = id {
                let d = w.drives.borrow().iter().find(|d| d.key == k).cloned();
                if let Some(d) = d {
                    w.mount_drive(&d, false);
                }
            }
        });
        add("side-eject", |w, id| {
            if let Id::Drive(k) = id {
                w.eject_drive(&k);
            }
        });
        add("side-open-tab", |w, id| {
            let f = match id {
                Id::Folder(s, i) => match w.cfg.borrow().sections.get(s).and_then(|s| s.items.get(i)) {
                    Some(Item::Folder { path, .. }) => Some(file_from_config_path(path)),
                    _ => None,
                },
                Id::Drive(k) => w.drives.borrow().iter().find(|d| d.key == k).and_then(|d| d.root.clone()),
                Id::Section(_) => None,
            };
            if let Some(f) = f {
                w.new_tab(Loc::Files(f), false);
            }
        });

        // move a row to another section: (row id, section id)
        let mv = gio::SimpleAction::new("side-move", Some(&glib::VariantType::new("(ss)").unwrap()));
        let weak = Rc::downgrade(w);
        mv.connect_activate(move |_, p| {
            let (Some(w), Some((id, sid))) = (weak.upgrade(), p.and_then(|p| p.get::<(String, String)>())) else { return };
            let to = w.cfg.borrow().sections.iter().position(|s| s.id == sid);
            if let (Some(to), Some(id)) = (to, parse_id(&id)) {
                move_row(&w, id, to, None);
            }
        });
        w.window.add_action(&mv);
    }
}

// ---------------------------------------------------------------- visible rows

enum VRow {
    Folder(usize),
    Drive(Drive),
}

/// Rows of a section in display order: its items (connected, non-hidden drives only),
/// then drives that fall into it automatically.
fn visible_rows(cfg: &Config, si: usize, drives: &[Drive]) -> Vec<VRow> {
    let s = &cfg.sections[si];
    let mut out = vec![];
    for (ii, it) in s.items.iter().enumerate() {
        match it {
            Item::Folder { .. } => out.push(VRow::Folder(ii)),
            Item::Drive { key } => {
                if let Some(d) = drives.iter().find(|d| &d.key == key) {
                    if !cfg.drive(key).hidden {
                        out.push(VRow::Drive(d.clone()));
                    }
                }
            }
        }
    }
    if s.auto_drives {
        for d in drives {
            let auto_here = cfg.section_for_drive(&d.key) == Some(s.id.as_str());
            let explicit = s.items.iter().any(|i| matches!(i, Item::Drive { key } if *key == d.key));
            if auto_here && !explicit && !cfg.drive(&d.key).hidden {
                out.push(VRow::Drive(d.clone()));
            }
        }
    }
    out
}

/// Make every visible auto drive of a section an explicit item, so row order == item order.
fn materialize(cfg: &mut Config, si: usize, drives: &[Drive]) {
    let autos: Vec<String> = visible_rows(cfg, si, drives)
        .into_iter()
        .filter_map(|r| match r {
            VRow::Drive(d) => Some(d.key),
            _ => None,
        })
        .filter(|k| !cfg.sections[si].items.iter().any(|i| matches!(i, Item::Drive { key } if key == k)))
        .collect();
    for k in autos {
        cfg.sections[si].items.push(Item::Drive { key: k });
    }
}

/// Item index in section `si` for visible row `row` (after materialize). Rows past the end -> len.
fn row_to_item(cfg: &Config, si: usize, row: usize, drives: &[Drive]) -> usize {
    let s = &cfg.sections[si];
    let mut visible = 0;
    for (ii, it) in s.items.iter().enumerate() {
        let shown = match it {
            Item::Folder { .. } => true,
            Item::Drive { key } => drives.iter().any(|d| &d.key == key) && !cfg.drive(key).hidden,
        };
        if shown {
            if visible == row {
                return ii;
            }
            visible += 1;
        }
    }
    s.items.len()
}

/// Move a row (folder or drive) to section `to`, before visible row `row` of the section as
/// it looks *after* the row was taken out (None = end).
fn move_row(w: &Rc<Win>, id: Id, to: usize, row: Option<usize>) {
    let drives = w.drives.borrow().clone();
    w.edit_config(|c| {
        if to >= c.sections.len() {
            return;
        }
        let moving = match &id {
            Id::Folder(s, i) => match c.sections.get_mut(*s) {
                Some(sec) if *i < sec.items.len() => sec.items.remove(*i),
                _ => return,
            },
            Id::Drive(k) => {
                for s in &mut c.sections {
                    s.items.retain(|i| !matches!(i, Item::Drive { key } if key == k));
                }
                Item::Drive { key: k.clone() }
            }
            Id::Section(_) => return,
        };
        // With the item out, make the target's auto drives explicit so rows map to items.
        // (A drive being moved is no longer placed anywhere, so it would show up as an auto
        // drive of the auto section: exclude it by placing it temporarily.)
        let tmp = if let Item::Drive { key } = &moving {
            c.sections[to].items.push(Item::Drive { key: key.clone() });
            true
        } else {
            false
        };
        materialize(c, to, &drives);
        if tmp {
            if let Item::Drive { key } = &moving {
                let pos = c.sections[to].items.iter().position(|i| matches!(i, Item::Drive { key: k } if k == key));
                if let Some(p) = pos {
                    c.sections[to].items.remove(p);
                }
            }
        }
        let at = match row {
            Some(r) => row_to_item(c, to, r, &drives),
            None => c.sections[to].items.len(),
        };
        let at = at.min(c.sections[to].items.len());
        c.sections[to].items.insert(at, moving);
    });
}

fn shift(w: &Rc<Win>, id: Id, d: isize) {
    match id {
        Id::Section(si) => {
            w.edit_config(|c| {
                let j = si as isize + d;
                if j >= 0 && (j as usize) < c.sections.len() {
                    c.sections.swap(si, j as usize);
                }
            });
        }
        Id::Folder(..) | Id::Drive(_) => {
            let cfg = w.cfg.borrow().clone();
            let drives = w.drives.borrow().clone();
            let Some((si, row)) = find_row(&cfg, &id, &drives) else { return };
            let n = visible_rows(&cfg, si, &drives).len();
            let target = row as isize + d;
            if target < 0 || target as usize >= n {
                return;
            }
            // rows are counted with the moving row removed, so row±1 is simply `target`
            move_row(w, id, si, Some(target as usize));
        }
    }
}

fn find_row(cfg: &Config, id: &Id, drives: &[Drive]) -> Option<(usize, usize)> {
    for si in 0..cfg.sections.len() {
        for (r, v) in visible_rows(cfg, si, drives).iter().enumerate() {
            let hit = match (v, id) {
                (VRow::Folder(i), Id::Folder(s, j)) => si == *s && i == j,
                (VRow::Drive(d), Id::Drive(k)) => &d.key == k,
                _ => false,
            };
            if hit {
                return Some((si, r));
            }
        }
    }
    None
}

// ---------------------------------------------------------------- drag and drop

fn drop_target() -> gtk::DropTarget {
    let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::MOVE | gdk::DragAction::COPY | gdk::DragAction::LINK);
    drop.set_types(&[glib::Type::STRING, gdk::FileList::static_type()]);
    drop.set_preload(true);
    drop
}

/// A sidebar row (reorder/move) or folders from the file view / other apps (pin).
/// `row` is the visible row the drop landed before, counted with the dragged row still there.
fn handle_drop(w: &Rc<Win>, v: &glib::Value, section: usize, row: Option<usize>) -> bool {
    if let Ok(s) = v.get::<String>() {
        let Some(id) = s.strip_prefix(DND_PREFIX).and_then(parse_id) else { return false };
        // convert to "row with the dragged row removed"
        let row = row.map(|r| {
            let cfg = w.cfg.borrow().clone();
            let drives = w.drives.borrow().clone();
            match find_row(&cfg, &id, &drives) {
                Some((s, cur)) if s == section && r > cur => r - 1,
                _ => r,
            }
        });
        move_row(w, id, section, row);
        return true;
    }
    if let Ok(list) = v.get::<gdk::FileList>() {
        let sid = w.cfg.borrow().sections.get(section).map(|s| s.id.clone());
        let Some(sid) = sid else { return false };
        let drives = w.drives.borrow().clone();
        let base = {
            let cfg = w.cfg.borrow();
            row.map(|r| row_to_item(&cfg, section, r, &drives))
        };
        for (n, f) in list.files().into_iter().enumerate() {
            // only folders: check the type off the UI thread
            let (weak, sid) = (Rc::downgrade(w), sid.clone());
            f.clone().query_info_async(
                "standard::type",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |r| {
                    let Some(w) = weak.upgrade() else { return };
                    match r.map(|i| i.file_type()) {
                        Ok(gio::FileType::Directory) | Ok(gio::FileType::Mountable) => w.pin_folder(&f, &sid, base.map(|b| b + n)),
                        _ => w.show_notice("Only folders can be pinned to the sidebar", true),
                    }
                },
            );
        }
        return true;
    }
    false
}

// ---------------------------------------------------------------- menus & edits

fn row_menu(w: &Rc<Win>, t: &Target) -> gio::Menu {
    let id = t.id();
    let cfg = w.cfg.borrow();
    let m = gio::Menu::new();
    let open = gio::Menu::new();
    match t {
        Target::DrivesPage => {}
        Target::Folder { section, item, .. } => {
            open.append(Some("Open in new tab"), Some(&action("win.side-open-tab", &id)));
            m.append_section(None, &open);
            let edit = gio::Menu::new();
            edit.append(Some("Rename…"), Some(&action("win.side-rename", &id)));
            edit.append(Some("Change icon…"), Some(&action("win.side-icon", &id)));
            if matches!(cfg.sections.get(*section).and_then(|s| s.items.get(*item)), Some(Item::Folder { icon: Some(_), .. })) {
                edit.append(Some("Reset icon"), Some(&action("win.side-icon-reset", &id)));
            }
            m.append_section(None, &edit);
        }
        Target::Drive { key, root, .. } => {
            let d = w.drives.borrow().iter().find(|d| &d.key == key).cloned();
            if root.is_some() {
                open.append(Some("Open in new tab"), Some(&action("win.side-open-tab", &id)));
            } else {
                open.append(Some("Mount"), Some(&action("win.side-mount", &id)));
            }
            if let Some(d) = d.as_ref().filter(|d| d.can_eject() || d.can_unmount()) {
                open.append(Some(if d.can_eject() { "Eject" } else { "Unmount" }), Some(&action("win.side-eject", &id)));
            }
            m.append_section(None, &open);
            let edit = gio::Menu::new();
            edit.append(Some("Rename…"), Some(&action("win.side-rename", &id)));
            edit.append(Some("Change icon…"), Some(&action("win.side-icon", &id)));
            if cfg.drive(key).icon.is_some() {
                edit.append(Some("Reset icon"), Some(&action("win.side-icon-reset", &id)));
            }
            edit.append(Some("Hide"), Some(&action("win.side-hide", &id)));
            m.append_section(None, &edit);
        }
    }
    if !matches!(t, Target::DrivesPage) {
        let order = gio::Menu::new();
        order.append(Some("Move up"), Some(&action("win.side-up", &id)));
        order.append(Some("Move down"), Some(&action("win.side-down", &id)));
        let to = gio::Menu::new();
        let cur = match t {
            Target::Folder { section, .. } | Target::Drive { section, .. } => *section,
            _ => usize::MAX,
        };
        let esc = |s: &str| s.replace('\\', "\\\\").replace('\'', "\\'");
        for (i, s) in cfg.sections.iter().enumerate() {
            if i != cur {
                to.append(Some(&s.name), Some(&format!("win.side-move(('{}','{}'))", esc(&id), esc(&s.id))));
            }
        }
        if to.n_items() > 0 {
            order.append_submenu(Some("Move to section"), &to);
        }
        m.append_section(None, &order);
        let rm = gio::Menu::new();
        match t {
            Target::Folder { .. } => rm.append(Some("Remove from sidebar"), Some(&action("win.side-remove", &id))),
            Target::Drive { key, .. } if cfg.sections.iter().any(|s| s.items.iter().any(|i| matches!(i, Item::Drive { key: k } if k == key))) => {
                rm.append(Some("Back to automatic section"), Some(&action("win.side-remove", &id)))
            }
            _ => {}
        }
        if rm.n_items() > 0 {
            m.append_section(None, &rm);
        }
    }
    m
}

fn background_menu(w: &Rc<Win>) -> gio::Menu {
    let m = gio::Menu::new();
    m.append(Some("New section…"), Some("win.section-new"));
    let cfg = w.cfg.borrow();
    let hidden = gio::Menu::new();
    for d in w.drives.borrow().iter() {
        if cfg.drive(&d.key).hidden {
            hidden.append(Some(&format!("Show {}", w.drive_display_name(d))), Some(&action("win.side-unhide", &format!("d:{}", d.key))));
        }
    }
    if hidden.n_items() > 0 {
        m.append_section(Some("Hidden drives"), &hidden);
    }
    m
}

fn rename(w: &Rc<Win>, id: Id) {
    let cfg = w.cfg.borrow().clone();
    let (title, hint, initial) = match &id {
        Id::Folder(s, i) => match cfg.sections.get(*s).and_then(|s| s.items.get(*i)) {
            Some(Item::Folder { path, label, .. }) => (
                "Rename in sidebar",
                "Only the sidebar label changes, not the folder. Leave empty to use the folder name.",
                label.clone().unwrap_or_else(|| super::display_name(&file_from_config_path(path), &w.home)),
            ),
            _ => return,
        },
        Id::Drive(k) => {
            let d = w.drives.borrow().iter().find(|d| &d.key == k).cloned();
            let Some(d) = d else { return };
            ("Rename drive", "Saved by the drive's UUID, so it sticks after replugging. Leave empty to reset.", w.drive_display_name(&d))
        }
        Id::Section(s) => match cfg.sections.get(*s) {
            Some(s) => ("Rename section", "", s.name.clone()),
            None => return,
        },
    };
    let weak = Rc::downgrade(w);
    super::dialogs::prompt_text(&w.window, title, hint, &initial, move |text| {
        let Some(w) = weak.upgrade() else { return };
        let val = (!text.is_empty()).then_some(text);
        w.edit_config(|c| match &id {
            Id::Folder(s, i) => {
                if let Some(Item::Folder { label, .. }) = c.sections.get_mut(*s).and_then(|s| s.items.get_mut(*i)) {
                    *label = val;
                }
            }
            Id::Drive(k) => {
                let e = c.drives.entry(k.clone()).or_default();
                e.name = val;
                if *e == Default::default() {
                    c.drives.remove(k);
                }
            }
            Id::Section(s) => {
                if let (Some(s), Some(v)) = (c.sections.get_mut(*s), val) {
                    s.name = v;
                }
            }
        });
    });
}

fn change_icon(w: &Rc<Win>, id: Id) {
    let weak = Rc::downgrade(w);
    let title = match id {
        Id::Folder(..) => "Sidebar icon",
        Id::Drive(_) => "Drive icon",
        Id::Section(_) => "Section icon",
    };
    super::dialogs::pick_icon(&w.window, title, true, move |icon| {
        let Some(w) = weak.upgrade() else { return };
        w.edit_config(|c| match &id {
            Id::Folder(s, i) => {
                if let Some(Item::Folder { icon: ic, .. }) = c.sections.get_mut(*s).and_then(|s| s.items.get_mut(*i)) {
                    *ic = Some(icon);
                }
            }
            Id::Drive(k) => c.drives.entry(k.clone()).or_default().icon = Some(icon),
            Id::Section(s) => {
                if let Some(s) = c.sections.get_mut(*s) {
                    s.icon = Some(icon);
                }
            }
        });
    });
}

fn slug(name: &str) -> String {
    let s = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "section".into() } else { s }
}

// ---------------------------------------------------------------- widgets

/// Fills a usage bar + "X free of Y" label asynchronously (Windows-style).
pub fn set_usage(root: &gio::File, bar: &gtk::ProgressBar, label: &gtk::Label) {
    let bar = bar.downgrade();
    let label = label.downgrade();
    drives::query_usage(root, move |u| {
        let (Some(bar), Some(label)) = (bar.upgrade(), label.upgrade()) else { return };
        match u.and_then(|(size, free)| util::usage(size, free)) {
            Some(u) => {
                bar.set_fraction(u.fraction);
                bar.set_visible(true);
                if u.full {
                    bar.add_css_class("full");
                } else {
                    bar.remove_css_class("full");
                }
                label.set_text(&u.text);
                bar.set_tooltip_text(Some(&format!("{:.0}% used", u.fraction * 100.0)));
            }
            None => {
                bar.set_visible(false);
                label.set_text("");
                label.set_visible(false);
            }
        }
    });
}

fn row(icon: &gio::Icon, name: &str, extra: Option<&gtk::Box>) -> gtk::ListBoxRow {
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let img = gtk::Image::from_gicon(icon);
    img.set_pixel_size(16);
    img.set_valign(if extra.is_some() { gtk::Align::Start } else { gtk::Align::Center });
    if extra.is_some() {
        img.set_margin_top(1);
    }
    hb.append(&img);
    let vb = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vb.set_hexpand(true);
    let l = gtk::Label::new(Some(name));
    l.add_css_class("side-label");
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    vb.append(&l);
    if let Some(e) = extra {
        vb.append(e);
    }
    hb.append(&vb);
    let r = gtk::ListBoxRow::new();
    r.set_child(Some(&hb));
    r
}

fn widget_css(w: &gtk::Widget, css: &str) {
    let p = gtk::CssProvider::new();
    p.load_from_string(css);
    #[allow(deprecated)]
    w.style_context().add_provider(&p, gtk::STYLE_PROVIDER_PRIORITY_USER + 2);
}

/// Per-widget background colour (section dots use the section's own accent).
fn set_bg(w: &gtk::Widget, hex: &str) {
    widget_css(w, &format!("* {{ background: {hex}; }}"));
}

fn set_color(w: &gtk::Widget, hex: &str) {
    widget_css(w, &format!("* {{ color: {hex}; }}"));
}
