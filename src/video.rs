use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use image::{RgbaImage, imageops::FilterType};
use libloading::Library;

use crate::decode::THUMB_EDGE;
use crate::i18n;

const GST_STATE_NULL: i32 = 1;
const GST_STATE_PAUSED: i32 = 3;
const GST_STATE_PLAYING: i32 = 4;
const GST_FORMAT_TIME: i32 = 3;
const GST_SEEK_FLAG_FLUSH: i32 = 1 << 0;
const GST_SEEK_FLAG_KEY_UNIT: i32 = 1 << 2;
const GST_MAP_READ: i32 = 1;
const GST_SECOND: u64 = 1_000_000_000;
const GST_MESSAGE_EOS: u32 = 1 << 0;
const GST_MESSAGE_ERROR: u32 = 1 << 1;
const GST_STATE_CHANGE_FAILURE: i32 = 0;

struct Api {
    _gst: Library,
    _app: Library,
    _glib: Library,
    _gobject: Library,
    gst_init_check:
        unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> i32,
    gst_element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    gst_element_set_state: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    gst_element_get_state: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32, u64) -> i32,
    gst_element_query_position: unsafe extern "C" fn(*mut c_void, i32, *mut i64) -> i32,
    gst_element_query_duration: unsafe extern "C" fn(*mut c_void, i32, *mut i64) -> i32,
    gst_element_seek_simple: unsafe extern "C" fn(*mut c_void, i32, i32, i64) -> i32,
    gst_element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_object_ref_sink: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_object_unref: unsafe extern "C" fn(*mut c_void),
    gst_bus_timed_pop_filtered: unsafe extern "C" fn(*mut c_void, u64, u32) -> *mut c_void,
    gst_message_parse_error: unsafe extern "C" fn(*mut c_void, *mut *mut GError, *mut *mut c_char),
    gst_message_unref: unsafe extern "C" fn(*mut c_void),
    gst_util_set_object_arg: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
    gst_app_sink_try_pull_sample: unsafe extern "C" fn(*mut c_void, u64) -> *mut c_void,
    gst_app_sink_try_pull_preroll: unsafe extern "C" fn(*mut c_void, u64) -> *mut c_void,
    gst_sample_get_buffer: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_sample_get_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_sample_unref: unsafe extern "C" fn(*mut c_void),
    gst_buffer_map: unsafe extern "C" fn(*mut c_void, *mut GstMapInfo, i32) -> i32,
    gst_buffer_unmap: unsafe extern "C" fn(*mut c_void, *mut GstMapInfo),
    gst_caps_get_structure: unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void,
    gst_structure_get_int: unsafe extern "C" fn(*mut c_void, *const c_char, *mut i32) -> i32,
    g_filename_to_uri:
        unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut GError) -> *mut c_char,
    g_free: unsafe extern "C" fn(*mut c_void),
    g_error_free: unsafe extern "C" fn(*mut GError),
    g_value_init: unsafe extern "C" fn(*mut GValue, usize) -> *mut GValue,
    g_value_set_object: unsafe extern "C" fn(*mut GValue, *mut c_void),
    g_value_unset: unsafe extern "C" fn(*mut GValue),
    g_object_set_property: unsafe extern "C" fn(*mut c_void, *const c_char, *const GValue),
}

const G_TYPE_OBJECT: usize = 20 << 2;

#[repr(C)]
struct GValue {
    g_type: usize,
    data: [u64; 2],
}

#[repr(C)]
struct GError {
    domain: u32,
    code: i32,
    message: *mut c_char,
}

#[repr(C)]
struct GstMapInfo {
    memory: *mut c_void,
    flags: i32,
    _pad: i32,
    data: *mut u8,
    size: usize,
    user_data: [*mut c_void; 4],
    reserved: [*mut c_void; 4],
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    match API.get_or_init(load_api) {
        Ok(api) => Ok(api),
        Err(err) => Err(err.clone()),
    }
}

