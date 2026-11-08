//! Metadaten für Info-Panel und Suchindex. Das Lesen übernimmt
//! `lichtkasten_core::meta` (sprachneutrale Schlüssel); hier werden die
//! Schlüssel übersetzt und die Übersicht zusammengestellt.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lichtkasten_core::meta as core_meta;

use crate::i18n;
use crate::library::Photo;

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub path: PathBuf,
    pub overview: Vec<(String, String)>,
    pub exif: Vec<(String, String)>,
    pub iptc: Vec<(String, String)>,
    pub xmp: Vec<(String, String)>,
    pub captured: Option<u64>,
    pub albums: Vec<String>,
}

pub fn read_file_info(photo: &Photo) -> FileInfo {
    let mut info = FileInfo {
        path: photo.path.clone(),
        ..FileInfo::default()
    };

    info.overview
        .push((i18n::t("Datei", "File").into(), photo.name.clone()));
    if let Some(parent) = photo.path.parent() {
        info.overview.push((
            i18n::t("Ordner", "Folder").into(),
            parent.display().to_string(),
        ));
    }
    info.overview
        .push((i18n::t("Größe", "Size").into(), format_bytes(photo.len)));
    info.overview.push((
        i18n::t("Geändert", "Modified").into(),
        format_unix(photo.mtime),
    ));

    if crate::library::is_video_path(&photo.path) {
        if let Some(probe) = crate::video::probe(&photo.path) {
            info.overview.push((
                i18n::t("Pixel", "Pixels").into(),
                format!("{} × {}", probe.width, probe.height),
            ));
            if probe.duration > 0.0 {
                info.overview.push((
                    i18n::t("Dauer", "Duration").into(),
                    crate::video::format_time(probe.duration),
                ));
            }
        }
    } else if !crate::library::is_raw_path(&photo.path)
        && let Some((w, h)) = image::ImageReader::open(&photo.path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
    {
        info.overview
            .push((i18n::t("Pixel", "Pixels").into(), format!("{w} × {h}")));
    }

    if let Some(ext) = photo.path.extension().and_then(|e| e.to_str()) {
        let kind = if crate::library::is_raw_path(&photo.path) {
            format!("RAW ({})", ext.to_ascii_uppercase())
        } else if crate::library::is_video_path(&photo.path) {
            format!("Video ({})", ext.to_ascii_uppercase())
        } else {
            ext.to_ascii_uppercase()
        };
        info.overview
            .push((i18n::t("Format", "Format").into(), kind));
    }

    if crate::library::is_video_path(&photo.path) {
        info.albums = crate::albums::read(&photo.path);
        if !info.albums.is_empty() {
            info.overview
                .push((i18n::t("Alben", "Albums").into(), info.albums.join(", ")));
        }
        return info;
    }

    let blob = core_meta::read_blob_path(&photo.path).unwrap_or_default();
    info.iptc = localize_iptc(core_meta::extract_iptc(&blob));
    info.xmp = core_meta::extract_xmp(&blob);

    let exif = core_meta::read_exif_path(&photo.path);
    info.captured = core_meta::capture_stamp(&exif);
    info.exif = localize_exif(exif);
    fill_overview_from_metadata(&mut info);
    info.albums = crate::albums::read(&photo.path);
    if !info.albums.is_empty() {
        info.overview
            .push((i18n::t("Alben", "Albums").into(), info.albums.join(", ")));
    }
    info
}

/// Textfelder für den Suchindex. Sprachneutral: nur Werte, keine Beschriftungen.
/// Kein Video-Probe (zu teuer), kein Bildmaß.
pub struct IndexFields {
    pub captured: Option<u64>,
    pub albums: String,
    pub meta: String,
}

pub fn index_fields(path: &Path) -> IndexFields {
    let tags = crate::albums::read_tags(path);
    let albums = tags.albums.join(" ");
    if crate::library::is_video_path(path) {
        return IndexFields {
            captured: None,
            albums,
            meta: String::new(),
        };
    }
    let blob = core_meta::read_blob_path(path).unwrap_or_default();
    let exif = core_meta::read_exif_path(path);
    let captured = core_meta::capture_stamp(&exif);
    let mut meta = String::new();
    for (key, value) in exif
        .iter()
        .chain(core_meta::extract_iptc(&blob).iter())
        .chain(core_meta::extract_xmp(&blob).iter())
    {
        // Zahlenwüsten (Rationale, Offsets) tragen nichts zur Suche bei.
        if value.len() > 400
            || value
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '/' | '.' | ' ' | ',' | '-' | '+' | ':'))
        {
            continue;
        }
        if key.starts_with("Unknown") {
            continue;
        }
        meta.push_str(value);
        meta.push('\n');
    }
    IndexFields {
        captured,
        albums,
        meta,
    }
}

