//! Custom context-menu actions: a command run on the selected files.
//!
//! The command is split into arguments once, and file names are put in as whole arguments.
//! There's no shell in between, so a file called "; rm -rf ~" is just a strange file name.

use gtk::glib;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Action {
    pub name: String,
    /// e.g. "magick %f -resize 50% %d/small-%n". %f file, %F all files, %d folder, %n name,
    /// %s name without extension, %u address, %U all addresses, %% a percent sign
    pub command: String,
    /// "image/*", ".png", "folder", "file"; empty = everything
    pub types: Vec<String>,
    /// Ask before running
    pub confirm: bool,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub path: PathBuf,
    pub uri: String,
    pub mime: String,
    pub folder: bool,
}

impl Action {
    pub fn valid(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the action a name".into());
        }
        let argv = split(&self.command)?;
        if argv[0].contains('%') {
            return Err("The program itself can't be a placeholder".into());
        }
        Ok(())
    }

    /// Every selected file has to match one of the types.
    pub fn applies(&self, sel: &[Target]) -> bool {
        !sel.is_empty() && sel.iter().all(|t| self.types.is_empty() || self.types.iter().any(|p| matches(p, t)))
    }

    /// The commands to run: one per file if it only uses single-file placeholders.
    pub fn commands(&self, sel: &[Target]) -> Result<Vec<Vec<String>>, String> {
        let argv = split(&self.command)?;
        let has = |c: &str| argv.iter().any(|a| a.contains(c));
        if has("%F") || has("%U") {
            return Ok(vec![expand(&argv, sel, &sel[0])]);
        }
        if ["%f", "%u", "%n", "%s"].iter().any(|p| has(p)) {
            return Ok(sel.iter().map(|t| expand(&argv, std::slice::from_ref(t), t)).collect());
        }
        Ok(vec![expand(&argv, sel, sel.first().ok_or("nothing selected")?)])
    }
}

fn split(cmd: &str) -> Result<Vec<String>, String> {
    let argv = glib::shell_parse_argv(cmd).map_err(|e| format!("The command isn't valid: {}", e.message()))?;
    let argv: Vec<String> = argv.iter().map(|a| a.to_string_lossy().to_string()).collect();
    if argv.is_empty() || argv[0].is_empty() { Err("The command is empty".into()) } else { Ok(argv) }
}

fn matches(p: &str, t: &Target) -> bool {
    let p = p.trim().to_lowercase();
    match p.as_str() {
        "folder" => t.folder,
        "file" => !t.folder,
        _ if p.starts_with('.') => !t.folder && t.path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(&p)),
        _ => match p.strip_suffix("/*") {
            Some(major) => !t.folder && t.mime.split('/').next() == Some(major),
            None => t.mime == p,
        },
    }
}

/// Names starting with "-" would be read as options: paths are absolute, %n and %s aren't.
fn bare(s: String) -> String {
    if s.starts_with('-') { format!("./{s}") } else { s }
}

fn expand(argv: &[String], all: &[Target], one: &Target) -> Vec<String> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = vec![];
    for a in argv {
        // a lone %F / %U becomes one argument per file
        match a.as_str() {
            "%F" => {
                out.extend(all.iter().map(|t| t.path.to_string_lossy().to_string()));
                continue;
            }
            "%U" => {
                out.extend(all.iter().map(|t| t.uri.clone()));
                continue;
            }
            _ => {}
        }
        let mut s = String::new();
        let mut it = a.chars();
        while let Some(c) = it.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match it.next() {
                Some('f') | Some('F') => s.push_str(&one.path.to_string_lossy()),
                Some('u') | Some('U') => s.push_str(&one.uri),
                Some('d') => s.push_str(&one.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
                Some('n') => s.push_str(&name(&one.path)),
                Some('s') => s.push_str(crate::rename::split_ext(&name(&one.path)).0),
                Some('%') => s.push('%'),
                // "50%" and friends: leave unknown ones alone
                Some(o) => {
                    s.push('%');
                    s.push(o);
                }
                None => s.push('%'),
            }
        }
        out.push(if a.starts_with("%n") || a.starts_with("%s") { bare(s) } else { s });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(p: &str, mime: &str) -> Target {
        Target { path: p.into(), uri: format!("file://{p}"), mime: mime.into(), folder: mime == "inode/directory" }
    }
    fn act(cmd: &str) -> Action {
        Action { name: "x".into(), command: cmd.into(), ..Default::default() }
    }

    #[test]
    fn one_run_per_file_or_all_at_once() {
        let sel = [t("/a/x y.png", "image/png"), t("/a/b.png", "image/png")];
        assert_eq!(act("magick %f -resize 50% %d/small-%n").commands(&sel).unwrap()[0], ["magick", "/a/x y.png", "-resize", "50%", "/a/small-x y.png"]);
        assert_eq!(act("magick %f o.png").commands(&sel).unwrap().len(), 2);
        assert_eq!(act("viewer %F").commands(&sel).unwrap(), [["viewer", "/a/x y.png", "/a/b.png"]]);
        assert_eq!(act("notify-send '%%' done").commands(&sel).unwrap(), [["notify-send", "%", "done"]]);
        assert_eq!(act("echo %s").commands(&sel[..1]).unwrap()[0], ["echo", "x y"]);
    }

    #[test]
    fn names_cant_inject() {
        let sel = [t("/a/; rm -rf ~ $(id) `id` %f.png", "image/png")];
        assert_eq!(act("open %f").commands(&sel).unwrap(), [vec!["open", "/a/; rm -rf ~ $(id) `id` %f.png"]]);
        let dash = [t("/a/--delete", "text/plain")];
        assert_eq!(act("tool %n").commands(&dash).unwrap()[0][1], "./--delete");
    }

    #[test]
    fn types_and_validity() {
        let mut a = act("x %f");
        a.types = vec!["image/*".into(), ".md".into()];
        assert!(a.applies(&[t("/p.png", "image/png"), t("/R.MD", "text/markdown")]));
        assert!(!a.applies(&[t("/p.png", "image/png"), t("/v.mp4", "video/mp4")]) && !a.applies(&[]));
        a.types = vec!["folder".into()];
        assert!(a.applies(&[t("/d", "inode/directory")]));
        assert!(act("").valid().is_err() && act("%f").valid().is_err() && act("a 'b").valid().is_err() && act("ok %f").valid().is_ok());
    }
}
