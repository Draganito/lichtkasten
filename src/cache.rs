use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const VERSION: &str = "fit-v3-folder";
pub const DIR_NAME: &str = ".lichtkasten";
const THUMBS: &str = "miniaturen";

/// Miniaturen liegen im angeschauten Ordner, nicht auf dem Rechner.
/// Dateien im Papierkorb nutzen weiter den Cache ihres Ursprungsordners
/// (Name, mtime und Länge bleiben beim Rename gleich).
pub fn dir_for(photo: &Path) -> PathBuf {
    let folder = crate::trash::origin_folder(photo)
        .unwrap_or_else(|| photo.parent().unwrap_or(photo).to_path_buf());
    folder.join(DIR_NAME).join(THUMBS)
}

pub fn is_app_dir(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(DIR_NAME))
    })
}

pub fn file_name(path: &Path, mtime: u64, len: u64, edge: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update(mtime.to_le_bytes());
    hasher.update(len.to_le_bytes());
    hasher.update(edge.to_le_bytes());
    hasher.update(VERSION.as_bytes());
    format!("{:x}.jpg", hasher.finalize())
}

pub fn file(dir: &Path, path: &Path, mtime: u64, len: u64, edge: u32) -> PathBuf {
    dir.join(file_name(path, mtime, len, edge))
}

pub fn read(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 32 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    Some(bytes)
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("jpg.part");
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}
