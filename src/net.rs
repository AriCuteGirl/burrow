//! Network addresses and LAN discovery. Passwords never end up in an address: they are
//! stripped from anything typed, pasted or read from the config (the keyring holds them).

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Proto {
    Smb,
    Sftp,
    Ftp,
    Ftps,
    Dav,
    Davs,
    Nfs,
}

impl Proto {
    pub const ALL: [Proto; 7] = [Proto::Smb, Proto::Sftp, Proto::Ftp, Proto::Ftps, Proto::Dav, Proto::Davs, Proto::Nfs];

    pub fn scheme(self) -> &'static str {
        match self {
            Proto::Smb => "smb",
            Proto::Sftp => "sftp",
            Proto::Ftp => "ftp",
            Proto::Ftps => "ftps",
            Proto::Dav => "dav",
            Proto::Davs => "davs",
            Proto::Nfs => "nfs",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Proto::Smb => "Windows / Samba share (smb)",
            Proto::Sftp => "SSH (sftp)",
            Proto::Ftp => "FTP, not encrypted (ftp)",
            Proto::Ftps => "FTP over TLS (ftps)",
            Proto::Dav => "WebDAV, not encrypted (dav)",
            Proto::Davs => "WebDAV over HTTPS (davs)",
            Proto::Nfs => "NFS",
        }
    }
    /// Sends the password and files in plain text.
    pub fn insecure(self) -> bool {
        matches!(self, Proto::Ftp | Proto::Dav)
    }
    pub fn from_scheme(s: &str) -> Option<Proto> {
        Some(match s.to_ascii_lowercase().as_str() {
            "smb" | "cifs" => Proto::Smb,
            "sftp" | "ssh" => Proto::Sftp,
            "ftp" => Proto::Ftp,
            "ftps" => Proto::Ftps,
            "dav" | "webdav" => Proto::Dav,
            "davs" | "webdavs" => Proto::Davs,
            "nfs" => Proto::Nfs,
            _ => return None,
        })
    }
    /// Does the installed GVFS know this protocol?
    pub fn supported(self) -> bool {
        gio::Vfs::default().supported_uri_schemes().iter().any(|s| s == self.scheme())
    }
}

/// Host names, IPv4, or IPv6 (with or without brackets). Returns it ready for a URI.
pub fn clean_host(h: &str) -> Option<String> {
    let h = h.trim();
    let bare = h.trim_start_matches('[').trim_end_matches(']');
    if bare.contains(':') {
        // IPv6: hex digits, colons, dots, and an optional %zone
        let (addr, zone) = bare.split_once('%').map(|(a, z)| (a, Some(z))).unwrap_or((bare, None));
        let ok = addr.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') && zone.is_none_or(|z| !z.is_empty() && z.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)));
        return ok.then(|| format!("[{}]", bare.replace('%', "%25")));
    }
    let ok = !h.is_empty() && h.len() <= 253 && !h.starts_with(['-', '.']) && h.chars().all(|c| c.is_alphanumeric() || "-._".contains(c));
    ok.then(|| h.to_string())
}

fn esc(s: &str, keep: &str) -> String {
    glib::Uri::escape_string(s, Some(keep), true).to_string()
}

/// Build an address from the Connect dialog's fields.
pub fn build_uri(p: Proto, host: &str, port: Option<u16>, user: &str, path: &str) -> Result<String, String> {
    let host = clean_host(host).ok_or("That server name isn't valid")?;
    let user = user.trim();
    if user.contains(':') {
        return Err("Leave the password out of the user name: you'll be asked for it".into());
    }
    let mut s = format!("{}://", p.scheme());
    if !user.is_empty() && p != Proto::Nfs {
        // Samba logins may carry a domain: DOMAIN;user
        s.push_str(&esc(user, if p == Proto::Smb { ";" } else { "" }));
        s.push('@');
    }
    s.push_str(&host);
    if let Some(port) = port.filter(|p| *p != 0) {
        s.push_str(&format!(":{port}"));
    }
    s.push('/');
    s.push_str(&esc(path.trim().replace('\\', "/").trim_start_matches('/'), "/"));
    Ok(s)
}

/// Remove a password from "scheme://user:secret@host/…". True if there was one.
pub fn strip_password(uri: &str) -> (String, bool) {
    let Some((scheme, rest)) = uri.split_once("://") else { return (uri.to_string(), false) };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    // the last '@' separates the login from the host
    let Some(at) = auth.rfind('@') else { return (uri.to_string(), false) };
    let (login, host) = (&auth[..at], &auth[at + 1..]);
    match login.split_once(':') {
        Some((user, _)) => (format!("{scheme}://{}{}{host}{tail}", user, if user.is_empty() { "" } else { "@" }), true),
        None => (uri.to_string(), false),
    }
}

