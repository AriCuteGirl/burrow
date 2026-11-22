//! Sharing a folder on the LAN through Samba user shares (`net usershare`). No root needed
//! once the system allows it; Burrow never changes the Samba configuration itself.
//!
//! Safe defaults: login required, read-only, no guests. Folders that would expose secrets
//! or the system are refused outright.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Ready,
    /// `net` isn't installed
    NoSamba,
    /// Samba is there, but user shares are turned off or we may not use them
    Disabled(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Share {
    pub name: String,
    pub path: String,
    pub comment: String,
    pub writable: bool,
    pub guests: bool,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Options {
    /// Others may change files (default: read-only)
    pub writable: bool,
    /// Anyone on the network, without a login (default: no)
    pub guests: bool,
}

fn net(args: &[&str]) -> Result<String, String> {
    let o = Command::new("net").arg("usershare").args(args).env("LC_ALL", "C").stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    // `net` reports some failures on stderr with exit code 0
    let bad = !o.status.success() || err.contains("usershares are currently disabled") || err.contains("cannot open usershare directory");
    if bad { Err(err.trim().lines().last().unwrap_or("failed").trim_start_matches("net usershare: ").to_string()) } else { Ok(out) }
}

pub fn status() -> Status {
    let found = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("net").is_file()));
    if !found {
        return Status::NoSamba;
    }
    match net(&["info"]) {
        Ok(_) => Status::Ready,
        Err(e) => Status::Disabled(e),
    }
}

/// `net usershare info`: ini-like blocks.
pub fn parse_info(text: &str) -> Vec<Share> {
    let mut out: Vec<Share> = vec![];
    for l in text.lines().map(str::trim) {
        if let Some(n) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            out.push(Share { name: n.to_string(), path: String::new(), comment: String::new(), writable: false, guests: false });
        } else if let (Some(s), Some((k, v))) = (out.last_mut(), l.split_once('=')) {
            match k {
                "path" => s.path = v.to_string(),
                "comment" => s.comment = v.to_string(),
                "usershare_acl" => s.writable = v.split(',').any(|a| a.trim().ends_with(":F") || a.trim().ends_with(":f")),
                "guest_ok" => s.guests = v.eq_ignore_ascii_case("y"),
                _ => {}
            }
        }
    }
    out
}

pub fn list() -> Vec<Share> {
    net(&["info"]).map(|t| parse_info(&t)).unwrap_or_default()
}

/// Share names: what Windows and Samba both accept.
pub fn check_name(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        Some("Give the share a name")
    } else if n.chars().count() > 60 {
        Some("That name is too long")
    } else if n.starts_with('-') || n.starts_with('.') {
        Some("The name can't start with “-” or “.”")
    } else if n.chars().any(|c| c.is_control() || "%<>*?|/\\+=;:\",[]$".contains(c)) {
        Some("The name can't contain % < > * ? | / \\ + = ; : \" , [ ] $")
    } else if ["global", "homes", "printers", "ipc", "admin"].contains(&n.to_lowercase().as_str()) {
        Some("That name is reserved by Samba")
    } else {
        None
    }
}

/// How risky it is to share this folder.
#[derive(Clone, Debug, PartialEq)]
pub enum Risk {
    Fine,
    /// Allowed, but ask twice with this reason
    Warn(String),
    /// Refused
    Refuse(String),
}

/// `path` must already be canonical (no symlinks, no "..").
pub fn risk(path: &Path, home: &Path) -> Risk {
    let p = path.to_string_lossy();
    if path == Path::new("/") {
        return Risk::Refuse("That's the whole computer.".into());
    }
    for sys in ["/etc", "/root", "/boot", "/proc", "/sys", "/dev", "/run", "/var", "/usr", "/bin", "/sbin", "/lib", "/opt"] {
        // removable drives live under /run/media
        if path.starts_with(sys) && !path.starts_with("/run/media") {
            return Risk::Refuse(format!("{p} is part of the system."));
        }
    }
    if home.starts_with(path) && path != home {
        return Risk::Refuse("That folder contains every user's home folder.".into());
    }
    if let Ok(rel) = path.strip_prefix(home) {
        if rel.as_os_str().is_empty() {
            return Risk::Warn("This is your whole home folder, with your passwords, SSH keys and browser data in hidden folders.".into());
        }
        let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().to_string()).unwrap_or_default();
        if [".ssh", ".gnupg", ".password-store", ".pki", ".aws", ".kube"].contains(&first.as_str()) || rel.starts_with(".local/share/keyrings") {
            return Risk::Refuse("That folder holds keys or passwords.".into());
        }
        if first.starts_with('.') {
            return Risk::Warn("That's a hidden folder; apps keep settings and logins in those.".into());
        }
    }
    Risk::Fine
}

/// Arguments for `net usershare add`. Login-only shares name just this user.
pub fn add_args(name: &str, path: &str, comment: &str, user: &str, o: Options) -> Vec<String> {
    let right = if o.writable { "F" } else { "R" };
    let acl = if o.guests { format!("Everyone:{right}") } else { format!("{user}:{right}") };
    let comment: String = comment.chars().filter(|c| !c.is_control()).take(120).collect();
    vec!["add".into(), name.trim().into(), path.into(), comment, acl, format!("guest_ok={}", if o.guests { "y" } else { "n" })]
}

