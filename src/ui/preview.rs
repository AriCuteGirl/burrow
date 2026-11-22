//! What's in a file, without opening it: images, text, a frame of a video, the first page of
//! a PDF, a font sample, or a folder's contents. Used by the preview pane and by quick look.

use super::pane::Pane;
use super::{Win, files, thumbq};
use crate::thumbs::{self, Size};
use crate::util;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::rc::Rc;

const TEXT_LIMIT: u64 = 96 * 1024;

pub struct Preview {
    pub widget: gtk::Box,
    stack: gtk::Stack,
    icon: gtk::Image,
    picture: gtk::Picture,
    text: gtk::TextView,
    title: gtk::Label,
    meta: gtk::Label,
    /// Bumped on every change, so late results for an older file are ignored.
    turn: Rc<Cell<u64>>,
}

impl Preview {
    pub fn new(big: bool) -> Preview {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);
        widget.add_css_class("preview");
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let icon = gtk::Image::new();
        icon.set_pixel_size(if big { 160 } else { 96 });
        icon.add_css_class("state-icon");
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        let text = gtk::TextView::new();
        text.set_editable(false);
        text.set_cursor_visible(false);
        text.set_monospace(true);
        text.set_focusable(false);
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_top_margin(10);
        text.set_bottom_margin(10);
        text.set_left_margin(12);
        text.set_right_margin(12);
        let scroll = gtk::ScrolledWindow::new();
        scroll.add_css_class("preview-text");
        scroll.set_child(Some(&text));
        stack.add_named(&icon, Some("icon"));
        stack.add_named(&picture, Some("picture"));
        stack.add_named(&scroll, Some("text"));
        widget.append(&stack);
        let title = gtk::Label::new(None);
        title.add_css_class("card-title");
        title.add_css_class("preview-title");
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_width_chars(8);
        title.set_max_width_chars(if big { 80 } else { 28 });
        let meta = gtk::Label::new(None);
        meta.add_css_class("settings-sub");
        meta.set_xalign(0.0);
        meta.set_wrap(true);
        meta.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        meta.set_max_width_chars(if big { 80 } else { 28 });
        widget.append(&title);
        widget.append(&meta);
        Preview { widget, stack, icon, picture, text, title, meta, turn: Rc::default() }
    }

    fn show_icon(&self, icon: &gio::Icon) {
        self.icon.set_from_gicon(icon);
        self.stack.set_visible_child_name("icon");
        self.picture.set_paintable(None::<&gdk::Paintable>);
    }

    pub fn show_nothing(&self, text: &str) {
        self.turn.set(self.turn.get() + 1);
        self.show_icon(&gio::ThemedIcon::new("edit-find-symbolic").upcast());
        self.title.set_text(text);
        self.meta.set_text("");
    }

    pub fn show_many(&self, infos: &[gio::FileInfo]) {
        self.turn.set(self.turn.get() + 1);
        self.show_icon(&gio::ThemedIcon::new("edit-select-all-symbolic").upcast());
        self.title.set_text(&format!("{} items", infos.len()));
        let bytes: u64 = infos.iter().filter(|i| !files::is_folder(i)).map(|i| i.size().max(0) as u64).sum();
        self.meta.set_text(&util::human_size(bytes));
    }

    pub fn show(&self, w: &Win, info: &gio::FileInfo) {
        let turn = self.turn.get() + 1;
        self.turn.set(turn);
        let Some(f) = files::info_file(info) else { return };
        let folder = files::is_folder(info);
        let icon = info.icon().unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast());
        let icon = match w.folder_color().filter(|_| folder) {
            Some(c) => files::tint_folder(&icon, &c),
            None => icon,
        };
        self.show_icon(&icon);
        self.title.set_text(&info.display_name());
        let mime = info.content_type().map(|c| c.to_string()).unwrap_or_default();
        let when = info
            .modification_date_time()
            .and_then(|d| d.to_local().ok())
            .and_then(|d| d.format("%-d %b %Y %H:%M").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let kind = if folder { "Folder".to_string() } else { format!("{} · {}", gio::content_type_get_description(&mime), util::human_size(info.size().max(0) as u64)) };
        let base = if when.is_empty() { kind } else { format!("{kind}\n{when}") };
        self.meta.set_text(&base);
        if info.file_type() != gio::FileType::Regular && !folder {
            return; // sockets, devices, pipes: never open those
        }
        let fresh = {
            let t = self.turn.clone();
            move || t.get() == turn
        };

        if folder {
            if !f.is_native() {
                return;
            }
            let meta = self.meta.downgrade();
            glib::spawn_future_local(async move {
                let Some(p) = f.path() else { return };
                let n = gio::spawn_blocking(move || std::fs::read_dir(p).map(|r| r.take(100_000).count()).ok()).await.ok().flatten();
                if let (Some(n), Some(m), true) = (n, meta.upgrade(), fresh()) {
                    m.set_text(&format!("{base}\n{n} item{} inside", if n == 1 { "" } else { "s" }));
                }
            });
            return;
        }
        if !f.is_native() {
            return; // don't download things just to peek at them
        }
        let Some(path) = f.path() else { return };

        if mime.starts_with("image/") && info.size() < 64 * 1024 * 1024 {
            let (pic, stack, meta) = (self.picture.downgrade(), self.stack.downgrade(), self.meta.downgrade());
            glib::spawn_future_local(async move {
                let got = gio::spawn_blocking(move || {
                    let dims = gtk::gdk_pixbuf::Pixbuf::file_info(&path).map(|(_, w, h)| (w, h));
                    let p = thumbs::load_image(&path, 1800)?;
                    Some((p.width(), p.height(), p.rowstride(), p.has_alpha(), p.read_pixel_bytes().to_vec(), dims))
                })
                .await
                .ok()
                .flatten();
                let (Some((w, h, stride, alpha, px, dims)), true) = (got, fresh()) else { return };
                let (Some(pic), Some(stack)) = (pic.upgrade(), stack.upgrade()) else { return };
                let fmt = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
                pic.set_paintable(Some(&gdk::MemoryTexture::new(w, h, fmt, &glib::Bytes::from_owned(px), stride as usize)));
                stack.set_visible_child_name("picture");
                if let (Some((w, h)), Some(m)) = (dims, meta.upgrade()) {
                    m.set_text(&format!("{base}\n{w} × {h}"));
                }
            });
            return;
        }
        let texty = gio::content_type_is_a(&mime, "text/plain") || ["application/json", "application/toml", "application/x-yaml", "application/xml"].contains(&mime.as_str());
        if texty {
            let (text, stack) = (self.text.downgrade(), self.stack.downgrade());
            glib::spawn_future_local(async move {
                let got = gio::spawn_blocking(move || {
                    let mut buf = vec![];
                    std::fs::File::open(path).ok()?.take(TEXT_LIMIT).read_to_end(&mut buf).ok()?;
                    let cut = buf.len() as u64 == TEXT_LIMIT;
                    // binary data pretending to be text
                    (!buf.contains(&0)).then(|| (String::from_utf8_lossy(&buf).into_owned(), cut))
                })
                .await
                .ok()
                .flatten();
                let (Some((mut s, cut)), Some(t), Some(stack), true) = (got, text.upgrade(), stack.upgrade(), fresh()) else { return };
                if cut {
                    s.push_str("\n\n… (first part only)");
                }
                t.buffer().set_text(&s);
                stack.set_visible_child_name("text");
            });
            return;
        }
        if let Some(src) = thumbs::source(info, &f) {
            let (pic, stack) = (self.picture.downgrade(), self.stack.downgrade());
            thumbq::request(src, Size::XLarge, fresh, move |t| {
                if let (Some(p), Some(s)) = (pic.upgrade(), stack.upgrade()) {
                    p.set_paintable(Some(&t));
                    s.set_visible_child_name("picture");
                }
            });
        }
    }
}

