//! Background jobs: the work runs on a worker thread, the UI polls its `Progress`.
//! A status-bar button (only shown while something takes longer than a moment) opens a
//! popover with one row per job: bar, "3 of 10 · name", pause/resume and cancel.

use super::Win;
use crate::ops::Progress;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct Jobs {
    pub button: gtk::MenuButton,
    list: gtk::Box,
    label: gtk::Label,
    running: Cell<usize>,
}

impl Jobs {
    pub fn new() -> Jobs {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        list.add_css_class("jobs-list");
        list.set_width_request(340);
        let pop = gtk::Popover::new();
        pop.set_child(Some(&list));
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spin = gtk::Spinner::new();
        spin.start();
        let label = gtk::Label::new(None);
        hb.append(&spin);
        hb.append(&label);
        let button = gtk::MenuButton::new();
        button.set_child(Some(&hb));
        button.set_popover(Some(&pop));
        button.add_css_class("jobs-button");
        button.set_visible(false);
        button.set_tooltip_text(Some("Running jobs"));
        Jobs { button, list, label, running: Cell::new(0) }
    }

    fn update_button(&self) {
        let n = self.running.get();
        self.label.set_text(&format!("{n} job{}", if n == 1 { "" } else { "s" }));
        if n == 0 {
            self.button.set_visible(false);
            self.button.popdown();
        }
    }
}

/// Run `work` on a worker thread with progress + pause/cancel; `done` runs on the UI thread.
pub fn run<R, F, D>(w: &Rc<Win>, title: &str, work: F, done: D)
where
    R: Send + 'static,
    F: FnOnce(&Progress, &gio::Cancellable) -> R + Send + 'static,
    D: FnOnce(&Rc<Win>, R, &Progress, bool) + 'static,
{
    let progress = Arc::new(Progress::default());
    let cancel = gio::Cancellable::new();

    // row in the popover
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("job-row");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("settings-title");
    t.set_xalign(0.0);
    t.set_hexpand(true);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let pause = gtk::Button::from_icon_name("media-playback-pause-symbolic");
    pause.add_css_class("eject");
    pause.set_tooltip_text(Some("Pause"));
    let stop = gtk::Button::from_icon_name("process-stop-symbolic");
    stop.add_css_class("eject");
    stop.set_tooltip_text(Some("Cancel"));
    top.append(&t);
    top.append(&pause);
    top.append(&stop);
    let bar = gtk::ProgressBar::new();
    bar.add_css_class("usage");
    let sub = gtk::Label::new(Some("Starting…"));
    sub.add_css_class("settings-sub");
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    row.append(&top);
    row.append(&bar);
    row.append(&sub);
    w.jobs.list.append(&row);
    w.jobs.running.set(w.jobs.running.get() + 1);
    w.jobs.update_button();

    let p = progress.clone();
    pause.connect_clicked(move |b| {
        let now = !p.paused.load(Ordering::Relaxed);
        p.paused.store(now, Ordering::Relaxed);
        b.set_icon_name(if now { "media-playback-start-symbolic" } else { "media-playback-pause-symbolic" });
        b.set_tooltip_text(Some(if now { "Resume" } else { "Pause" }));
    });
    let c = cancel.clone();
    stop.connect_clicked(move |_| c.cancel());

    // only show the panel for jobs that take a moment
    let weak = Rc::downgrade(w);
    let finished = Rc::new(Cell::new(false));
    let fin = finished.clone();
    glib::timeout_add_local_once(Duration::from_millis(350), move || {
        if let (Some(w), false) = (weak.upgrade(), fin.get()) {
            w.jobs.button.set_visible(true);
        }
    });

    // UI poll
    let (p, barw, subw, fin) = (progress.clone(), bar.downgrade(), sub.downgrade(), finished.clone());
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let (Some(bar), Some(sub)) = (barw.upgrade(), subw.upgrade()) else { return glib::ControlFlow::Break };
        if fin.get() {
            return glib::ControlFlow::Break;
        }
        let (d, t) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        if t > 0 {
            bar.set_fraction((d as f64 / t as f64).min(1.0));
            let cur = p.current.lock().map(|c| c.clone()).unwrap_or_default();
            let paused = if p.paused.load(Ordering::Relaxed) { "Paused · " } else { "" };
            sub.set_text(&format!("{paused}{} of {t} · {cur}", d.min(t)));
        } else {
            bar.pulse();
        }
        glib::ControlFlow::Continue
    });

    let weak = Rc::downgrade(w);
    glib::spawn_future_local(async move {
        let (p2, c2) = (progress.clone(), cancel.clone());
        let res = gio::spawn_blocking(move || work(&p2, &c2)).await;
        finished.set(true);
        let Some(w) = weak.upgrade() else { return };
        w.jobs.list.remove(&row);
        w.jobs.running.set(w.jobs.running.get().saturating_sub(1));
        w.jobs.update_button();
        match res {
            Ok(r) => done(&w, r, &progress, cancel.is_cancelled()),
            Err(_) => w.show_notice("A background job crashed; nothing else was touched", true),
        }
    });
}

/// "Moved 3 items to Trash" plus the first error, if any.
pub fn summary(ok_text: &str, p: &Progress, cancelled: bool) -> (String, bool) {
    let errs = p.errors();
    let mut s = ok_text.to_string();
    if cancelled {
        s.push_str(" (cancelled)");
    }
    if let Some(first) = errs.first() {
        s.push_str(&format!(" · {} problem{}: {first}", errs.len(), if errs.len() == 1 { "" } else { "s" }));
    }
    (s, !errs.is_empty())
}
