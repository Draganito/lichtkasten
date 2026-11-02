use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::library;

const NS: &str = "https://lichtkasten.app/ns/1.0/";
const XMP_MAGIC: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

#[derive(Clone, Debug, Default)]
pub struct AlbumStore {
    pub albums: Vec<Album>,
}

#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub albums: Vec<String>,
    pub covers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Album {
    pub name: String,
    pub paths: Vec<PathBuf>,
    pub cover: Option<PathBuf>,
    pub cover_mtime: u64,
    pub cover_len: u64,
}

impl AlbumStore {
    pub fn load() -> Self {
        Self::default()
    }

    pub fn save(&self) {}

    pub fn ensure(&mut self, name: &str) -> bool {
        let name = normalize_name(name);
        if name.is_empty() || self.albums.iter().any(|album| album.name == name) {
            return false;
        }
        self.albums.push(Album {
            name,
            paths: Vec::new(),
            cover: None,
            cover_mtime: 0,
            cover_len: 0,
        });
        self.albums.sort_by_key(|a| a.name.to_lowercase());
        true
    }

    pub fn remove_album(&mut self, name: &str) -> Vec<PathBuf> {
        let Some(index) = self.albums.iter().position(|album| album.name == name) else {
            return Vec::new();
        };
        self.albums.remove(index).paths
    }

    pub fn add_path(&mut self, name: &str, path: PathBuf) -> bool {
        self.ensure(name);
        let Some(album) = self.albums.iter_mut().find(|album| album.name == name) else {
            return false;
        };
        if album.paths.iter().any(|entry| entry == &path) {
            return false;
        }
        if album.cover.is_none() {
            album.cover = Some(path.clone());
        }
        album.paths.push(path);
        true
    }

    pub fn drop_path(&mut self, name: &str, path: &Path) -> bool {
        let Some(album) = self.albums.iter_mut().find(|album| album.name == name) else {
            return false;
        };
        let before = album.paths.len();
        album.paths.retain(|entry| entry != path);
        if album.cover.as_deref() == Some(path) {
            album.cover = album.paths.first().cloned();
            album.cover_mtime = 0;
            album.cover_len = 0;
        }
        before != album.paths.len()
    }

    pub fn remember(&mut self, path: &Path, albums: &[String]) {
        for album in &mut self.albums {
            if albums.iter().any(|name| name == &album.name) {
                if !album.paths.iter().any(|entry| entry == path) {
                    album.paths.push(path.to_path_buf());
                }
                if album.cover.is_none() {
                    album.cover = Some(path.to_path_buf());
                }
            } else {
                album.paths.retain(|entry| entry != path);
            }
        }
        for name in albums {
            if !self.albums.iter().any(|album| album.name == *name) {
                self.albums.push(Album {
                    name: name.clone(),
                    paths: vec![path.to_path_buf()],
                    cover: Some(path.to_path_buf()),
                    cover_mtime: 0,
                    cover_len: 0,
                });
            }
        }
        self.albums.sort_by_key(|a| a.name.to_lowercase());
    }

    pub fn touch_cover_meta(&mut self, path: &Path, mtime: u64, len: u64) {
        for album in &mut self.albums {
            if album.cover.as_ref().is_some_and(|cover| cover == path) && album.cover_mtime == 0 {
                album.cover_mtime = mtime;
                album.cover_len = len;
            }
        }
    }

    pub fn remember_cover(&mut self, path: &Path, covers: &[String], mtime: u64, len: u64) {
        for name in covers {
            self.ensure(name);
            if let Some(album) = self.albums.iter_mut().find(|album| album.name == *name) {
                album.cover = Some(path.to_path_buf());
                album.cover_mtime = mtime;
                album.cover_len = len;
            }
        }
    }

    pub fn set_cover(&mut self, name: &str, path: PathBuf, mtime: u64, len: u64) -> Option<PathBuf> {
        let album = self.albums.iter_mut().find(|album| album.name == name)?;
        if !album.paths.iter().any(|entry| entry == &path) {
            album.paths.insert(0, path.clone());
        }
        let previous = album.cover.clone();
        album.cover = Some(path);
        album.cover_mtime = mtime;
        album.cover_len = len;
        previous.filter(|old| album.cover.as_ref() != Some(old))
    }

    pub fn covers_of(&self, path: &Path) -> Vec<String> {
        self.albums
            .iter()
            .filter(|album| album.cover.as_ref().is_some_and(|cover| cover == path))
            .map(|album| album.name.clone())
            .collect()
    }

    pub fn cover_stubs(&self) -> Vec<library::Photo> {
        self.albums
            .iter()
            .filter_map(|album| {
                let path = album.cover.as_ref()?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Some(library::Photo {
                    path: path.clone(),
                    name,
                    mtime: album.cover_mtime,
                    len: album.cover_len,
                    capture: None,
                    capture_known: true,
                    albums: Vec::new(),
                    albums_known: true,
                    covers: Vec::new(),
                })
            })
            .collect()
    }

