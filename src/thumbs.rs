//! Thumbnails in the shared XDG cache (~/.cache/thumbnails), so other apps' thumbnails are
//! reused and ours are useful to them. Runs on worker threads.
//!
//! Files come from anywhere, so the external tools (video, PDF, fonts, office…) run in a
//! bubblewrap sandbox: no network, no home folder, only the one input file, killed after
//! a few seconds. Without bubblewrap they're not run at all.

use gtk::gdk_pixbuf::Pixbuf;
use gtk::gio::prelude::*;
use gtk::{gio, glib};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Size {
    Large,
    XLarge,
}

impl Size {
    pub fn px(self) -> i32 {
        match self {
            Size::Large => 256,
            Size::XLarge => 512,
        }
    }
    fn dir(self) -> &'static str {
        match self {
            Size::Large => "large",
            Size::XLarge => "x-large",
        }
    }
}

const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PIXELS: i64 = 120_000_000;
const TOOL_LIMIT: Duration = Duration::from_secs(12);

fn cache_root() -> PathBuf {
    glib::user_cache_dir().join("thumbnails")
}

fn md5(uri: &str) -> String {
    glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri).map(|s| s.to_string()).unwrap_or_default()
}

fn cache_path(root: &Path, uri: &str, size: Size) -> PathBuf {
    root.join(size.dir()).join(format!("{}.png", md5(uri)))
}

fn fail_path(root: &Path, uri: &str) -> PathBuf {
    root.join("fail/burrow").join(format!("{}.png", md5(uri)))
}

/// Read one tEXt value from a PNG without decoding it.
pub fn png_text(data: &[u8], key: &str) -> Option<String> {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut i = 8usize;
    while i.checked_add(8)? <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        let kind = &data[i + 4..i + 8];
        let body = data.get(i + 8..(i + 8).checked_add(len)?)?;
        if kind == b"tEXt" {
            if let Some(z) = body.iter().position(|b| *b == 0) {
                if &body[..z] == key.as_bytes() {
                    return Some(String::from_utf8_lossy(&body[z + 1..]).into_owned());
                }
            }
        }
        if kind == b"IEND" {
            return None;
        }
        i = (i + 8).checked_add(len)?.checked_add(4)?;
    }
    None
}

/// The stored thumbnail matches this version of the file.
fn is_valid(p: &Path, uri: &str, mtime: i64) -> bool {
    let Ok(mut f) = fs::File::open(p) else { return false };
    // the text chunks come first; a few kB is plenty
    let mut head = vec![0u8; 8192];
    let n = f.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    png_text(head, "Thumb::MTime").and_then(|m| m.trim().split('.').next().and_then(|s| s.parse::<i64>().ok())) == Some(mtime)
        && png_text(head, "Thumb::URI").is_none_or(|u| u == uri)
}

/// What to make a thumbnail of.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub uri: String,
    pub mtime: i64,
    pub mime: String,
}

/// Could this ever get a thumbnail? (cheap check before queueing)
pub fn wanted(mime: &str) -> bool {
    mime.starts_with("image/")
        || (sandbox_available() && (is_pdf(mime) || is_font(mime) || tool_for(mime).is_some()))
}

/// PNG bytes of the thumbnail: from the cache, or made now and stored.
pub fn get(s: &Source, size: Size) -> Option<Vec<u8>> {
    get_in(&cache_root(), s, size)
}

fn get_in(root: &Path, s: &Source, size: Size) -> Option<Vec<u8>> {
    // never thumbnail the thumbnails
    if s.path.starts_with(root) {
        return None;
    }
    let cached = cache_path(root, &s.uri, size);
    if is_valid(&cached, &s.uri, s.mtime) {
        return fs::read(cached).ok();
    }
    if is_valid(&fail_path(root, &s.uri), &s.uri, s.mtime) {
        return None;
    }
    let made = make(s, size);
    match made {
        Some(pix) => store(&cached, &pix, s).and_then(|_| fs::read(&cached).ok()).or_else(|| pix.save_to_bufferv("png", &[]).ok()),
        None => {
            // remember the failure so a broken file isn't retried every time
            if let Some(blank) = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 1, 1) {
                blank.fill(0);
                store(&fail_path(root, &s.uri), &blank, s);
            }
            None
        }
    }
}

