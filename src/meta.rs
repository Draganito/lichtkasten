use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use quick_xml::Reader;
use quick_xml::events::Event;

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

    info.overview.push(("Datei".into(), photo.name.clone()));
    if let Some(parent) = photo.path.parent() {
        info.overview
            .push(("Ordner".into(), parent.display().to_string()));
    }
    info.overview
        .push(("Größe".into(), format_bytes(photo.len)));
    info.overview
        .push(("Geändert".into(), format_unix(photo.mtime)));

    if crate::library::is_video_path(&photo.path) {
        if let Some(probe) = crate::video::probe(&photo.path) {
            info.overview
                .push(("Pixel".into(), format!("{} × {}", probe.width, probe.height)));
            if probe.duration > 0.0 {
                info.overview
                    .push(("Dauer".into(), crate::video::format_time(probe.duration)));
            }
        }
    } else if !crate::library::is_raw_path(&photo.path)
        && let Some((w, h)) = image::ImageReader::open(&photo.path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
    {
        info.overview
            .push(("Pixel".into(), format!("{w} × {h}")));
    }

    if let Some(ext) = photo
        .path
        .extension()
        .and_then(|e| e.to_str())
    {
        let kind = if crate::library::is_raw_path(&photo.path) {
            format!("RAW ({})", ext.to_ascii_uppercase())
        } else if crate::library::is_video_path(&photo.path) {
            format!("Video ({})", ext.to_ascii_uppercase())
        } else {
            ext.to_ascii_uppercase()
        };
        info.overview.push(("Format".into(), kind));
    }

    if crate::library::is_video_path(&photo.path) {
        info.albums = crate::albums::read(&photo.path);
        if !info.albums.is_empty() {
            info.overview
                .push(("Alben".into(), info.albums.join(", ")));
        }
        return info;
    }

    let blob = read_head_and_tail(&photo.path).unwrap_or_default();
    info.iptc = extract_iptc(&blob);
    info.xmp = extract_xmp(&blob);

    info.exif = read_exif(&photo.path);
    fill_overview_from_metadata(&mut info);
    info.captured = lookup(&info.exif, "Aufnahmedatum")
        .or_else(|| lookup(&info.exif, "Digitalisiert"))
        .and_then(|s| parse_exif_datetime(&s));
    info.albums = crate::albums::read(&photo.path);
    if !info.albums.is_empty() {
        info.overview
            .push(("Alben".into(), info.albums.join(", ")));
    }
    info
}

pub fn read_capture_stamp(path: &Path) -> Option<u64> {
    let path = path.to_path_buf();
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Metadata::new_from_path(&path)
    }));
    let Ok(Ok(meta)) = parsed else {
        return None;
    };
    let mut digitized = None;
    for tag in &meta {
        let Some((name, value)) = describe_tag(tag) else {
            continue;
        };
        if name == "Aufnahmedatum" {
            if let Some(stamp) = parse_exif_datetime(&value) {
                return Some(stamp);
            }
        } else if name == "Digitalisiert" {
            digitized = parse_exif_datetime(&value);
        }
    }
    digitized
}

