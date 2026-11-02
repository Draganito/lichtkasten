//! Papierkorb: Dateien werden per Rename nach `<Ordner>/.lichtkasten/papierkorb/`
//! verschoben. Nichts verlässt das Laufwerk, nichts landet auf dem Rechner.
//! Wiederherstellen ist das Rename zurück; Leeren löscht endgültig.

use std::fs;
use std::path::{Path, PathBuf};

use crate::albums;
use crate::cache;
use crate::library::{self, Photo};

pub const DIR_NAME: &str = "papierkorb";

pub fn dir_for_folder(folder: &Path) -> PathBuf {
    folder.join(cache::DIR_NAME).join(DIR_NAME)
}

/// `<folder>/.lichtkasten/papierkorb/x.jpg` → `Some(<folder>)`.
pub fn origin_folder(trashed: &Path) -> Option<PathBuf> {
    let bin = trashed.parent()?;
    if bin.file_name()?.to_str()? != DIR_NAME {
        return None;
    }
    let app_dir = bin.parent()?;
    if !app_dir
        .file_name()?
        .to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(cache::DIR_NAME))
    {
        return None;
    }
    app_dir.parent().map(Path::to_path_buf)
}

pub fn is_trashed(path: &Path) -> bool {
    origin_folder(path).is_some()
}

/// Verschiebt eine Datei (und ihr XMP-Sidecar) in den Papierkorb ihres Ordners.
pub fn put(path: &Path) -> Result<PathBuf, String> {
    let folder = path
        .parent()
        .ok_or_else(|| "no parent folder".to_string())?;
    let bin = dir_for_folder(folder);
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    move_with_sidecar(path, &bin)
}

/// Holt eine Datei aus dem Papierkorb zurück in ihren Ursprungsordner.
pub fn restore(trashed: &Path) -> Result<PathBuf, String> {
    let folder = origin_folder(trashed).ok_or_else(|| "not in trash".to_string())?;
    move_with_sidecar(trashed, &folder)
}

/// Löscht endgültig (Datei + Sidecar). Nur für Pfade im Papierkorb.
pub fn delete_forever(trashed: &Path) -> Result<(), String> {
    if !is_trashed(trashed) {
        return Err("refusing to delete outside trash".into());
    }
    fs::remove_file(trashed).map_err(|e| e.to_string())?;
    let sidecar = albums::sidecar_path(trashed);
    if sidecar != trashed && sidecar.is_file() {
        let _ = fs::remove_file(sidecar);
    }
    Ok(())
}

/// Sammelt alle Papierkörbe unter `root` (rekursiv, nur Verzeichnisse werden gelesen).
pub fn collect(root: &Path) -> Vec<Photo> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0u8)];
    while let Some((dir, depth)) = stack.pop() {
        if let Ok(photos) = library::scan_photos_unchecked(&dir_for_folder(&dir)) {
            out.extend(photos);
        }
        if depth >= 24 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
                continue;
            }
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            stack.push((entry.path(), depth + 1));
        }
    }
    out.sort_by_cached_key(|photo| photo.name.to_lowercase());
    out
}

fn move_with_sidecar(from: &Path, into: &Path) -> Result<PathBuf, String> {
    let name = from
        .file_name()
        .ok_or_else(|| "file without name".to_string())?;
    let target = unique_target(into, Path::new(name));
    fs::rename(from, &target).map_err(|e| e.to_string())?;
    let sidecar = albums::sidecar_path(from);
    if sidecar != from && sidecar.is_file() {
        let sidecar_target = albums::sidecar_path(&target);
        if !sidecar_target.exists() {
            let _ = fs::rename(&sidecar, &sidecar_target);
        }
    }
    Ok(target)
}

/// `name.jpg`, bei Kollision `name (2).jpg`, `name (3).jpg`, …
fn unique_target(dir: &Path, name: &Path) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = name
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}