fn load_api() -> Result<Api, String> {
    unsafe {
        let gst = Library::new("libgstreamer-1.0.so.0").map_err(i18n::gstreamer_missing)?;
        let app = Library::new("libgstapp-1.0.so.0").map_err(i18n::gstreamer_app_missing)?;
        let glib = Library::new("libglib-2.0.so.0").map_err(i18n::glib_missing)?;
        let gobject = Library::new("libgobject-2.0.so.0").map_err(i18n::gobject_missing)?;
        let api = Api {
            gst_init_check: *gst.get(b"gst_init_check\0").map_err(|e| e.to_string())?,
            gst_element_factory_make: *gst
                .get(b"gst_element_factory_make\0")
                .map_err(|e| e.to_string())?,
            gst_element_set_state: *gst
                .get(b"gst_element_set_state\0")
                .map_err(|e| e.to_string())?,
            gst_element_get_state: *gst
                .get(b"gst_element_get_state\0")
                .map_err(|e| e.to_string())?,
            gst_element_query_position: *gst
                .get(b"gst_element_query_position\0")
                .map_err(|e| e.to_string())?,
            gst_element_query_duration: *gst
                .get(b"gst_element_query_duration\0")
                .map_err(|e| e.to_string())?,
            gst_element_seek_simple: *gst
                .get(b"gst_element_seek_simple\0")
                .map_err(|e| e.to_string())?,
            gst_element_get_bus: *gst
                .get(b"gst_element_get_bus\0")
                .map_err(|e| e.to_string())?,
            gst_object_ref_sink: *gst
                .get(b"gst_object_ref_sink\0")
                .map_err(|e| e.to_string())?,
            gst_object_unref: *gst.get(b"gst_object_unref\0").map_err(|e| e.to_string())?,
            gst_bus_timed_pop_filtered: *gst
                .get(b"gst_bus_timed_pop_filtered\0")
                .map_err(|e| e.to_string())?,
            gst_message_parse_error: *gst
                .get(b"gst_message_parse_error\0")
                .map_err(|e| e.to_string())?,
            gst_message_unref: *gst.get(b"gst_message_unref\0").map_err(|e| e.to_string())?,
            gst_util_set_object_arg: *gst
                .get(b"gst_util_set_object_arg\0")
                .map_err(|e| e.to_string())?,
            gst_app_sink_try_pull_sample: *app
                .get(b"gst_app_sink_try_pull_sample\0")
                .map_err(|e| e.to_string())?,
            gst_app_sink_try_pull_preroll: *app
                .get(b"gst_app_sink_try_pull_preroll\0")
                .map_err(|e| e.to_string())?,
            gst_sample_get_buffer: *gst
                .get(b"gst_sample_get_buffer\0")
                .map_err(|e| e.to_string())?,
            gst_sample_get_caps: *gst
                .get(b"gst_sample_get_caps\0")
                .map_err(|e| e.to_string())?,
            gst_sample_unref: *gst.get(b"gst_sample_unref\0").map_err(|e| e.to_string())?,
            gst_buffer_map: *gst.get(b"gst_buffer_map\0").map_err(|e| e.to_string())?,
            gst_buffer_unmap: *gst.get(b"gst_buffer_unmap\0").map_err(|e| e.to_string())?,
            gst_caps_get_structure: *gst
                .get(b"gst_caps_get_structure\0")
                .map_err(|e| e.to_string())?,
            gst_structure_get_int: *gst
                .get(b"gst_structure_get_int\0")
                .map_err(|e| e.to_string())?,
            g_filename_to_uri: *glib
                .get(b"g_filename_to_uri\0")
                .map_err(|e| e.to_string())?,
            g_free: *glib.get(b"g_free\0").map_err(|e| e.to_string())?,
            g_error_free: *glib.get(b"g_error_free\0").map_err(|e| e.to_string())?,
            g_value_init: *gobject.get(b"g_value_init\0").map_err(|e| e.to_string())?,
            g_value_set_object: *gobject
                .get(b"g_value_set_object\0")
                .map_err(|e| e.to_string())?,
            g_value_unset: *gobject.get(b"g_value_unset\0").map_err(|e| e.to_string())?,
            g_object_set_property: *gobject
                .get(b"g_object_set_property\0")
                .map_err(|e| e.to_string())?,
            _gst: gst,
            _app: app,
            _glib: glib,
            _gobject: gobject,
        };
        let mut err = ptr::null_mut();
        if (api.gst_init_check)(ptr::null_mut(), ptr::null_mut(), &mut err) == 0 {
            return Err(take_error(&api, err).unwrap_or_else(|| {
                i18n::t("GStreamer startet nicht", "GStreamer failed to start").into()
            }));
        }
        Ok(api)
    }
}

