use std::fs;
use std::io::Read;
use std::path::Path;

use image::DynamicImage;

use crate::library;
use crate::orient;

const SCAN_LIMIT: u64 = 80 * 1024 * 1024;
const MAX_JPEG: usize = 16 * 1024 * 1024;

pub fn is_raw(path: &Path) -> bool {
    library::is_raw_path(path)
}

pub fn load_preview(path: &Path, target: u32) -> Result<DynamicImage, String> {
    if let Some(img) = load_embedded_jpeg(path, target) {
        return Ok(img);
    }
    if let Some(img) = load_rawler_preview(path, target >= 1200) {
        return Ok(img);
    }
    Err("keine RAW-Vorschau gefunden".into())
}

fn load_embedded_jpeg(path: &Path, target: u32) -> Option<DynamicImage> {
    let data = read_scan_prefix(path)?;
    let mut best: Option<(u32, u32, usize, usize)> = None;
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF
            && let Some((w, h, end)) = parse_jpeg(&data[i..]) {
                let pixels = w.saturating_mul(h);
                let better = best
                    .map(|(bw, bh, _, _)| pixels > bw.saturating_mul(bh))
                    .unwrap_or(true);
                if better {
                    best = Some((w, h, i, end));
                }
                i += end.max(2);
                continue;
            }
        i += 1;
    }
    let (w, h, start, end) = best?;
    if w < 64 || h < 64 {
        return None;
    }
    let _ = (w, h, target);
    let jpeg = &data[start..start + end];
    let img = image::load_from_memory(jpeg).ok()?;
    Some(orient::apply(img, orient::from_bytes(jpeg)))
}

fn read_scan_prefix(path: &Path) -> Option<Vec<u8>> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len().min(SCAN_LIMIT) as usize;
    let mut data = vec![0u8; len];
    file.read_exact(&mut data).ok()?;
    Some(data)
}

fn parse_jpeg(data: &[u8]) -> Option<(u32, u32, usize)> {
    if data.len() < 8 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    let mut width = 0u32;
    let mut height = 0u32;
    while i + 1 < data.len() && i < MAX_JPEG {
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
        if marker == 0xD9 {
            break;
        }
        if marker == 0xDA {
            let eoi = find_eoi(&data[i..])?;
            i += eoi;
            if (64..=20000).contains(&width) && (64..=20000).contains(&height) {
                return Some((width, height, i));
            }
            return None;
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if i + 1 >= data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return None;
        }
        if matches!(marker, 0xC0..=0xC3) && len >= 7 {
            height = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            width = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
        }
        i += len;
    }
    None
}

fn find_eoi(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(MAX_JPEG);
    let mut i = 0;
    while i + 1 < limit {
        if data[i] == 0xFF {
            let next = data[i + 1];
            if next == 0xD9 {
                return Some(i + 2);
            }
            if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

fn load_rawler_preview(path: &Path, prefer_full: bool) -> Option<DynamicImage> {
    let rgb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rawler_rgb(path, prefer_full)
    }))
    .ok()
    .flatten()?;
    let (w, h, bytes) = rgb;
    let buf = image::RgbImage::from_raw(w, h, bytes)?;
    Some(DynamicImage::ImageRgb8(buf))
}

fn rawler_rgb(path: &Path, prefer_full: bool) -> Option<(u32, u32, Vec<u8>)> {
    let rawfile = rawler::rawsource::RawSource::new(path).ok()?;
    let decoder = rawler::get_decoder(&rawfile).ok()?;
    let params = rawler::decoders::RawDecodeParams::default();
    let img = if prefer_full {
        decoder
            .full_image(&rawfile, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.preview_image(&rawfile, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(&rawfile, &params).ok().flatten())
    } else {
        decoder
            .preview_image(&rawfile, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.full_image(&rawfile, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(&rawfile, &params).ok().flatten())
    }?;
    let rgb = img.to_rgb8();
    Some((rgb.width(), rgb.height(), rgb.into_raw()))
}
