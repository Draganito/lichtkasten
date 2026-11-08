//! RAW previews: the largest embedded JPEG when there is one, otherwise
//! (feature `rawler`) a rawler render. rawler is wrapped in `catch_unwind`;
//! it is a large parser fed with camera firmware output.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use image::DynamicImage;

use crate::decode::{DecodeError, DecodeLimits, stream_len};
use crate::orient;

const SCAN_LIMIT: u64 = 80 * 1024 * 1024;
const MAX_JPEG: usize = 16 * 1024 * 1024;

fn no_preview() -> DecodeError {
    DecodeError::Unsupported("keine RAW-Vorschau gefunden".into())
}

/// Preview from a seekable stream (Android: a file descriptor). With the
/// `rawler` feature it falls back to rawler on the whole file, which is read
/// into memory up to `limits.max_file`. On this path rawler is asked for its
/// preview first whatever `target` is: a full demosaic of a 50 MP file is
/// hundreds of megabytes, more than a phone process should allocate for one
/// view. Without the feature only the embedded JPEG is used.
pub fn load_preview<R: Read + Seek>(
    reader: &mut R,
    target: u32,
    limits: &DecodeLimits,
) -> Result<DynamicImage, DecodeError> {
    let _ = target;
    let len = stream_len(reader)?;
    if len > limits.max_file {
        return Err(DecodeError::TooLarge(format!(
            "Datei zu groß ({} MB)",
            len / (1024 * 1024)
        )));
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut data = Vec::new();
    reader
        .by_ref()
        .take(len.min(SCAN_LIMIT))
        .read_to_end(&mut data)?;
    if let Some(img) = embedded_jpeg(&data, limits) {
        return Ok(img);
    }
    #[cfg(feature = "rawler")]
    {
        if (data.len() as u64) < len {
            reader.read_to_end(&mut data)?;
        }
        let source = rawler::rawsource::RawSource::new_from_shared_vec(std::sync::Arc::new(data));
        rawler_preview(&source, false, limits).ok_or_else(no_preview)
    }
    #[cfg(not(feature = "rawler"))]
    Err(no_preview())
}

/// Preview from a path (desktop): same scan, but rawler maps the file
/// instead of copying it.
pub fn load_preview_path(
    path: &Path,
    target: u32,
    limits: &DecodeLimits,
) -> Result<DynamicImage, DecodeError> {
    if let Some(data) = read_scan_prefix(path)
        && let Some(img) = embedded_jpeg(&data, limits)
    {
        return Ok(img);
    }
    #[cfg(feature = "rawler")]
    {
        let source = rawler::rawsource::RawSource::new(path)?;
        rawler_preview(&source, target >= 1200, limits).ok_or_else(no_preview)
    }
    #[cfg(not(feature = "rawler"))]
    {
        let _ = target;
        Err(no_preview())
    }
}

fn read_scan_prefix(path: &Path) -> Option<Vec<u8>> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len().min(SCAN_LIMIT) as usize;
    let mut data = vec![0u8; len];
    file.read_exact(&mut data).ok()?;
    Some(data)
}

fn embedded_jpeg(data: &[u8], limits: &DecodeLimits) -> Option<DynamicImage> {
    let mut best: Option<(u32, u32, usize, usize)> = None;
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0xFF
            && data[i + 1] == 0xD8
            && data[i + 2] == 0xFF
            && let Some((w, h, end)) = parse_jpeg(&data[i..])
        {
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
    if w < 64 || h < 64 || u64::from(w) * u64::from(h) > limits.max_pixels {
        return None;
    }
    let jpeg = &data[start..start + end];
    let mut reader = image::ImageReader::with_format(
        std::io::Cursor::new(jpeg),
        image::ImageFormat::Jpeg,
    );
    let mut lim = image::Limits::default();
    lim.max_alloc = Some(limits.max_alloc);
    reader.limits(lim);
    let img = reader.decode().ok()?;
    Some(orient::apply(img, orient::from_bytes(jpeg)))
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

#[cfg(feature = "rawler")]
fn rawler_preview(
    source: &rawler::rawsource::RawSource,
    prefer_full: bool,
    limits: &DecodeLimits,
) -> Option<DynamicImage> {
    let rgb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rawler_rgb(source, prefer_full)
    }))
    .ok()
    .flatten()?;
    let (w, h, bytes) = rgb;
    if u64::from(w) * u64::from(h) > limits.max_pixels {
        return None;
    }
    let buf = image::RgbImage::from_raw(w, h, bytes)?;
    Some(DynamicImage::ImageRgb8(buf))
}

#[cfg(feature = "rawler")]
fn rawler_rgb(rawfile: &rawler::rawsource::RawSource, prefer_full: bool) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = rawler::get_decoder(rawfile).ok()?;
    let params = rawler::decoders::RawDecodeParams::default();
    let img = if prefer_full {
        decoder
            .full_image(rawfile, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.preview_image(rawfile, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(rawfile, &params).ok().flatten())
    } else {
        decoder
            .preview_image(rawfile, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.full_image(rawfile, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(rawfile, &params).ok().flatten())
    }?;
    let rgb = img.to_rgb8();
    Some((rgb.width(), rgb.height(), rgb.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgb, RgbImage};
    use std::io::Cursor;

    fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = Rgb([x as u8, 0, 0]);
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn finds_largest_embedded_jpeg() {
        // A fake RAW container: junk, a small JPEG, junk, a bigger JPEG.
        let mut container = vec![0x11u8; 500];
        container.extend(jpeg_bytes(64, 64));
        container.extend(std::iter::repeat_n(0x22u8, 333));
        container.extend(jpeg_bytes(160, 96));
        container.extend(std::iter::repeat_n(0x33u8, 77));
        let img = load_preview(&mut Cursor::new(container), 512, &DecodeLimits::MOBILE).unwrap();
        assert_eq!((img.width(), img.height()), (160, 96));
    }

    #[test]
    fn no_preview_is_unsupported() {
        let err = load_preview(&mut Cursor::new(vec![0u8; 4096]), 512, &DecodeLimits::MOBILE).unwrap_err();
        assert!(matches!(err, DecodeError::Unsupported(_)));
    }

    #[test]
    fn embedded_jpeg_respects_pixel_limit() {
        let limits = DecodeLimits {
            max_pixels: 100,
            ..DecodeLimits::MOBILE
        };
        let bytes = jpeg_bytes(64, 64);
        assert!(embedded_jpeg(&bytes, &limits).is_none());
    }
}
