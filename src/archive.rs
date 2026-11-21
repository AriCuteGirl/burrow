//! Archives with the system tools (bsdtar from libarchive, 7z), on a worker thread.
//!
//! - Extract goes into a hidden temp folder next to the destination, then the result is
//!   renamed into place under a free name: nothing is ever overwritten and a cancelled
//!   extract leaves nothing half-done behind.
//! - "Extract here" is smart: one top-level folder in the archive -> that folder;
//!   loose files -> a new folder named after the archive.
//! - Compress writes to a hidden temp name and renames when finished.
//! - bsdtar refuses absolute paths and ".." entries by default (no "zip slip").

use crate::ops::{Progress, free_name};
use gtk::gio;
use gtk::gio::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Extensions we can extract, longest first so ".tar.gz" wins over ".gz".
const EXTRACTABLE: &[&str] = &[
    ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz4", ".tar.lzma", ".tgz", ".txz", ".tbz2", ".tzst", ".tar", ".zip", ".7z", ".rar", ".cbz",
    ".cbr", ".jar", ".apk", ".xpi", ".deb", ".cpio",
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Zip,
    TarGz,
    TarXz,
    TarZst,
    SevenZ,
}

impl Format {
    pub const ALL: [Format; 5] = [Format::Zip, Format::TarGz, Format::TarXz, Format::TarZst, Format::SevenZ];
    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::TarZst => ".tar.zst",
            Format::SevenZ => ".7z",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP (.zip) · works everywhere",
            Format::TarGz => "tar.gz · Linux standard",
            Format::TarXz => "tar.xz · smaller, slower",
            Format::TarZst => "tar.zst · fast and small",
            Format::SevenZ => "7z · smallest",
        }
    }
}

pub fn archive_ext(name: &str) -> Option<&'static str> {
    let l = name.to_lowercase();
    EXTRACTABLE.iter().copied().find(|e| l.ends_with(e) && l.len() > e.len())
}

pub fn is_archive(name: &str) -> bool {
    archive_ext(name).is_some()
}

/// "photos.tar.gz" -> "photos"
pub fn stem(name: &str) -> String {
    match archive_ext(name) {
        Some(e) => name[..name.len() - e.len()].to_string(),
        None => name.to_string(),
    }
}