fn read_exif(path: &Path) -> Vec<(String, String)> {
    let path = path.to_path_buf();
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Metadata::new_from_path(&path)
    }));
    let Ok(Ok(meta)) = parsed else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for tag in &meta {
        if skip_tag(tag) {
            continue;
        }
        if let Some(row) = describe_tag(tag) {
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    rows.dedup();
    rows
}

fn skip_tag(tag: &ExifTag) -> bool {
    matches!(
        tag,
        ExifTag::StripOffsets(..)
            | ExifTag::StripByteCounts(_)
            | ExifTag::ThumbnailOffset(..)
            | ExifTag::ThumbnailLength(_)
            | ExifTag::ExifOffset(_)
            | ExifTag::GPSInfo(_)
            | ExifTag::InteropOffset(_)
            | ExifTag::MakerNote(_)
            | ExifTag::CFAPattern(_)
            | ExifTag::OECF(_)
            | ExifTag::DeviceSettingDescription(_)
    )
}

fn describe_tag(tag: &ExifTag) -> Option<(String, String)> {
    let raw = format!("{tag:?}");
    let (name, inner) = split_debug(&raw)?;
    if name.starts_with("Unknown") && inner.len() > 80 {
        return None;
    }
    let mut value = pretty_value(&inner);
    if value.is_empty() || value == "[]" {
        return None;
    }
    if name == "Orientation" {
        value = orientation_label(&value);
    }
    Some((human_name(&name), value))
}

fn orientation_label(value: &str) -> String {
    match value.trim() {
        "1" => "Normal".into(),
        "2" => "Gespiegelt".into(),
        "3" => "180°".into(),
        "4" => "Vertikal gespiegelt".into(),
        "5" => "90° + Spiegel".into(),
        "6" => "90° im Uhrzeigersinn".into(),
        "7" => "270° + Spiegel".into(),
        "8" => "90° gegen Uhrzeigersinn".into(),
        other => other.to_string(),
    }
}

fn split_debug(raw: &str) -> Option<(String, String)> {
    let start = raw.find('(')?;
    let name = raw[..start].to_string();
    let inner = raw[start + 1..].trim_end_matches(')').to_string();
    Some((name, inner))
}

fn pretty_value(inner: &str) -> String {
    let trimmed = inner.trim().trim_matches('"').to_string();
    if let Some(pretty) = pretty_rational_list(&trimmed) {
        return pretty;
    }
    trimmed
        .trim_start_matches('[')
        .trim_end_matches(']')
        .replace('"', "")
        .trim()
        .to_string()
}

fn pretty_rational_list(inner: &str) -> Option<String> {
    let mut values = Vec::new();
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            if let Some(end) = inner[i + 1..].find(')') {
                let pair = &inner[i + 1..i + 1 + end];
                let mut parts = pair.split(',');
                let n: f64 = parts.next()?.trim().parse().ok()?;
                let d: f64 = parts.next()?.trim().parse().ok()?;
                values.push(format_rational(n, d));
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

fn format_rational(n: f64, d: f64) -> String {
    if d == 0.0 {
        return n.to_string();
    }
    let v = n / d;
    if n.abs() <= 1.0 && d.abs() > 1.0 && (d / n.abs()).fract() < 0.001 {
        format!("1/{}", (d / n.abs()).round() as i64)
    } else if (v - v.round()).abs() < 0.0001 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn human_name(name: &str) -> String {
    match name {
        "DateTimeOriginal" => "Aufnahmedatum".into(),
        "CreateDate" => "Digitalisiert".into(),
        "ModifyDate" => "EXIF geändert".into(),
        "Make" => "Kamera-Hersteller".into(),
        "Model" => "Kamera".into(),
        "LensModel" => "Objektiv".into(),
        "LensMake" => "Objektiv-Hersteller".into(),
        "ExposureTime" => "Belichtung".into(),
        "FNumber" => "Blende".into(),
        "ISO" => "ISO".into(),
        "FocalLength" => "Brennweite".into(),
        "FocalLengthIn35mmFormat" => "Brennweite (KB)".into(),
        "ExposureCompensation" => "Belichtungskorrektur".into(),
        "Flash" => "Blitz".into(),
        "WhiteBalance" => "Weißabgleich".into(),
        "ColorSpace" => "Farbraum".into(),
        "Artist" => "Autor".into(),
        "Copyright" => "Copyright".into(),
        "ImageDescription" => "Beschreibung".into(),
        "Software" => "Software".into(),
        "Orientation" => "Ausrichtung".into(),
        "ExifImageWidth" => "EXIF-Breite".into(),
        "ExifImageHeight" => "EXIF-Höhe".into(),
        "UserComment" => "Kommentar".into(),
        other => other.to_string(),
    }
}

fn fill_overview_from_metadata(info: &mut FileInfo) {
    const KEYS: &[&str] = &[
        "Aufnahmedatum",
        "Kamera",
        "Kamera-Hersteller",
        "Objektiv",
        "Belichtung",
        "Blende",
        "ISO",
        "Brennweite",
        "Farbraum",
        "Autor",
        "Copyright",
        "Beschreibung",
        "Software",
    ];
    for key in KEYS {
        if let Some(value) = lookup(&info.exif, key).or_else(|| lookup(&info.iptc, key)) {
            if !info.overview.iter().any(|(k, _)| k == key) {
                info.overview.push(((*key).into(), value));
            }
        }
    }
    if let Some(title) = lookup(&info.iptc, "Titel").or_else(|| lookup(&info.xmp, "dc:title")) {
        info.overview.insert(1, ("Titel".into(), title));
    }
    if let Some(caption) = lookup(&info.iptc, "Beschreibung").or_else(|| lookup(&info.xmp, "dc:description"))
        && !info.overview.iter().any(|(k, _)| k == "Beschreibung")
    {
        info.overview.push(("Beschreibung".into(), caption));
    }
}

fn lookup(rows: &[(String, String)], key: &str) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn read_head_and_tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let head = len.min(12 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; head];
    file.read_exact(&mut buf)?;
    if len > head as u64 {
        let tail = 128 * 1024u64;
        let seek = i64::try_from(len.saturating_sub(tail)).unwrap_or(0);
        file.seek(SeekFrom::Start(seek as u64))?;
        let mut end = Vec::new();
        file.read_to_end(&mut end)?;
        buf.extend_from_slice(&end);
    }
    Ok(buf)
}

fn extract_xmp(bytes: &[u8]) -> Vec<(String, String)> {
    let xml = match find_xmp(bytes) {
        Some(xml) => xml,
        None => return Vec::new(),
    };
    parse_xmp(&xml)
}

fn find_xmp(bytes: &[u8]) -> Option<String> {
    let start = find_sub(bytes, b"<?xpacket")
        .or_else(|| find_sub(bytes, b"<x:xmpmeta"))
        .or_else(|| find_sub(bytes, b"<xmpmeta"))?;
    let rest = &bytes[start..];
    let end_rel = find_sub(rest, b"<?xpacket end")
        .or_else(|| find_sub(rest, b"</x:xmpmeta>"))
        .or_else(|| find_sub(rest, b"</xmpmeta>"))?;
    let end = if rest[end_rel..].starts_with(b"<?xpacket end") {
        end_rel + find_sub(&rest[end_rel..], b"?>").unwrap_or(16) + 2
    } else {
        end_rel + 12
    };
    String::from_utf8_lossy(&rest[..end.min(rest.len())])
        .into_owned()
        .into()
}

fn parse_xmp(xml: &str) -> Vec<(String, String)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                push_xmp_attrs(&e, &reader, &mut rows);
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                push_xmp_attrs(&e, &reader, &mut rows);
            }
            Ok(Event::Text(t)) => {
                if let Ok(text) = t.decode() {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if let Some(name) = stack.last() {
                        if skip_xmp_name(name) {
                            continue;
                        }
                        rows.push((short_xmp_name(name), text.to_string()));
                    }
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup();
    rows
}

fn push_xmp_attrs(
    e: &quick_xml::events::BytesStart<'_>,
    reader: &Reader<&[u8]>,
    rows: &mut Vec<(String, String)>,
) {
    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        if key.starts_with("xmlns") || key == "rdf:about" || key == "about" {
            continue;
        }
        if let Ok(value) = attr.decode_and_unescape_value(reader.decoder()) {
            let value = value.trim();
            if !value.is_empty() {
                rows.push((key, value.to_string()));
            }
        }
    }
}

fn skip_xmp_name(name: &str) -> bool {
    matches!(
        name,
        "x:xmpmeta"
            | "xmpmeta"
            | "rdf:RDF"
            | "rdf:Description"
            | "rdf:Bag"
            | "rdf:Seq"
            | "rdf:Alt"
            | "rdf:li"
    )
}

fn short_xmp_name(name: &str) -> String {
    name.to_string()
}

fn extract_iptc(bytes: &[u8]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for block in photoshop_irb_blocks(bytes) {
        rows.extend(parse_iim(&block));
    }
    if rows.is_empty() {
        rows = parse_iim(bytes);
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup();
    rows
}

fn photoshop_irb_blocks(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = find_sub(&bytes[i..], b"8BIM") {
        let at = i + rel;
        if at + 10 > bytes.len() {
            break;
        }
        let id = u16::from_be_bytes([bytes[at + 4], bytes[at + 5]]);
        let name_len = bytes[at + 6] as usize;
        let mut pos = at + 7 + name_len;
        if name_len % 2 == 0 {
            pos += 1;
        }
        if pos + 4 > bytes.len() {
            break;
        }
        let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap_or([0; 4])) as usize;
        pos += 4;
        if pos + size > bytes.len() {
            break;
        }
        if id == 0x0404 {
            out.push(bytes[pos..pos + size].to_vec());
        }
        i = pos + size + (size % 2);
    }
    out
}

fn parse_iim(data: &[u8]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i + 5 <= data.len() {
        if data[i] != 0x1C {
            i += 1;
            continue;
        }
        let record = data[i + 1];
        let dataset = data[i + 2];
        let (size, hdr) = if data[i + 3] & 0x80 != 0 {
            let n = (data[i + 3] & 0x7F) as usize;
            if n == 0 || i + 4 + n > data.len() {
                i += 1;
                continue;
            }
            let mut size = 0usize;
            for b in &data[i + 4..i + 4 + n] {
                size = (size << 8) | *b as usize;
            }
            (size, 4 + n)
        } else {
            let size = u16::from_be_bytes([data[i + 3], data[i + 4]]) as usize;
            (size, 5)
        };
        let start = i + hdr;
        if start + size > data.len() {
            break;
        }
        if record == 2 {
            let text = String::from_utf8_lossy(&data[start..start + size])
                .trim()
                .to_string();
            if !text.is_empty() {
                rows.push((iptc_name(dataset), text));
            }
        }
        i = start + size;
    }
    rows
}

fn iptc_name(dataset: u8) -> String {
    match dataset {
        5 => "Titel".into(),
        7 => "Edit-Status".into(),
        10 => "Dringlichkeit".into(),
        15 => "Kategorie".into(),
        20 => "Ergänzung".into(),
        22 => "Fixture-ID".into(),
        25 => "Stichwort".into(),
        40 => "Anweisungen".into(),
        55 => "Datum".into(),
        60 => "Zeit".into(),
        80 => "Autor".into(),
        85 => "Autoren-Titel".into(),
        90 => "Stadt".into(),
        92 => "Region".into(),
        95 => "Bundesland".into(),
        100 => "Land".into(),
        101 => "Land (Name)".into(),
        103 => "Übertragungsreferenz".into(),
        105 => "Überschrift".into(),
        110 => "Credit".into(),
        115 => "Quelle".into(),
        116 => "Copyright".into(),
        120 => "Beschreibung".into(),
        122 => "Autor der Beschreibung".into(),
        other => format!("IPTC {other}"),
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
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
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
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

fn parse_exif_datetime(text: &str) -> Option<u64> {
    let digits: String = text
        .chars()
        .map(|c| if c.is_ascii_digit() { c } else { ' ' })
        .collect();
    let mut parts = digits.split_whitespace();
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    let hh: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let mm: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let ss: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    Some((days * 86400 + (hh * 3600 + mm * 60 + ss) as i64).max(0) as u64)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + i64::from(doe) - 719468
}