    pub fn paths(&self, name: &str) -> Vec<PathBuf> {
        self.albums
            .iter()
            .find(|album| album.name == name)
            .map(|album| album.paths.clone())
            .unwrap_or_default()
    }
}

pub fn normalize_name(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    name.chars().take(80).collect()
}

pub fn sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("xmp")
}

pub fn prefers_sidecar(path: &Path) -> bool {
    if library::is_raw_path(path) || library::is_video_path(path) {
        return true;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "tif" | "tiff" | "png" | "gif" | "bmp" | "webp"
            )
        })
}

pub fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
}

/// Sidecar only — cheap enough for a folder scan.
pub fn read_fast(path: &Path) -> Option<Tags> {
    let sidecar = sidecar_path(path);
    let text = fs::read_to_string(sidecar).ok()?;
    let tags = parse_tags(&text);
    if tags.albums.is_empty() && tags.covers.is_empty() {
        None
    } else {
        Some(tags)
    }
}

pub fn read(path: &Path) -> Vec<String> {
    read_tags(path).albums
}

pub fn read_tags(path: &Path) -> Tags {
    if let Some(tags) = read_fast(path) {
        return tags;
    }
    if let Ok(bytes) = fs::read(path)
        && let Some(xml) = extract_xmp_text(&bytes)
    {
        return parse_tags(&xml);
    }
    Tags::default()
}

pub fn write(path: &Path, albums: &[String], covers: &[String]) -> Result<Tags, String> {
    let tags = Tags {
        albums: normalize_list(albums),
        covers: normalize_list(covers),
    };
    if prefers_sidecar(path) || sidecar_path(path).exists() || !is_jpeg(path) {
        write_sidecar(path, &tags)?;
        return Ok(tags);
    }
    if write_jpeg_embed(path, &tags).is_err() {
        write_sidecar(path, &tags)?;
    }
    Ok(tags)
}

fn normalize_list(albums: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in albums {
        let name = normalize_name(name);
        if !name.is_empty() && !out.iter().any(|have| have == &name) {
            out.push(name);
        }
    }
    out.sort_by_key(|a| a.to_lowercase());
    out
}

fn write_sidecar(path: &Path, tags: &Tags) -> Result<(), String> {
    let sidecar = sidecar_path(path);
    let existing = fs::read_to_string(&sidecar).unwrap_or_default();
    let xml = merge_xmp(&existing, tags);
    atomic_write(&sidecar, xml.as_bytes())
}

fn write_jpeg_embed(path: &Path, tags: &Tags) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err("kein JPEG".into());
    }
    let existing = extract_xmp_text(&data).unwrap_or_default();
    let xml = merge_xmp(&existing, tags);
    let app1 = jpeg_xmp_segment(&xml)?;
    let out = jpeg_with_xmp(&data, &app1).ok_or_else(|| "JPEG-XMP fehlgeschlagen".to_string())?;
    let tmp = path.with_file_name(format!(
        ".{}.lk-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("foto")
    ));
    fs::write(&tmp, &out).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("xmp.part");
    {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

fn packet(xml: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>{xml}<?xpacket end=\"w\"?>"
    )
}

fn bag_xml(tag: &str, names: &[String]) -> String {
    let mut items = String::new();
    for name in names {
        items.push_str(&format!("<rdf:li>{}</rdf:li>", escape(name)));
    }
    format!("<lichtkasten:{tag} xmlns:lichtkasten=\"{NS}\"><rdf:Bag>{items}</rdf:Bag></lichtkasten:{tag}>")
}

fn tags_xml(tags: &Tags) -> String {
    format!("{}{}", bag_xml("albums", &tags.albums), bag_xml("covers", &tags.covers))
}

fn new_xmp(tags: &Tags) -> String {
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:lichtkasten=\"{NS}\">{}</rdf:Description></rdf:RDF></x:xmpmeta>",
        tags_xml(tags)
    )
}

fn merge_xmp(existing: &str, tags: &Tags) -> String {
    let body = strip_packet(existing);
    if body.trim().is_empty() {
        return packet(&new_xmp(tags));
    }
    let mut out = replace_bag(body, "albums", &tags.albums);
    out = replace_bag(out, "covers", &tags.covers);
    if !out.contains("xmlns:lichtkasten")
        && let Some(desc) = find_ci(&out, "<rdf:Description")
        && let Some(gt) = out[desc..].find('>')
    {
        out.insert_str(desc + gt, &format!(" xmlns:lichtkasten=\"{NS}\""));
    }
    packet(&out)
}

fn replace_bag(body: String, tag: &str, names: &[String]) -> String {
    let open = format!("<lichtkasten:{tag}");
    let close = format!("</lichtkasten:{tag}>");
    let block = bag_xml(tag, names);
    if let Some(start) = find_ci(&body, &open)
        && let Some(rel_end) = find_ci(&body[start..], &close)
    {
        let end = start + rel_end + close.len();
        return format!("{}{}{}", &body[..start], block, &body[end..]);
    }
    if let Some(desc) = find_ci(&body, "<rdf:Description")
        && let Some(gt) = body[desc..].find('>')
    {
        let mut out = body;
        out.insert_str(desc + gt + 1, &block);
        return out;
    }
    new_xmp(&Tags {
        albums: if tag == "albums" { names.to_vec() } else { Vec::new() },
        covers: if tag == "covers" { names.to_vec() } else { Vec::new() },
    })
}

