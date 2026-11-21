//! File operations that run on a worker thread (gio::spawn_blocking).
//! They report into a shared `Progress`, honour pause/cancel between items, and never
//! overwrite anything: restores pick a free name instead.

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub current: Mutex<String>,
    pub paused: AtomicBool,
    pub errors: Mutex<Vec<String>>,
    /// Byte progress for copies (0 total = count-based only).
    pub bytes_done: AtomicU64,
    pub bytes_total: AtomicU64,
    /// A name clash the worker is waiting on; the UI answers through `answer` + `answered`.
    pub ask: Mutex<Option<crate::transfer::Conflict>>,
    pub answer: Mutex<Option<(crate::transfer::Choice, bool)>>,
    pub answered: Condvar,
}

impl Progress {
    pub fn set_current(&self, s: &str) {
        if let Ok(mut c) = self.current.lock() {
            *c = s.to_string();
        }
    }
    pub fn error(&self, what: &str, e: &glib::Error) {
        if let Ok(mut v) = self.errors.lock() {
            v.push(format!("{what}: {}", e.message()));
        }
    }
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}

/// Blocks while paused. Returns false if the job was cancelled.
pub fn checkpoint(p: &Progress, c: &gio::Cancellable) -> bool {
    while p.paused.load(Ordering::Relaxed) && !c.is_cancelled() {
        std::thread::sleep(Duration::from_millis(100));
    }
    !c.is_cancelled()
}

fn name_of(f: &gio::File) -> String {
    f.basename().map(|b| b.to_string_lossy().to_string()).unwrap_or_else(|| f.uri().to_string())
}

/// Move files to the trash. Returns the ones that made it (their original paths are what
/// undo needs) and the unix time the job started.
pub fn trash(files: &[gio::File], p: &Progress, c: &gio::Cancellable) -> (Vec<gio::File>, i64) {
    let start = glib::real_time() / 1_000_000;
    p.total.store(files.len() as u64, Ordering::Relaxed);
    let mut ok = vec![];
    for f in files {
        if !checkpoint(p, c) {
            break;
        }
        let name = name_of(f);
        p.set_current(&name);
        match f.trash(Some(c)) {
            Ok(()) => ok.push(f.clone()),
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => break,
            Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => {
                if let Ok(mut v) = p.errors.lock() {
                    v.push(format!("{name}: this drive has no trash (use Shift+Delete to delete it for good)"));
                }
            }
            Err(e) => p.error(&name, &e),
        }
        p.tick();
    }
    (ok, start)
}

/// Number of entries in the trees (for a meaningful progress bar). Doesn't follow symlinks.
pub fn count_tree(files: &[gio::File], c: &gio::Cancellable) -> u64 {
    let mut n = 0;
    let mut stack: Vec<gio::File> = files.to_vec();
    while let Some(f) = stack.pop() {
        if c.is_cancelled() {
            break;
        }
        n += 1;
        let is_dir = f.query_file_type(gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(c)) == gio::FileType::Directory;
        if is_dir {
            if let Ok(en) = f.enumerate_children("standard::name", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(c)) {
                while let Ok(Some(info)) = en.next_file(Some(c)) {
                    stack.push(f.child(info.name()));
                }
            }
        }
    }
    n
}

/// Delete for good, depth first. Symlinks are removed, never followed.
pub fn delete(files: &[gio::File], p: &Progress, c: &gio::Cancellable) {
    p.total.store(count_tree(files, c), Ordering::Relaxed);
    for f in files {
        if !delete_one(f, p, c) {
            break;
        }
    }
}

fn delete_one(f: &gio::File, p: &Progress, c: &gio::Cancellable) -> bool {
    if !checkpoint(p, c) {
        return false;
    }
    let name = name_of(f);
    p.set_current(&name);
    let is_dir = f.query_file_type(gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(c)) == gio::FileType::Directory;
    if is_dir {
        match f.enumerate_children("standard::name", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(c)) {
            Ok(en) => {
                let mut kids = vec![];
                while let Ok(Some(info)) = en.next_file(Some(c)) {
                    kids.push(f.child(info.name()));
                }
                drop(en);
                for k in kids {
                    if !delete_one(&k, p, c) {
                        return false;
                    }
                }
            }
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => return false,
            Err(e) => {
                p.error(&name, &e);
                p.tick();
                return true; // can't list it: leave it, carry on with the rest
            }
        }
    }
    match f.delete(Some(c)) {
        Ok(()) => {}
        Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => return false,
        Err(e) => p.error(&name, &e),
    }
    p.tick();
    true
}

/// An item in trash:/// and where it came from.
#[derive(Clone, Debug)]
pub struct Trashed {
    pub item: gio::File,
    pub orig: String,
    pub deleted: i64,
}

/// List the trash (top-level items only).
pub fn list_trash(c: &gio::Cancellable) -> Vec<Trashed> {
    let trash = gio::File::for_uri("trash:///");
    let mut out = vec![];
    let Ok(en) = trash.enumerate_children("standard::name,trash::orig-path,trash::deletion-date", gio::FileQueryInfoFlags::NONE, Some(c)) else {
        return out;
    };
    while let Ok(Some(info)) = en.next_file(Some(c)) {
        let Some(orig) = info.attribute_byte_string("trash::orig-path") else { continue };
        let deleted = info.deletion_date().map(|d| d.to_unix()).unwrap_or(0);
        out.push(Trashed { item: trash.child(info.name()), orig: orig.to_string(), deleted });
    }
    out
}

