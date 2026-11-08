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
///
/// Two steps, like a thumbnailer rather than a photo editor: an integer
/// *area* shrink (every output pixel is the plain mean of an f×f block)
/// takes the image down to between 1× and 2× the target, then a Triangle
/// resample covers the remaining fractional factor. The area step has no
/// negative lobes, so no ringing at edges, and it averages sensor noise
/// and JPEG artefacts away instead of sharpening them; it is also a single
/// pass of additions, far cheaper than a wide kernel on a 50 MP image.
pub fn fit_long_edge(img: DynamicImage, edge: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    let edge = edge.max(1);
    if long <= edge {
        return img.to_rgba8();
    }
    let scale = edge as f64 / long as f64;
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);

    let factor = long / edge;
    let mut rgba = if factor >= 2 {
        area_shrink(&img, factor)
    } else {
        img.into_rgba8()
    };
    if (rgba.width(), rgba.height()) != (nw, nh) {
        rgba = image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Triangle);
    }
    rgba
}

/// Mean of each `f`×`f` block; edge blocks average whatever is left.
/// Output is `ceil(w/f)` × `ceil(h/f)`, always RGBA8. Works on the native
/// buffer of every 8- and 16-bit layout, so a 24 MP 16-bit TIFF is reduced
/// first and converted after, not expanded to 100 MB of RGBA8 up front.
fn area_shrink(img: &DynamicImage, f: u32) -> RgbaImage {
    use DynamicImage as D;
    match img {
        D::ImageLuma8(b) => area_shrink_raw::<u8, 1>(b.as_raw(), b.width(), b.height(), f, 0),
        D::ImageLumaA8(b) => area_shrink_raw::<u8, 2>(b.as_raw(), b.width(), b.height(), f, 0),
        D::ImageRgb8(b) => area_shrink_raw::<u8, 3>(b.as_raw(), b.width(), b.height(), f, 0),
        D::ImageRgba8(b) => area_shrink_raw::<u8, 4>(b.as_raw(), b.width(), b.height(), f, 0),
        D::ImageLuma16(b) => area_shrink_raw::<u16, 1>(b.as_raw(), b.width(), b.height(), f, 8),
        D::ImageLumaA16(b) => area_shrink_raw::<u16, 2>(b.as_raw(), b.width(), b.height(), f, 8),
        D::ImageRgb16(b) => area_shrink_raw::<u16, 3>(b.as_raw(), b.width(), b.height(), f, 8),
        D::ImageRgba16(b) => area_shrink_raw::<u16, 4>(b.as_raw(), b.width(), b.height(), f, 8),
        other => {
            let b = other.to_rgba8();
            area_shrink_raw::<u8, 4>(b.as_raw(), b.width(), b.height(), f, 0)
        }
    }
}

