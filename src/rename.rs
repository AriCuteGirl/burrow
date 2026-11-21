//! Batch rename rules (pure, tested) and the two-phase apply on a worker thread.

use crate::ops::{Progress, checkpoint};
use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    Title,
}

#[derive(Clone, Debug)]
pub struct Rules {
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub match_case: bool,
    /// "{name}", "{n}", "{ext}" are filled in; empty = "{name}"
    pub template: String,
    pub start: u32,
    pub pad: usize,
    pub case: Case,
    /// Rules apply to the name without its extension, which is kept as is.
    pub keep_ext: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            find: String::new(),
            replace: String::new(),
            regex: false,
            match_case: false,
            template: "{name}".into(),
            start: 1,
            pad: 2,
            case: Case::Keep,
            keep_ext: true,
        }
    }
}

/// ("photo.final", "jpg") — dotfiles and names without a dot have no extension.
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            out.extend(c.to_lowercase());
        }
        if c.is_whitespace() || c == '-' || c == '_' {
            start = true;
        }
    }
    out
}

/// New names for `names` (in order). Err = the find pattern is invalid.
pub fn apply(r: &Rules, names: &[String]) -> Result<Vec<String>, String> {
    let re = if r.find.is_empty() {
        None
    } else {
        let pattern = if r.regex { r.find.clone() } else { glib::Regex::escape_string(&r.find).to_string() };
        let flags = if r.match_case { glib::RegexCompileFlags::DEFAULT } else { glib::RegexCompileFlags::CASELESS };
        match glib::Regex::new(&pattern, flags, glib::RegexMatchFlags::DEFAULT) {
            Ok(Some(re)) => Some(re),
            Ok(None) => return Err("invalid pattern".into()),
            Err(e) => return Err(e.message().to_string()),
        }
    };
    let template = if r.template.trim().is_empty() { "{name}" } else { r.template.as_str() };
    let mut out = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let (base, ext) = if r.keep_ext { split_ext(name) } else { (name.as_str(), "") };
        let mut base = base.to_string();
        if let Some(re) = &re {
            let replaced = if r.regex {
                re.replace(&base, 0, &r.replace, glib::RegexMatchFlags::DEFAULT)
            } else {
                re.replace_literal(&base, 0, &r.replace, glib::RegexMatchFlags::DEFAULT)
            };
            base = replaced.map_err(|e| e.message().to_string())?.to_string();
        }
        let n = format!("{:0width$}", r.start as u64 + i as u64, width = r.pad.min(10));
        let mut new = template.replace("{name}", &base).replace("{n}", &n).replace("{ext}", ext);
        new = match r.case {
            Case::Keep => new,
            Case::Lower => new.to_lowercase(),
            Case::Upper => new.to_uppercase(),
            Case::Title => title_case(&new),
        };
        if r.keep_ext && !ext.is_empty() && !template.contains("{ext}") {
            new = format!("{new}.{ext}");
        }
        out.push(new);
    }
    Ok(out)
}

/// Per item: why the new name can't be used (None = fine). `others` = names in the folder
/// that are not being renamed.
pub fn problems(new: &[String], others: &HashSet<String>) -> Vec<Option<&'static str>> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for n in new {
        *count.entry(n.as_str()).or_default() += 1;
    }
    new.iter()
        .map(|n| {
            if n.is_empty() || n == "." || n == ".." {
                Some("empty name")
            } else if n.contains('/') || n.contains('\0') {
                Some("contains /")
            } else if n.len() > 255 {
                Some("too long")
            } else if count[n.as_str()] > 1 {
                Some("same name twice")
            } else if others.contains(n) {
                Some("already exists")
            } else {
                None
            }
        })
        .collect()
}

