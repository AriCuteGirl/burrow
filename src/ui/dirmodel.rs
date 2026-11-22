//! Folder contents as a list model: loads in batches (async GIO), then follows a directory
//! monitor and applies each change in place (add / remove / rename / update), so the view
//! keeps its selection and scroll position. Replaces GtkDirectoryList, whose monitoring
//! didn't deliver changes here.

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

const BATCH: i32 = 256;

pub struct DirModel {
    pub store: gio::ListStore,
    attrs: &'static str,
    file: RefCell<Option<gio::File>>,
    /// names currently in `store`, for O(1) duplicate checks while loading
    names: RefCell<HashSet<String>>,
    loading: Cell<bool>,
    error: RefCell<Option<glib::Error>>,
    cancel: RefCell<gio::Cancellable>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Called whenever loading/error changes.
    on_state: RefCell<Option<Box<dyn Fn()>>>,
}

/// Our own temp files during a copy: never shown, never flicker in and out.
fn ignored(name: &str) -> bool {
    name.contains(".burrow-part-")
}

impl DirModel {
    pub fn new(attrs: &'static str) -> Rc<DirModel> {
        Rc::new(DirModel {
            store: gio::ListStore::new::<gio::FileInfo>(),
            attrs,
            file: RefCell::new(None),
            names: RefCell::new(HashSet::new()),
            loading: Cell::new(false),
            error: RefCell::new(None),
            cancel: RefCell::new(gio::Cancellable::new()),
            monitor: RefCell::new(None),
            on_state: RefCell::new(None),
        })
    }

    pub fn connect_state(&self, f: impl Fn() + 'static) {
        *self.on_state.borrow_mut() = Some(Box::new(f));
    }

    fn state_changed(&self) {
        if let Some(f) = self.on_state.borrow().as_ref() {
            f();
        }
    }

    pub fn file(&self) -> Option<gio::File> {
        self.file.borrow().clone()
    }
    pub fn is_loading(&self) -> bool {
        self.loading.get()
    }
    pub fn error(&self) -> Option<glib::Error> {
        self.error.borrow().clone()
    }
    pub fn n_items(&self) -> u32 {
        self.store.n_items()
    }

    /// Load a folder (None = clear). Cancels whatever was loading before.
    pub fn set_file(self: &Rc<Self>, f: Option<&gio::File>) {
        self.cancel.borrow().cancel();
        *self.cancel.borrow_mut() = gio::Cancellable::new();
        *self.monitor.borrow_mut() = None;
        *self.error.borrow_mut() = None;
        self.store.remove_all();
        self.names.borrow_mut().clear();
        *self.file.borrow_mut() = f.cloned();
        let Some(dir) = f.cloned() else {
            self.loading.set(false);
            self.state_changed();
            return;
        };
        self.loading.set(true);
        self.state_changed();

        // watch first, so nothing that happens during loading is missed
        if let Ok(m) = dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, Some(&*self.cancel.borrow())) {
            let weak = Rc::downgrade(self);
            let d = dir.clone();
            m.connect_changed(move |_, file, other, ev| {
                if let Some(me) = weak.upgrade() {
                    if me.file.borrow().as_ref().is_some_and(|x| x.equal(&d)) {
                        me.on_event(file, other, ev);
                    }
                }
            });
            *self.monitor.borrow_mut() = Some(m);
        }