pub fn add(name: &str, path: &Path, comment: &str, o: Options) -> Result<(), String> {
    if let Some(e) = check_name(name) {
        return Err(e.into());
    }
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.is_dir() {
        return Err("Only folders can be shared".into());
    }
    if let Risk::Refuse(why) = risk(&path, &gtk::glib::home_dir()) {
        return Err(why);
    }
    if list().iter().any(|s| s.name.eq_ignore_ascii_case(name.trim())) {
        return Err(format!("There's already a share called “{}”", name.trim()));
    }
    let args = add_args(name, &path.to_string_lossy(), comment, &gtk::glib::user_name().to_string_lossy(), o);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    net(&refs).map(|_| ())
}

pub fn remove(name: &str) -> Result<(), String> {
    // only ever a name Samba itself listed
    if !list().iter().any(|s| s.name == name) {
        return Err("That share doesn't exist any more".into());
    }
    net(&["delete", name]).map(|_| ())
}

/// Restrictions in smb.conf that decide who can reach a share at all.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reach {
    pub interfaces: Option<String>,
    pub hosts_allow: Option<String>,
    pub guests_blocked: bool,
}

pub fn parse_reach(conf: &str) -> Reach {
    let mut section = String::new();
    let mut g: BTreeMap<String, String> = BTreeMap::new();
    for l in conf.lines().map(str::trim) {
        if l.is_empty() || l.starts_with(['#', ';']) {
            continue;
        }
        if let Some(n) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = n.trim().to_lowercase();
        } else if section == "global" {
            if let Some((k, v)) = l.split_once('=') {
                g.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    let yes = |k: &str| g.get(k).is_some_and(|v| ["yes", "true", "1"].contains(&v.to_lowercase().as_str()));
    Reach {
        interfaces: if yes("bind interfaces only") { g.get("interfaces").cloned() } else { None },
        hosts_allow: g.get("hosts allow").or_else(|| g.get("allow hosts")).cloned(),
        guests_blocked: !g.get("map to guest").is_some_and(|v| v.to_lowercase().starts_with("bad")),
    }
}

pub fn reach() -> Reach {
    std::fs::read_to_string("/etc/samba/smb.conf").map(|s| parse_reach(&s)).unwrap_or_default()
}

impl Reach {
    /// One sentence for the share dialog, or None when nothing restricts it.
    pub fn explain(&self) -> Option<String> {
        let mut parts = vec![];
        if let Some(i) = &self.interfaces {
            parts.push(format!("Samba only listens on {i}"));
        }
        if let Some(h) = &self.hosts_allow {
            parts.push(format!("only these addresses may connect: {h}"));
        }
        (!parts.is_empty()).then(|| format!("Your smb.conf limits who can reach shares: {}. Other devices won't see this folder.", parts.join(", and ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_blocks() {
        let t = "[Music]\npath=/home/a/Music\ncomment=tunes\nusershare_acl=Everyone:R,\nguest_ok=y\n\n[Work]\npath=/home/a/w=x\ncomment=\nusershare_acl=HOST\\a:F,\nguest_ok=n\n";
        let s = parse_info(t);
        assert_eq!(s.len(), 2);
        assert!(s[0].guests && !s[0].writable);
        assert!(s[1].writable && !s[1].guests);
        assert_eq!(s[1].path, "/home/a/w=x");
        assert!(parse_info("garbage\npath=/x\n").is_empty());
    }

    #[test]
    fn names() {
        for ok in ["Music", "My photos", "Hudba č. 1"] {
            assert_eq!(check_name(ok), None, "{ok}");
        }
        for bad in ["", "-x", "a/b", "a%b", "homes", "IPC", "x\ny", "a$", ".h"] {
            assert!(check_name(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn risky_folders() {
        let h = Path::new("/home/ari");
        for p in ["/", "/etc", "/etc/ssh", "/home", "/home/ari/.ssh", "/home/ari/.gnupg/x", "/home/ari/.local/share/keyrings", "/var/lib", "/run/user/1000"] {
            assert!(matches!(risk(Path::new(p), h), Risk::Refuse(_)), "{p}");
        }
        for p in ["/home/ari", "/home/ari/.config"] {
            assert!(matches!(risk(Path::new(p), h), Risk::Warn(_)), "{p}");
        }
        for p in ["/home/ari/Hudba", "/mnt/Games", "/run/media/ari/USB"] {
            assert_eq!(risk(Path::new(p), h), Risk::Fine, "{p}");
        }
    }

    #[test]
    fn defaults_are_safe() {
        let a = add_args("Music", "/home/a/Music", "x\ny", "ari", Options::default());
        assert_eq!(a, ["add", "Music", "/home/a/Music", "xy", "ari:R", "guest_ok=n"]);
        let a = add_args("M", "/m", "", "ari", Options { writable: true, guests: true });
        assert_eq!(a[4..], ["Everyone:F".to_string(), "guest_ok=y".to_string()]);
    }

    #[test]
    fn reach_of_a_locked_down_samba() {
        let r = parse_reach("[global]\n  interfaces = lo virbr0\n  bind interfaces only = yes\n hosts allow = 127. 192.168.122.\n map to guest = never\n[Games]\n hosts allow = 1.2.3.4\n");
        assert_eq!(r.interfaces.as_deref(), Some("lo virbr0"));
        assert_eq!(r.hosts_allow.as_deref(), Some("127. 192.168.122."));
        assert!(r.guests_blocked && r.explain().unwrap().contains("virbr0"));
        let open = parse_reach("[global]\n interfaces = eth0\n map to guest = Bad User\n");
        assert_eq!(open, Reach { interfaces: None, hosts_allow: None, guests_blocked: false });
        assert!(open.explain().is_none());
    }
}