unsafe fn take_error(api: &Api, err: *mut GError) -> Option<String> {
    unsafe {
        if err.is_null() {
            return None;
        }
        let message = if (*err).message.is_null() {
            i18n::t("unbekannter GStreamer-Fehler", "unknown GStreamer error").into()
        } else {
            CStr::from_ptr((*err).message)
                .to_string_lossy()
                .into_owned()
        };
        (api.g_error_free)(err);
        Some(message)
    }
}

fn file_uri(api: &Api, path: &Path) -> Result<String, String> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let c_path = CString::new(abs.to_string_lossy().as_bytes())
        .map_err(|_| i18n::t("Pfad enthält Nullbytes", "Path contains null bytes").to_string())?;
    unsafe {
        let mut err = ptr::null_mut();
        let raw = (api.g_filename_to_uri)(c_path.as_ptr(), ptr::null(), &mut err);
        if raw.is_null() {
            return Err(take_error(api, err)
                .unwrap_or_else(|| i18n::t("URI ungültig", "Invalid URI").into()));
        }
        let uri = CStr::from_ptr(raw).to_string_lossy().into_owned();
        (api.g_free)(raw as *mut c_void);
        Ok(uri)
    }
}

struct Pipe {
    playbin: *mut c_void,
    sink: *mut c_void,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if let Ok(api) = api() {
            unsafe {
                if !self.playbin.is_null() {
                    (api.gst_element_set_state)(self.playbin, GST_STATE_NULL);
                }
                if !self.sink.is_null() {
                    (api.gst_object_unref)(self.sink);
                }
                if !self.playbin.is_null() {
                    (api.gst_object_unref)(self.playbin);
                }
            }
        }
        self.playbin = ptr::null_mut();
        self.sink = ptr::null_mut();
    }
}

unsafe impl Send for Pipe {}

fn make_element(api: &Api, factory: &CStr, name: &CStr) -> Result<*mut c_void, String> {
    unsafe {
        let element = (api.gst_element_factory_make)(factory.as_ptr(), name.as_ptr());
        if element.is_null() {
            return Err(i18n::missing_plugin(factory.to_string_lossy()));
        }
        (api.gst_object_ref_sink)(element);
        Ok(element)
    }
}

fn set_object_prop(api: &Api, object: *mut c_void, name: &CStr, value: *mut c_void) {
    let mut gvalue = GValue {
        g_type: 0,
        data: [0, 0],
    };
    unsafe {
        (api.g_value_init)(&mut gvalue, G_TYPE_OBJECT);
        (api.g_value_set_object)(&mut gvalue, value);
        (api.g_object_set_property)(object, name.as_ptr(), &gvalue);
        (api.g_value_unset)(&mut gvalue);
    }
}

fn set_arg(api: &Api, object: *mut c_void, name: &CStr, value: &CStr) {
    unsafe {
        (api.gst_util_set_object_arg)(object, name.as_ptr(), value.as_ptr());
    }
}

fn open_pipe(path: &Path, with_audio: bool) -> Result<Pipe, String> {
    let api = api()?;
    let uri = file_uri(api, path)?;
    let playbin = make_element(api, c"playbin", c"lk-play")?;
    let sink = make_element(api, c"appsink", c"lk-video")?;
    set_arg(api, sink, c"caps", c"video/x-raw,format=RGBA");
    set_arg(
        api,
        sink,
        c"max-buffers",
        if with_audio { c"3" } else { c"1" },
    );
    set_arg(api, sink, c"drop", c"true");
    set_arg(
        api,
        sink,
        c"sync",
        if with_audio { c"true" } else { c"false" },
    );
    set_object_prop(api, playbin, c"video-sink", sink);
    if !with_audio && let Ok(fake) = make_element(api, c"fakesink", c"lk-audio") {
        set_object_prop(api, playbin, c"audio-sink", fake);
        unsafe {
            (api.gst_object_unref)(fake);
        }
    }
    let c_uri = CString::new(uri).map_err(|_| "URI".to_string())?;
    unsafe {
        (api.gst_util_set_object_arg)(playbin, c"uri".as_ptr(), c_uri.as_ptr());
    }
    Ok(Pipe { playbin, sink })
}

