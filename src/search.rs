//! Recursive search by name on a worker thread. Results stream into `Shared`; the UI drains
//! it, so typing and scrolling stay smooth however big the tree is.

use gtk::gio;
use gtk::gio::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAX_RESULTS: usize = 5_000;

#[derive(Default)]
pub struct Shared {
    pub found: Mutex<Vec<gio::File>>,
    pub scanned: AtomicU64,
    pub done: AtomicBool,
    pub truncated: AtomicBool,
}

/// "report" = anywhere in the name; "*.pdf" or "img_??.png" = the whole name.
pub struct Query {
    needle: Vec<char>,
    glob: bool,
}

impl Query {
    pub fn new(text: &str) -> Query {
        let t = text.trim().to_lowercase();
        Query { glob: t.contains(['*', '?']), needle: t.chars().collect() }
    }
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }
    pub fn matches(&self, name: &str) -> bool {
        let low = name.to_lowercase();
        if self.glob {
            let n: Vec<char> = low.chars().collect();
            glob(&self.needle, &n)
        } else {
            let needle: String = self.needle.iter().collect();
            low.contains(&needle)
        }
    }
}

/// * and ? only, linear time (no exponential backtracking on "*a*a*a*…").
fn glob(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) && p[pi] != '*' {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            back = Some((pi, si));
            pi += 1;
        } else if let Some((bp, bs)) = back {
            pi = bp + 1;
            si = bs + 1;
            back = Some((bp, bs + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Not worth walking: kernel views, and our own half-written files.
fn skip_dir(p: &Path) -> bool {
    ["/proc", "/sys", "/dev", "/run"].iter().any(|x| p == Path::new(x))
}

pub fn run(root: &gio::File, q: &Query, hidden: bool, out: &Shared, c: &gio::Cancellable) {
    match root.path().filter(|_| root.is_native()) {
        Some(p) => local(p, q, hidden, out, c),
        None => remote(root, q, hidden, out, c),
    }
    out.done.store(true, Ordering::Release);
}

/// False = enough results.
fn push(out: &Shared, total: &mut usize, f: gio::File) -> bool {
    if *total >= MAX_RESULTS {
        out.truncated.store(true, Ordering::Relaxed);
        return false;
    }
    *total += 1;
    if let Ok(mut v) = out.found.lock() {
        v.push(f);
    }
    true
}

fn local(root: PathBuf, q: &Query, hidden: bool, out: &Shared, c: &gio::Cancellable) {
    let mut total = 0;
    // breadth first: nearby matches show up first
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(dir) = queue.pop_front() {
        // unreadable folders are simply skipped
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if c.is_cancelled() {
                return;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if (!hidden && name.starts_with('.')) || name.contains(".burrow-part-") {
                continue;
            }
            out.scanned.fetch_add(1, Ordering::Relaxed);
            if q.matches(&name) && !push(out, &mut total, gio::File::for_path(e.path())) {
                return;
            }
            // file_type() doesn't follow symlinks, so link loops can't trap us
            if e.file_type().is_ok_and(|t| t.is_dir()) && !skip_dir(&e.path()) {
                queue.push_back(e.path());
            }
        }
    }
}

fn remote(root: &gio::File, q: &Query, hidden: bool, out: &Shared, c: &gio::Cancellable) {
    let mut total = 0;
    let mut queue = std::collections::VecDeque::from([root.clone()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(en) = dir.enumerate_children("standard::name,standard::type", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, Some(c)) else {
            if c.is_cancelled() {
                return;
            }
            continue;
        };
        while let Ok(Some(i)) = en.next_file(Some(c)) {
            let name = i.name();
            let text = name.to_string_lossy();
            if !hidden && text.starts_with('.') {
                continue;
            }
            out.scanned.fetch_add(1, Ordering::Relaxed);
            if q.matches(&text) && !push(out, &mut total, dir.child(&name)) {
                return;
            }
            if i.file_type() == gio::FileType::Directory {
                queue.push_back(dir.child(&name));
            }
        }
        if c.is_cancelled() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn matching() {
        let m = |q: &str, n: &str| Query::new(q).matches(n);
        assert!(m("rep", "My Report.pdf") && m("*.PDF", "a.pdf") && m("img_??.png", "IMG_01.png"));
        assert!(!m("*.pdf", "a.pdf.bak") && !m("img_?.png", "img_01.png") && !m("xyz", "abc"));
        assert!(m("č", "Hudba Č.1") && m("*", "anything"));
        // would take forever with naive backtracking
        assert!(!m(&"*a".repeat(40), &"a".repeat(39)));
    }

    #[test]
    fn walks_without_following_links_or_hidden() {
        let d = std::env::temp_dir().join(format!("burrow-search-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::create_dir_all(d.join(".secret")).unwrap();
        for f in ["hit1.txt", "a/hit2.txt", "a/b/hit3.txt", ".secret/hit4.txt", "a/miss.md"] {
            fs::write(d.join(f), "").unwrap();
        }
        std::os::unix::fs::symlink(&d, d.join("a/loop")).unwrap();
        let go = |hidden| {
            let s = Shared::default();
            run(&gio::File::for_path(&d), &Query::new("hit*.txt"), hidden, &s, &gio::Cancellable::new());
            assert!(s.done.load(Ordering::Relaxed));
            s.found.lock().unwrap().len()
        };
        assert_eq!((go(false), go(true)), (3, 4));
        let c = gio::Cancellable::new();
        c.cancel();
        let s = Shared::default();
        run(&gio::File::for_path(&d), &Query::new("hit"), true, &s, &c);
        assert!(s.found.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(d);
    }
}
