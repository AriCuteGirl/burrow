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
