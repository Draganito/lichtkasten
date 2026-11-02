use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

static LANG: OnceLock<Lang> = OnceLock::new();

pub fn init() {
    let _ = LANG.get_or_init(detect);
}

pub fn lang() -> Lang {
    *LANG.get_or_init(detect)
}

pub fn t(de: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::De => de,
        Lang::En => en,
    }
}

fn detect() -> Lang {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        let Ok(value) = std::env::var(var) else {
            continue;
        };
        for part in value.split(':') {
            let tag = part.split('.').next().unwrap_or("").trim();
            if tag.is_empty() || tag.eq_ignore_ascii_case("C") || tag.eq_ignore_ascii_case("POSIX")
            {
                continue;
            }
            let code = tag.split(['_', '-']).next().unwrap_or("");
            if code.eq_ignore_ascii_case("de") {
                return Lang::De;
            }
            return Lang::En;
        }
    }
    Lang::En
}

pub fn reading_path(path: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("Lese {path} …"),
        Lang::En => format!("Reading {path}…"),
    }
}

pub fn folder_unreadable(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("Ordner nicht lesbar: {err}"),
        Lang::En => format!("Folder unreadable: {err}"),
    }
}

pub fn album_unreadable(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("Album nicht lesbar: {err}"),
        Lang::En => format!("Album unreadable: {err}"),
    }
}

pub fn added_to_album(name: &str) -> String {
    match lang() {
        Lang::De => format!("Zu „{name}“ gelegt"),
        Lang::En => format!("Added to “{name}”"),
    }
}

pub fn cover_set(name: &str) -> String {
    match lang() {
        Lang::De => format!("Deckblatt für „{name}“"),
        Lang::En => format!("Cover set for “{name}”"),
    }
}

pub fn reading_album(name: &str) -> String {
    match lang() {
        Lang::De => format!("Lese Album {name} …"),
        Lang::En => format!("Reading album {name}…"),
    }
}

pub fn selected_count(n: usize) -> String {
    match lang() {
        Lang::De => format!("{n} gewählt"),
        Lang::En if n == 1 => "1 selected".into(),
        Lang::En => format!("{n} selected"),
    }
}

pub fn thumbnail_count(n: usize) -> String {
    match lang() {
        Lang::De => format!("{n} Vorschaubilder"),
        Lang::En if n == 1 => "1 thumbnail".into(),
        Lang::En => format!("{n} thumbnails"),
    }
}

pub fn media_count(stills: usize, videos: usize) -> String {
    match lang() {
        Lang::De => match (stills, videos) {
            (0, 0) => "Keine Medien".into(),
            (1, 0) => "1 Foto".into(),
            (n, 0) => format!("{n} Fotos"),
            (0, 1) => "1 Video".into(),
            (0, n) => format!("{n} Videos"),
            (1, 1) => "1 Foto · 1 Video".into(),
            (s, 1) => format!("{s} Fotos · 1 Video"),
            (1, v) => format!("1 Foto · {v} Videos"),
            (s, v) => format!("{s} Fotos · {v} Videos"),
        },
        Lang::En => match (stills, videos) {
            (0, 0) => "No media".into(),
            (1, 0) => "1 photo".into(),
            (n, 0) => format!("{n} photos"),
            (0, 1) => "1 video".into(),
            (0, n) => format!("{n} videos"),
            (1, 1) => "1 photo · 1 video".into(),
            (s, 1) => format!("{s} photos · 1 video"),
            (1, v) => format!("1 photo · {v} videos"),
            (s, v) => format!("{s} photos · {v} videos"),
        },
    }
}

pub fn moved_to_trash(n: usize) -> String {
    match lang() {
        Lang::De if n == 1 => "1 Datei in den Papierkorb gelegt".into(),
        Lang::De => format!("{n} Dateien in den Papierkorb gelegt"),
        Lang::En if n == 1 => "1 file moved to trash".into(),
        Lang::En => format!("{n} files moved to trash"),
    }
}

pub fn trash_failed(path: &std::path::Path, err: impl std::fmt::Display) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    match lang() {
        Lang::De => format!("„{name}“ konnte nicht in den Papierkorb: {err}"),
        Lang::En => format!("Could not move “{name}” to trash: {err}"),
    }
}

pub fn restore_failed(path: &std::path::Path, err: impl std::fmt::Display) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    match lang() {
        Lang::De => format!("„{name}“ konnte nicht wiederhergestellt werden: {err}"),
        Lang::En => format!("Could not restore “{name}”: {err}"),
    }
}

pub fn empty_failed(n: usize) -> String {
    match lang() {
        Lang::De => format!("{n} Dateien konnten nicht gelöscht werden"),
        Lang::En => format!("{n} files could not be deleted"),
    }
}

pub fn trash_count(n: usize) -> String {
    match lang() {
        Lang::De if n == 1 => "1 Datei im Papierkorb".into(),
        Lang::De => format!("{n} Dateien im Papierkorb"),
        Lang::En if n == 1 => "1 file in trash".into(),
        Lang::En => format!("{n} files in trash"),
    }
}

pub fn confirm_empty(n: usize) -> String {
    match lang() {
        Lang::De if n == 1 => "1 Datei endgültig löschen?".into(),
        Lang::De => format!("{n} Dateien endgültig löschen?"),
        Lang::En if n == 1 => "Permanently delete 1 file?".into(),
        Lang::En => format!("Permanently delete {n} files?"),
    }
}

pub fn hits(n: usize) -> String {
    match lang() {
        Lang::De if n == 1 => "1 Treffer".into(),
        Lang::De => format!("{n} Treffer"),
        Lang::En if n == 1 => "1 match".into(),
        Lang::En => format!("{n} matches"),
    }
}

pub fn indexing(done: usize, total: usize) -> String {
    match lang() {
        Lang::De => format!("Index {done} / {total}"),
        Lang::En => format!("Indexing {done} / {total}"),
    }
}

pub fn missing_plugin(name: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("{name} fehlt"),
        Lang::En => format!("{name} is missing"),
    }
}

pub fn gstreamer_missing(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("GStreamer fehlt: {err}"),
        Lang::En => format!("GStreamer is missing: {err}"),
    }
}

pub fn gstreamer_app_missing(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("GStreamer App fehlt: {err}"),
        Lang::En => format!("GStreamer App is missing: {err}"),
    }
}

pub fn glib_missing(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("GLib fehlt: {err}"),
        Lang::En => format!("GLib is missing: {err}"),
    }
}

pub fn gobject_missing(err: impl std::fmt::Display) -> String {
    match lang() {
        Lang::De => format!("GObject fehlt: {err}"),
        Lang::En => format!("GObject is missing: {err}"),
    }
}