/// The quick look window: Space or Esc closes it, the arrows move through the folder.
pub struct QuickLook {
    pub window: gtk::Window,
    preview: Preview,
}

impl QuickLook {
    fn update(&self, w: &Win, p: &Pane) {
        match p.files.selected_infos().as_slice() {
            [] => self.preview.show_nothing("Nothing selected"),
            [one] => self.preview.show(w, one),
            many => self.preview.show_many(many),
        }
    }
}

impl Win {
    pub fn toggle_quick_look(self: &Rc<Self>) {
        let open = self.quick_look.borrow_mut().take();
        if let Some(q) = open {
            q.window.destroy();
            return;
        }
        let pane = self.active();
        if pane.files.selected_infos().is_empty() {
            return;
        }
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .title("Quick look")
            .decorated(false)
            .default_width(900)
            .default_height(660)
            .build();
        window.add_css_class("burrow");
        window.add_css_class("dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("dialog-root");
        let preview = Preview::new(true);
        preview.widget.set_vexpand(true);
        root.append(&preview.widget);
        window.set_child(Some(&root));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, k, _, mods| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            let pane = w.active();
            match k {
                gdk::Key::Escape | gdk::Key::space | gdk::Key::q if mods.is_empty() => w.toggle_quick_look(),
                gdk::Key::Left => pane.move_cursor(-1, 0),
                gdk::Key::Right => pane.move_cursor(1, 0),
                gdk::Key::Up => pane.move_cursor(0, -1),
                gdk::Key::Down => pane.move_cursor(0, 1),
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    w.toggle_quick_look();
                    pane.open_selected();
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        window.add_controller(keys);
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(w) = weak.upgrade() {
                w.quick_look.borrow_mut().take();
            }
            glib::Propagation::Proceed
        });
        let q = QuickLook { window, preview };
        q.update(self, &pane);
        q.window.present();
        *self.quick_look.borrow_mut() = Some(q);
    }

    /// The selection (or the active pane) changed: refresh whatever is previewing.
    /// Debounced, so holding an arrow key doesn't load every file on the way.
    pub fn refresh_preview(self: &Rc<Self>) {
        let pane_open = self.preview_rev.reveals_child();
        if !pane_open && self.quick_look.borrow().is_none() {
            return;
        }
        let n = self.preview_turn.get() + 1;
        self.preview_turn.set(n);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(70), move || {
            let Some(w) = weak.upgrade() else { return };
            if w.preview_turn.get() != n {
                return;
            }
            let p = w.active();
            if let Some(q) = w.quick_look.borrow().as_ref() {
                q.update(&w, &p);
            }
            if w.preview_rev.reveals_child() {
                match p.files.selected_infos().as_slice() {
                    [] => w.preview.show_nothing("Select something to preview it"),
                    [one] => w.preview.show(&w, one),
                    many => w.preview.show_many(many),
                }
            }
        });
    }
}

/// Kept here so `Win` only needs one field type.
pub type QuickLookSlot = RefCell<Option<QuickLook>>;
