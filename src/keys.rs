//! Every shortcut with its default, the user's overrides from `[keys]`, and the optional
//! vim-style keys for the file view.

use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scope {
    /// Works anywhere in the window
    Window,
    /// Only while the file view has the focus, so text fields keep their own keys
    View,
}

pub struct Key {
    /// Window action it triggers ("win.<id>")
    pub id: &'static str,
    pub label: &'static str,
    pub defaults: &'static [&'static str],
    pub scope: Scope,
}

const fn k(id: &'static str, label: &'static str, defaults: &'static [&'static str], scope: Scope) -> Key {
    Key { id, label, defaults, scope }
}

use Scope::{View, Window};
pub const KEYS: &[Key] = &[
    k("back", "Back", &["<Alt>Left", "<Alt>KP_Left", "Back"], Window),
    k("forward", "Forward", &["<Alt>Right", "<Alt>KP_Right", "Forward"], Window),
    k("up", "Parent folder", &["<Alt>Up"], Window),
    k("parent", "Parent folder (in the file view)", &["BackSpace"], View),
    k("home", "Home", &["<Alt>Home"], Window),
    k("drives", "Drives page", &["<Ctrl>d"], Window),
    k("network", "Network page", &["<Ctrl><Shift>d"], Window),
    k("connect", "Connect to server", &["<Ctrl><Shift>k"], Window),
    k("location", "Type a path", &["<Ctrl>l", "F6"], Window),
    k("filter", "Filter this folder", &["<Ctrl>f"], Window),
    k("search", "Search in subfolders", &["<Ctrl><Shift>f"], Window),
    k("reload", "Reload", &["F5", "<Ctrl>r"], Window),
    k("hidden", "Show hidden files", &["<Ctrl>h"], Window),
    k("mode-grid", "Grid view", &["<Ctrl>1"], Window),
    k("mode-compact", "Compact view", &["<Ctrl>2"], Window),
    k("mode-list", "List view", &["<Ctrl>3"], Window),
    k("preview", "Preview pane", &["F4"], Window),
    k("quicklook", "Quick look", &["space"], View),
    k("new-tab", "New tab", &["<Ctrl>t"], Window),
    k("close-tab", "Close tab", &["<Ctrl>w"], Window),
    k("next-tab", "Next tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"], Window),
    k("prev-tab", "Previous tab", &["<Ctrl><Shift>Tab", "<Ctrl><Shift>ISO_Left_Tab", "<Ctrl>Page_Up"], Window),
    k("split", "Split view", &["F3"], Window),
    k("other-pane", "Switch pane", &["<Ctrl>grave"], Window),
    k("copy", "Copy", &["<Ctrl>c"], View),
    k("cut", "Cut", &["<Ctrl>x"], View),
    k("paste", "Paste", &["<Ctrl>v"], View),
    k("select-all", "Select all", &["<Ctrl>a"], View),
    k("rename", "Rename", &["F2"], View),
    k("trash", "Move to Trash", &["Delete", "KP_Delete"], View),
    k("delete", "Delete for good", &["<Shift>Delete", "<Shift>KP_Delete"], View),
    k("undo", "Undo", &["<Ctrl>z"], Window),
    k("new-folder", "New folder", &["<Ctrl><Shift>n"], Window),
    k("terminal-here", "Open terminal here", &["<Ctrl><Alt>t"], Window),
    k("properties", "Properties", &["<Alt>Return"], Window),
    k("menu", "Context menu", &["Menu", "<Shift>F10"], View),
    k("settings", "Settings", &["<Ctrl>comma"], Window),
    k("quit", "Close window", &["<Ctrl>q"], Window),
];

pub type Overrides = BTreeMap<String, Vec<String>>;

pub fn accels(over: &Overrides, key: &Key) -> Vec<String> {
    over.get(key.id).cloned().unwrap_or_else(|| key.defaults.iter().map(|s| s.to_string()).collect())
}

/// Drop unknown ids and things GTK can't read as a shortcut.
pub fn sanitize(over: &mut Overrides, valid: impl Fn(&str) -> bool) {
    over.retain(|id, _| KEYS.iter().any(|k| k.id == id));
    for v in over.values_mut() {
        v.retain(|a| valid(a));
    }
}

/// Which other shortcut already uses this one (compared by `norm`).
pub fn clash(over: &Overrides, id: &str, accel: &str, norm: impl Fn(&str) -> String) -> Option<&'static Key> {
    let want = norm(accel);
    KEYS.iter().find(|k| k.id != id && accels(over, k).iter().any(|a| norm(a) == want))
}

// ---------------------------------------------------------------- vim

#[derive(Debug, PartialEq, Clone)]
pub enum Cmd {
    /// Key used, nothing to do yet (first half of "gg", "dd", a count…)
    Wait,
    /// Columns right, rows down
    Move(i32, i32),
    Top,
    Bottom,
    Act(&'static str),
    /// Not a vim key
    None,
}

#[derive(Default)]
pub struct Vim {
    pending: Option<char>,
    count: u32,
}

impl Vim {
    pub fn reset(&mut self) {
        *self = Vim::default();
    }

