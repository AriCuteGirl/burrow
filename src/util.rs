//! Small pure helpers: natural sort, ~ expansion, Windows-style usage text.

use std::cmp::Ordering;

/// Natural, case-insensitive compare: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "a", "B", "file02", "file2"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "B", "file1", "File2", "file2", "file02", "file10"]);
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
