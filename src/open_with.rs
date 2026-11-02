use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use egui::ColorImage;

use crate::i18n;
use crate::library;

#[derive(Clone, Debug)]
pub struct Icon {
    pub size: [usize; 2],
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub icon: Option<Icon>,
    exec: String,
    desktop: PathBuf,
}

#[derive(Default)]
pub struct Catalog {
    apps: HashMap<String, App>,
    by_mime: HashMap<String, Vec<String>>,
}

pub type SharedCatalog = Arc<Mutex<Option<Catalog>>>;

pub fn spawn_load() -> SharedCatalog {
    let shared = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&shared);
    thread::Builder::new()
        .name("lk-open-with".into())
        .spawn(move || {
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(Catalog::load());
            }
        })
        .ok();
    shared
}

impl Catalog {
    fn load() -> Self {
        let mut catalog = Self::default();
        for dir in data_dirs() {
            catalog.read_mime_cache(&dir.join("applications/mimeinfo.cache"));
            catalog.read_desktop_dir(&dir.join("applications"));
        }
        catalog
    }

    fn read_mime_cache(&mut self, path: &Path) {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let mut in_cache = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_cache = line.eq_ignore_ascii_case("[MIME Cache]");
                continue;
            }
            if !in_cache {
                continue;
            }
            let Some((mime, ids)) = line.split_once('=') else {
                continue;
            };
            let mime = mime.trim();
            if mime.is_empty() {
                continue;
            }
            let list = self.by_mime.entry(mime.to_string()).or_default();
            for id in ids.split(';').map(str::trim).filter(|id| !id.is_empty()) {
                if !list.iter().any(|have| have == id) {
                    list.push(id.to_string());
                }
            }
        }
    }

    fn read_desktop_dir(&mut self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if self.apps.contains_key(id) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(app) = parse_desktop(id, &path, &text) {
                for mime in &app.1 {
                    let list = self.by_mime.entry(mime.clone()).or_default();
                    if !list.iter().any(|have| have == id) {
                        list.push(id.to_string());
                    }
                }
                self.apps.insert(id.to_string(), app.0);
            }
        }
    }

    pub fn apps_for(&self, path: &Path) -> Vec<App> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for mime in mimes_for(path) {
            if let Some(ids) = self.by_mime.get(mime) {
                for id in ids {
                    push_app(self, id, &mut seen, &mut out);
                }
            }
        }
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    }
}

fn push_app(catalog: &Catalog, id: &str, seen: &mut HashSet<String>, out: &mut Vec<App>) {
    if is_self(id) || !seen.insert(id.to_string()) {
        return;
    }
    if let Some(app) = catalog.apps.get(id) {
        out.push(app.clone());
    }
}

fn is_self(id: &str) -> bool {
    id.eq_ignore_ascii_case("lichtkasten.desktop")
}