/// Something typed as a server address: a URI, or a Windows path (\\host\share).
/// Returns the address and whether a password had to be removed.
pub fn parse_address(text: &str) -> Result<(String, bool), String> {
    let t = text.trim();
    if t.chars().any(|c| c.is_control()) {
        return Err("That address has invalid characters".into());
    }
    if let Some(unc) = t.strip_prefix("\\\\") {
        let unc = unc.replace('\\', "/");
        let (host, path) = unc.split_once('/').unwrap_or((unc.as_str(), ""));
        return build_uri(Proto::Smb, host, None, "", path).map(|u| (u, false));
    }
    let (scheme, rest) = t.split_once("://").ok_or("Type an address like smb://server/share")?;
    let p = Proto::from_scheme(scheme).ok_or_else(|| format!("“{scheme}” isn't a kind of server Burrow knows"))?;
    let (clean, had) = strip_password(&format!("{}://{rest}", p.scheme()));
    let auth_start = p.scheme().len() + 3;
    let auth = &clean[auth_start..];
    let auth = &auth[..auth.find('/').unwrap_or(auth.len())];
    let hostport = auth.rsplit('@').next().unwrap_or(auth);
    // split the port off, minding [v6]:port
    let host = match hostport.strip_prefix('[') {
        Some(r) => r.split(']').next().unwrap_or(""),
        None => hostport.split(':').next().unwrap_or(""),
    };
    clean_host(host).ok_or("That server name isn't valid")?;
    Ok((if clean[auth_start..].contains('/') { clean } else { format!("{clean}/") }, had))
}

/// A server found on the LAN.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub proto: Proto,
    pub uri: String,
    pub host: String,
}