fn wait_state(api: &Api, playbin: *mut c_void, want: i32, timeout_s: u64) -> Result<(), String> {
    unsafe {
        if (api.gst_element_set_state)(playbin, want) == GST_STATE_CHANGE_FAILURE {
            return Err(bus_error(api, playbin).unwrap_or_else(|| {
                i18n::t("Zustand fehlgeschlagen", "State change failed").into()
            }));
        }
        let mut state = 0;
        let mut pending = 0;
        let ret =
            (api.gst_element_get_state)(playbin, &mut state, &mut pending, timeout_s * GST_SECOND);
        if ret == GST_STATE_CHANGE_FAILURE {
            return Err(bus_error(api, playbin)
                .unwrap_or_else(|| i18n::t("Decoder fehlgeschlagen", "Decoder failed").into()));
        }
        Ok(())
    }
}

unsafe fn bus_error(api: &Api, playbin: *mut c_void) -> Option<String> {
    unsafe {
        let bus = (api.gst_element_get_bus)(playbin);
        if bus.is_null() {
            return None;
        }
        let msg = (api.gst_bus_timed_pop_filtered)(bus, 0, GST_MESSAGE_ERROR);
        (api.gst_object_unref)(bus);
        if msg.is_null() {
            return None;
        }
        let mut err = ptr::null_mut();
        let mut debug = ptr::null_mut();
        (api.gst_message_parse_error)(msg, &mut err, &mut debug);
        (api.gst_message_unref)(msg);
        if !debug.is_null() {
            (api.g_free)(debug as *mut c_void);
        }
        take_error(api, err)
    }
}

unsafe fn drain_bus(api: &Api, playbin: *mut c_void) -> Option<BusEvent> {
    unsafe {
        let bus = (api.gst_element_get_bus)(playbin);
        if bus.is_null() {
            return None;
        }
        let err_msg = (api.gst_bus_timed_pop_filtered)(bus, 0, GST_MESSAGE_ERROR);
        if !err_msg.is_null() {
            let mut err = ptr::null_mut();
            let mut debug = ptr::null_mut();
            (api.gst_message_parse_error)(err_msg, &mut err, &mut debug);
            (api.gst_message_unref)(err_msg);
            if !debug.is_null() {
                (api.g_free)(debug as *mut c_void);
            }
            (api.gst_object_unref)(bus);
            return Some(BusEvent::Error(take_error(api, err).unwrap_or_else(|| {
                i18n::t("Wiedergabe fehlgeschlagen", "Playback failed").into()
            })));
        }
        let eos = (api.gst_bus_timed_pop_filtered)(bus, 0, GST_MESSAGE_EOS);
        (api.gst_object_unref)(bus);
        if eos.is_null() {
            return None;
        }
        (api.gst_message_unref)(eos);
        Some(BusEvent::Eos)
    }
}

enum BusEvent {
    Eos,
    Error(String),
}

struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

fn pull_frame(api: &Api, sink: *mut c_void, preroll: bool, wait: u64) -> Option<Frame> {
    unsafe {
        let sample = if preroll {
            (api.gst_app_sink_try_pull_preroll)(sink, wait)
        } else {
            (api.gst_app_sink_try_pull_sample)(sink, wait)
        };
        if sample.is_null() {
            return None;
        }
        let frame = sample_to_frame(api, sample);
        (api.gst_sample_unref)(sample);
        frame
    }
}

unsafe fn sample_to_frame(api: &Api, sample: *mut c_void) -> Option<Frame> {
    unsafe {
        let caps = (api.gst_sample_get_caps)(sample);
        let buffer = (api.gst_sample_get_buffer)(sample);
        if caps.is_null() || buffer.is_null() {
            return None;
        }
        let structure = (api.gst_caps_get_structure)(caps, 0);
        if structure.is_null() {
            return None;
        }
        let mut width = 0;
        let mut height = 0;
        if (api.gst_structure_get_int)(structure, c"width".as_ptr(), &mut width) == 0
            || (api.gst_structure_get_int)(structure, c"height".as_ptr(), &mut height) == 0
            || width <= 0
            || height <= 0
        {
            return None;
        }
        let mut map = GstMapInfo {
            memory: ptr::null_mut(),
            flags: 0,
            _pad: 0,
            data: ptr::null_mut(),
            size: 0,
            user_data: [ptr::null_mut(); 4],
            reserved: [ptr::null_mut(); 4],
        };
        if (api.gst_buffer_map)(buffer, &mut map, GST_MAP_READ) == 0 || map.data.is_null() {
            return None;
        }
        let w = width as usize;
        let h = height as usize;
        let stride = if h > 0 && map.size >= w * h * 4 {
            (map.size / h).max(w * 4)
        } else {
            w * 4
        };
        let mut pixels = vec![0u8; w * h * 4];
        if map.size >= stride * h {
            for y in 0..h {
                let src = std::slice::from_raw_parts(map.data.add(y * stride), w * 4);
                pixels[y * w * 4..(y + 1) * w * 4].copy_from_slice(src);
            }
        }
        (api.gst_buffer_unmap)(buffer, &mut map);
        Some(Frame {
            width: w,
            height: h,
            pixels,
        })
    }
}

