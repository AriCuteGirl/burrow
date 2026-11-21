//! Copy and move, on a worker thread.
//!
//! Safety rules:
//! - files are copied to a hidden temp name next to the target and renamed into place,
//!   so a cancelled or failed copy never leaves a half file under the real name, and
//!   "Replace" swaps the old file out in one atomic rename;
//! - a move across drives is copy + delete, and the source is only deleted when its copy
//!   finished without errors;
//! - symlinks are copied as links, never followed;
//! - a folder can't be copied or moved into itself.
//! Name clashes are asked about through `Progress::ask` (the worker blocks until answered).

use crate::ops::{Progress, checkpoint, free_name};
use gtk::gio;
use gtk::gio::prelude::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Copy,
    Move,
    /// Move within the same drive, copy across drives (like dragging in most file managers).
    Auto,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Choice {
    Skip,
    Replace,
    KeepBoth,
    /// Folder onto folder: copy/move the contents in, asking again per clash.
    Merge,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct Conflict {
    pub name: String,
    pub dest_dir: String,
    pub src_is_dir: bool,
    pub dest_is_dir: bool,
    pub src_size: u64,
    pub dest_size: u64,
    pub src_mtime: i64,
    pub dest_mtime: i64,
}

/// One top-level item that was copied or moved (for undo).
#[derive(Clone, Debug)]
pub struct Done {
    pub from: gio::File,
    pub to: gio::File,
    pub moved: bool,
}

struct Ctx<'a> {
    p: &'a Progress,
    c: &'a gio::Cancellable,
    /// "for all" answers: (for files, for folders)
    all_files: Option<Choice>,
    all_dirs: Option<Choice>,
}

const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

fn ftype(f: &gio::File, c: &gio::Cancellable) -> gio::FileType {
    f.query_file_type(NOFOLLOW, Some(c))
}

fn name_of(f: &gio::File) -> String {
    f.basename().map(|b| b.to_string_lossy().to_string()).unwrap_or_else(|| f.uri().to_string())
}

fn fs_id(f: &gio::File, c: &gio::Cancellable) -> Option<String> {
    f.query_info("id::filesystem", gio::FileQueryInfoFlags::NONE, Some(c)).ok()?.attribute_string("id::filesystem").map(|s| s.to_string())
}

/// Entry count and bytes of a tree (symlinks counted, not followed).
pub fn tree_stats(files: &[gio::File], c: &gio::Cancellable) -> (u64, u64) {
    let (mut n, mut bytes) = (0, 0);
    let mut stack: Vec<gio::File> = files.to_vec();
    while let Some(f) = stack.pop() {
        if c.is_cancelled() {
            break;
        }
        n += 1;
        let Ok(info) = f.query_info("standard::type,standard::size", NOFOLLOW, Some(c)) else { continue };
        if info.file_type() == gio::FileType::Directory {
            if let Ok(en) = f.enumerate_children("standard::name", NOFOLLOW, Some(c)) {
                while let Ok(Some(i)) = en.next_file(Some(c)) {
                    stack.push(f.child(i.name()));
                }
            }
        } else {
            bytes += info.size().max(0) as u64;
        }
    }
    (n, bytes)
}