/// Rename in two phases (all to temp names, then to the final ones) so swaps and chains
/// work and nothing is overwritten. Returns (renamed file, old name) for undo.
pub fn rename_all(items: &[(gio::File, String)], p: &Progress, c: &gio::Cancellable) -> Vec<(gio::File, String)> {
    use std::sync::atomic::Ordering;
    p.total.store(items.len() as u64 * 2, Ordering::Relaxed);
    let mut staged: Vec<(gio::File, String, String)> = vec![]; // (temp file, old name, new name)
    for (i, (f, new)) in items.iter().enumerate() {
        if !checkpoint(p, c) {
            break;
        }
        let old = f.basename().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
        if &old == new {
            p.tick();
            continue;
        }
        p.set_current(&old);
        let tmp = format!(".burrow-rename-{}-{i}", std::process::id());
        match f.set_display_name(&tmp, Some(c)) {
            Ok(t) => staged.push((t, old, new.clone())),
            Err(e) => p.error(&old, &e),
        }
        p.tick();
    }
    let mut done = vec![];
    for (t, old, new) in staged {
        p.set_current(&new);
        // never overwrite: the target must not exist
        let exists = t.parent().is_some_and(|d| d.child(&new).query_exists(gio::Cancellable::NONE));
        let res = if exists { Err(glib::Error::new(gio::IOErrorEnum::Exists, "a file with that name exists")) } else { t.set_display_name(&new, gio::Cancellable::NONE) };
        match res {
            Ok(nf) => done.push((nf, old)),
            Err(e) => {
                p.error(&new, &e);
                let _ = t.set_display_name(&old, gio::Cancellable::NONE); // put it back
            }
        }
        p.tick();
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn find_replace_literal_and_case() {
        let r = Rules { find: "IMG".into(), replace: "Photo".into(), ..Default::default() };
        assert_eq!(apply(&r, &v(&["img_01.JPG", "IMG_02.jpg", "notes.txt"])).unwrap(), v(&["Photo_01.JPG", "Photo_02.jpg", "notes.txt"]));
        let r = Rules { find: "img".into(), replace: "x".into(), match_case: true, ..Default::default() };
        assert_eq!(apply(&r, &v(&["IMG.a", "img.b"])).unwrap(), v(&["IMG.a", "x.b"]));
        // regex chars are literal unless regex is on
        let r = Rules { find: "a.b".into(), replace: "!".into(), ..Default::default() };
        assert_eq!(apply(&r, &v(&["a.b.txt", "axb.txt"])).unwrap(), v(&["!.txt", "axb.txt"]));
    }

    #[test]
    fn regex_groups() {
        let r = Rules { find: r"(\d+)-(\d+)".into(), replace: r"\2-\1".into(), regex: true, ..Default::default() };
        assert_eq!(apply(&r, &v(&["shot 12-34.png"])).unwrap(), v(&["shot 34-12.png"]));
        let bad = Rules { find: "(".into(), regex: true, ..Default::default() };
        assert!(apply(&bad, &v(&["x"])).is_err());
    }

    #[test]
    fn template_numbering_case() {
        let r = Rules { template: "Holiday {n}".into(), start: 7, pad: 3, ..Default::default() };
        assert_eq!(apply(&r, &v(&["a.jpg", "b.JPG", "c"])).unwrap(), v(&["Holiday 007.jpg", "Holiday 008.JPG", "Holiday 009"]));
        let r = Rules { case: Case::Title, ..Default::default() };
        assert_eq!(apply(&r, &v(&["my cool_song.MP3"])).unwrap(), v(&["My Cool_Song.MP3"]));
        let r = Rules { template: "{name}.{ext}.bak".into(), ..Default::default() };
        assert_eq!(apply(&r, &v(&["f.txt"])).unwrap(), v(&["f.txt.bak"]));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
    }

    #[test]
    fn problem_detection() {
        let others: HashSet<String> = ["taken.txt".to_string()].into();
        let p = problems(&v(&["a", "a", "", "x/y", "taken.txt", "ok"]), &others);
        assert_eq!(p, vec![Some("same name twice"), Some("same name twice"), Some("empty name"), Some("contains /"), Some("already exists"), None]);
    }

    #[test]
    fn swap_names_two_phase() {
        let d = std::env::temp_dir().join(format!("burrow-ren-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a"), "A").unwrap();
        std::fs::write(d.join("b"), "B").unwrap();
        let items = vec![(gio::File::for_path(d.join("a")), "b".to_string()), (gio::File::for_path(d.join("b")), "a".to_string())];
        let p = Progress::default();
        let done = rename_all(&items, &p, &gio::Cancellable::new());
        assert_eq!(done.len(), 2, "{:?}", p.errors());
        assert_eq!(std::fs::read_to_string(d.join("a")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(d.join("b")).unwrap(), "A");
        let _ = std::fs::remove_dir_all(&d);
    }
}
