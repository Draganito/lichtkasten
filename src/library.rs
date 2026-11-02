use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "gif", "bmp",
];

pub const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "kdc",
    "mdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "ori", "pef", "raf", "raw",
    "rw2", "rwl", "sr2", "srf", "srw", "x3f",
];

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "asf", "avi", "divx", "dv", "f4v", "flv", "m2t", "m2ts", "m4v",
    "mkv", "mod", "mov", "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt",
    "rm", "rmvb", "tod", "ts", "vob", "webm", "wmv",
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
    let mut photos = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !is_media_path(&path) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
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
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let has_subdirs = fs::read_dir(&path).ok().is_some_and(|iter| {
            iter.flatten().any(|child| {
                child.path().is_dir()
                    && child
                        .file_name()
                        .to_str()
                        .is_none_or(|n| !n.starts_with('.'))
            })
        });
        dirs.push(DirNode {
            path,
            name,
            has_subdirs,
        });
    }
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(dirs)
}

pub fn photo_from_path(path: &Path) -> Option<Photo> {
    if !path.is_file() || !is_media_path(path) {
        return None;
    }
    let meta = path.metadata().ok()?;
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
    let albums = crate::albums::read_fast(&path).unwrap_or_default();
    let albums_known = !albums.is_empty() || crate::albums::sidecar_path(&path).exists();
    Photo {
        path,
        name,
        mtime,
        len,
        capture: None,
        capture_known: false,
        albums,
        albums_known,
    }
}

pub fn pictures_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|dirs| dirs.picture_dir().map(Path::to_path_buf))
}