/// Copy or move `sources` into the folder `dest_dir`.
pub fn transfer(sources: &[gio::File], dest_dir: &gio::File, mode: Mode, p: &Progress, c: &gio::Cancellable) -> Vec<Done> {
    let (n, bytes) = tree_stats(sources, c);
    p.total.store(n, Ordering::Relaxed);
    p.bytes_total.store(bytes, Ordering::Relaxed);
    let dest_fs = fs_id(dest_dir, c);
    let mut ctx = Ctx { p, c, all_files: None, all_dirs: None };
    let mut out = vec![];
    for src in sources {
        if !checkpoint(p, c) {
            break;
        }
        let name = name_of(src);
        if dest_dir.equal(src) || dest_dir.has_prefix(src) {
            p.errors.lock().unwrap().push(format!("{name}: can't put a folder inside itself"));
            continue;
        }
        let same_fs = dest_fs.is_some() && fs_id(src, c) == dest_fs;
        let moving = match mode {
            Mode::Copy => false,
            Mode::Move => true,
            Mode::Auto => same_fs,
        };
        let same_parent = src.parent().is_some_and(|pa| pa.equal(dest_dir));
        let target = if same_parent {
            if moving {
                continue; // moving into the folder it's already in: nothing to do
            }
            // copy into the same folder: "name (2)" without asking
            dest_dir.child(free_name(&name, |n| dest_dir.child(n).query_exists(Some(c))))
        } else {
            dest_dir.child(&name)
        };
        let errs_before = p.errors().len();
        if let Some(to) = item(&mut ctx, src, &target, moving) {
            if p.errors().len() == errs_before || to.query_exists(Some(c)) {
                out.push(Done { from: src.clone(), to, moved: moving });
            }
        }
        if c.is_cancelled() {
            break;
        }
    }
    out
}

/// Ask the UI about a clash (or reuse a "for all" answer).
fn resolve(ctx: &mut Ctx, src: &gio::File, target: &gio::File) -> Choice {
    let c = ctx.c;
    let q = |f: &gio::File| f.query_info("standard::type,standard::size,time::modified", NOFOLLOW, Some(c)).ok();
    let (si, di) = (q(src), q(target));
    let is_dir = |i: &Option<gio::FileInfo>| i.as_ref().is_some_and(|i| i.file_type() == gio::FileType::Directory);
    let (sd, dd) = (is_dir(&si), is_dir(&di));
    let remembered = if sd && dd { ctx.all_dirs } else { ctx.all_files };
    if let Some(ch) = remembered {
        return ch;
    }
    let size = |i: &Option<gio::FileInfo>| i.as_ref().map(|i| i.size().max(0) as u64).unwrap_or(0);
    let mtime = |i: &Option<gio::FileInfo>| i.as_ref().and_then(|i| i.modification_date_time()).map(|d| d.to_unix()).unwrap_or(0);
    let conflict = Conflict {
        name: name_of(target),
        dest_dir: target.parent().map(|p| p.parse_name().to_string()).unwrap_or_default(),
        src_is_dir: sd,
        dest_is_dir: dd,
        src_size: size(&si),
        dest_size: size(&di),
        src_mtime: mtime(&si),
        dest_mtime: mtime(&di),
    };
    *ctx.p.answer.lock().unwrap() = None;
    *ctx.p.ask.lock().unwrap() = Some(conflict);
    let mut ans = ctx.p.answer.lock().unwrap();
    let (choice, for_all) = loop {
        if let Some(a) = ans.take() {
            break a;
        }
        if c.is_cancelled() {
            break (Choice::Cancel, false);
        }
        ans = ctx.p.answered.wait_timeout(ans, Duration::from_millis(200)).unwrap().0;
    };
    drop(ans);
    *ctx.p.ask.lock().unwrap() = None;
    // never Replace across types, never Merge non-folders
    let choice = match choice {
        Choice::Replace if sd != dd => Choice::KeepBoth,
        Choice::Merge if !(sd && dd) => Choice::KeepBoth,
        ch => ch,
    };
    if for_all {
        if sd && dd {
            ctx.all_dirs = Some(choice);
        } else {
            ctx.all_files = Some(choice);
        }
    }
    choice
}

