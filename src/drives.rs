//! Drive manager on top of GIO's VolumeMonitor (UDisks2 + GVFS behind it).
//! Gives every drive a stable key so per-drive settings survive replugging.

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    System,
    Internal,
    Removable,
    Network,
}

#[derive(Clone)]
pub struct Drive {
    pub key: String,
    pub name: String,
    pub icon: gio::Icon,
    pub kind: Kind,
    pub volume: Option<gio::Volume>,
    pub mount: Option<gio::Mount>,
    pub root: Option<gio::File>,
}

impl Drive {
    pub fn mounted(&self) -> bool {
        self.root.is_some()
    }
    pub fn can_eject(&self) -> bool {
        self.mount.as_ref().is_some_and(|m| m.can_eject())
            || self.volume.as_ref().is_some_and(|v| v.can_eject())
    }
    pub fn can_unmount(&self) -> bool {
        self.mount.as_ref().is_some_and(|m| m.can_unmount())
    }
}

/// Stable key: UUID, else filesystem label, else the GIO identifier / URI.
pub fn key_for(volume: Option<&gio::Volume>, mount: Option<&gio::Mount>) -> String {
    if let Some(v) = volume {
        if let Some(u) = v.identifier(gio::VOLUME_IDENTIFIER_KIND_UUID) {
            return format!("uuid:{u}");
        }
        if let Some(l) = v.identifier(gio::VOLUME_IDENTIFIER_KIND_LABEL) {
            return format!("label:{l}");
        }
        if let Some(c) = v.identifier(gio::VOLUME_IDENTIFIER_KIND_CLASS) {
            if c == "network" {
                if let Some(r) = v.activation_root() {
                    return format!("uri:{}", r.uri());
                }
            }
        }
        // phones and cameras: the USB device path changes on every replug, the address doesn't
        if let Some(r) = v.activation_root().filter(|r| !r.is_native()) {
            return format!("uri:{}", r.uri());
        }
        if let Some(d) = v.identifier(gio::VOLUME_IDENTIFIER_KIND_UNIX_DEVICE) {
            return format!("dev:{d}");
        }
        return format!("name:{}", v.name());
    }
    if let Some(m) = mount {
        if let Some(u) = m.uuid() {
            return format!("uuid:{u}");
        }
        return format!("uri:{}", m.root().uri());
    }
    "unknown".into()
}

fn kind_of(volume: Option<&gio::Volume>, mount: Option<&gio::Mount>) -> Kind {
    let root = mount.map(|m| m.root());
    let scheme = root.clone().or_else(|| volume.and_then(|v| v.activation_root())).and_then(|r| r.uri_scheme());
    if scheme.as_deref().is_some_and(is_device_scheme) {
        return Kind::Removable;
    }
    if root.as_ref().is_some_and(|r| !r.is_native()) {
        return Kind::Network;
    }
    if volume
        .and_then(|v| v.identifier(gio::VOLUME_IDENTIFIER_KIND_CLASS))
        .is_some_and(|c| c == "network")
    {
        return Kind::Network;
    }
    let removable = volume
        .and_then(|v| v.drive())
        .is_some_and(|d| d.is_removable() || d.is_media_removable() || d.can_eject());
    if removable || mount.is_some_and(|m| m.can_eject()) {
        Kind::Removable
    } else {
        Kind::Internal
    }
}

/// Phones (MTP, Apple) and cameras: plugged-in devices that GVFS shows as addresses.
pub fn is_device_scheme(s: &str) -> bool {
    matches!(s, "mtp" | "gphoto2" | "afc")
}

/// Everything the sidebar should show: the root filesystem, every volume (mounted or not),
/// and mounts without a volume (network shares, MTP phones once mounted, etc.).
pub fn list() -> Vec<Drive> {
    let mon = gio::VolumeMonitor::get();
    let mut out = vec![Drive {
        key: "root".into(),
        name: "File System".into(),
        icon: gio::ThemedIcon::new("drive-harddisk-system-symbolic").upcast(),
        kind: Kind::System,
        volume: None,
        mount: None,
        root: Some(gio::File::for_path("/")),
    }];
    for v in mon.volumes() {
        let m = v.get_mount();
        if m.as_ref().is_some_and(|m| m.is_shadowed()) {
            continue;
        }
        out.push(Drive {
            key: key_for(Some(&v), m.as_ref()),
            name: v.name().to_string(),
            icon: v.symbolic_icon(),
            kind: kind_of(Some(&v), m.as_ref()),
            root: m.as_ref().map(|m| m.root()),
            mount: m,
            volume: Some(v),
        });
    }
    for m in mon.mounts() {
        if m.volume().is_some() || m.is_shadowed() {
            continue;
        }
        out.push(Drive {
            key: key_for(None, Some(&m)),
            name: m.name().to_string(),
            icon: m.symbolic_icon(),
            kind: kind_of(None, Some(&m)),
            root: Some(m.root()),
            volume: None,
            mount: Some(m),
        });
    }
    add_fstab_mounts(&mut out);
    // de-duplicate keys (two partitions with the same label, etc.)
    let mut seen = std::collections::HashMap::<String, u32>::new();
    for d in &mut out {
        let n = seen.entry(d.key.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            d.key = format!("{}#{}", d.key, n);
        }
    }
    out
}

