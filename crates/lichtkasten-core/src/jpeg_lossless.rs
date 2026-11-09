//! Lossless JPEG cropping: the DCT coefficient blocks of the chosen region
//! are copied into a new file without ever being decoded, so the output
//! has exactly the pixels of the source, no new compression generation.
//! This is `jpegtran -crop`, through libjpeg-turbo's `transupp` as shipped
//! in mozjpeg-sys.
//!
//! The one constraint of the format: the crop origin must sit on the iMCU
//! grid (8 px, or 16 px with chroma subsampling). [`snap`] moves the
//! origin to the nearest grid line and keeps the far edges where they were,
//! so a crop drifts by at most half an MCU. The result rectangle is
//! reported back, so the caller can show what it really got.
//!
//! No marker of the source survives except the ICC profile (APP2): no
//! EXIF, no XMP, no thumbnails, no comments. The caller adds what it wants.
//!
//! Only `cfg(feature = "lossless-jpeg")`.

use std::ffi::{c_int, c_ulong};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use mozjpeg_sys::*;

use crate::decode::{DecodeError, DecodeLimits};

/// `jpeg_transform_info` as the vendored `transupp.h` really lays it out.
/// mozjpeg-sys 2.2.3 ships a binding that predates the `drop`/`wipe`
/// operations (four fields short), so libjpeg would write past the end of
/// that struct. This one matches the C header field for field; the
/// functions below are bound against it. Do not "simplify" to the crate's
/// type.
#[repr(C)]
struct TransformInfo {
    transform: JXFORM_CODE,
    perfect: boolean,
    trim: boolean,
    force_grayscale: boolean,
    crop: boolean,
    slow_hflip: boolean,
    crop_width: JDIMENSION,
    crop_width_set: JCROP_CODE,
    crop_height: JDIMENSION,
    crop_height_set: JCROP_CODE,
    crop_xoffset: JDIMENSION,
    crop_xoffset_set: JCROP_CODE,
    crop_yoffset: JDIMENSION,
    crop_yoffset_set: JCROP_CODE,
    drop_ptr: j_decompress_ptr,
    drop_coef_arrays: *mut jvirt_barray_ptr,
    num_components: c_int,
    workspace_coef_arrays: *mut jvirt_barray_ptr,
    output_width: JDIMENSION,
    output_height: JDIMENSION,
    x_crop_offset: JDIMENSION,
    y_crop_offset: JDIMENSION,
    drop_width: JDIMENSION,
    drop_height: JDIMENSION,
    imcu_sample_width: c_int,
    imcu_sample_height: c_int,
}

unsafe extern "C-unwind" {
    #[link_name = "jtransform_request_workspace"]
    fn tr_request_workspace(srcinfo: j_decompress_ptr, info: *mut TransformInfo) -> boolean;
    #[link_name = "jtransform_adjust_parameters"]
    fn tr_adjust_parameters(
        srcinfo: j_decompress_ptr,
        dstinfo: j_compress_ptr,
        src_coef_arrays: *mut jvirt_barray_ptr,
        info: *mut TransformInfo,
    ) -> *mut jvirt_barray_ptr;
    #[link_name = "jtransform_execute_transform"]
    fn tr_execute_transform(
        srcinfo: j_decompress_ptr,
        dstinfo: j_compress_ptr,
        src_coef_arrays: *mut jvirt_barray_ptr,
        info: *mut TransformInfo,
    );
}

/// A region in stored (unrotated) pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropBox {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// What the header says, before any pixel is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    /// iMCU size in pixels: 8 or 16 per axis, depending on subsampling.
    pub mcu_width: u32,
    pub mcu_height: u32,
}

/// The new file plus the rectangle it really contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LosslessCrop {
    pub jpeg: Vec<u8>,
    pub rect: CropBox,
}

const DCTSIZE: u32 = 8;

