//! ~/.config/burrow/config.toml: one human-readable file, hot-reloaded.
//!
//! Drive settings are keyed by a stable drive key (UUID, else label, else GIO identifier),
//! never by mount path, so they survive replugging.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub appearance: AppearanceCfg,
    pub view: ViewCfg,
    pub apps: AppsCfg,
    /// Sidebar sections, top to bottom.
    #[serde(rename = "section")]
    pub sections: Vec<Section>,
    /// Per-drive settings, keyed by drive key ("uuid:…", "label:…").
    pub drives: BTreeMap<String, DriveCfg>,
    /// Per-folder settings, keyed by absolute path or URI.
    pub folders: BTreeMap<String, FolderCfg>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppearanceCfg {
    /// "rice" to follow Hyprland rounding of the active rice, or a number of pixels.
    pub radius: RadiusCfg,
    /// Window background opacity (0.3–1.0) so Hyprland blur shows through.
    pub opacity: f64,
    pub font: String,
    /// Icon theme name; empty = the one in ~/.config/gtk-4.0/settings.ini
    pub icon_theme: String,
    /// Papirus folder colour: "rice" (nearest to the rice accent), "none", or a Papirus colour name
    pub folder_color: String,
    pub reduced_motion: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum RadiusCfg {
    Px(u32),
    Mode(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ViewCfg {
    /// "grid", "list" or "compact"
    pub default_mode: String,
    pub show_hidden: bool,
    pub folders_first: bool,
    pub icon_size: u32,
    /// Show the "Drives" overview page in the sidebar
    pub drives_page: bool,
    /// "name", "size", "modified" or "type"
    pub sort: String,
    pub sort_desc: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AppsCfg {
    /// Terminal for "Open terminal here" ("kitty", "foot --server"…). Empty = $TERMINAL or auto.
    pub terminal: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// css hex colour for the section dot / title
    pub accent: Option<String>,
    pub collapsed: bool,
    /// Drives that have no section of their own land in the section with `auto_drives = true`.
    pub auto_drives: bool,
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Item {
    /// A pinned folder (local path or URI). `label` renames it in the sidebar only.
    Folder {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// A drive placed in this section by key.
    Drive { key: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct DriveCfg {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct FolderCfg {
    pub view: Option<String>,
    pub icon: Option<String>,
}

impl Default for AppearanceCfg {
    fn default() -> Self {
        AppearanceCfg {
            radius: RadiusCfg::Mode("rice".into()),
            opacity: 0.94,
            font: "Google Sans Flex".into(),
            icon_theme: String::new(),
            folder_color: "rice".into(),
            reduced_motion: false,
        }
    }
}

impl Default for ViewCfg {
    fn default() -> Self {
        ViewCfg {
            default_mode: "grid".into(),
            show_hidden: false,
            folders_first: true,
            icon_size: 64,
            drives_page: true,
            sort: "name".into(),
            sort_desc: false,
        }
    }
}

impl Default for Section {
    fn default() -> Self {
        Section {
            id: String::new(),
            name: String::new(),
            icon: None,
            accent: None,
            collapsed: false,
            auto_drives: false,
            items: vec![],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            appearance: AppearanceCfg::default(),
            view: ViewCfg::default(),
            apps: AppsCfg::default(),
            sections: vec![],
            drives: BTreeMap::new(),
            folders: BTreeMap::new(),
        }
    }
}

impl AppearanceCfg {
    pub fn radius_px(&self) -> Option<u32> {
        match &self.radius {
            RadiusCfg::Px(p) => Some((*p).min(32)),
            RadiusCfg::Mode(_) => None,
        }
    }
}

impl Config {
    /// First-run config: the XDG user folders as favorites plus an auto Drives section.
    pub fn first_run(places: &[(String, Option<String>)]) -> Config {
        let items = places
            .iter()
            .map(|(p, icon)| Item::Folder { path: p.clone(), label: None, icon: icon.clone() })
            .collect();
        Config {
            sections: vec![
                Section { id: "places".into(), name: "Places".into(), items, ..Default::default() },
                Section {
                    id: "drives".into(),
                    name: "Drives".into(),
                    auto_drives: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    pub fn parse(s: &str) -> Result<Config, String> {
        let mut c: Config = toml::from_str(s).map_err(|e| e.to_string())?;
        c.sanitize();
        Ok(c)
    }

    /// Fix things a hand edit can break, without failing the whole file.
    pub fn sanitize(&mut self) {
        self.appearance.opacity = if self.appearance.opacity.is_finite() {
            self.appearance.opacity.clamp(0.3, 1.0)
        } else {
            0.94
        };
        self.view.icon_size = self.view.icon_size.clamp(32, 256);
        if !matches!(self.view.default_mode.as_str(), "grid" | "list" | "compact") {
            self.view.default_mode = "grid".into();
        }
        if !matches!(self.view.sort.as_str(), "name" | "size" | "modified" | "type") {
            self.view.sort = "name".into();
        }
        for f in self.folders.values_mut() {
            if f.view.as_deref().is_some_and(|v| !matches!(v, "grid" | "list" | "compact")) {
                f.view = None;
            }
        }
        self.folders.retain(|_, f| f.view.is_some() || f.icon.is_some());
        // unique, non-empty section ids
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.sections.iter_mut().enumerate() {
            if s.id.trim().is_empty() || !seen.insert(s.id.clone()) {
                s.id = format!("section-{i}");
                seen.insert(s.id.clone());
            }
            if s.name.trim().is_empty() {
                s.name = s.id.clone();
            }
            if s.accent.as_deref().is_some_and(|a| crate::theme::Rgb::parse(a).is_none()) {
                s.accent = None;
            }
        }
        // a drive key may only live in one section
        let mut placed = std::collections::HashSet::new();
        for s in &mut self.sections {
            s.items.retain(|it| match it {
                Item::Drive { key } => placed.insert(key.clone()),
                Item::Folder { path, .. } => !path.trim().is_empty(),
            });
        }
        if !self.sections.iter().any(|s| s.auto_drives) {
            if let Some(s) = self.sections.last_mut() {
                s.auto_drives = true;
            }
        }
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string_pretty(self).unwrap_or_default();
        format!("{HEADER}{body}")
    }

    /// Which section a drive is shown in: its explicit placement, else the auto section.
    pub fn section_for_drive(&self, key: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|s| s.items.iter().any(|i| matches!(i, Item::Drive { key: k } if k == key)))
            .or_else(|| self.sections.iter().find(|s| s.auto_drives))
            .map(|s| s.id.as_str())
    }

    pub fn drive(&self, key: &str) -> DriveCfg {
        self.drives.get(key).cloned().unwrap_or_default()
    }
}

const HEADER: &str = "# Burrow config. Edit freely: the app reloads this file when it changes.\n\
# radius = \"rice\" follows your Hyprland rounding; a number (px) overrides it.\n\
# Drives are keyed by UUID/label, so names and icons survive replugging.\n\n";

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("burrow/config.toml")
}

/// Atomic write: temp file in the same dir + fsync + rename, so a crash never leaves half a config.
pub fn save_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent dir"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".config.toml.tmp-{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.view.default_mode, "grid");
        assert_eq!(c.appearance.radius_px(), None);
    }

    #[test]
    fn round_trip() {
        let mut c = Config::first_run(&[("/home/x/Projects".into(), Some("folder-code".into()))]);
        c.drives.insert("uuid:1234".into(), DriveCfg { name: Some("Games SSD".into()), icon: None, hidden: false });
        c.appearance.radius = RadiusCfg::Px(12);
        let back = Config::parse(&c.to_toml()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.appearance.radius_px(), Some(12));
    }

    #[test]
    fn user_written_file() {
        let s = r##"
[appearance]
radius = 10
opacity = 5.0

[[section]]
id = "games"
name = "Games"
accent = "#BF124D"
items = [
  { kind = "folder", path = "~/Games", label = "My games" },
  { kind = "drive", key = "uuid:abcd" },
]

[[section]]
name = "Broken"
accent = "not-a-colour"
items = [{ kind = "drive", key = "uuid:abcd" }]

[drives."uuid:abcd"]
name = "Steam SSD"
hidden = true
"##;
        let c = Config::parse(s).unwrap();
        assert_eq!(c.appearance.opacity, 1.0);
        assert_eq!(c.sections[1].id, "section-1");
        assert_eq!(c.sections[1].accent, None);
        // duplicate drive placement removed from the second section
        assert!(c.sections[1].items.is_empty());
        assert_eq!(c.section_for_drive("uuid:abcd"), Some("games"));
        // unknown drive goes to the auto section (last one when none is marked)
        assert_eq!(c.section_for_drive("uuid:new"), Some("section-1"));
        assert!(c.drive("uuid:abcd").hidden);
        match &c.sections[0].items[0] {
            Item::Folder { label, .. } => assert_eq!(label.as_deref(), Some("My games")),
            _ => panic!(),
        }
    }

    #[test]
    fn bad_toml_is_an_error_not_a_panic() {
        assert!(Config::parse("[[section]\nname=").is_err());
    }

    #[test]
    fn atomic_save_replaces_file() {
        let dir = std::env::temp_dir().join(format!("burrow-cfg-{}", std::process::id()));
        let p = dir.join("config.toml");
        save_atomic(&p, "a = 1\n").unwrap();
        save_atomic(&p, "a = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "a = 2\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "temp file left behind");
        let _ = fs::remove_dir_all(dir);
    }
}