/// Run a command, polling for cancel (kills it) and pause. Returns stderr on failure.
fn run(mut cmd: Command, p: &Progress, c: &gio::Cancellable) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {:?}: {e}", cmd.get_program()))?;
    // read stderr on a thread so a chatty tool can't fill the pipe and stall
    let mut err = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    loop {
        if c.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        // pausing stops the tool itself (SIGSTOP/SIGCONT through kill(1) keeps us libc-free)
        if p.paused.load(Ordering::Relaxed) {
            let pid = child.id().to_string();
            let _ = Command::new("kill").args(["-STOP", &pid]).status();
            while p.paused.load(Ordering::Relaxed) && !c.is_cancelled() {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = Command::new("kill").args(["-CONT", &pid]).status();
        }
        match child.try_wait() {
            Ok(Some(st)) => {
                let msg = reader.join().unwrap_or_default();
                return if st.success() { Ok(()) } else { Err(msg.trim().lines().last().unwrap_or("failed").to_string()) };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(80)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

fn temp_dir_in(dest: &Path, tag: &str) -> std::io::Result<PathBuf> {
    for n in 0..1000 {
        let d = dest.join(format!(".{tag}.burrow-part-{}-{n}", std::process::id()));
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no temp name free"))
}

fn free_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(free_name(name, |n| dir.join(n).symlink_metadata().is_ok()))
}

/// Extract one archive into `dest`. `smart`: single top folder stays as is, else wrap.
/// Returns the created file/folder.
pub fn extract(archive: &Path, dest: &Path, smart: bool, p: &Progress, c: &gio::Cancellable) -> Result<PathBuf, String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    p.set_current(&name);
    let tmp = temp_dir_in(dest, "extract").map_err(|e| e.to_string())?;
    let cleanup = |t: &Path| {
        let _ = std::fs::remove_dir_all(t);
    };
    let mut cmd = Command::new("bsdtar");
    cmd.arg("-xf").arg(archive).arg("-C").arg(&tmp);
    let mut res = run(cmd, p, c);
    // bsdtar can't do every 7z/rar variant: try 7z for those
    if res.is_err() && !c.is_cancelled() && have("7z") {
        cleanup(&tmp);
        std::fs::create_dir(&tmp).map_err(|e| e.to_string())?;
        let mut cmd = Command::new("7z");
        cmd.arg("x").arg("-y").arg("-bd").arg(format!("-o{}", tmp.display())).arg("--").arg(archive);
        if let Ok(()) = run(cmd, p, c) {
            res = Ok(());
        }
    }
    if let Err(e) = res {
        cleanup(&tmp);
        return Err(e);
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(&tmp).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    let out = if smart && entries.len() == 1 {
        // one thing inside: move it out under its own (free) name
        let only = &entries[0];
        let n = only.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let target = free_in(dest, &n);
        std::fs::rename(only, &target).map_err(|e| e.to_string())?;
        cleanup(&tmp);
        target
    } else {
        let target = free_in(dest, &stem(&name));
        std::fs::rename(&tmp, &target).map_err(|e| {
            cleanup(&tmp);
            e.to_string()
        })?;
        target
    };
    p.tick();
    Ok(out)
}

/// Pack `files` (all in the same folder) into `<dir>/<name><ext>`. Returns the archive path.
pub fn compress(files: &[PathBuf], name: &str, fmt: Format, p: &Progress, c: &gio::Cancellable) -> Result<PathBuf, String> {
    let dir = files.first().and_then(|f| f.parent()).ok_or("nothing to compress")?.to_path_buf();
    if files.iter().any(|f| f.parent() != Some(dir.as_path())) {
        return Err("all items must be in the same folder".into());
    }
    let names: Vec<String> = files.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().to_string())).collect();
    let tmp = dir.join(format!(".{name}.burrow-part-{}{}", std::process::id(), fmt.ext()));
    p.set_current(&format!("{name}{}", fmt.ext()));
    let cmd = if fmt == Format::SevenZ {
        if !have("7z") {
            return Err("7z isn't installed".into());
        }
        let mut c7 = Command::new("7z");
        c7.current_dir(&dir).arg("a").arg("-bd").arg("-y").arg(&tmp).arg("--").args(&names);
        c7
    } else {
        // -a picks the format from the file extension
        let mut bt = Command::new("bsdtar");
        bt.arg("-a").arg("-cf").arg(&tmp).arg("-C").arg(&dir).arg("--").args(&names);
        bt
    };
    if let Err(e) = run(cmd, p, c) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let target = free_in(&dir, &format!("{name}{}", fmt.ext()));
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    p.tick();
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("burrow-arc-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names() {
        assert!(is_archive("a.tar.gz") && is_archive("B.ZIP") && is_archive("x.7z"));
        assert!(!is_archive("notes.txt") && !is_archive(".zip"));
        assert_eq!(stem("photos.tar.gz"), "photos");
        assert_eq!(stem("game.v1.2.zip"), "game.v1.2");
    }

    #[test]
    fn compress_then_smart_extract() {
        if !have("bsdtar") {
            return;
        }
        let d = tmp("roundtrip");
        fs::create_dir_all(d.join("proj/src")).unwrap();
        fs::write(d.join("proj/src/main.rs"), "fn main(){}").unwrap();
        fs::write(d.join("a.txt"), "A").unwrap();
        fs::write(d.join("b.txt"), "B").unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        for fmt in [Format::Zip, Format::TarGz, Format::TarZst] {
            // one folder -> extracts as that folder (next to the original: gets a free name)
            let arc = compress(&[d.join("proj")], "proj", fmt, &p, &c).unwrap();
            let out = extract(&arc, &d, true, &p, &c).unwrap();
            assert_eq!(out.file_name().unwrap(), "proj (2)", "{fmt:?}");
            assert_eq!(fs::read_to_string(out.join("src/main.rs")).unwrap(), "fn main(){}");
            fs::remove_dir_all(&out).unwrap();
            fs::remove_file(&arc).unwrap();
            // loose files -> wrapped in a folder named after the archive
            let arc = compress(&[d.join("a.txt"), d.join("b.txt")], "loose", fmt, &p, &c).unwrap();
            let out = extract(&arc, &d, true, &p, &c).unwrap();
            assert_eq!(out.file_name().unwrap(), "loose");
            assert!(out.join("a.txt").exists() && out.join("b.txt").exists());
            fs::remove_dir_all(&out).unwrap();
            fs::remove_file(&arc).unwrap();
        }
        // no temp leftovers
        assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("burrow-part")));
    }

    #[test]
    fn broken_archive_leaves_nothing() {
        if !have("bsdtar") {
            return;
        }
        let d = tmp("broken");
        fs::write(d.join("bad.zip"), "this is not a zip").unwrap();
        let p = Progress::default();
        let c = gio::Cancellable::new();
        assert!(extract(&d.join("bad.zip"), &d, true, &p, &c).is_err());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
    }
}