/// `C` = channels in the source (1 gray, 2 gray+alpha, 3 RGB, 4 RGBA);
/// `shift` = 8 for 16-bit samples, 0 for 8-bit.
fn area_shrink_raw<T: Copy + Into<u64>, const C: usize>(
    data: &[T],
    w: u32,
    h: u32,
    f: u32,
    shift: u32,
) -> RgbaImage {
    let (w, h, f) = (w as usize, h as usize, f as usize);
    let ow = w.div_ceil(f);
    let oh = h.div_ceil(f);
    let mut out = vec![0u8; ow * oh * 4];
    let mut sums = vec![0u64; ow * C];
    let round = (1u64 << shift) >> 1;
    for oy in 0..oh {
        sums.fill(0);
        let y0 = oy * f;
        let y1 = (y0 + f).min(h);
        for y in y0..y1 {
            let row = &data[y * w * C..(y + 1) * w * C];
            for (ox, s) in sums.as_chunks_mut::<C>().0.iter_mut().enumerate() {
                let x0 = ox * f;
                let x1 = (x0 + f).min(w);
                for px in row[x0 * C..x1 * C].as_chunks::<C>().0 {
                    for c in 0..C {
                        s[c] += px[c].into();
                    }
                }
            }
        }
        let rows = (y1 - y0) as u64;
        for ox in 0..ow {
            let cols = ((ox * f + f).min(w) - ox * f) as u64;
            let n = rows * cols;
            let s = &sums[ox * C..ox * C + C];
            let mean = |c: usize| (((s[c] + n / 2) / n + round) >> shift).min(255) as u8;
            let o = &mut out[(oy * ow + ox) * 4..(oy * ow + ox) * 4 + 4];
            match C {
                1 => {
                    let g = mean(0);
                    o.copy_from_slice(&[g, g, g, 255]);
                }
                2 => {
                    let g = mean(0);
                    o.copy_from_slice(&[g, g, g, mean(1)]);
                }
                3 => o.copy_from_slice(&[mean(0), mean(1), mean(2), 255]),
                _ => o.copy_from_slice(&[mean(0), mean(1), mean(2), mean(3)]),
            }
        }
    }
    RgbaImage::from_raw(ow as u32, oh as u32, out).expect("sized above")
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
    fn area_shrink_is_the_block_mean() {
        // 2×2 checkerboard blocks, factor 2 -> every output pixel is the mean.
        let mut img = RgbImage::new(6, 4);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = if (x + y) % 2 == 0 { 0 } else { 200 };
            *p = Rgb([v, v, v]);
        }
        let out = area_shrink(&DynamicImage::ImageRgb8(img), 2);
        assert_eq!((out.width(), out.height()), (3, 2));
        for p in out.pixels() {
            assert_eq!(p.0, [100, 100, 100, 255]);
        }
    }

    #[test]
    fn area_shrink_handles_ragged_edges() {
        // 7×5 at factor 3: 3×2 output, the last column/row average 1 and 2 pixels.
        let mut img = RgbaImage::new(7, 5);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([x as u8 * 30, 0, 0, 255]);
        }
        let out = area_shrink(&DynamicImage::ImageRgba8(img), 3);
        assert_eq!((out.width(), out.height()), (3, 2));
        assert_eq!(out.get_pixel(0, 0).0[0], 30); // mean of 0,30,60
        assert_eq!(out.get_pixel(2, 1).0[0], 180); // column 6 alone
        assert_eq!(out.get_pixel(2, 1).0[3], 255);
    }

    #[test]
    fn area_shrink_16bit_and_gray() {
        let mut g = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::new(4, 4);
        for p in g.pixels_mut() {
            *p = image::Luma([0x8000]);
        }
        let out = area_shrink(&DynamicImage::ImageLuma16(g), 2);
        assert_eq!((out.width(), out.height()), (2, 2));
        assert_eq!(out.get_pixel(1, 1).0, [128, 128, 128, 255]);

        let mut c = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(4, 2);
        for p in c.pixels_mut() {
            *p = image::Rgb([0xFFFF, 0, 0x0100]);
        }
        let out = area_shrink(&DynamicImage::ImageRgb16(c), 2);
        assert_eq!(out.get_pixel(0, 0).0, [255, 0, 1, 255]);
    }

    #[test]
    fn fit_long_edge_hits_exact_size_and_preserves_flat_colour() {
        let mut img = RgbImage::new(6000, 4000);
        for p in img.pixels_mut() {
            *p = Rgb([10, 128, 250]);
        }
        let out = fit_long_edge(DynamicImage::ImageRgb8(img), 512);
        assert_eq!((out.width(), out.height()), (512, 341));
        assert_eq!(out.get_pixel(100, 100).0, [10, 128, 250, 255]);
        assert_eq!(out.get_pixel(511, 340).0, [10, 128, 250, 255]);
    }

    #[test]
    fn fit_long_edge_never_enlarges_and_handles_small_factors() {
        let img = DynamicImage::ImageRgb8(RgbImage::new(100, 50));
        assert_eq!(fit_long_edge(img.clone(), 500).dimensions(), (100, 50));
        // factor 1.6: no area step, Triangle only
        assert_eq!(fit_long_edge(img, 60).dimensions(), (60, 30));
    }

    #[test]
    fn raw_hint_without_raw_content_is_unsupported() {
        let bytes = png_bytes(10, 10);
        let err = decode_fitted(Cursor::new(bytes), "a.nef", 60, &DecodeLimits::MOBILE).unwrap_err();
        assert!(matches!(err, DecodeError::Unsupported(_)), "{err:?}");
    }
}
