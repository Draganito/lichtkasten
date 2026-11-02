use std::path::Path;

use egui::ColorImage;
use image::{DynamicImage, GenericImageView, ImageReader, Limits, RgbaImage};

use crate::cache;
use crate::library;
use crate::orient;
use crate::raw;

pub const THUMB_EDGE: u32 = 512;
pub const VIEW_EDGE: u32 = 2560;
pub const FULL_VIEW_EDGE: u32 = 8192;
const MAX_PIXELS: u64 = 80_000_000;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
const MAX_THUMB_BYTES: u64 = 200 * 1024 * 1024;

pub fn load_or_cache_thumb(
    cache_dir: &Path,
    path: &Path,
    mtime: u64,
    len: u64,
) -> Result<ColorImage, String> {
    let cache_file = cache::file(cache_dir, path, mtime, len, THUMB_EDGE);
    if let Some(bytes) = cache::read(&cache_file)
        && let Ok(img) = image::load_from_memory(&bytes)
    {
        let rgba = img.to_rgba8();
        return Ok(ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        ));
    }
    if !library::is_raw_path(path) && !library::is_video_path(path) && len > MAX_THUMB_BYTES {
        return Err("Datei zu groß für Miniatur".into());
    }
    let color = load_thumb(path)?;
    if let Some(jpeg) = encode_jpeg(&color) {
        cache::write(&cache_file, &jpeg);
    }
    Ok(color)
}

fn encode_jpeg(image: &ColorImage) -> Option<Vec<u8>> {
    let [w, h] = image.size;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for pixel in &image.pixels {
        let [r, g, b, _] = pixel.to_srgba_unmultiplied();
        rgb.extend_from_slice(&[r, g, b]);
    }
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    encoder
        .encode(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

pub fn load_thumb(path: &Path) -> Result<ColorImage, String> {
    if library::is_video_path(path) {
        return crate::video::thumb(path);
    }
    let img = load_oriented(path, THUMB_EDGE)?;
    to_color_image(&fit_long_edge(img, THUMB_EDGE))
}

pub fn load_view(path: &Path, target: u32) -> Result<ColorImage, String> {
    if library::is_video_path(path) {
        return crate::video::thumb(path);
    }
    if raw::is_raw(path) {
        let img = raw::load_preview(path, target.max(1))?;
        return to_color_image(&fit_long_edge(img, target.max(VIEW_EDGE)));
    }
    let img = load_oriented(path, FULL_VIEW_EDGE)?;
    to_color_image(&fit_long_edge(img, FULL_VIEW_EDGE))
}

fn load_oriented(path: &Path, target: u32) -> Result<DynamicImage, String> {
    if raw::is_raw(path) {
        return raw::load_preview(path, target.max(1));
    }
    Ok(orient::apply(decode_raster(path)?, orient::from_path(path)))
}

fn decode_raster(path: &Path) -> Result<DynamicImage, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("Bild zu groß ({w}×{h})"));
    }

    let mut reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    reader.decode().map_err(|e| e.to_string())
}

fn fit_long_edge(img: DynamicImage, edge: u32) -> RgbaImage {
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

fn to_color_image(rgba: &RgbaImage) -> Result<ColorImage, String> {
    let w = rgba.width() as usize;
    let h = rgba.height() as usize;
    if w == 0 || h == 0 {
        return Err("leeres Bild".into());
    }
    Ok(ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()))
}