fn private_dir(d: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(d)
}

/// Write to a temp name and rename, readable only by the user (the spec asks for both).
fn store(to: &Path, pix: &Pixbuf, s: &Source) -> Option<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = to.parent()?;
    private_dir(dir).ok()?;
    let tmp = dir.join(format!(".burrow-{}-{}.png", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let mtime = s.mtime.to_string();
    let opts = [("tEXt::Thumb::URI", s.uri.as_str()), ("tEXt::Thumb::MTime", mtime.as_str()), ("tEXt::Software", "Burrow")];
    let ok = pix.savev(&tmp, "png", &opts).is_ok() && fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).is_ok() && fs::rename(&tmp, to).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
    ok.then_some(())
}

fn make(s: &Source, size: Size) -> Option<Pixbuf> {
    let len = fs::metadata(&s.path).ok()?.len();
    if len == 0 {
        return None;
    }
    if s.mime.starts_with("image/") {
        if len <= MAX_BYTES {
            if let Some(p) = load_image(&s.path, size.px()) {
                return Some(p);
            }
        }
    }
    if !sandbox_available() {
        return None;
    }
    let out = tempdir()?;
    let pix = run_tool(s, size, out.path()).and_then(|png| load_image(&png, size.px()));
    drop(out);
    pix
}

/// Decode an image scaled down to fit `px` (never up), the right way round.
pub fn load_image(path: &Path, px: i32) -> Option<Pixbuf> {
    if let Some((_, w, h)) = Pixbuf::file_info(path) {
        if w as i64 * h as i64 > MAX_PIXELS || w <= 0 || h <= 0 {
            return None;
        }
        if w <= px && h <= px {
            return Pixbuf::from_file(path).ok().map(|p| p.apply_embedded_orientation().unwrap_or(p));
        }
    }
    let p = Pixbuf::from_file_at_scale(path, px, px, true).ok()?;
    Some(p.apply_embedded_orientation().unwrap_or(p))
}

// ---------------------------------------------------------------- external tools

struct TempDir(PathBuf);
impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn tempdir() -> Option<TempDir> {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = glib::user_runtime_dir().join(format!("burrow-thumb-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    fs::DirBuilder::new().mode(0o700).create(&d).ok()?;
    Some(TempDir(d))
}

fn in_path(c: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(c).is_file()))
}

fn sandbox_available() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| Path::new("/usr/bin/bwrap").is_file())
}

fn is_pdf(m: &str) -> bool {
    m == "application/pdf" || m == "application/x-pdf"
}
fn is_font(m: &str) -> bool {
    m.starts_with("font/") || m.starts_with("application/x-font") || m == "application/vnd.ms-opentype"
}

/// (MIME types, command line) of the installed *.thumbnailer files.
fn thumbnailers() -> &'static Vec<(Vec<String>, Vec<String>)> {
    static T: OnceLock<Vec<(Vec<String>, Vec<String>)>> = OnceLock::new();
    T.get_or_init(|| {
        let mut out = vec![];
        // only system-installed ones: a file dropped into the home folder must not run
        let Ok(rd) = fs::read_dir("/usr/share/thumbnailers") else { return out };
        for e in rd.flatten() {
            if let Some(t) = fs::read_to_string(e.path()).ok().and_then(|t| parse_thumbnailer(&t)) {
                if in_path(t.1[0].rsplit('/').next().unwrap_or(&t.1[0])) {
                    out.push(t);
                }
            }
        }
        out
    })
}

pub fn parse_thumbnailer(text: &str) -> Option<(Vec<String>, Vec<String>)> {
    let val = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k)?.trim_start().strip_prefix('=').map(|v| v.trim().to_string()));
    let exec = glib::shell_parse_argv(val("Exec")?).ok()?;
    let argv: Vec<String> = exec.iter().map(|a| a.to_string_lossy().to_string()).collect();
    let mimes: Vec<String> = val("MimeType")?.split(';').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    (!argv.is_empty() && !mimes.is_empty() && argv.iter().any(|a| a.contains("%o"))).then_some((mimes, argv))
}