/// Moves the origin of `rect` to the nearest iMCU grid line, keeps the
/// right and bottom edges, and clamps everything into the picture.
/// Pure arithmetic; what [`crop_lossless`] will actually cut.
pub fn snap(rect: CropBox, info: &JpegInfo) -> CropBox {
    fn snap_axis(start: u32, len: u32, grid: u32, total: u32) -> (u32, u32) {
        let grid = grid.max(1);
        let total = total.max(1);
        let end = start.saturating_add(len).min(total);
        let mut s = ((start + grid / 2) / grid) * grid;
        if s >= end {
            // The nearest grid line is at or past the far edge; take the one below.
            s = (end.saturating_sub(1) / grid) * grid;
        }
        let s = s.min(total - 1);
        (s, (end - s).max(1))
    }
    let (left, width) = snap_axis(rect.left, rect.width, info.mcu_width, info.width);
    let (top, height) = snap_axis(rect.top, rect.height, info.mcu_height, info.height);
    CropBox {
        left,
        top,
        width,
        height,
    }
}

/// Dimensions and iMCU grid of a JPEG in memory.
pub fn jpeg_info(data: &[u8]) -> Result<JpegInfo, DecodeError> {
    guarded(|| {
        let mut src = Decompress::new()?;
        src.feed(data);
        src.read_header()?;
        Ok(src.info())
    })
}

/// Cuts `rect` (after [`snap`]) out of `data` without decoding it.
/// Memory is bounded by `limits.max_alloc`: libjpeg holds the source's
/// and the output's coefficient arrays at once, which is about two bytes
/// per pixel per component.
pub fn crop_lossless(
    data: &[u8],
    rect: CropBox,
    limits: &DecodeLimits,
) -> Result<LosslessCrop, DecodeError> {
    if data.len() as u64 > limits.max_file {
        return Err(DecodeError::TooLarge(format!("{} bytes", data.len())));
    }
    guarded(|| {
        let mut src = Decompress::new()?;
        src.feed(data);
        // SAFETY: `src.raw` is a live, created decompress object with a source set.
        unsafe { jcopy_markers_setup(&mut src.raw, JCOPY_OPTION_JCOPYOPT_ICC) };
        src.read_header()?;
        let info = src.info();
        let rect = snap(rect, &info);
        if rect.width == 0
            || rect.height == 0
            || rect.left + rect.width > info.width
            || rect.top + rect.height > info.height
        {
            return Err(DecodeError::Corrupt("crop outside the picture".into()));
        }

        let coef_bytes = src.coefficient_bytes(info.width, info.height)
            + src.coefficient_bytes(rect.width, rect.height);
        if coef_bytes > limits.max_alloc {
            return Err(DecodeError::TooLarge(format!(
                "{} MB of coefficients",
                coef_bytes >> 20
            )));
        }

        // SAFETY: plain-old-data struct; every field we rely on is set below.
        let mut t: TransformInfo = unsafe { mem::zeroed() };
        t.transform = JXFORM_CODE_JXFORM_NONE;
        t.crop = 1;
        t.crop_xoffset = rect.left;
        t.crop_xoffset_set = JCROP_CODE_JCROP_POS;
        t.crop_yoffset = rect.top;
        t.crop_yoffset_set = JCROP_CODE_JCROP_POS;
        t.crop_width = rect.width;
        t.crop_width_set = JCROP_CODE_JCROP_POS;
        t.crop_height = rect.height;
        t.crop_height_set = JCROP_CODE_JCROP_POS;

        let mut dst = Compress::new()?;
        let mut out = MemDest::default();

        // SAFETY: the jpegtran sequence, on objects that live until the end
        // of this closure; `t` outlives every call that holds a pointer to
        // it. Errors inside libjpeg unwind through `error_exit` and are
        // caught by `guarded`; Drop then destroys both objects.
        unsafe {
            if tr_request_workspace(&mut src.raw, &mut t) == 0 {
                return Err(DecodeError::Unsupported("transform not possible".into()));
            }
            let src_coefs = jpeg_read_coefficients(&mut src.raw);
            if src_coefs.is_null() {
                return Err(DecodeError::Corrupt("no coefficients".into()));
            }
            jpeg_copy_critical_parameters(&src.raw, &mut dst.raw);
            // libjpeg-turbo behaviour, not mozjpeg's progressive/trellis
            // defaults: baseline output, optimised Huffman tables.
            jpeg_c_set_int_param(
                &mut dst.raw,
                J_INT_PARAM::JINT_COMPRESS_PROFILE,
                JCP_FASTEST as i32,
            );
            let dst_coefs = tr_adjust_parameters(&mut src.raw, &mut dst.raw, src_coefs, &mut t);
            dst.raw.optimize_coding = 1;
            jpeg_mem_dest(&mut dst.raw, &mut out.buf, &mut out.size);
            jpeg_write_coefficients(&mut dst.raw, dst_coefs);
            jcopy_markers_execute(&mut src.raw, &mut dst.raw, JCOPY_OPTION_JCOPYOPT_ICC);
            tr_execute_transform(&mut src.raw, &mut dst.raw, src_coefs, &mut t);
            jpeg_finish_compress(&mut dst.raw);
            jpeg_finish_decompress(&mut src.raw);
        }
        let rect = CropBox {
            left: rect.left,
            top: rect.top,
            width: t.output_width,
            height: t.output_height,
        };
        Ok(LosslessCrop {
            jpeg: out.take(),
            rect,
        })
    })
}