/// Copy/move one item to `target`. Returns where it ended up (None = skipped/failed).
fn item(ctx: &mut Ctx, src: &gio::File, target: &gio::File, moving: bool) -> Option<gio::File> {
    let (p, c) = (ctx.p, ctx.c);
    if !checkpoint(p, c) {
        return None;
    }
    let name = name_of(src);
    p.set_current(&name);
    let mut target = target.clone();
    let mut replace = false;
    if ftype(&target, c) != gio::FileType::Unknown {
        match resolve(ctx, src, &target) {
            Choice::Skip => {
                let (n, b) = tree_stats(std::slice::from_ref(src), c);
                p.done.fetch_add(n, Ordering::Relaxed);
                p.bytes_done.fetch_add(b, Ordering::Relaxed);
                return None;
            }
            Choice::Cancel => {
                c.cancel();
                return None;
            }
            Choice::KeepBoth => {
                let parent = target.parent()?;
                let free = free_name(&name_of(&target), |n| parent.child(n).query_exists(Some(c)));
                target = parent.child(free);
            }
            Choice::Merge => {
                p.tick();
                let kids = children(src, c);
                for k in kids {
                    let t = target.child(name_of(&k));
                    item(ctx, &k, &t, moving);
                    if c.is_cancelled() {
                        return None;
                    }
                }
                if moving && children(src, c).is_empty() {
                    let _ = src.delete(Some(c));
                }
                return Some(target);
            }
            Choice::Replace => replace = true,
        }
    }

    if moving {
        let (n, b) = tree_stats(std::slice::from_ref(src), c);
        let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE | gio::FileCopyFlags::ALL_METADATA;
        if replace {
            flags |= gio::FileCopyFlags::OVERWRITE; // a rename over the old one: atomic
        }
        match src.move_(&target, flags, Some(c), None) {
            Ok(()) => {
                p.done.fetch_add(n, Ordering::Relaxed);
                p.bytes_done.fetch_add(b, Ordering::Relaxed);
                return Some(target);
            }
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => return None,
            Err(e) if e.matches(gio::IOErrorEnum::NotSupported) || e.matches(gio::IOErrorEnum::WouldRecurse) || e.matches(gio::IOErrorEnum::WouldMerge) => {
                // another drive: copy, then delete the source only if the copy was clean
                let errs = p.errors().len();
                let res = copy_tree(ctx, src, &target, replace);
                if res.is_some() && p.errors().len() == errs && !c.is_cancelled() {
                    crate::ops::delete(std::slice::from_ref(src), &Progress::default(), c);
                }
                return res;
            }
            Err(e) => {
                p.error(&name, &e);
                return None;
            }
        }
    }
    copy_tree(ctx, src, &target, replace)
}

fn children(dir: &gio::File, c: &gio::Cancellable) -> Vec<gio::File> {
    let mut v = vec![];
    if let Ok(en) = dir.enumerate_children("standard::name", NOFOLLOW, Some(c)) {
        while let Ok(Some(i)) = en.next_file(Some(c)) {
            v.push(dir.child(i.name()));
        }
    }
    v
}

/// Recursive copy. Files go through a temp name + rename so the real name only ever holds
/// a complete file.
fn copy_tree(ctx: &mut Ctx, src: &gio::File, target: &gio::File, replace: bool) -> Option<gio::File> {
    let (p, c) = (ctx.p, ctx.c);
    if !checkpoint(p, c) {
        return None;
    }
    let name = name_of(src);
    p.set_current(&name);
    if ftype(src, c) == gio::FileType::Directory {
        if replace {
            // folder replacing a folder is offered as Merge; anything else was turned into KeepBoth
            p.errors.lock().unwrap().push(format!("{name}: can't replace a folder, skipped"));
            return None;
        }
        match target.make_directory(Some(c)) {
            Ok(()) => {}
            Err(e) if e.matches(gio::IOErrorEnum::Exists) && ftype(target, c) == gio::FileType::Directory => {}
            Err(e) => {
                p.error(&name, &e);
                return None;
            }
        }
        let _ = src.copy_attributes(target, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, Some(c));
        p.tick();
        for k in children(src, c) {
            let t = target.child(name_of(&k));
            if ftype(&t, c) != gio::FileType::Unknown {
                item(ctx, &k, &t, false);
            } else {
                copy_tree(ctx, &k, &t, false);
            }
            if c.is_cancelled() {
                return None;
            }
        }
        return Some(target.clone());
    }

    // file or symlink: copy next to the target under a hidden temp name, then rename in
    let parent = target.parent()?;
    let tmp = parent.child(format!(".{}.burrow-part-{}", name_of(target), std::process::id()));
    let base = p.bytes_done.load(Ordering::Relaxed);
    let mut cb = |cur: i64, _tot: i64| {
        p.bytes_done.store(base + cur.max(0) as u64, Ordering::Relaxed);
    };
    let flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA | gio::FileCopyFlags::OVERWRITE;
    let res = src.copy(&tmp, flags, Some(c), Some(&mut cb));
    if let Err(e) = res {
        let _ = tmp.delete(gio::Cancellable::NONE);
        if !e.matches(gio::IOErrorEnum::Cancelled) {
            p.error(&name, &e);
        }
        return None;
    }
    let mut mv = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE;
    if replace {
        mv |= gio::FileCopyFlags::OVERWRITE;
    }
    match tmp.move_(target, mv, gio::Cancellable::NONE, None) {
        Ok(()) => {
            p.tick();
            Some(target.clone())
        }
        Err(e) => {
            let _ = tmp.delete(gio::Cancellable::NONE);
            p.error(&name, &e);
            None
        }
    }
}

