//! File-type classification by extension. Shared lists for the folder scan
//! on the desktop and the MIME fallback on Android.

use std::path::Path;

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

/// Extension of a file name (or path string), lower-cased, without the dot.
pub fn extension_of(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (_, ext) = base.rsplit_once('.')?;
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

fn name_has_ext(name: &str, exts: &[&str]) -> bool {
    extension_of(name).is_some_and(|ext| exts.contains(&ext.as_str()))
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

pub fn is_raw_name(name: &str) -> bool {
    name_has_ext(name, RAW_EXTENSIONS)
}

pub fn is_image_name(name: &str) -> bool {
    name_has_ext(name, IMAGE_EXTENSIONS) || is_raw_name(name)
}

pub fn is_video_name(name: &str) -> bool {
    name_has_ext(name, VIDEO_EXTENSIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_names() {
        assert!(is_raw_name("IMG_0001.NEF"));
        assert!(is_raw_name("/storage/emulated/0/DCIM/a.dng"));
        assert!(is_image_name("x.TIF"));
        assert!(!is_image_name("x.mp4"));
        assert!(is_video_name("clip.MOV"));
        assert!(!is_raw_name("noext"));
        assert!(!is_raw_name("trailing."));
        assert_eq!(extension_of("a/b.c/d.JPG").as_deref(), Some("jpg"));
    }

    #[test]
    fn classifies_paths() {
        assert!(is_raw_path(Path::new("a.ARW")));
        assert!(is_image_path(Path::new("a.jpeg")));
        assert!(is_video_path(Path::new("a.webm")));
        assert!(!is_media_path(Path::new("a.txt")));
    }
}