// ---- libjpeg plumbing -------------------------------------------------------

/// libjpeg reports errors by calling `error_exit`, which must not return.
/// We unwind out of it (mozjpeg-sys is built with `-fexceptions` and
/// `C-unwind` ABIs for exactly this) and turn the panic into an error.
fn guarded<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "libjpeg failed".to_string());
            Err(classify(msg))
        }
    }
}

fn classify(msg: String) -> DecodeError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("insufficient memory") || lower.contains("too large") {
        DecodeError::TooLarge(msg)
    } else if lower.contains("unsupported")
        || lower.contains("not supported")
        || lower.contains("arithmetic")
    {
        DecodeError::Unsupported(msg)
    } else {
        DecodeError::Corrupt(msg)
    }
}

unsafe extern "C-unwind" fn error_exit(cinfo: &mut jpeg_common_struct) {
    // The binding types the buffer as `&[u8; 80]`; libjpeg writes through it.
    let buf = [0u8; 80];
    // SAFETY: `cinfo.err` was set by us to a live error manager.
    let err = unsafe { &*cinfo.err };
    if let Some(format) = err.format_message {
        // SAFETY: libjpeg's own formatter writing into an 80-byte buffer, as its API requires.
        unsafe { format(cinfo, &buf) };
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let msg = String::from_utf8_lossy(&buf[..end]).into_owned();
    std::panic::panic_any(format!("libjpeg: {msg}"));
}

unsafe extern "C-unwind" fn silence(_: &mut jpeg_common_struct) {}
unsafe extern "C-unwind" fn silence_level(_: &mut jpeg_common_struct, _: std::ffi::c_int) {}

fn error_manager() -> Box<jpeg_error_mgr> {
    // SAFETY: `jpeg_std_error` fills in every field of a zeroed struct.
    let mut err: Box<jpeg_error_mgr> = Box::new(unsafe { mem::zeroed() });
    unsafe { jpeg_std_error(&mut err) };
    err.error_exit = Some(error_exit);
    err.output_message = Some(silence);
    err.emit_message = Some(silence_level);
    err
}

struct Decompress {
    raw: jpeg_decompress_struct,
    _err: Box<jpeg_error_mgr>,
}

impl Decompress {
    fn new() -> Result<Self, DecodeError> {
        let mut err = error_manager();
        // SAFETY: zeroed struct, error manager attached before creation, as libjpeg requires.
        let raw = unsafe {
            let mut raw: jpeg_decompress_struct = mem::zeroed();
            raw.common.err = &mut *err;
            jpeg_create_decompress(&mut raw);
            raw
        };
        Ok(Self { raw, _err: err })
    }

    fn feed(&mut self, data: &[u8]) {
        // SAFETY: the caller keeps `data` alive for the lifetime of the
        // object (both live in the same closure); libjpeg reads it only.
        unsafe { jpeg_mem_src(&mut self.raw, data.as_ptr(), data.len() as c_ulong) };
    }

    fn read_header(&mut self) -> Result<(), DecodeError> {
        // SAFETY: created object with a source attached.
        // JPEG_HEADER_OK == 1. Arithmetic-coded files fail inside libjpeg
        // ("Unsupported JPEG process"), which is not compiled with that
        // decoder, and surface as `Unsupported` through `classify`.
        if unsafe { jpeg_read_header(&mut self.raw, 1) } != 1 {
            return Err(DecodeError::Corrupt("not a JPEG".into()));
        }
        Ok(())
    }

    fn info(&self) -> JpegInfo {
        JpegInfo {
            width: self.raw.image_width,
            height: self.raw.image_height,
            mcu_width: DCTSIZE * self.raw.max_h_samp_factor.max(1) as u32,
            mcu_height: DCTSIZE * self.raw.max_v_samp_factor.max(1) as u32,
        }
    }

    /// Bytes libjpeg needs for the coefficient arrays of a `w`×`h` picture
    /// with this file's components and sampling.
    fn coefficient_bytes(&self, w: u32, h: u32) -> u64 {
        let n = self.raw.num_components.max(0) as usize;
        let max_h = self.raw.max_h_samp_factor.max(1) as u64;
        let max_v = self.raw.max_v_samp_factor.max(1) as u64;
        let mut total = 0u64;
        for i in 0..n {
            // SAFETY: `comp_info` has `num_components` entries after read_header.
            let c = unsafe { &*self.raw.comp_info.add(i) };
            let cw = (w as u64 * c.h_samp_factor.max(1) as u64).div_ceil(max_h);
            let ch = (h as u64 * c.v_samp_factor.max(1) as u64).div_ceil(max_v);
            let blocks = cw.div_ceil(DCTSIZE as u64) * ch.div_ceil(DCTSIZE as u64);
            total += blocks * (DCTSIZE * DCTSIZE) as u64 * mem::size_of::<JCOEF>() as u64;
        }
        total
    }
}

impl Drop for Decompress {
    fn drop(&mut self) {
        // SAFETY: created in `new`; destroy is valid in every state, including after an error.
        unsafe { jpeg_destroy_decompress(&mut self.raw) };
    }
}

struct Compress {
    raw: jpeg_compress_struct,
    _err: Box<jpeg_error_mgr>,
}

impl Compress {
    fn new() -> Result<Self, DecodeError> {
        let mut err = error_manager();
        // SAFETY: as in `Decompress::new`.
        let raw = unsafe {
            let mut raw: jpeg_compress_struct = mem::zeroed();
            raw.common.err = &mut *err;
            jpeg_create_compress(&mut raw);
            raw
        };
        Ok(Self { raw, _err: err })
    }
}

impl Drop for Compress {
    fn drop(&mut self) {
        // SAFETY: created in `new`.
        unsafe { jpeg_destroy_compress(&mut self.raw) };
    }
}

/// The buffer `jpeg_mem_dest` mallocs; freed here whatever happens.
struct MemDest {
    buf: *mut u8,
    size: c_ulong,
}

impl Default for MemDest {
    fn default() -> Self {
        Self {
            buf: ptr::null_mut(),
            size: 0,
        }
    }
}

impl MemDest {
    fn take(&mut self) -> Vec<u8> {
        if self.buf.is_null() {
            return Vec::new();
        }
        // SAFETY: libjpeg wrote `size` bytes into `buf`.
        unsafe { std::slice::from_raw_parts(self.buf, self.size as usize) }.to_vec()
    }
}

impl Drop for MemDest {
    fn drop(&mut self) {
        if !self.buf.is_null() {
            // SAFETY: allocated by libjpeg with malloc/realloc; we own it after finish_compress.
            unsafe { libc::free(self.buf.cast()) };
            self.buf = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ColorType, DynamicImage, ImageBuffer, Luma, Rgb};
    use std::io::Cursor;

    const LIMITS: DecodeLimits = DecodeLimits::DESKTOP;

    fn encode<P: image::Pixel<Subpixel = u8>>(
        img: &ImageBuffer<P, Vec<u8>>,
        color: ColorType,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 85)
            .encode(img.as_raw(), img.width(), img.height(), color.into())
            .unwrap();
        out
    }

    fn decode(data: &[u8]) -> DynamicImage {
        image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).unwrap()
    }

    fn pattern_gray(w: u32, h: u32) -> ImageBuffer<Luma<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, y| {
            Luma([((x * 7) ^ (y * 13) ^ ((x * y) >> 3)) as u8])
        })
    }

    fn pattern_rgb(w: u32, h: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([(x * 3) as u8, (y * 5) as u8, ((x + y) * 2) as u8])
        })
    }

    #[test]
    fn snap_rounds_origin_and_keeps_far_edge() {
        let info = JpegInfo {
            width: 100,
            height: 70,
            mcu_width: 16,
            mcu_height: 16,
        };
        let r = snap(
            CropBox {
                left: 21,
                top: 9,
                width: 40,
                height: 30,
            },
            &info,
        );
        assert_eq!(
            r,
            CropBox {
                left: 16,
                top: 16,
                width: 45,
                height: 23
            }
        );
        let r = snap(
            CropBox {
                left: 25,
                top: 7,
                width: 40,
                height: 30,
            },
            &info,
        );
        assert_eq!(
            r,
            CropBox {
                left: 32,
                top: 0,
                width: 33,
                height: 37
            }
        );
        // Near the far edge: the nearest grid line still inside wins.
        let r = snap(
            CropBox {
                left: 95,
                top: 65,
                width: 5,
                height: 5,
            },
            &info,
        );
        assert_eq!(
            r,
            CropBox {
                left: 96,
                top: 64,
                width: 4,
                height: 6
            }
        );
        // Nearest grid line at or past the far edge: fall back to the one below.
        let r = snap(
            CropBox {
                left: 97,
                top: 68,
                width: 3,
                height: 2,
            },
            &info,
        );
        assert_eq!(
            r,
            CropBox {
                left: 96,
                top: 64,
                width: 4,
                height: 6
            }
        );
        let r = snap(
            CropBox {
                left: 60,
                top: 0,
                width: 3,
                height: 70,
            },
            &info,
        );
        assert_eq!(
            r,
            CropBox {
                left: 48,
                top: 0,
                width: 15,
                height: 70
            }
        );
        // Full picture stays the full picture.
        assert_eq!(
            snap(
                CropBox {
                    left: 0,
                    top: 0,
                    width: 100,
                    height: 70
                },
                &info
            ),
            CropBox {
                left: 0,
                top: 0,
                width: 100,
                height: 70
            }
        );
    }

    #[test]
    fn info_reads_dimensions_and_mcu() {
        let data = encode(&pattern_gray(100, 70), ColorType::L8);
        let info = jpeg_info(&data).unwrap();
        assert_eq!((info.width, info.height), (100, 70));
        assert_eq!((info.mcu_width, info.mcu_height), (8, 8));
        let data = encode(&pattern_rgb(100, 70), ColorType::Rgb8);
        let info = jpeg_info(&data).unwrap();
        assert!(info.mcu_width == 8 || info.mcu_width == 16);
    }

    #[test]
    fn grayscale_crop_is_pixel_exact() {
        let src = pattern_gray(100, 70);
        let data = encode(&src, ColorType::L8);
        let want = CropBox {
            left: 16,
            top: 24,
            width: 41,
            height: 30,
        };
        let out = crop_lossless(&data, want, &LIMITS).unwrap();
        assert_eq!(out.rect, want);
        let full = decode(&data).to_luma8();
        let cut = decode(&out.jpeg).to_luma8();
        assert_eq!((cut.width(), cut.height()), (41, 30));
        for y in 0..30 {
            for x in 0..41 {
                assert_eq!(
                    cut.get_pixel(x, y),
                    full.get_pixel(x + 16, y + 24),
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn colour_crop_matches_inside_the_border() {
        let data = encode(&pattern_rgb(128, 96), ColorType::Rgb8);
        let info = jpeg_info(&data).unwrap();
        let want = snap(
            CropBox {
                left: 30,
                top: 18,
                width: 60,
                height: 50,
            },
            &info,
        );
        let out = crop_lossless(&data, want, &LIMITS).unwrap();
        assert_eq!(out.rect, want);
        let full = decode(&data).to_rgb8();
        let cut = decode(&out.jpeg).to_rgb8();
        // Chroma upsampling looks at neighbouring blocks, so the outermost
        // pixels may differ; everything inside is identical.
        for y in 2..want.height - 2 {
            for x in 2..want.width - 2 {
                assert_eq!(
                    cut.get_pixel(x, y),
                    full.get_pixel(x + want.left, y + want.top),
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn unaligned_request_is_snapped_and_reported() {
        let data = encode(&pattern_gray(100, 70), ColorType::L8);
        let out = crop_lossless(
            &data,
            CropBox {
                left: 13,
                top: 5,
                width: 40,
                height: 30,
            },
            &LIMITS,
        )
        .unwrap();
        assert_eq!(
            out.rect,
            CropBox {
                left: 16,
                top: 8,
                width: 37,
                height: 27
            }
        );
        let cut = decode(&out.jpeg);
        assert_eq!((cut.width(), cut.height()), (37, 27));
    }

    #[test]
    fn garbage_and_other_formats_are_errors_not_panics() {
        assert!(matches!(
            jpeg_info(b"not a jpeg at all"),
            Err(DecodeError::Corrupt(_))
        ));
        let mut png = Vec::new();
        DynamicImage::ImageLuma8(pattern_gray(8, 8))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(matches!(jpeg_info(&png), Err(DecodeError::Corrupt(_))));
        let data = encode(&pattern_gray(100, 70), ColorType::L8);
        let truncated = &data[..data.len() / 2];
        // Either an error or a (padded) result; never a panic across the boundary.
        let _ = crop_lossless(
            truncated,
            CropBox {
                left: 0,
                top: 0,
                width: 50,
                height: 50,
            },
            &LIMITS,
        );
    }

    #[test]
    fn memory_budget_is_enforced() {
        let data = encode(&pattern_gray(100, 70), ColorType::L8);
        let tiny = DecodeLimits {
            max_pixels: 1_000_000,
            max_alloc: 1024,
            max_file: 1 << 30,
        };
        assert!(matches!(
            crop_lossless(
                &data,
                CropBox {
                    left: 0,
                    top: 0,
                    width: 100,
                    height: 70
                },
                &tiny
            ),
            Err(DecodeError::TooLarge(_))
        ));
    }

    #[test]
    fn source_markers_are_dropped() {
        // A JFIF APP0 is what the encoder writes; the output must have no APP1 (EXIF) or COM.
        let data = encode(&pattern_gray(64, 64), ColorType::L8);
        let out = crop_lossless(
            &data,
            CropBox {
                left: 0,
                top: 0,
                width: 64,
                height: 64,
            },
            &LIMITS,
        )
        .unwrap();
        assert_eq!(&out.jpeg[..2], &[0xFF, 0xD8]);
        let mut i = 2;
        while i + 4 <= out.jpeg.len() && out.jpeg[i] == 0xFF {
            let marker = out.jpeg[i + 1];
            if marker == 0xDA {
                break;
            }
            assert_ne!(marker, 0xE1, "APP1 present");
            assert_ne!(marker, 0xFE, "COM present");
            let len = u16::from_be_bytes([out.jpeg[i + 2], out.jpeg[i + 3]]) as usize;
            i += 2 + len;
        }
    }
}