        let weak = Rc::downgrade(self);
        let cancel = self.cancel.borrow().clone();
        let attrs = self.attrs;
        glib::spawn_future_local(async move {
            let en = dir.enumerate_children_future(attrs, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT).await;
            let en = match en {
                Ok(en) => en,
                Err(e) => {
                    if let (Some(me), false) = (weak.upgrade(), cancel.is_cancelled()) {
                        *me.error.borrow_mut() = Some(e);
                        me.loading.set(false);
                        me.state_changed();
                    }
                    return;
                }
            };
            loop {
                let batch = en.next_files_future(BATCH, glib::Priority::DEFAULT).await;
                let Some(me) = weak.upgrade() else { return };
                if cancel.is_cancelled() {
                    return;
                }
                match batch {
                    Ok(infos) if infos.is_empty() => break,
                    Ok(infos) => {
                        let items: Vec<gio::FileInfo> = infos
                            .into_iter()
                            .filter(|i| {
                                let n = i.name().to_string_lossy().to_string();
                                !ignored(&n) && me.names.borrow_mut().insert(n)
                            })
                            .map(|i| {
                                i.set_attribute_object("standard::file", &dir.child(i.name()));
                                i
                            })
                            .collect();
                        me.store.splice(me.store.n_items(), 0, &items);
                    }
                    Err(e) => {
                        *me.error.borrow_mut() = Some(e);
                        break;
                    }
                }
            }
            if let Some(me) = weak.upgrade() {
                if !cancel.is_cancelled() {
                    me.loading.set(false);
                    me.state_changed();
                }
            }
        });
    }

    /// Show an error that happened outside listing (e.g. connecting failed).
    pub fn set_error(&self, e: glib::Error) {
        *self.error.borrow_mut() = Some(e);
        self.loading.set(false);
        self.state_changed();
    }

    pub fn reload(self: &Rc<Self>) {
        let f = self.file();
        self.set_file(f.as_ref());
    }

    fn position_of(&self, name: &str) -> Option<u32> {
        if !self.names.borrow().contains(name) {
            return None;
        }
        let n = self.store.n_items();
        (0..n).find(|&i| {
            self.store.item(i).and_downcast::<gio::FileInfo>().is_some_and(|x| x.name().to_string_lossy() == name)
        })
    }

    fn remove_name(&self, name: &str) {
        if let Some(p) = self.position_of(name) {
            self.store.remove(p);
        }
        self.names.borrow_mut().remove(name);
    }

    /// Query a child and insert it, or replace the existing entry with the same name.
    fn refresh_child(self: &Rc<Self>, child: &gio::File) {
        let Some(name) = child.basename().map(|b| b.to_string_lossy().to_string()) else { return };
        if ignored(&name) {
            return;
        }
        let weak = Rc::downgrade(self);
        let cancel = self.cancel.borrow().clone();
        let c = child.clone();
        child.query_info_async(self.attrs, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT, Some(&cancel), move |r| {
            let Some(me) = weak.upgrade() else { return };
            match r {
                Ok(info) => {
                    info.set_attribute_object("standard::file", &c);
                    match me.position_of(&name) {
                        Some(p) => me.store.splice(p, 1, &[info]),
                        None => {
                            me.names.borrow_mut().insert(name.clone());
                            me.store.append(&info);
                        }
                    }
                }
                // gone again before we looked (temp files etc.)
                Err(_) => me.remove_name(&name),
            }
        });
    }

    fn on_event(self: &Rc<Self>, file: &gio::File, other: Option<&gio::File>, ev: gio::FileMonitorEvent) {
        use gio::FileMonitorEvent as E;
        let name = || file.basename().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
        match ev {
            E::Created | E::MovedIn | E::ChangesDoneHint | E::AttributeChanged => self.refresh_child(file),
            E::Deleted | E::MovedOut => self.remove_name(&name()),
            E::Renamed => {
                self.remove_name(&name());
                if let Some(o) = other {
                    self.refresh_child(o);
                }
            }
            E::Changed | E::PreUnmount | E::Unmounted | E::Moved => {}
            _ => {}
        }
        // the folder itself went away
        if matches!(ev, E::Deleted | E::MovedOut) && self.file.borrow().as_ref().is_some_and(|d| d.equal(file)) {
            *self.error.borrow_mut() = Some(glib::Error::new(gio::IOErrorEnum::NotFound, "The folder was removed"));
            self.state_changed();
        }
    }
}