fn frame_to_image(frame: &Frame, max_edge: u32) -> Result<ColorImage, String> {
    let img = RgbaImage::from_raw(
        frame.width as u32,
        frame.height as u32,
        frame.pixels.clone(),
    )
    .ok_or_else(|| i18n::t("Ungültiges Videobild", "Invalid video frame").to_string())?;
    let long = frame.width.max(frame.height) as u32;
    let rgba = if long > max_edge {
        let scale = max_edge as f32 / long as f32;
        let nw = (frame.width as f32 * scale).round().max(1.0) as u32;
        let nh = (frame.height as f32 * scale).round().max(1.0) as u32;
        image::imageops::resize(&img, nw, nh, FilterType::Triangle)
    } else {
        img
    };
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

pub fn thumb(path: &Path) -> Result<ColorImage, String> {
    let api = api()?;
    let pipe = open_pipe(path, false)?;
    wait_state(api, pipe.playbin, GST_STATE_PAUSED, 8)?;
    let duration = query_time(api, pipe.playbin, false);
    if duration > 2.0 {
        seek_to(api, pipe.playbin, 1.0);
        let _ = wait_state(api, pipe.playbin, GST_STATE_PAUSED, 4);
    }
    let frame = pull_frame(api, pipe.sink, true, 2 * GST_SECOND)
        .or_else(|| pull_frame(api, pipe.sink, false, 2 * GST_SECOND))
        .ok_or_else(|| i18n::t("Kein Videobild", "No video frame").to_string())?;
    frame_to_image(&frame, THUMB_EDGE)
}

pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

pub fn probe(path: &Path) -> Option<Probe> {
    let api = api().ok()?;
    let pipe = open_pipe(path, false).ok()?;
    wait_state(api, pipe.playbin, GST_STATE_PAUSED, 6).ok()?;
    let duration = query_time(api, pipe.playbin, false);
    let frame = pull_frame(api, pipe.sink, true, GST_SECOND)
        .or_else(|| pull_frame(api, pipe.sink, false, GST_SECOND))?;
    Some(Probe {
        width: frame.width as u32,
        height: frame.height as u32,
        duration,
    })
}

fn query_time(api: &Api, playbin: *mut c_void, position: bool) -> f64 {
    let mut value = 0i64;
    let ok = unsafe {
        if position {
            (api.gst_element_query_position)(playbin, GST_FORMAT_TIME, &mut value)
        } else {
            (api.gst_element_query_duration)(playbin, GST_FORMAT_TIME, &mut value)
        }
    };
    if ok != 0 && value > 0 {
        value as f64 / GST_SECOND as f64
    } else {
        0.0
    }
}

fn seek_to(api: &Api, playbin: *mut c_void, seconds: f64) {
    let ns = (seconds.max(0.0) * GST_SECOND as f64) as i64;
    unsafe {
        (api.gst_element_seek_simple)(
            playbin,
            GST_FORMAT_TIME,
            GST_SEEK_FLAG_FLUSH | GST_SEEK_FLAG_KEY_UNIT,
            ns,
        );
    }
}

pub struct Player {
    pub path: PathBuf,
    pipe: Option<Pipe>,
    pub error: Option<String>,
    texture: Option<TextureHandle>,
    pub playing: bool,
    pub muted: bool,
    pub duration: f64,
    pub position: f64,
    size: [usize; 2],
}

impl Player {
    pub fn spawn(path: PathBuf) -> std::sync::mpsc::Receiver<Result<Self, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("lk-video".into())
            .spawn(move || {
                let _ = tx.send(Self::open(path));
            })
            .ok();
        rx
    }

    fn open(path: PathBuf) -> Result<Self, String> {
        let api = api()?;
        let pipe = open_pipe(&path, true)?;
        wait_state(api, pipe.playbin, GST_STATE_PAUSED, 12)?;
        let duration = query_time(api, pipe.playbin, false);
        wait_state(api, pipe.playbin, GST_STATE_PLAYING, 6)?;
        Ok(Self {
            path,
            pipe: Some(pipe),
            error: None,
            texture: None,
            playing: true,
            muted: false,
            duration,
            position: 0.0,
            size: [0, 0],
        })
    }

    pub fn texture(&self) -> Option<&TextureHandle> {
        self.texture.as_ref()
    }

    pub fn toggle(&mut self) {
        if self.pipe.is_none() {
            return;
        }
        if self.playing {
            self.pause();
        } else {
            if self.duration > 0.0 && self.position >= self.duration - 0.05 {
                self.seek(0.0);
            }
            self.play();
        }
    }

    pub fn play(&mut self) {
        let Some(pipe) = &self.pipe else {
            return;
        };
        if let Ok(api) = api() {
            unsafe {
                (api.gst_element_set_state)(pipe.playbin, GST_STATE_PLAYING);
            }
            self.playing = true;
        }
    }

    pub fn pause(&mut self) {
        let Some(pipe) = &self.pipe else {
            return;
        };
        if let Ok(api) = api() {
            unsafe {
                (api.gst_element_set_state)(pipe.playbin, GST_STATE_PAUSED);
            }
            self.playing = false;
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        let Some(pipe) = &self.pipe else {
            return;
        };
        if let Ok(api) = api() {
            let target = if self.duration > 0.0 {
                seconds.clamp(0.0, self.duration)
            } else {
                seconds.max(0.0)
            };
            seek_to(api, pipe.playbin, target);
            self.position = target;
        }
    }

    pub fn set_mute(&mut self, mute: bool) {
        let Some(pipe) = &self.pipe else {
            return;
        };
        if let Ok(api) = api() {
            unsafe {
                (api.gst_util_set_object_arg)(
                    pipe.playbin,
                    c"mute".as_ptr(),
                    if mute {
                        c"true".as_ptr()
                    } else {
                        c"false".as_ptr()
                    },
                );
            }
            self.muted = mute;
        }
    }

    pub fn tick(&mut self, ctx: &Context) -> bool {
        let Some(pipe) = &self.pipe else {
            return false;
        };
        let playbin = pipe.playbin;
        let sink = pipe.sink;
        let Ok(api) = api() else {
            return false;
        };
        let event = unsafe { drain_bus(api, playbin) };
        match event {
            Some(BusEvent::Eos) => {
                self.pause();
                if self.duration > 0.0 {
                    self.position = self.duration;
                }
            }
            Some(BusEvent::Error(error)) => {
                self.error = Some(error);
                self.playing = false;
            }
            None => {}
        }
        if self.duration <= 0.0 {
            self.duration = query_time(api, playbin, false);
        }
        let pos = query_time(api, playbin, true);
        if pos > 0.0 {
            self.position = pos;
        }
        if let Some(frame) = pull_frame(api, sink, false, 0)
            && let Ok(image) = frame_to_image(&frame, 1920)
        {
            let size = image.size;
            if self.texture.as_ref().is_some_and(|tex| tex.size() == size) {
                if let Some(texture) = &mut self.texture {
                    texture.set(image, TextureOptions::LINEAR);
                }
            } else {
                self.texture = Some(ctx.load_texture(
                    format!("video:{}", self.path.display()),
                    image,
                    TextureOptions::LINEAR,
                ));
            }
            self.size = size;
        }
        if self.playing {
            ctx.request_repaint();
        }
        self.playing
    }
}

pub fn format_time(seconds: f64) -> String {
    if seconds <= 0.0 || !seconds.is_finite() {
        return "0:00".into();
    }
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn init() {
    let _ = api();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_from_system_mp4() {
        let path = Path::new(
            "/home/dragan/Dokumente/Projekte/PlatformIO/kicad-mcp/contrib/aristo-d2-led-panel/head_on_enlarger.mp4",
        );
        if !path.exists() {
            return;
        }
        let image = thumb(path).expect("GStreamer-Miniatur");
        assert!(image.width() > 8 && image.height() > 8);
    }
}