fn tool_for(mime: &str) -> Option<&'static Vec<String>> {
    thumbnailers().iter().find(|(m, _)| m.iter().any(|x| x == mime)).map(|(_, a)| a)
}

/// Run the right tool in the sandbox; returns the picture it made.
fn run_tool(s: &Source, size: Size, out: &Path) -> Option<PathBuf> {
    let px = size.px().to_string();
    let png = out.join("o.png");
    // inside the sandbox the input is always /in/file.<ext> (some tools look at the extension)
    let ext: String = s.path.extension().map(|e| e.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect()).unwrap_or_default();
    let input = if ext.is_empty() { "/in/file".to_string() } else { format!("/in/file.{ext}") };
    let mut env: Vec<(&str, &str)> = vec![];
    let argv: Vec<String> = if is_pdf(&s.mime) && in_path("pdftoppm") {
        ["pdftoppm", "-png", "-singlefile", "-f", "1", "-l", "1", "-scale-to", &px, &input, "/out/o"].map(String::from).to_vec()
    } else if is_font(&s.mime) && in_path("pango-view") && in_path("fc-scan") {
        let family = Command::new("fc-scan").args(["--format", "%{family[0]}"]).arg(&s.path).stderr(Stdio::null()).output().ok()?;
        let family: String = String::from_utf8_lossy(&family.stdout).chars().filter(|c| !c.is_control() && *c != ',').take(80).collect();
        if family.is_empty() {
            return None;
        }
        // a font list with only this font, so the preview really is this font
        fs::write(out.join("fonts.conf"), "<?xml version=\"1.0\"?><fontconfig><dir>/in</dir><cachedir>/tmp</cachedir></fontconfig>").ok()?;
        env.push(("FONTCONFIG_FILE", "/out/fonts.conf"));
        ["pango-view", "-q", "--font", &format!("{family} 64"), "--text", "Aa", "--margin", "18", "--foreground", "#ede6e3", "--background", "transparent", "-o", "/out/o.png"]
            .map(String::from)
            .to_vec()
    } else {
        tool_for(&s.mime)?
            .iter()
            .map(|a| a.replace("%i", &input).replace("%u", &format!("file://{input}")).replace("%o", "/out/o.png").replace("%s", &px))
            .collect()
    };

    let mut c = Command::new("/usr/bin/bwrap");
    c.args(["--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL"]);
    c.args(["--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64", "--symlink", "usr/bin", "/sbin"]);
    c.args(["--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache", "--ro-bind-try", "/etc/fonts", "/etc/fonts", "--ro-bind-try", "/var/cache/fontconfig", "/var/cache/fontconfig"]);
    c.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--clearenv", "--setenv", "PATH", "/usr/bin", "--setenv", "HOME", "/tmp", "--setenv", "LC_ALL", "C.UTF-8"]);
    for (k, v) in env {
        c.args(["--setenv", k, v]);
    }
    c.arg("--ro-bind").arg(&s.path).arg(&input).arg("--bind").arg(out).arg("/out").args(["--chdir", "/tmp", "--"]).args(&argv);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => break,
            Ok(None) if start.elapsed() < TOOL_LIMIT => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    // only a regular file of a sane size counts as a result
    let m = fs::symlink_metadata(&png).ok()?;
    (m.is_file() && m.len() > 0 && m.len() < 64 * 1024 * 1024).then_some(png)
}