pub fn read_capture_stamp(path: &Path) -> Option<u64> {
    core_meta::read_capture_stamp_path(path)
}

/// Neutrale EXIF-Schlüssel → Anzeige. Die Reihenfolge folgt danach dem
/// übersetzten Namen, wie bisher.
fn localize_exif(rows: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = rows
        .into_iter()
        .map(|(key, value)| {
            let value = if key == core_meta::KEY_ORIENTATION {
                orientation_label(&value)
            } else {
                value
            };
            (human_name(&key), value)
        })
        .collect();
    rows.sort_by_key(|a| a.0.to_lowercase());
    rows.dedup();
    rows
}

fn localize_iptc(rows: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = rows
        .into_iter()
        .map(|(key, value)| (iptc_name(&key), value))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup();
    rows
}

fn orientation_label(value: &str) -> String {
    match value.trim() {
        "1" => i18n::t("Normal", "Normal").into(),
        "2" => i18n::t("Gespiegelt", "Mirrored").into(),
        "3" => "180°".into(),
        "4" => i18n::t("Vertikal gespiegelt", "Flipped vertically").into(),
        "5" => i18n::t("90° + Spiegel", "90° + mirror").into(),
        "6" => i18n::t("90° im Uhrzeigersinn", "90° clockwise").into(),
        "7" => i18n::t("270° + Spiegel", "270° + mirror").into(),
        "8" => i18n::t("90° gegen Uhrzeigersinn", "90° counterclockwise").into(),
        other => other.to_string(),
    }
}

fn human_name(name: &str) -> String {
    match name {
        "DateTimeOriginal" => i18n::t("Aufnahmedatum", "Date taken").into(),
        "CreateDate" => i18n::t("Digitalisiert", "Digitized").into(),
        "ModifyDate" => i18n::t("EXIF geändert", "EXIF modified").into(),
        "Make" => i18n::t("Kamera-Hersteller", "Camera make").into(),
        "Model" => i18n::t("Kamera", "Camera").into(),
        "LensModel" => i18n::t("Objektiv", "Lens").into(),
        "LensMake" => i18n::t("Objektiv-Hersteller", "Lens make").into(),
        "ExposureTime" => i18n::t("Belichtung", "Exposure").into(),
        "FNumber" => i18n::t("Blende", "Aperture").into(),
        "ISO" => "ISO".into(),
        "FocalLength" => i18n::t("Brennweite", "Focal length").into(),
        "FocalLengthIn35mmFormat" => i18n::t("Brennweite (KB)", "Focal length (35mm)").into(),
        "ExposureCompensation" => i18n::t("Belichtungskorrektur", "Exposure compensation").into(),
        "Flash" => i18n::t("Blitz", "Flash").into(),
        "WhiteBalance" => i18n::t("Weißabgleich", "White balance").into(),
        "ColorSpace" => i18n::t("Farbraum", "Color space").into(),
        "Artist" => i18n::t("Autor", "Artist").into(),
        "Copyright" => "Copyright".into(),
        "ImageDescription" => i18n::t("Beschreibung", "Description").into(),
        "Software" => "Software".into(),
        "Orientation" => i18n::t("Ausrichtung", "Orientation").into(),
        "ExifImageWidth" => i18n::t("EXIF-Breite", "EXIF width").into(),
        "ExifImageHeight" => i18n::t("EXIF-Höhe", "EXIF height").into(),
        "UserComment" => i18n::t("Kommentar", "Comment").into(),
        other => other.to_string(),
    }
}