pub fn mimes_for(path: &Path) -> Vec<&'static str> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let specific = match ext.as_str() {
        "jpg" | "jpeg" => &["image/jpeg"][..],
        "png" => &["image/png"],
        "tif" | "tiff" => &["image/tiff"],
        "webp" => &["image/webp"],
        "gif" => &["image/gif"],
        "bmp" => &["image/bmp"],
        "cr2" => &["image/x-canon-cr2", "image/x-dcraw"],
        "cr3" => &["image/x-canon-cr3", "image/x-dcraw"],
        "crw" => &["image/x-canon-crw", "image/x-dcraw"],
        "nef" | "nrw" => &["image/x-nikon-nef", "image/x-dcraw"],
        "arw" | "sr2" | "srf" => &["image/x-sony-arw", "image/x-dcraw"],
        "dng" => &["image/x-adobe-dng", "image/x-dcraw"],
        "raf" => &["image/x-fuji-raf", "image/x-dcraw"],
        "orf" | "ori" => &["image/x-olympus-orf", "image/x-dcraw"],
        "rw2" => &["image/x-panasonic-rw2", "image/x-dcraw"],
        "pef" => &["image/x-pentax-pef", "image/x-dcraw"],
        "srw" => &["image/x-samsung-srw", "image/x-dcraw"],
        "3fr" | "fff" => &["image/x-hasselblad-3fr", "image/x-dcraw"],
        "iiq" => &["image/x-phaseone-iiq", "image/x-dcraw"],
        "mos" => &["image/x-leaf-mos", "image/x-dcraw"],
        "mrw" => &["image/x-minolta-mrw", "image/x-dcraw"],
        "kdc" | "dcr" => &["image/x-kodak-dcr", "image/x-dcraw"],
        "raw" | "rwl" | "x3f" | "mdc" | "erf" => &["image/x-dcraw"],
        "mp4" | "m4v" | "f4v" => &["video/mp4"],
        "mov" | "qt" => &["video/quicktime"],
        "mkv" => &["video/x-matroska"],
        "webm" => &["video/webm"],
        "avi" => &["video/x-msvideo"],
        "wmv" | "asf" => &["video/x-ms-wmv"],
        "mpg" | "mpeg" | "mpe" => &["video/mpeg"],
        "m2ts" | "mts" | "m2t" | "ts" => &["video/mp2t"],
        "ogv" => &["video/ogg"],
        "3gp" => &["video/3gpp"],
        "3g2" => &["video/3gpp2"],
        _ => &[],
    };
    let mut mimes = specific.to_vec();
    if library::is_raw_path(path) {
        for extra in ["image/x-dcraw", "image/tiff", "image/x-adobe-dng"] {
            if !mimes.contains(&extra) {
                mimes.push(extra);
            }
        }
    }
    if library::is_image_path(path) {
        mimes.push("image/*");
    } else if library::is_video_path(path) {
        mimes.push("video/*");
    }
    mimes
}

pub fn launch(app: &App, path: &Path) {
    let id = app.id.clone();
    let exec = app.exec.clone();
    let desktop = app.desktop.clone();
    let path = path.to_path_buf();
    thread::Builder::new()
        .name("lk-open-app".into())
        .spawn(move || {
            if desktop.is_file()
                && launched(
                    Command::new("gio")
                        .arg("launch")
                        .arg(&desktop)
                        .arg(&path),
                )
            {
                return;
            }
            let stem = id.trim_end_matches(".desktop");
            if launched(Command::new("gtk-launch").arg(stem).arg(&path)) {
                return;
            }
            if let Some(argv) = expand_exec(&exec, &path) {
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let _ = cmd.spawn();
            }
        })
        .ok();
}

