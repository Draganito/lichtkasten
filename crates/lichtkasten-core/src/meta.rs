//! EXIF, IPTC and XMP as language-neutral `(key, value)` rows. Keys are the
//! tag identifiers (`DateTimeOriginal`, `Caption-Abstract`, `dc:title`);
//! the apps translate them for display. Values are already human-readable
//! (rationals reduced, lists joined).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::decode::{DecodeLimits, stream_len};

/// How much of a file the IPTC/XMP scan reads: the head and the tail,
/// because JPEG puts APP segments first and TIFF-based files put the IFDs
/// at either end.
const HEAD_BYTES: u64 = 12 * 1024 * 1024;
const TAIL_BYTES: u64 = 128 * 1024;

/// EXIF key that holds the capture time.
pub const KEY_DATE_TIME_ORIGINAL: &str = "DateTimeOriginal";
/// EXIF key that holds the digitization time (fallback for the capture time).
pub const KEY_CREATE_DATE: &str = "CreateDate";
pub const KEY_ORIENTATION: &str = "Orientation";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub exif: Vec<(String, String)>,
    pub iptc: Vec<(String, String)>,
    pub xmp: Vec<(String, String)>,
    /// Unix seconds from `DateTimeOriginal`, else `CreateDate`.
    pub captured: Option<u64>,
}

/// Everything from one seekable stream (Android: a file descriptor). The
/// EXIF parser needs the whole file in memory; above `limits.max_file`
/// only IPTC/XMP from head and tail are returned.
pub fn read_metadata<R: Read + Seek>(reader: &mut R, limits: &DecodeLimits) -> std::io::Result<Metadata> {
    let len = stream_len(reader)?;
    let (exif, blob) = if len <= limits.max_file {
        reader.seek(SeekFrom::Start(0))?;
        let mut all = Vec::new();
        reader.read_to_end(&mut all)?;
        let exif = read_exif_bytes(&all);
        (exif, head_and_tail_of(all))
    } else {
        (Vec::new(), read_head_and_tail(reader, len)?)
    };
    let captured = capture_stamp(&exif);
    Ok(Metadata {
        iptc: extract_iptc(&blob),
        xmp: extract_xmp(&blob),
        exif,
        captured,
    })
}

/// Head and tail of a file, see [`HEAD_BYTES`] / [`TAIL_BYTES`].
pub fn read_blob_path(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    read_head_and_tail(&mut file, len)
}

fn read_head_and_tail<R: Read + Seek>(reader: &mut R, len: u64) -> std::io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(0))?;
    let head = len.min(HEAD_BYTES);
    let mut buf = Vec::with_capacity(head as usize);
    reader.by_ref().take(head).read_to_end(&mut buf)?;
    if len > head {
        let seek = len.saturating_sub(TAIL_BYTES).max(head);
        reader.seek(SeekFrom::Start(seek))?;
        reader.read_to_end(&mut buf)?;
    }
    Ok(buf)
}

fn head_and_tail_of(mut all: Vec<u8>) -> Vec<u8> {
    let len = all.len() as u64;
    if len <= HEAD_BYTES {
        return all;
    }
    let tail_start = len.saturating_sub(TAIL_BYTES).max(HEAD_BYTES) as usize;
    let tail = all.split_off(tail_start);
    all.truncate(HEAD_BYTES as usize);
    all.extend_from_slice(&tail);
    all
}

/// EXIF rows from a path, neutral keys, sorted by key, deduplicated.
pub fn read_exif_path(path: &Path) -> Vec<(String, String)> {
    let path = path.to_path_buf();
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        little_exif::metadata::Metadata::new_from_path(&path)
    }));
    match parsed {
        Ok(Ok(meta)) => rows_from(&meta),
        _ => Vec::new(),
    }
}

/// EXIF rows from a whole file in memory. The container is detected from
/// the content; RAW files that are TIFF inside work as TIFF.
pub fn read_exif_bytes(bytes: &[u8]) -> Vec<(String, String)> {
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let kind = FileExtension::auto_detect(&mut std::io::Cursor::new(bytes))?;
        little_exif::metadata::Metadata::new_from_vec(&bytes.to_vec(), kind).ok()
    }));
    match parsed {
        Ok(Some(meta)) => rows_from(&meta),
        _ => Vec::new(),
    }
}

/// Capture time from EXIF rows: `DateTimeOriginal`, else `CreateDate`.
pub fn capture_stamp(exif: &[(String, String)]) -> Option<u64> {
    lookup(exif, KEY_DATE_TIME_ORIGINAL)
        .and_then(|s| parse_exif_datetime(&s))
        .or_else(|| lookup(exif, KEY_CREATE_DATE).and_then(|s| parse_exif_datetime(&s)))
}

/// Capture time of a file on disk, nothing else parsed.
pub fn read_capture_stamp_path(path: &Path) -> Option<u64> {
    capture_stamp(&read_exif_path(path))
}