fn iptc_name(key: &str) -> String {
    match key {
        "ObjectName" => i18n::t("Titel", "Title").into(),
        "EditStatus" => i18n::t("Edit-Status", "Edit status").into(),
        "Urgency" => i18n::t("Dringlichkeit", "Urgency").into(),
        "Category" => i18n::t("Kategorie", "Category").into(),
        "SupplementalCategories" => i18n::t("Ergänzung", "Supplemental category").into(),
        "FixtureIdentifier" => i18n::t("Fixture-ID", "Fixture ID").into(),
        "Keywords" => i18n::t("Stichwort", "Keyword").into(),
        "SpecialInstructions" => i18n::t("Anweisungen", "Special instructions").into(),
        "DateCreated" => i18n::t("Datum", "Date").into(),
        "TimeCreated" => i18n::t("Zeit", "Time").into(),
        "By-line" => i18n::t("Autor", "Artist").into(),
        "By-lineTitle" => i18n::t("Autoren-Titel", "Byline title").into(),
        "City" => i18n::t("Stadt", "City").into(),
        "Sub-location" => i18n::t("Region", "Region").into(),
        "Province-State" => i18n::t("Bundesland", "Province/State").into(),
        "Country-PrimaryLocationCode" => i18n::t("Land", "Country code").into(),
        "Country-PrimaryLocationName" => i18n::t("Land (Name)", "Country").into(),
        "OriginalTransmissionReference" => {
            i18n::t("Übertragungsreferenz", "Transmission reference").into()
        }
        "Headline" => i18n::t("Überschrift", "Headline").into(),
        "Credit" => "Credit".into(),
        "Source" => i18n::t("Quelle", "Source").into(),
        "CopyrightNotice" => "Copyright".into(),
        "Caption-Abstract" => i18n::t("Beschreibung", "Description").into(),
        "Writer-Editor" => i18n::t("Autor der Beschreibung", "Caption writer").into(),
        other => other.to_string(),
    }
}

fn fill_overview_from_metadata(info: &mut FileInfo) {
    let keys = [
        i18n::t("Aufnahmedatum", "Date taken"),
        i18n::t("Kamera", "Camera"),
        i18n::t("Kamera-Hersteller", "Camera make"),
        i18n::t("Objektiv", "Lens"),
        i18n::t("Belichtung", "Exposure"),
        i18n::t("Blende", "Aperture"),
        "ISO",
        i18n::t("Brennweite", "Focal length"),
        i18n::t("Farbraum", "Color space"),
        i18n::t("Autor", "Artist"),
        "Copyright",
        i18n::t("Beschreibung", "Description"),
        "Software",
    ];
    for key in keys {
        if let Some(value) = lookup(&info.exif, key).or_else(|| lookup(&info.iptc, key))
            && !info.overview.iter().any(|(k, _)| k == key)
        {
            info.overview.push((key.into(), value));
        }
    }
    let title_key = i18n::t("Titel", "Title");
    let caption_key = i18n::t("Beschreibung", "Description");
    if let Some(title) = lookup(&info.iptc, title_key).or_else(|| lookup(&info.xmp, "dc:title")) {
        info.overview.insert(1, (title_key.into(), title));
    }
    if let Some(caption) =
        lookup(&info.iptc, caption_key).or_else(|| lookup(&info.xmp, "dc:description"))
        && !info.overview.iter().any(|(k, _)| k == caption_key)
    {
        info.overview.push((caption_key.into(), caption));
    }
}

fn lookup(rows: &[(String, String)], key: &str) -> Option<String> {
    core_meta::lookup(rows, key)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_unix(secs: u64) -> String {
    let Some(time) = UNIX_EPOCH.checked_add(Duration::from_secs(secs)) else {
        return secs.to_string();
    };
    let elapsed = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = elapsed / 86400;
    let tod = elapsed % 86400;
    let (y, m, d) = civil_from_days(days as i64);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC")
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}
