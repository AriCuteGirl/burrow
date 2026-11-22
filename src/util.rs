//! Small pure helpers: natural sort, ~ expansion, Windows-style usage text.

use std::cmp::Ordering;

/// Natural, case-insensitive compare: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return natural_ascii(a.as_bytes(), b.as_bytes());
    }
    natural_unicode(a, b)
}

/// The same order without any allocation (most names): sorting a big folder calls this a lot.
fn natural_ascii(a: &[u8], b: &[u8]) -> Ordering {
    let digits = |s: &[u8], i: usize| s[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (na, nb) = (&a[i..i + digits(a, i)], &b[j..j + digits(b, j)]);
            let cut = |s: &[u8]| s.iter().take_while(|c| **c == b'0').count();
            let (ta, tb) = (&na[cut(na)..], &nb[cut(nb)..]);
            let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
            if o != Ordering::Equal {
                return o;
            }
            i += na.len();
            j += nb.len();
        } else {
            let (x, y) = (a[i].to_ascii_lowercase(), b[j].to_ascii_lowercase());
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

fn natural_unicode(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b), // equal ignoring case: stable tie-break
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| ai.next_if(|c| c.is_ascii_digit())).collect();
                let nb: String = std::iter::from_fn(|| bi.next_if(|c| c.is_ascii_digit())).collect();
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_lowercase().next().unwrap_or(x), y.to_lowercase().next().unwrap_or(y));
                if lx != ly {
                    return lx.cmp(&ly);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// "~" and "~/x" -> home-relative. Anything else is returned unchanged.
pub fn expand_tilde(p: &str, home: &str) -> String {
    if p == "~" {
        home.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else {
        p.to_string()
    }
}

/// Inverse of expand_tilde, for writing paths into the config.
pub fn contract_tilde(p: &str, home: &str) -> String {
    let h = home.trim_end_matches('/');
    if !h.is_empty() && p == h {
        "~".into()
    } else if let Some(rest) = p.strip_prefix(&format!("{h}/")).filter(|_| !h.is_empty()) {
        format!("~/{rest}")
    } else {
        p.to_string()
    }
}

/// Bytes with SI units, like file managers and Windows' drive view ("118 GB").
pub fn human_size(b: u64) -> String {
    const U: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Fraction used and the "X free of Y" line. `full` = past the warning threshold (90%).
#[derive(Debug, PartialEq)]
pub struct Usage {
    pub fraction: f64,
    pub text: String,
    pub full: bool,
}

pub fn usage(size: u64, free: u64) -> Option<Usage> {
    if size == 0 {
        return None;
    }
    let free = free.min(size);
    let fraction = (size - free) as f64 / size as f64;
    Some(Usage {
        fraction,
        text: format!("{} free of {}", human_size(free), human_size(size)),
        full: fraction >= 0.9,
    })
}

/// "rwxr-xr-x" for the low 9 permission bits.
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// "Allow executing": add x wherever r is set (like chmod +x for readable classes).
pub fn with_exec(mode: u32, on: bool) -> u32 {
    let r = mode & 0o444;
    if on { mode | (r >> 2) } else { mode & !0o111 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o100640), "rw-r-----");
        assert_eq!(with_exec(0o644, true), 0o755);
        assert_eq!(with_exec(0o640, true), 0o750);
        assert_eq!(with_exec(0o755, false), 0o644);
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "a", "B", "file02", "file2"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "B", "file1", "File2", "file2", "file02", "file10"]);
        // the fast path gives exactly the same order
        let names = ["", "0", "00", "a1b", "a01b", "a1B", "A1b", "x10y2", "x10y10", "x9", "_", "-", "file", "File", "1a", "01a"];
        for a in names {
            for b in names {
                assert_eq!(natural_ascii(a.as_bytes(), b.as_bytes()), natural_unicode(a, b), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn tilde() {
        assert_eq!(expand_tilde("~/Games", "/home/a"), "/home/a/Games");
        assert_eq!(expand_tilde("~", "/home/a"), "/home/a");
        assert_eq!(expand_tilde("~x", "/home/a"), "~x");
        assert_eq!(contract_tilde("/home/a/Games", "/home/a"), "~/Games");
        assert_eq!(contract_tilde("/home/ab", "/home/a"), "/home/ab");
        assert_eq!(contract_tilde("smb://h/s", "/home/a"), "smb://h/s");
    }

    #[test]
    fn usage_text() {
        let u = usage(500_000_000_000, 120_400_000_000).unwrap();
        assert_eq!(u.text, "120 GB free of 500 GB");
        assert!(!u.full);
        assert!((u.fraction - 0.7592).abs() < 1e-3);
        assert!(usage(1000, 50).unwrap().full);
        assert!(usage(0, 0).is_none());
        assert_eq!(usage(100, 500).unwrap().fraction, 0.0);
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_500_000), "1.5 MB");
    }
}