pub fn lookup(rows: &[(String, String)], key: &str) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn rows_from(meta: &little_exif::metadata::Metadata) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for tag in meta {
        if skip_tag(tag) {
            continue;
        }
        if let Some(row) = describe_tag(tag) {
            rows.push(row);
        }
    }
    rows.sort_by_key(|a| a.0.to_lowercase());
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
    let value = pretty_value(&inner);
    if value.is_empty() || value == "[]" {
        return None;
    }
    Some((name, value))
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
        if bytes[i] == b'('
            && let Some(end) = inner[i + 1..].find(')')
        {
            let pair = &inner[i + 1..i + 1 + end];
            let mut parts = pair.split(',');
            let n: f64 = parts.next()?.trim().parse().ok()?;
            let d: f64 = parts.next()?.trim().parse().ok()?;
            values.push(format_rational(n, d));
            i += end + 2;
            continue;
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
        format!("{v:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

// ------------------------------------------------------------------ XMP

pub fn extract_xmp(bytes: &[u8]) -> Vec<(String, String)> {
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
                    // Text inside rdf:Alt/rdf:Seq/rdf:Bag/rdf:li belongs to
                    // the property that wraps the container (dc:title …).
                    if let Some(name) = stack.iter().rev().find(|n| !skip_xmp_name(n)) {
                        rows.push((name.clone(), text.to_string()));
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

// ----------------------------------------------------------------- IPTC

pub fn extract_iptc(bytes: &[u8]) -> Vec<(String, String)> {
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
        if name_len.is_multiple_of(2) {
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
            if n == 0 || n > 4 || i + 4 + n > data.len() {
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
                rows.push((iptc_key(dataset).to_string(), text));
            }
        }
        i = start + size;
    }
    rows
}

/// IIM record 2 dataset → IPTC identifier (as ExifTool spells them).
pub fn iptc_key(dataset: u8) -> &'static str {
    match dataset {
        5 => "ObjectName",
        7 => "EditStatus",
        10 => "Urgency",
        15 => "Category",
        20 => "SupplementalCategories",
        22 => "FixtureIdentifier",
        25 => "Keywords",
        40 => "SpecialInstructions",
        55 => "DateCreated",
        60 => "TimeCreated",
        80 => "By-line",
        85 => "By-lineTitle",
        90 => "City",
        92 => "Sub-location",
        95 => "Province-State",
        100 => "Country-PrimaryLocationCode",
        101 => "Country-PrimaryLocationName",
        103 => "OriginalTransmissionReference",
        105 => "Headline",
        110 => "Credit",
        115 => "Source",
        116 => "CopyrightNotice",
        120 => "Caption-Abstract",
        122 => "Writer-Editor",
        0 => "IPTC 0",
        1 => "IPTC 1",
        2 => "IPTC 2",
        3 => "IPTC 3",
        4 => "IPTC 4",
        _ => "IPTC",
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ----------------------------------------------------------------- time

/// `YYYY:MM:DD HH:MM:SS` (or any digit grouping in that order) → Unix seconds.
pub fn parse_exif_datetime(text: &str) -> Option<u64> {
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

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + i64::from(doe) - 719468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_exif_datetime() {
        assert_eq!(parse_exif_datetime("2024:03:05 10:20:30"), Some(1_709_634_030));
        assert_eq!(parse_exif_datetime("2024-03-05T10:20:30"), Some(1_709_634_030));
        assert_eq!(parse_exif_datetime("2024:13:05 00:00:00"), None);
        assert_eq!(parse_exif_datetime("garbage"), None);
    }

    #[test]
    fn xmp_from_sidecar_text() {
        let xml = br#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="r"><rdf:Description dc:creator="Me"><dc:title><rdf:Alt><rdf:li>Hello</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let rows = extract_xmp(xml);
        assert!(rows.contains(&("dc:creator".into(), "Me".into())));
        assert!(rows.contains(&("dc:title".into(), "Hello".into())));
    }

    #[test]
    fn iptc_iim_records() {
        // 0x1C record 2 dataset 120 (caption), 5 bytes "Hallo"; dataset 25 keyword.
        let mut data = vec![0x1C, 2, 120, 0, 5];
        data.extend_from_slice(b"Hallo");
        data.extend_from_slice(&[0x1C, 2, 25, 0, 3]);
        data.extend_from_slice(b"Dog");
        let rows = extract_iptc(&data);
        assert_eq!(
            rows,
            vec![
                ("Caption-Abstract".to_string(), "Hallo".to_string()),
                ("Keywords".to_string(), "Dog".to_string())
            ]
        );
    }

    #[test]
    fn iptc_garbage_does_not_panic() {
        for data in [vec![0x1C; 7], vec![0x1C, 2, 5, 0x84, 0xff, 0xff, 0xff, 0xff, 0], vec![0x1C, 2, 5, 0x80]] {
            let _ = extract_iptc(&data);
        }
        let _ = extract_iptc(b"8BIM\x04\x04\x00\x00\xff\xff\xff\xff");
    }

    #[test]
    fn metadata_from_small_stream() {
        let bytes = b"\x1C\x02\x78\x00\x02hi".to_vec();
        let meta = read_metadata(&mut Cursor::new(bytes), &DecodeLimits::MOBILE).unwrap();
        assert_eq!(meta.iptc, vec![("Caption-Abstract".to_string(), "hi".to_string())]);
        assert!(meta.exif.is_empty());
        assert_eq!(meta.captured, None);
    }

    #[test]
    fn head_and_tail_keeps_both_ends() {
        let mut data = vec![1u8; (HEAD_BYTES + 3 * TAIL_BYTES) as usize];
        let n = data.len();
        data[n - 1] = 9;
        let blob = head_and_tail_of(data.clone());
        assert_eq!(blob.len() as u64, HEAD_BYTES + TAIL_BYTES);
        assert_eq!(*blob.last().unwrap(), 9);
        let via_reader = read_head_and_tail(&mut Cursor::new(data), n as u64).unwrap();
        assert_eq!(via_reader, blob);
    }
}
