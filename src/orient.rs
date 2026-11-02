use std::fs::File;
use std::io::Read;
use std::path::Path;

use image::DynamicImage;

/// EXIF Orientation 1–8. 1 = unverändert.
pub fn from_path(path: &Path) -> u8 {
    let Ok(mut file) = File::open(path) else {
        return 1;
    };
    let mut buf = vec![0u8; 256 * 1024];
    let n = file.read(&mut buf).unwrap_or(0);
    from_bytes(&buf[..n])
}

pub fn from_bytes(data: &[u8]) -> u8 {
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0xD8 {
        return jpeg_orientation(data).unwrap_or(1);
    }
    if data.len() >= 8 && (data.starts_with(b"II") || data.starts_with(b"MM")) {
        return tiff_orientation(data, 0).unwrap_or(1);
    }
    1
}

pub fn apply(img: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.flipv().rotate90(),
        6 => img.rotate90(),
        7 => img.flipv().rotate270(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn jpeg_orientation(data: &[u8]) -> Option<u8> {
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
        if marker == 0xE1 && len >= 8 && data[i + 2..i + 8].starts_with(b"Exif\0\0") {
            return tiff_orientation(&data[i + 8..i + len], 0);
        }
        i += len;
    }
    None
}

fn tiff_orientation(data: &[u8], base: usize) -> Option<u8> {
    let le = data.get(base..base + 2)? == b"II";
    if !le && data.get(base..base + 2)? != b"MM" {
        return None;
    }
    let ifd0 = base.checked_add(u32_at(data, base + 4, le)? as usize)?;
    ifd_orientation(data, base, ifd0, le)
}

fn ifd_orientation(data: &[u8], base: usize, ifd: usize, le: bool) -> Option<u8> {
    let count = u16_at(data, ifd, le)? as usize;
    let mut found = None;
    let mut exif_off = None;
    for n in 0..count {
        let e = ifd + 2 + n * 12;
        let tag = u16_at(data, e, le)?;
        let typ = u16_at(data, e + 2, le)?;
        let cnt = u32_at(data, e + 4, le)?;
        if tag == 0x0112 && typ == 3 && cnt >= 1 {
            found = Some(u16_at(data, e + 8, le)? as u8);
        }
        if tag == 0x8769 && cnt == 1 {
            exif_off = Some(u32_at(data, e + 8, le)? as usize);
        }
    }
    if let Some(value) = found.filter(|v| (1..=8).contains(v)) {
        return Some(value);
    }
    let exif = base.checked_add(exif_off?)?;
    ifd_orientation(data, base, exif, le)
}

fn u16_at(data: &[u8], at: usize, le: bool) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    })
}

fn u32_at(data: &[u8], at: usize, le: bool) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    })
}
