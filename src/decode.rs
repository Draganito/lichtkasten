use std::path::Path;

use egui::ColorImage;
use image::{DynamicImage, RgbaImage};
use lichtkasten_core::decode::{self as core_decode, DecodeLimits, fit_long_edge};
use lichtkasten_core::{orient, raw};

use crate::cache;
use crate::library;

pub const THUMB_EDGE: u32 = 512;
pub const VIEW_EDGE: u32 = 2560;
pub const FULL_VIEW_EDGE: u32 = 8192;
const LIMITS: DecodeLimits = DecodeLimits::DESKTOP;
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
    if library::is_raw_path(path) {
        let img = raw::load_preview_path(path, target.max(1), &LIMITS).map_err(|e| e.to_string())?;
        return to_color_image(&fit_long_edge(img, target.max(VIEW_EDGE)));
    }
    let img = load_oriented(path, FULL_VIEW_EDGE)?;
    to_color_image(&fit_long_edge(img, FULL_VIEW_EDGE))
}

fn load_oriented(path: &Path, target: u32) -> Result<DynamicImage, String> {
    if library::is_raw_path(path) {
        return raw::load_preview_path(path, target.max(1), &LIMITS).map_err(|e| e.to_string());
    }
    let img = core_decode::decode_raster_path(path, &LIMITS).map_err(|e| e.to_string())?;
    Ok(orient::apply(img, orient::from_path(path)))
}

fn to_color_image(rgba: &RgbaImage) -> Result<ColorImage, String> {
    let w = rgba.width() as usize;
    let h = rgba.height() as usize;
    if w == 0 || h == 0 {
        return Err("leeres Bild".into());
    }
    Ok(ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()))
}