/// Avahi's \DDD (decimal) and \x escapes.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            let v = (b[i + 1] - b'0') as u32 * 100 + (b[i + 2] - b'0') as u32 * 10 + (b[i + 3] - b'0') as u32;
            if v <= 255 {
                out.push(v as u8);
                i += 4;
                continue;
            }
        }
        if b[i] == b'\\' && i + 1 < b.len() {
            out.push(b[i + 1]);
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `avahi-browse -arptk`. Other machines only, one entry per (name, protocol).
/// Everything in here comes from the network, so host names are validated.
pub fn parse_avahi(text: &str, own_host: &str) -> Vec<Found> {
    let own = own_host.trim().to_lowercase();
    let mut seen = HashSet::new();
    let mut out = vec![];
    for l in text.lines().filter(|l| l.starts_with("=;")) {
        // =;iface;IPv4;name;type;domain;host;address;port;txt
        let f: Vec<&str> = l.splitn(10, ';').collect();
        if f.len() < 9 {
            continue;
        }
        let (proto, default_port) = match f[4] {
            "_smb._tcp" => (Proto::Smb, 445),
            "_sftp-ssh._tcp" | "_ssh._tcp" => (Proto::Sftp, 22),
            "_ftp._tcp" => (Proto::Ftp, 21),
            "_webdav._tcp" => (Proto::Dav, 80),
            "_webdavs._tcp" => (Proto::Davs, 443),
            "_nfs._tcp" => (Proto::Nfs, 2049),
            _ => continue,
        };
        let host = f[6].trim_end_matches('.').to_string();
        let short = host.trim_end_matches(".local").to_lowercase();
        if short == own || host.eq_ignore_ascii_case("localhost") {
            continue;
        }
        let port: u16 = f[8].parse().unwrap_or(0);
        let port = (port != 0 && port != default_port).then_some(port);
        let name: String = unescape(f[3]).chars().filter(|c| !c.is_control()).take(80).collect();
        let Ok(uri) = build_uri(proto, &host, port, "", "") else { continue };
        if seen.insert((host.to_lowercase(), proto.scheme())) {
            out.push(Found { name: if name.trim().is_empty() { host.clone() } else { name }, proto, uri, host });
        }
    }
    out.sort_by(|a, b| crate::util::natural_cmp(&a.name, &b.name));
    out
}

/// Ask Avahi what's on the LAN (worker thread; gives up after `limit`).
pub fn discover(limit: Duration) -> Result<Vec<Found>, String> {
    let mut child = Command::new("avahi-browse")
        .args(["-a", "-r", "-p", "-t", "-k"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "avahi-browse isn't installed".to_string())?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() > limit => {
                // a slow resolver: use whatever arrived so far
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(60)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let text = reader.join().unwrap_or_default();
    if status.is_some_and(|s| !s.success()) && text.is_empty() {
        return Err("the Avahi service isn't running".into());
    }
    let own = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    Ok(parse_avahi(&text, &own))
}

/// Is this a server rather than something on this computer?
pub fn is_network_uri(uri: &str) -> bool {
    uri.split_once("://").is_some_and(|(s, _)| Proto::from_scheme(s).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn building() {
        assert_eq!(build_uri(Proto::Smb, "nas", None, "", "Media/My Films").unwrap(), "smb://nas/Media/My%20Films");
        assert_eq!(build_uri(Proto::Sftp, "10.0.0.5", Some(2222), "ari", "/home/ari").unwrap(), "sftp://ari@10.0.0.5:2222/home/ari");
        assert_eq!(build_uri(Proto::Smb, "nas", None, "WORK;ari", "a\\b").unwrap(), "smb://WORK;ari@nas/a/b");
        assert_eq!(build_uri(Proto::Sftp, "fe80::1%eth0", None, "", "").unwrap(), "sftp://[fe80::1%25eth0]/");
        assert_eq!(build_uri(Proto::Nfs, "nas", None, "ari", "export").unwrap(), "nfs://nas/export");
    }

    #[test]
    fn refuses_bad_hosts_and_passwords_in_user() {
        for h in ["", "-oProxyCommand=x", "a b", "host/evil", "a@b", "x;rm", "$(id)", ".hidden"] {
            assert!(build_uri(Proto::Sftp, h, None, "", "").is_err(), "{h:?}");
        }
        assert!(build_uri(Proto::Smb, "nas", None, "ari:secret", "").is_err());
    }

    #[test]
    fn passwords_are_stripped() {
        assert_eq!(strip_password("smb://ari:hunter2@nas/share"), ("smb://ari@nas/share".into(), true));
        assert_eq!(strip_password("sftp://ari:p@ss:w@host:22/x"), ("sftp://ari@host:22/x".into(), true));
        assert_eq!(strip_password("ftp://:pw@host/"), ("ftp://host/".into(), true));
        assert_eq!(strip_password("smb://ari@nas/a:b"), ("smb://ari@nas/a:b".into(), false));
        assert_eq!(strip_password("/home/a@b:c"), ("/home/a@b:c".into(), false));
    }

    #[test]
    fn typed_addresses() {
        assert_eq!(parse_address(r"\\NAS\Media\Films").unwrap(), ("smb://NAS/Media/Films".into(), false));
        assert_eq!(parse_address("webdavs://cloud.example/dav").unwrap(), ("davs://cloud.example/dav".into(), false));
        assert_eq!(parse_address("ssh://ari@box").unwrap(), ("sftp://ari@box/".into(), false));
        assert_eq!(parse_address("smb://u:pw@nas").unwrap(), ("smb://u@nas/".into(), true));
        assert_eq!(parse_address("sftp://[fe80::1]:22/x").unwrap().0, "sftp://[fe80::1]:22/x");
        assert!(parse_address("javascript://x").is_err());
        assert!(parse_address("nas").is_err());
        assert!(parse_address("smb:///").is_err());
        assert!(parse_address("smb://a\nb/").is_err());
    }

    #[test]
    fn avahi() {
        let t = "+;wlan0;IPv4;NAS;_smb._tcp;local\n\
=;wlan0;IPv4;My\\032NAS;_smb._tcp;local;nas.local;10.0.0.5;445;\"a;b\"\n\
=;eth0;IPv6;My\\032NAS;_smb._tcp;local;nas.local;fe80::1;445;\n\
=;wlan0;IPv4;pi;_sftp-ssh._tcp;local;pi.local;10.0.0.6;2222;\n\
=;wlan0;IPv4;CUTIEOS;_smb._tcp;local;CutieOS.local;10.0.0.2;445;\n\
=;wlan0;IPv4;evil;_ssh._tcp;local;-oProxyCommand=x;10.0.0.9;22;\n\
=;wlan0;IPv4;tv;_googlecast._tcp;local;tv.local;10.0.0.7;8009;\n";
        let f = parse_avahi(t, "CutieOS\n");
        let got: Vec<(&str, &str)> = f.iter().map(|x| (x.name.as_str(), x.uri.as_str())).collect();
        assert_eq!(got, [("My NAS", "smb://nas.local/"), ("pi", "sftp://pi.local:2222/")]);
    }
}