/// Everything needed from a listed file, or None if it can't have a thumbnail.
pub fn source(info: &gio::FileInfo, f: &gio::File) -> Option<Source> {
    if info.file_type() != gio::FileType::Regular || !f.is_native() {
        return None;
    }
    let mime = info.content_type()?.to_string();
    if !wanted(&mime) {
        return None;
    }
    Some(Source { path: f.path()?, uri: f.uri().to_string(), mtime: info.attribute_uint64("time::modified") as i64, mime })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("burrow-th-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn src(p: &Path, mime: &str) -> Source {
        Source { path: p.to_path_buf(), uri: gio::File::for_path(p).uri().to_string(), mtime: 1234, mime: mime.into() }
    }

    #[test]
    fn spec_cache_names() {
        // the example from the freedesktop thumbnail spec
        assert_eq!(md5("file:///home/jens/photos/me.png"), "c6ee772d9e49320e97ec29a7eb5b1697");
    }

    #[test]
    fn image_is_cached_and_revalidated() {
        let d = tmp("img");
        let p = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 800, 400).unwrap();
        p.fill(0xbe3144ff);
        let f = d.join("a.png");
        p.savev(&f, "png", &[]).unwrap();
        let root = d.join("cache");
        let mut s = src(&f, "image/png");
        let png = get_in(&root, &s, Size::Large).unwrap();
        assert_eq!(png_text(&png, "Thumb::MTime").as_deref(), Some("1234"));
        assert_eq!(png_text(&png, "Thumb::URI").as_deref(), Some(s.uri.as_str()));
        let c = cache_path(&root, &s.uri, Size::Large);
        assert_eq!(fs::metadata(&c).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(c.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let (_, w, h) = Pixbuf::file_info(&c).unwrap();
        assert_eq!((w, h), (256, 128));
        assert!(is_valid(&c, &s.uri, 1234));
        // changed file -> stale
        s.mtime = 9999;
        assert!(!is_valid(&c, &s.uri, 9999));
    }

    #[test]
    fn broken_files_fail_once_and_small_images_stay_small() {
        let d = tmp("bad");
        fs::write(d.join("x.png"), b"not a png").unwrap();
        let root = d.join("cache");
        let s = src(&d.join("x.png"), "image/png");
        assert!(get_in(&root, &s, Size::Large).is_none());
        assert!(is_valid(&fail_path(&root, &s.uri), &s.uri, 1234));
        let p = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 16, 16).unwrap();
        p.savev(d.join("s.png"), "png", &[]).unwrap();
        assert_eq!(load_image(&d.join("s.png"), 256).unwrap().width(), 16);
    }

    #[test]
    fn png_text_survives_garbage() {
        assert!(png_text(b"", "k").is_none());
        assert!(png_text(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xfftEXtk\0v", "k").is_none());
        assert!(png_text(b"\x89PNG\r\n\x1a\n\0\0\0\x03tEXtk\0v\0\0\0\0", "k").as_deref() == Some("v"));
    }

    #[test]
    fn thumbnailer_files() {
        let (m, a) = parse_thumbnailer("[Thumbnailer Entry]\nTryExec=t\nExec=t -i %i -o %o -s %s\nMimeType=video/mp4;video/ogg;\n").unwrap();
        assert_eq!(m, ["video/mp4", "video/ogg"]);
        assert_eq!(a[0], "t");
        assert!(parse_thumbnailer("[x]\nExec=t\nMimeType=a/b;\n").is_none()); // no output
    }

    /// PDF, video and font through the real sandbox (needs the tools; run with --ignored).
    #[test]
    #[ignore]
    fn sandboxed_tools() {
        let d = tmp("tools");
        let root = d.join("cache");
        let ok = |c: &mut Command| c.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        assert!(ok(Command::new("ffmpeg").args(["-y", "-f", "lavfi", "-i", "testsrc=d=1:s=320x180"]).arg(d.join("v.mp4"))));
        let font = String::from_utf8(Command::new("fc-match").args(["-f", "%{file}", "sans"]).output().unwrap().stdout).unwrap();
        fs::copy(&font, d.join("f.ttf")).unwrap();
        assert!(ok(Command::new("pango-view").args(["-q", "--text", "Hello", "-o"]).arg(d.join("p.pdf"))));
        for (f, m) in [("v.mp4", "video/mp4"), ("p.pdf", "application/pdf"), ("f.ttf", "font/ttf")] {
            assert!(get_in(&root, &src(&d.join(f), m), Size::Large).is_some(), "{f}");
        }
        // nothing left in the runtime dir
        assert!(!fs::read_dir(glib::user_runtime_dir()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(&format!("burrow-thumb-{}", std::process::id()))));
    }
}
