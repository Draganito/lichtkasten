use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "gif", "bmp"];

pub const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "kdc", "mdc", "mef",
    "mos", "mrw", "nef", "nrw", "orf", "ori", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf",
    "srw", "x3f",
];

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "asf", "avi", "divx", "dv", "f4v", "flv", "m2t", "m2ts", "m4v", "mkv", "mod",
    "mov", "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt", "rm", "rmvb", "tod", "ts",
    "vob", "webm", "wmv",
];

#[derive(Clone, Debug)]
pub struct Photo {
    pub path: PathBuf,
    pub name: String,
    pub mtime: u64,
    pub len: u64,
    pub capture: Option<u64>,
    pub capture_known: bool,
    pub albums: Vec<String>,
    pub albums_known: bool,
    pub covers: Vec<String>,
}

impl Photo {
    pub fn sort_time(&self) -> u64 {
        self.capture.unwrap_or(self.mtime)
    }
}

#[derive(Clone, Debug)]
pub struct DirNode {
    pub path: PathBuf,
    pub name: String,
    pub has_subdirs: bool,
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| exts.iter().any(|ok| ext.eq_ignore_ascii_case(ok)))
}

pub fn is_raw_path(path: &Path) -> bool {
    has_ext(path, RAW_EXTENSIONS)
}

pub fn is_image_path(path: &Path) -> bool {
    has_ext(path, IMAGE_EXTENSIONS) || is_raw_path(path)
}

pub fn is_video_path(path: &Path) -> bool {
    has_ext(path, VIDEO_EXTENSIONS)
}

pub fn is_media_path(path: &Path) -> bool {
    is_image_path(path) || is_video_path(path)
}

pub fn scan_photos(dir: &Path) -> std::io::Result<Vec<Photo>> {
    if crate::cache::is_app_dir(dir) {
        return Ok(Vec::new());
    }
    scan_photos_unchecked(dir)
}

/// Wie `scan_photos`, liest aber auch innerhalb von `.lichtkasten` (Papierkorb).
pub fn scan_photos_unchecked(dir: &Path) -> std::io::Result<Vec<Photo>> {
    let mut photos = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_media_path(&path) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) if meta.is_file() => meta,
            _ => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        photos.push(photo_from_meta(path, name, mtime, meta.len()));
    }
    Ok(photos)
}

pub fn scan_subdirs(dir: &Path) -> std::io::Result<Vec<DirNode>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let has_subdirs = dir_has_subdirs(&path);
        dirs.push(DirNode {
            path,
            name,
            has_subdirs,
        });
    }
    dirs.sort_by_key(|a| a.name.to_lowercase());
    Ok(dirs)
}

fn dir_has_subdirs(path: &Path) -> bool {
    let Ok(iter) = fs::read_dir(path) else {
        return false;
    };
    for (seen, child) in iter.flatten().enumerate() {
        if seen >= 80 {
            return true;
        }
        if child.file_type().ok().is_some_and(|kind| kind.is_dir())
            && !child
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.'))
        {
            return true;
        }
    }
    false
}

pub fn photo_from_path(path: &Path) -> Option<Photo> {
    if !is_media_path(path) {
        return None;
    }
    let meta = path.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(photo_from_meta(path.to_path_buf(), name, mtime, meta.len()))
}

fn photo_from_meta(path: PathBuf, name: String, mtime: u64, len: u64) -> Photo {
    Photo {
        path,
        name,
        mtime,
        len,
        capture: None,
        capture_known: false,
        albums: Vec::new(),
        albums_known: false,
        covers: Vec::new(),
    }
}