/// Async free/total for a mounted root. Never blocks the UI thread.
pub fn query_usage<F: Fn(Option<(u64, u64)>) + 'static>(root: &gio::File, cb: F) {
    root.query_filesystem_info_async(
        "filesystem::size,filesystem::free",
        glib::Priority::LOW,
        gio::Cancellable::NONE,
        move |res| {
            let v = res.ok().and_then(|i| {
                let size = i.attribute_uint64("filesystem::size");
                let free = i.attribute_uint64("filesystem::free");
                (size > 0).then_some((size, free))
            });
            cb(v)
        },
    );
}

/// One line of /proc/mounts: (device, mount point, fs type).
pub fn parse_proc_mounts(text: &str) -> Vec<(String, String, String)> {
    // fields are space-separated; spaces etc. inside them are octal escapes like \040
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
                let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
                out.push(v);
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    text.lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            Some((unescape(f.next()?), unescape(f.next()?), f.next()?.to_string()))
        })
        .collect()
}

/// Mounts a user would call "a drive": a real block device mounted somewhere user-facing.
/// GIO hides plain fstab mounts (no x-gvfs-show), e.g. /mnt/Games.
pub fn user_facing(dev: &str, mp: &str, home: &str) -> bool {
    let h = home.trim_end_matches('/');
    dev.starts_with("/dev/")
        && !dev.starts_with("/dev/loop")
        && (["/mnt/", "/media/", "/run/media/", "/srv/"].iter().any(|p| mp.starts_with(p))
            || (!h.is_empty() && mp.starts_with(&format!("{h}/"))))
}

/// Reverse lookup in /dev/disk/by-uuid or by-label: which name points at `dev`.
fn disk_name(dir: &str, dev: &str) -> Option<String> {
    let target = std::fs::canonicalize(dev).ok()?;
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = std::fs::canonicalize(e.path()).ok()?;
        (p == target).then(|| e.file_name().to_string_lossy().replace("\\x20", " "))
    })
}

fn add_fstab_mounts(out: &mut Vec<Drive>) {
    let Ok(text) = std::fs::read_to_string("/proc/mounts") else { return };
    let mounts = parse_proc_mounts(&text);
    let home = glib::home_dir().to_string_lossy().to_string();
    // devices GIO already shows (a bind-mounted drive must not appear twice)
    let mut seen: std::collections::HashSet<String> = out
        .iter()
        .filter_map(|d| d.root.as_ref()?.path())
        .filter_map(|p| {
            let p = p.to_string_lossy().to_string();
            mounts.iter().find(|(_, mp, _)| *mp == p).map(|(dev, _, _)| dev.clone())
        })
        .collect();
    for (dev, mp, _) in &mounts {
        if !user_facing(dev, mp, &home) || !seen.insert(dev.clone()) {
            continue;
        }
        let uuid = disk_name("/dev/disk/by-uuid", dev);
        let label = disk_name("/dev/disk/by-label", dev);
        let key = uuid.as_ref().map(|u| format!("uuid:{u}")).or_else(|| label.as_ref().map(|l| format!("label:{l}"))).unwrap_or_else(|| format!("dev:{dev}"));
        let name = label.clone().unwrap_or_else(|| std::path::Path::new(mp).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| mp.clone()));
        out.push(Drive {
            key,
            name,
            icon: gio::ThemedIcon::new("drive-harddisk-symbolic").upcast(),
            kind: Kind::Internal,
            volume: None,
            mount: None,
            root: Some(gio::File::for_path(mp)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_mounts_and_filter() {
        let t = "/dev/nvme0n1p3 / ext4 rw 0 0\n/dev/sda1 /mnt/Games ext4 rw 0 0\n/dev/sdb1 /mnt/My\\040Disk ntfs3 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/nvme0n1p1 /boot/efi vfat rw 0 0\n/dev/loop0 /mnt/iso iso9660 ro 0 0\n";
        let m = parse_proc_mounts(t);
        assert_eq!(m[2].1, "/mnt/My Disk");
        let shown: Vec<&str> = m.iter().filter(|(d, p, _)| user_facing(d, p, "/home/a")).map(|(_, p, _)| p.as_str()).collect();
        assert_eq!(shown, ["/mnt/Games", "/mnt/My Disk"]);
        assert!(user_facing("/dev/sdb1", "/home/a/Elements", "/home/a"));
        assert!(!user_facing("/dev/sdb1", "/home/ab/x", "/home/a"));
    }
}