/// Pick the trash entries that undo for "trashed these paths at `since`" should restore:
/// the newest entry per path, deleted no earlier than `since` (minus clock slack).
pub fn match_trashed(all: &[Trashed], paths: &[String], since: i64) -> Vec<Trashed> {
    let mut out = vec![];
    for p in paths {
        let best = all.iter().filter(|t| &t.orig == p && t.deleted >= since - 2).max_by_key(|t| t.deleted);
        if let Some(t) = best {
            out.push(t.clone());
        }
    }
    out
}

/// "name.ext" -> "name (2).ext", "name (3).ext"… until `exists` says no.
pub fn free_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    // keep compound extensions like .tar.gz together; dotfiles have no extension
    let (stem, ext) = match name.find('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (2..10_000).map(|n| format!("{stem} ({n}){ext}")).find(|c| !exists(c)).unwrap_or_else(|| format!("{stem} (restored){ext}"))
}

/// Move trash items back to where they came from. If something already exists there,
/// the restored item gets a free name instead; nothing is overwritten.
pub fn restore(items: &[Trashed], p: &Progress, c: &gio::Cancellable) -> u64 {
    p.total.store(items.len() as u64, Ordering::Relaxed);
    let mut n = 0;
    for t in items {
        if !checkpoint(p, c) {
            break;
        }
        let dest = gio::File::for_path(&t.orig);
        let name = name_of(&dest);
        p.set_current(&name);
        let Some(parent) = dest.parent() else { continue };
        if let Err(e) = parent.make_directory_with_parents(Some(c)) {
            if !e.matches(gio::IOErrorEnum::Exists) {
                p.error(&name, &e);
                p.tick();
                continue;
            }
        }
        let free = free_name(&name, |cand| parent.child(cand).query_exists(Some(c)));
        let target = parent.child(&free);
        match t.item.move_(&target, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, Some(c), None) {
            Ok(()) => n += 1,
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => break,
            Err(e) => p.error(&name, &e),
        }
        p.tick();
    }
    n
}

/// Delete everything in the trash for good.
pub fn empty_trash(p: &Progress, c: &gio::Cancellable) {
    let items: Vec<gio::File> = list_trash(c).into_iter().map(|t| t.item).collect();
    p.total.store(items.len() as u64, Ordering::Relaxed);
    for f in items {
        if !checkpoint(p, c) {
            break;
        }
        p.set_current(&name_of(&f));
        // the trash backend deletes whole items, folders included
        if let Err(e) = f.delete(Some(c)) {
            if e.matches(gio::IOErrorEnum::Cancelled) {
                break;
            }
            p.error(&name_of(&f), &e);
        }
        p.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("burrow-ops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn delete_tree_without_following_symlinks() {
        let d = tmp("del");
        let keep = tmp("keep");
        fs::write(keep.join("precious.txt"), "x").unwrap();
        fs::create_dir_all(d.join("a/b/c")).unwrap();
        fs::write(d.join("a/b/c/f.txt"), "1").unwrap();
        fs::write(d.join("a/g.txt"), "2").unwrap();
        std::os::unix::fs::symlink(&keep, d.join("a/link-to-keep")).unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        delete(&[gio::File::for_path(d.join("a"))], &p, &c);
        assert!(!d.join("a").exists());
        assert!(keep.join("precious.txt").exists(), "followed a symlink!");
        assert_eq!(p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        assert!(p.errors().is_empty(), "{:?}", p.errors());
        let _ = fs::remove_dir_all(keep);
    }

    #[test]
    fn cancelled_delete_stops() {
        let d = tmp("cancel");
        for i in 0..5 {
            fs::write(d.join(format!("{i}")), "x").unwrap();
        }
        let p = Progress::default();
        let c = gio::Cancellable::new();
        c.cancel();
        let files: Vec<_> = (0..5).map(|i| gio::File::for_path(d.join(format!("{i}")))).collect();
        delete(&files, &p, &c);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 5);
    }

    #[test]
    fn free_names() {
        let taken = ["a.txt", "a (2).txt", "x", "f.tar.gz", ".bashrc"];
        let ex = |n: &str| taken.contains(&n);
        assert_eq!(free_name("b.txt", ex), "b.txt");
        assert_eq!(free_name("a.txt", ex), "a (3).txt");
        assert_eq!(free_name("x", ex), "x (2)");
        assert_eq!(free_name("f.tar.gz", ex), "f (2).tar.gz");
        assert_eq!(free_name(".bashrc", ex), ".bashrc (2)");
    }

    #[test]
    fn undo_matches_newest_recent_entry() {
        let f = |o: &str, d| Trashed { item: gio::File::for_uri("trash:///x"), orig: o.into(), deleted: d };
        let all = [f("/h/a", 100), f("/h/a", 500), f("/h/b", 50), f("/h/c", 499)];
        let m = match_trashed(&all, &["/h/a".into(), "/h/b".into(), "/h/c".into()], 500);
        assert_eq!(m.len(), 2); // b was trashed long before this job
        assert_eq!(m[0].deleted, 500);
        assert_eq!(m[1].orig, "/h/c"); // within clock slack
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Real trash round trip (uses ~/.local/share/Trash). Run with --ignored.
    #[test]
    #[ignore]
    fn trash_then_undo_restores() {
        let d = glib::home_dir().join(".cache/burrow-test");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("burrow-undo-test.txt");
        std::fs::write(&f, "hello").unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        let (ok, since) = trash(&[gio::File::for_path(&f)], &p, &c);
        assert_eq!(ok.len(), 1, "{:?}", p.errors());
        assert!(!f.exists());
        let all = list_trash(&c);
        let m = match_trashed(&all, &[f.to_string_lossy().to_string()], since);
        assert_eq!(m.len(), 1);
        assert_eq!(restore(&m, &p, &c), 1, "{:?}", p.errors());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