    /// `list`: single column, where h/l go to the parent folder / open instead of sideways.
    pub fn feed(&mut self, c: char, list: bool) -> Cmd {
        if let Some(d) = c.to_digit(10).filter(|d| self.pending.is_none() && (*d > 0 || self.count > 0)) {
            self.count = (self.count * 10 + d).min(9999);
            return Cmd::Wait;
        }
        let n = self.count.max(1) as i32;
        let first = self.pending.take();
        self.count = 0;
        match (first, c) {
            (Some('g'), 'g') => Cmd::Top,
            (Some('y'), 'y') => Cmd::Act("copy"),
            (Some('d'), 'd') => Cmd::Act("cut"),
            (Some(_), _) => Cmd::Wait, // unknown pair: swallow it
            (None, 'g' | 'y' | 'd') => {
                self.pending = Some(c);
                Cmd::Wait
            }
            (None, 'j') => Cmd::Move(0, n),
            (None, 'k') => Cmd::Move(0, -n),
            (None, 'h') if list => Cmd::Act("up"),
            (None, 'l') if list => Cmd::Act("open-selected"),
            (None, 'h') => Cmd::Move(-n, 0),
            (None, 'l') => Cmd::Move(n, 0),
            (None, 'G') => Cmd::Bottom,
            (None, 'H') => Cmd::Act("back"),
            (None, 'L') => Cmd::Act("forward"),
            (None, '-') => Cmd::Act("up"),
            (None, 'o') => Cmd::Act("open-selected"),
            (None, 'p') => Cmd::Act("paste"),
            (None, 'D') => Cmd::Act("trash"),
            (None, 'r') => Cmd::Act("rename"),
            (None, 'u') => Cmd::Act("undo"),
            (None, 'V') => Cmd::Act("select-all"),
            (None, '/') => Cmd::Act("filter"),
            (None, '.') => Cmd::Act("hidden"),
            _ => Cmd::None,
        }
    }
}

/// New cursor position in a view of `n` items, `cols` per row.
pub fn step(pos: Option<u32>, n: u32, cols: u32, dx: i32, dy: i32) -> Option<u32> {
    if n == 0 {
        return None;
    }
    let Some(p) = pos else { return Some(0) };
    let to = p as i64 + dx as i64 + dy as i64 * cols.max(1) as i64;
    Some(to.clamp(0, n as i64 - 1) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_dont_clash() {
        let o = Overrides::new();
        for key in KEYS {
            for a in key.defaults {
                assert!(clash(&o, key.id, a, |s| s.to_lowercase()).is_none(), "{} {a}", key.id);
            }
        }
        let mut ids: Vec<_> = KEYS.iter().map(|k| k.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), KEYS.len());
    }

    #[test]
    fn overrides() {
        let mut o: Overrides = [("copy".to_string(), vec!["<Ctrl>Insert".to_string(), "junk!!".into()]), ("nope".into(), vec!["F1".into()]), ("split".into(), vec![])].into();
        sanitize(&mut o, |a| !a.contains('!'));
        let find = |id: &str| KEYS.iter().find(|k| k.id == id).unwrap();
        assert_eq!(accels(&o, find("copy")), ["<Ctrl>Insert"]);
        assert!(accels(&o, find("split")).is_empty()); // turned off on purpose
        assert_eq!(accels(&o, find("paste")), ["<Ctrl>v"]);
        assert!(!o.contains_key("nope"));
        assert_eq!(clash(&o, "paste", "<Ctrl>INSERT", |s| s.to_lowercase()).map(|k| k.id), Some("copy"));
    }

    #[test]
    fn vim_keys() {
        let mut v = Vim::default();
        let mut t = |s: &str| s.chars().map(|c| v.feed(c, false)).last().unwrap();
        assert_eq!(t("gg"), Cmd::Top);
        assert_eq!(t("12j"), Cmd::Move(0, 12));
        assert_eq!(t("j"), Cmd::Move(0, 1)); // the count is used up
        assert_eq!(t("3h"), Cmd::Move(-3, 0));
        assert_eq!(t("yy"), Cmd::Act("copy"));
        assert_eq!(t("dx"), Cmd::Wait); // never half-executes
        assert_eq!(t("10G"), Cmd::Bottom);
        assert_eq!(t("z"), Cmd::None);
        assert_eq!(Vim::default().feed('h', true), Cmd::Act("up"));
    }

    #[test]
    fn stepping() {
        assert_eq!(step(None, 10, 4, 1, 0), Some(0));
        assert_eq!(step(Some(1), 10, 4, 0, 1), Some(5));
        assert_eq!(step(Some(8), 10, 4, 0, 1), Some(9));
        assert_eq!(step(Some(1), 10, 4, 0, -1), Some(0));
        assert_eq!(step(Some(0), 0, 4, 1, 0), None);
    }
}
