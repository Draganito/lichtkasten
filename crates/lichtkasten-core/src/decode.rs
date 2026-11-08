//! Raster decoding under limits. Everything goes through [`DecodeLimits`],
//! and an error is a value, never a panic.

use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use image::{DynamicImage, GenericImageView, ImageReader, Limits, RgbaImage};

use crate::{ext, orient, raw};

/// Upper bounds for one decode. A picture above `max_pixels` or a file
/// above `max_file` is refused before any pixel memory is touched;
/// `max_alloc` is handed to the `image` crate as its allocation cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_pixels: u64,
    pub max_alloc: u64,
    pub max_file: u64,
}

impl DecodeLimits {
    /// What the desktop app has always used.
    pub const DESKTOP: Self = Self {
        max_pixels: 80_000_000,
        max_alloc: 512 * 1024 * 1024,
        max_file: 512 * 1024 * 1024,
    };

    /// Phone: half the pixels, half the memory, 200 MB per file.
    pub const MOBILE: Self = Self {
        max_pixels: 50_000_000,
        max_alloc: 256 * 1024 * 1024,
        max_file: 200 * 1024 * 1024,
    };

    fn image_limits(&self) -> Limits {
        let mut limits = Limits::default();
        limits.max_alloc = Some(self.max_alloc);
        limits
    }
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::DESKTOP
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Dimensions or file size above the limits.
    TooLarge(String),
    /// No decoder for this container (or no preview inside a RAW).
    Unsupported(String),
    /// The decoder rejected the data.
    Corrupt(String),
    /// Reading failed.
    Io(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooLarge(s)
            | DecodeError::Unsupported(s)
            | DecodeError::Corrupt(s)
            | DecodeError::Io(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e.to_string())
    }
}

/// Size of a seekable stream, position restored.
pub fn stream_len<R: Seek>(reader: &mut R) -> std::io::Result<u64> {
    let pos = reader.stream_position()?;
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(pos))?;
    Ok(len)
}

/// Decode a raster image (JPEG, PNG, TIFF, WebP, GIF, BMP) from a reader.
/// Dimensions are checked against `limits.max_pixels` before decoding.
/// Orientation is *not* applied here; see [`decode_oriented`].
pub fn decode_raster<R: BufRead + Seek>(
    mut reader: R,
    limits: &DecodeLimits,
) -> Result<DynamicImage, DecodeError> {
    reader.seek(SeekFrom::Start(0))?;
    let probe = ImageReader::new(reader)
        .with_guessed_format()
        .map_err(|e| DecodeError::Io(e.to_string()))?;
    let format = probe
        .format()
        .ok_or_else(|| DecodeError::Unsupported("unbekanntes Bildformat".into()))?;
    let mut inner = probe.into_inner();
    inner.seek(SeekFrom::Start(0))?;
    let (w, h) = ImageReader::with_format(&mut inner, format)
        .into_dimensions()
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    if u64::from(w) * u64::from(h) > limits.max_pixels {
        return Err(DecodeError::TooLarge(format!("Bild zu groß ({w}×{h})")));
    }
    inner.seek(SeekFrom::Start(0))?;
    let mut decoder = ImageReader::with_format(inner, format);
    decoder.limits(limits.image_limits());
    decoder.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => DecodeError::TooLarge(e.to_string()),
        image::ImageError::Unsupported(_) => DecodeError::Unsupported(e.to_string()),
        image::ImageError::IoError(_) => DecodeError::Io(e.to_string()),
        _ => DecodeError::Corrupt(e.to_string()),
    })
}

/// Like [`decode_raster`] but from a path (desktop).
pub fn decode_raster_path(path: &Path, limits: &DecodeLimits) -> Result<DynamicImage, DecodeError> {
    let file = File::open(path)?;
    decode_raster(BufReader::new(file), limits)
}

/// Decode and apply the EXIF orientation. `name_hint` (file name or path)
/// decides between the RAW path and the raster path; `target` is the long
/// edge a RAW preview should reach at least (the full raster is returned
/// as decoded, see [`fit_long_edge`]).
pub fn decode_oriented<R: Read + Seek>(
    mut reader: R,
    name_hint: &str,
    target: u32,
    limits: &DecodeLimits,
) -> Result<DynamicImage, DecodeError> {
    let len = stream_len(&mut reader)?;
    if len > limits.max_file {
        return Err(DecodeError::TooLarge(format!(
            "Datei zu groß ({} MB)",
            len / (1024 * 1024)
        )));
    }
    if ext::is_raw_name(name_hint) {
        return raw::load_preview(&mut reader, target.max(1), limits);
    }
    reader.seek(SeekFrom::Start(0))?;
    let orientation = orient::from_reader(&mut reader);
    let img = decode_raster(BufReader::new(reader), limits)?;
    Ok(orient::apply(img, orientation))
}