fn strip_packet(xml: &str) -> String {
    let mut text = xml.trim().to_string();
    if let Some(start) = text.find("<x:xmpmeta") {
        text = text[start..].to_string();
    } else if let Some(start) = text.find("<xmpmeta") {
        text = text[start..].to_string();
    }
    if let Some(end) = text.rfind("<?xpacket end") {
        text.truncate(end);
    }
    text.trim().to_string()
}

pub fn parse_tags(xml: &str) -> Tags {
    Tags {
        albums: parse_bag(xml, "albums"),
        covers: parse_bag(xml, "covers"),
    }
}

fn parse_bag(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<lichtkasten:{tag}");
    let close = format!("</lichtkasten:{tag}>");
    let Some(start) = find_ci(xml, &open) else {
        return Vec::new();
    };
    let rest = &xml[start..];
    let Some(rel_end) = find_ci(rest, &close) else {
        return Vec::new();
    };
    let block = &rest[..rel_end];
    let mut names = Vec::new();
    let mut search = block;
    while let Some(li) = find_ci(search, "<rdf:li") {
        let after = &search[li..];
        let Some(gt) = after.find('>') else { break };
        let content = &after[gt + 1..];
        let Some(close_li) = find_ci(content, "</rdf:li>") else { break };
        let name = normalize_name(&unescape(&content[..close_li]));
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
        search = &content[close_li + 6..];
    }
    names
}

fn extract_xmp_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return jpeg_xmp(bytes);
    }
    let start = find_bytes(bytes, b"<?xpacket")
        .or_else(|| find_bytes(bytes, b"<x:xmpmeta"))
        .or_else(|| find_bytes(bytes, b"<xmpmeta"))?;
    let rest = &bytes[start..];
    let end = find_bytes(rest, b"<?xpacket end")
        .or_else(|| find_bytes(rest, b"</x:xmpmeta>"))
        .or_else(|| find_bytes(rest, b"</xmpmeta>"))?;
    let stop = if rest[end..].starts_with(b"<?xpacket end") {
        end + find_bytes(&rest[end..], b"?>").unwrap_or(16) + 2
    } else {
        end + 12
    };
    Some(String::from_utf8_lossy(&rest[..stop.min(rest.len())]).into_owned())
}

fn jpeg_xmp(data: &[u8]) -> Option<String> {
    let mut i = 2usize;
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            return None;
        }
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let marker = data[i];
        i += 1;
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        if i + 1 >= data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return None;
        }
        let payload = &data[i + 2..i + len];
        if marker == 0xE1 && payload.starts_with(XMP_MAGIC) {
            return Some(String::from_utf8_lossy(&payload[XMP_MAGIC.len()..]).into_owned());
        }
        i += len;
    }
    None
}

fn jpeg_xmp_segment(xml: &str) -> Result<Vec<u8>, String> {
    let mut payload = XMP_MAGIC.to_vec();
    payload.extend_from_slice(xml.as_bytes());
    let len = payload.len() + 2;
    if len > 0xFFFF {
        return Err("XMP zu groß für JPEG".into());
    }
    let mut out = vec![0xFF, 0xE1, (len >> 8) as u8, (len & 0xFF) as u8];
    out.extend_from_slice(&payload);
    Ok(out)
}

fn jpeg_with_xmp(jpeg: &[u8], app1: &[u8]) -> Option<Vec<u8>> {
    let mut i = 2usize;
    let mut insert_at = 2usize;
    let mut replace: Option<(usize, usize)> = None;
    while i + 4 < jpeg.len() {
        if jpeg[i] != 0xFF {
            break;
        }
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            break;
        }
        let marker = jpeg[i];
        i += 1;
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if i + 1 >= jpeg.len() {
            break;
        }
        let len = u16::from_be_bytes([jpeg[i], jpeg[i + 1]]) as usize;
        if len < 2 || i + len > jpeg.len() {
            break;
        }
        let seg_start = i.saturating_sub(2);
        let payload = &jpeg[i + 2..i + len];
        if marker == 0xE1 && payload.starts_with(XMP_MAGIC) {
            replace = Some((seg_start, i + len));
            break;
        }
        if marker == 0xE0 {
            insert_at = i + len;
        }
        i += len;
    }
    let mut out = Vec::with_capacity(jpeg.len() + app1.len());
    out.extend_from_slice(&jpeg[..2]);
    if let Some((from, to)) = replace {
        out.extend_from_slice(&jpeg[2..from]);
        out.extend_from_slice(app1);
        out.extend_from_slice(&jpeg[to..]);
    } else {
        let at = insert_at.max(2);
        out.extend_from_slice(&jpeg[2..at]);
        out.extend_from_slice(app1);
        out.extend_from_slice(&jpeg[at..]);
    }
    Some(out)
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}