fn launched(cmd: &mut Command) -> bool {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn parse_desktop(id: &str, desktop: &Path, text: &str) -> Option<(App, Vec<String>)> {
    let mut in_entry = false;
    let mut name = None;
    let mut name_de = None;
    let mut name_en = None;
    let mut exec = None;
    let mut desktop_type = None;
    let mut hidden = false;
    let mut no_display = false;
    let mut mimes = Vec::new();
    let mut icon_name = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Type" => desktop_type = Some(value),
            "Name" => name = Some(value.to_string()),
            "Name[de]" | "Name[de_DE]" | "Name[de_CH]" | "Name[de_AT]" => {
                name_de = Some(value.to_string());
            }
            "Name[en]" | "Name[en_US]" | "Name[en_GB]" => {
                name_en = Some(value.to_string());
            }
            "Exec" => exec = Some(value.to_string()),
            "Icon" => icon_name = Some(value.to_string()),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "MimeType" => {
                mimes = value
                    .split(';')
                    .map(str::trim)
                    .filter(|mime| !mime.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    if hidden || no_display {
        return None;
    }
    if desktop_type.is_some_and(|kind| !kind.eq_ignore_ascii_case("Application")) {
        return None;
    }
    let name = match i18n::lang() {
        i18n::Lang::De => name_de.or(name).or(name_en)?,
        i18n::Lang::En => name_en.or(name).or(name_de)?,
    };
    let exec = exec?;
    if is_self(id) {
        return None;
    }
    Some((
        App {
            id: id.to_string(),
            name,
            icon: icon_candidates(icon_name.as_deref(), id)
                .into_iter()
                .find_map(|name| load_icon(&name)),
            exec,
            desktop: desktop.to_path_buf(),
        },
        mimes,
    ))
}

fn expand_exec(exec: &str, path: &Path) -> Option<Vec<String>> {
    let mut parts = split_exec(exec);
    if parts.is_empty() {
        return None;
    }
    let file = path.to_string_lossy().into_owned();
    let uri = format!("file://{}", path.display());
    let mut placed = false;
    for part in &mut parts {
        if !part.contains('%') {
            continue;
        }
        let next = part
            .replace("%F", &file)
            .replace("%f", &file)
            .replace("%U", &uri)
            .replace("%u", &uri)
            .replace("%%", "\u{0}")
            .replace("%i", "")
            .replace("%c", "")
            .replace("%k", "")
            .replace('\u{0}', "%");
        placed |= next.contains(&file) || next.contains(&uri);
        *part = next;
    }
    parts.retain(|part| !part.starts_with('%') && !part.is_empty());
    if !placed {
        parts.push(file);
    }
    Some(parts)
}

fn split_exec(exec: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = exec.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (_, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share"));
        dirs.push(home.join(".local/share/flatpak/exports/share"));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        dirs.extend(xdg.split(':').filter(|part| !part.is_empty()).map(PathBuf::from));
    }
    dirs.push(PathBuf::from("/usr/local/share"));
    dirs.push(PathBuf::from("/usr/share"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs
}

fn icon_candidates(icon: Option<&str>, id: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(icon) = icon.filter(|name| !name.is_empty()) {
        names.push(icon.to_string());
    }
    let stem = id.trim_end_matches(".desktop");
    if !names.iter().any(|name| name == stem) {
        names.push(stem.to_string());
    }
    if let Some((prefix, _)) = stem.split_once('-')
        && !names.iter().any(|name| name == prefix)
    {
        names.push(prefix.to_string());
    }
    names
}

fn load_icon(name: &str) -> Option<Icon> {
    let path = resolve_icon(name)?;
    let bytes = fs::read(&path).ok()?;
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "svg" => decode_svg(&bytes),
        _ => decode_raster(&bytes),
    }
}

fn resolve_icon(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() && path.is_file() {
        return Some(path.to_path_buf());
    }
    let stem = name.trim_end_matches(".png").trim_end_matches(".svg");
    const SIZES: &[&str] = &[
        "48x48", "32x32", "64x64", "24x24", "22x22", "16x16", "128x128", "256x256", "scalable",
    ];
    for root in icon_roots() {
        if root.ends_with("pixmaps") {
            for ext in ["png", "svg"] {
                let candidate = root.join(format!("{stem}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            continue;
        }
        for size in SIZES {
            let dir = root.join(size).join("apps");
            let png = dir.join(format!("{stem}.png"));
            if png.is_file() {
                return Some(png);
            }
        }
        let svg = root.join("scalable/apps").join(format!("{stem}.svg"));
        if svg.is_file() {
            return Some(svg);
        }
    }
    None
}

fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for data in data_dirs() {
        roots.push(data.join("icons/hicolor"));
        roots.push(data.join("icons/Adwaita"));
        roots.push(data.join("pixmaps"));
    }
    roots
}

fn decode_raster(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let img = if img.width() > 64 || img.height() > 64 {
        image::imageops::resize(&img, 48, 48, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Some(Icon {
        size: [img.width() as usize, img.height() as usize],
        rgba: Arc::from(img.into_raw()),
    })
}

fn decode_svg(bytes: &[u8]) -> Option<Icon> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size().to_int_size();
    let long = size.width().max(size.height()).max(1);
    let scale = 48.0 / long as f32;
    let width = (size.width() as f32 * scale).round().max(1.0) as u32;
    let height = (size.height() as f32 * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(Icon {
        size: [width as usize, height as usize],
        rgba: Arc::from(pixmap.take()),
    })
}

pub fn color_image(icon: &Icon) -> ColorImage {
    ColorImage::from_rgba_unmultiplied(icon.size, &icon.rgba)
}