/// [`decode_oriented`] followed by [`fit_long_edge`]: the one call the
/// Android side needs for a thumbnail or a screen-sized view.
pub fn decode_fitted<R: Read + Seek>(
    reader: R,
    name_hint: &str,
    max_edge: u32,
    limits: &DecodeLimits,
) -> Result<RgbaImage, DecodeError> {
    let img = decode_oriented(reader, name_hint, max_edge, limits)?;
    let fitted = fit_long_edge(img, max_edge.max(1));
    if fitted.width() == 0 || fitted.height() == 0 {
        return Err(DecodeError::Corrupt("leeres Bild".into()));
    }
    Ok(fitted)
}

/// Shrink so that the long edge is at most `edge`; never enlarges.
pub fn fit_long_edge(img: DynamicImage, edge: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= edge {
        return img.to_rgba8();
    }
    let scale = edge as f32 / long as f32;
    let nw = (w as f32 * scale).round().max(1.0) as u32;
    let nh = (h as f32 * scale).round().max(1.0) as u32;
    img.resize(nw, nh, image::imageops::FilterType::Triangle)
        .to_rgba8()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgb, RgbImage};
    use std::io::Cursor;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([x as u8, y as u8, 7]);
        }
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    fn tiff_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([x as u8, y as u8, 200]);
        }
        let mut out = Cursor::new(Vec::new());
        image::codecs::tiff::TiffEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn decodes_png_and_fits() {
        let bytes = png_bytes(300, 100);
        let out = decode_fitted(Cursor::new(bytes), "a.png", 60, &DecodeLimits::MOBILE).unwrap();
        assert_eq!((out.width(), out.height()), (60, 20));
    }

    #[test]
    fn decodes_tiff() {
        let bytes = tiff_bytes(40, 30);
        let out = decode_fitted(Cursor::new(bytes), "a.tif", 512, &DecodeLimits::MOBILE).unwrap();
        assert_eq!((out.width(), out.height()), (40, 30));
        assert_eq!(out.get_pixel(3, 5).0, [3, 5, 200, 255]);
    }

    #[test]
    fn refuses_too_many_pixels() {
        let bytes = png_bytes(300, 100);
        let limits = DecodeLimits {
            max_pixels: 1000,
            ..DecodeLimits::MOBILE
        };
        let err = decode_fitted(Cursor::new(bytes), "a.png", 60, &limits).unwrap_err();
        assert!(matches!(err, DecodeError::TooLarge(_)), "{err:?}");
    }

    #[test]
    fn refuses_too_large_file() {
        let bytes = png_bytes(10, 10);
        let limits = DecodeLimits {
            max_file: 16,
            ..DecodeLimits::MOBILE
        };
        let err = decode_fitted(Cursor::new(bytes), "a.png", 60, &limits).unwrap_err();
        assert!(matches!(err, DecodeError::TooLarge(_)), "{err:?}");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for data in [
            Vec::new(),
            vec![0u8; 3],
            b"\x89PNG\r\n\x1a\n".to_vec(),
            b"II*\0\x08\0\0\0\xff\xff".to_vec(),
            b"\xff\xd8\xff\xe1\0\x10Exif\0\0II*\0".to_vec(),
        ] {
            let res = decode_fitted(Cursor::new(data), "x.tif", 100, &DecodeLimits::MOBILE);
            assert!(res.is_err());
        }
        let mut truncated = tiff_bytes(64, 64);
        truncated.truncate(truncated.len() / 2);
        assert!(decode_fitted(Cursor::new(truncated), "x.tif", 100, &DecodeLimits::MOBILE).is_err());
    }

    #[test]
    fn raw_hint_without_raw_content_is_unsupported() {
        let bytes = png_bytes(10, 10);
        let err = decode_fitted(Cursor::new(bytes), "a.nef", 60, &DecodeLimits::MOBILE).unwrap_err();
        assert!(matches!(err, DecodeError::Unsupported(_)), "{err:?}");
    }
}