/// Undo a transfer: moves go back where they came from (never overwriting), copies go to
/// the trash.
pub fn undo(items: &[Done], p: &Progress, c: &gio::Cancellable) -> u64 {
    p.total.store(items.len() as u64, Ordering::Relaxed);
    let mut n = 0;
    for d in items {
        if !checkpoint(p, c) {
            break;
        }
        let name = name_of(&d.to);
        p.set_current(&name);
        let res = if d.moved {
            if d.from.query_exists(Some(c)) {
                p.errors.lock().unwrap().push(format!("{name}: something new is in its old place, left it where it is"));
                p.tick();
                continue;
            }
            d.to.move_(&d.from, gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA, Some(c), None)
        } else {
            d.to.trash(Some(c))
        };
        match res {
            Ok(()) => n += 1,
            Err(e) => p.error(&name, &e),
        }
        p.tick();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Progress;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("burrow-tr-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn f(p: &Path) -> gio::File {
        gio::File::for_path(p)
    }
    /// Answer every question with `choice` from another thread.
    fn auto_answer(p: &Arc<Progress>, choice: Choice) -> std::thread::JoinHandle<u32> {
        let p = p.clone();
        std::thread::spawn(move || {
            let mut asked = 0;
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(20));
                if p.ask.lock().unwrap().take().is_some() {
                    asked += 1;
                    *p.answer.lock().unwrap() = Some((choice, false));
                    p.answered.notify_all();
                }
            }
            asked
        })
    }

    #[test]
    fn copy_tree_with_symlink_and_bytes() {
        let d = tmp("copy");
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/a.txt"), "aaaa").unwrap();
        fs::write(d.join("src/sub/b.txt"), "bb").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", d.join("src/link")).unwrap();
        fs::create_dir_all(d.join("dst")).unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        let done = transfer(&[f(&d.join("src"))], &f(&d.join("dst")), Mode::Copy, &p, &c);
        assert_eq!(done.len(), 1);
        assert_eq!(fs::read_to_string(d.join("dst/src/sub/b.txt")).unwrap(), "bb");
        assert!(fs::symlink_metadata(d.join("dst/src/link")).unwrap().file_type().is_symlink());
        assert!(d.join("src/a.txt").exists(), "copy removed the source");
        assert!(p.errors().is_empty(), "{:?}", p.errors());
        // no temp files left
        assert!(fs::read_dir(d.join("dst/src")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("burrow-part")));
    }

    #[test]
    fn move_same_drive_and_into_itself_refused() {
        let d = tmp("move");
        fs::create_dir_all(d.join("a/inner")).unwrap();
        fs::write(d.join("a/x"), "x").unwrap();
        fs::create_dir_all(d.join("b")).unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        transfer(&[f(&d.join("a"))], &f(&d.join("a/inner")), Mode::Move, &p, &c);
        assert!(d.join("a/x").exists());
        assert_eq!(p.errors().len(), 1);
        let p = Progress::default();
        let done = transfer(&[f(&d.join("a"))], &f(&d.join("b")), Mode::Auto, &p, &c);
        assert!(done[0].moved);
        assert!(!d.join("a").exists() && d.join("b/a/x").exists());
        // undo puts it back
        assert_eq!(undo(&done, &p, &c), 1);
        assert!(d.join("a/x").exists() && !d.join("b/a").exists());
    }

    #[test]
    fn copy_into_same_folder_makes_numbered_copy() {
        let d = tmp("dup");
        fs::write(d.join("n.txt"), "1").unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        transfer(&[f(&d.join("n.txt"))], &f(&d), Mode::Copy, &p, &c);
        assert_eq!(fs::read_to_string(d.join("n (2).txt")).unwrap(), "1");
    }

    #[test]
    fn conflicts_skip_replace_keepboth() {
        let d = tmp("conf");
        fs::create_dir_all(d.join("s")).unwrap();
        fs::create_dir_all(d.join("t")).unwrap();
        fs::write(d.join("s/f.txt"), "new").unwrap();
        fs::write(d.join("t/f.txt"), "old").unwrap();
        let c = gio::Cancellable::new();
        for (choice, expect_old, expect_copy) in [(Choice::Skip, "old", false), (Choice::KeepBoth, "old", true), (Choice::Replace, "new", false)] {
            let _ = fs::remove_file(d.join("t/f (2).txt"));
            let p = Arc::new(Progress::default());
            let h = auto_answer(&p, choice);
            transfer(&[f(&d.join("s/f.txt"))], &f(&d.join("t")), Mode::Copy, &p, &c);
            assert!(h.join().unwrap() >= 1, "{choice:?} was never asked");
            assert_eq!(fs::read_to_string(d.join("t/f.txt")).unwrap(), expect_old, "{choice:?}");
            assert_eq!(d.join("t/f (2).txt").exists(), expect_copy, "{choice:?}");
            fs::write(d.join("t/f.txt"), "old").unwrap();
        }
    }

    #[test]
    fn merge_folders() {
        let d = tmp("merge");
        fs::create_dir_all(d.join("s/dir")).unwrap();
        fs::create_dir_all(d.join("t/dir")).unwrap();
        fs::write(d.join("s/dir/new.txt"), "n").unwrap();
        fs::write(d.join("t/dir/keep.txt"), "k").unwrap();
        let p = Arc::new(Progress::default());
        let h = auto_answer(&p, Choice::Merge);
        let c = gio::Cancellable::new();
        transfer(&[f(&d.join("s/dir"))], &f(&d.join("t")), Mode::Copy, &p, &c);
        h.join().unwrap();
        assert!(d.join("t/dir/new.txt").exists() && d.join("t/dir/keep.txt").exists());
    }

    #[test]
    fn move_across_drives_copies_then_deletes() {
        let src = tmp("xdev"); // /tmp (tmpfs)
        fs::create_dir_all(src.join("pkg/d")).unwrap();
        fs::write(src.join("pkg/d/f.bin"), vec![7u8; 100_000]).unwrap();
        let dst = gtk::glib::home_dir().join(format!(".cache/burrow-test-xdev-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dst);
        fs::create_dir_all(&dst).unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        let done = transfer(&[f(&src.join("pkg"))], &f(&dst), Mode::Move, &p, &c);
        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert_eq!(done.len(), 1);
        assert_eq!(fs::read(dst.join("pkg/d/f.bin")).unwrap().len(), 100_000);
        assert!(!src.join("pkg").exists(), "source kept after cross-drive move");
        assert_eq!(p.bytes_done.load(Ordering::Relaxed), 100_000);
        let _ = fs::remove_dir_all(&dst);
    }
}
