use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use lru::LruCache;

use crate::cache;
use crate::decode::{FULL_VIEW_EDGE, VIEW_EDGE};
use crate::jobs::{self, Done, JobJoin, JobQueue, Work};
use crate::library;
use crate::library::Photo;
use crate::meta::FileInfo;

/// Grundkapazität des Texturcaches; wächst mit, wenn mehr sichtbar ist.
const TEXTURE_LIMIT: usize = 240;
/// Reserve über der sichtbaren Menge, damit Scrollen nicht sofort verdrängt.
const TEXTURE_MARGIN: usize = 64;
const APPLY_BUDGET: Duration = Duration::from_millis(4);

/// Hochgeladene Miniatur samt dem, was sie abdecken kann.
struct Tex {
    handle: TextureHandle,
    /// Kurze Seite in Pixeln; muss die Zelle (Cover-Zuschnitt) abdecken.
    short: u32,
    /// Volle Auflösung des Plattencaches, größer geht nicht.
    full: bool,
}

pub struct ThumbService {
    generation: u64,
    jobs: JobQueue,
    results: JobJoin,
    inflight: HashSet<PathBuf>,
    /// Alles, was in diesem Frame sichtbar gemeldet wurde (Raster, Filmstreifen,
    /// Album-Deckblätter). Jobs außerhalb dieser Menge werden verworfen.
    wanted: HashSet<PathBuf>,
    textures: LruCache<PathBuf, Tex>,
    /// Zellengröße in Gerätepixeln; Texturen werden darauf verkleinert.
    cell_px: u32,
    failed: HashSet<PathBuf>,
    pub pending: usize,
    pub ready: usize,
    view: Option<(PathBuf, TextureHandle)>,
    view_loading: Option<PathBuf>,
    pub more_results: bool,
    capture_inflight: HashSet<PathBuf>,
    album_inflight: HashSet<PathBuf>,
    album_cursor: usize,
    capture_cursor: usize,
    inbox: VecDeque<Done>,
}

pub struct PollOut {
    pub meta: Option<FileInfo>,
    pub captures: Vec<(PathBuf, Option<u64>)>,
    pub albums: Vec<(PathBuf, Vec<String>, Vec<String>)>,
    /// Dateien, deren XMP gerade geschrieben wurde (für den Suchindex).
    pub written: Vec<PathBuf>,
}

impl ThumbService {
    pub fn new(ctx: Context) -> Self {
        let (jobs, results) = JobQueue::start(ctx);

        Self {
            generation: 1,
            jobs,
            results,
            inflight: HashSet::new(),
            wanted: HashSet::new(),
            textures: LruCache::new(NonZeroUsize::new(TEXTURE_LIMIT).unwrap()),
            cell_px: 512,
            failed: HashSet::new(),
            pending: 0,
            ready: 0,
            view: None,
            view_loading: None,
            more_results: false,
            capture_inflight: HashSet::new(),
            album_inflight: HashSet::new(),
            album_cursor: 0,
            capture_cursor: 0,
            inbox: VecDeque::new(),
        }
    }

    pub fn reset_folder(&mut self) {
        self.generation += 1;
        self.jobs.clear_generation(self.generation);
        self.inflight.clear();
        self.wanted.clear();
        self.textures.clear();
        self.textures
            .resize(NonZeroUsize::new(TEXTURE_LIMIT).unwrap());
        self.failed.clear();
        self.pending = 0;
        self.ready = 0;
        self.view = None;
        self.view_loading = None;
        self.more_results = false;
        self.capture_inflight.clear();
        self.album_inflight.clear();
        self.album_cursor = 0;
        self.capture_cursor = 0;
        self.inbox.clear();
        while self.results.rx.try_recv().is_ok() {}
    }

    pub fn poll(&mut self, ctx: &Context) -> PollOut {
        // Neuer Frame: Sichtbarkeit wird von den Zeichenroutinen frisch gemeldet.
        self.wanted.clear();
        while let Ok(done) = self.results.rx.try_recv() {
            self.inbox.push_back(done);
        }

        let deadline = jobs::budget(APPLY_BUDGET);
        let mut out = PollOut {
            meta: None,
            captures: Vec::new(),
            albums: Vec::new(),
            written: Vec::new(),
        };
        self.more_results = false;
        let mut textures = 0usize;

        // Miniaturen zuerst, Aufnahmedaten füllen den Rest.
        let mut rest = VecDeque::new();
        while let Some(done) = self.inbox.pop_front() {
            if !jobs::within(deadline) {
                rest.push_back(done);
                rest.append(&mut self.inbox);
                break;
            }
            let is_texture = matches!(done, Done::Thumb { .. } | Done::View { .. });
            if is_texture && textures >= 6 {
                rest.push_back(done);
                continue;
            }
            self.apply(ctx, done, &mut out);
            if is_texture {
                textures += 1;
            }
        }
        self.inbox = rest;
        self.more_results = !self.inbox.is_empty();
        if self.more_results {
            ctx.request_repaint();
        }
        out
    }

    fn apply(&mut self, ctx: &Context, done: Done, out: &mut PollOut) {
        match done {
            Done::Thumb {
                path,
                generation,
                image,
            } => {
                self.inflight.remove(&path);
                self.pending = self.inflight.len();
                if generation != self.generation {
                    return;
                }
                match image {
                    Ok(image) => {
                        // Auf Anzeigegröße verkleinern: spart GPU-Speicher und
                        // filtert sauberer als 512 px → 72 px ohne Mipmaps.
                        let source_short = image.width().min(image.height()) as u32;
                        let factor = shrink_factor(source_short, self.cell_px);
                        let image = if factor > 1 {
                            shrink(&image, factor)
                        } else {
                            image
                        };
                        let short = image.width().min(image.height()) as u32;
                        let handle = ctx.load_texture(
                            path.to_string_lossy(),
                            image,
                            TextureOptions::LINEAR
                                .with_mipmap_mode(Some(egui::TextureFilter::Linear)),
                        );
                        if !self.textures.contains(&path) {
                            self.ready += 1;
                        }
                        self.textures.put(
                            path,
                            Tex {
                                handle,
                                short,
                                full: factor == 1,
                            },
                        );
                    }
                    Err(_) => {
                        self.failed.insert(path);
                    }
                }
            }
            Done::View {
                path,
                generation,
                image,
            } => {
                if generation != self.generation {
                    return;
                }
                if self.view_loading.as_ref() != Some(&path) {
                    return;
                }
                self.view_loading = None;
                if let Ok(image) = image {
                    let texture = ctx.load_texture(
                        format!("view:{}", path.display()),
                        image,
                        TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
                    );
                    self.view = Some((path, texture));
                }
            }
            Done::Meta(info) => out.meta = Some(info),
            Done::Capture {
                path,
                generation,
                stamp,
            } => {
                self.capture_inflight.remove(&path);
                if generation == self.generation {
                    out.captures.push((path, stamp));
                }
            }
            Done::Albums {
                path,
                generation,
                albums,
                covers,
            } => {
                self.album_inflight.remove(&path);
                if generation == 0 {
                    out.written.push(path.clone());
                }
                if generation == 0 || generation == self.generation {
                    out.albums.push((path, albums, covers));
                }
            }
        }
    }

    pub fn texture(&mut self, path: &Path) -> Option<TextureHandle> {
        self.textures.get(path).map(|tex| tex.handle.clone())
    }

    /// Zellengröße der Miniaturen in Punkten; vor dem Zeichnen je Frame setzen.
    pub fn set_cell(&mut self, points: f32, pixels_per_point: f32) {
        self.cell_px = (points * pixels_per_point).ceil().max(1.0) as u32;
    }

    /// Passt die vorhandene Textur nicht mehr zur Zelle (zu klein, oder so
    /// groß, dass eine ganzzahlige Verkleinerung Speicher spart), wird sie
    /// neu geladen; bis dahin bleibt die alte sichtbar.
    fn needs_reload(&self, path: &Path) -> bool {
        let Some(tex) = self.textures.peek(path) else {
            return false;
        };
        let too_small = tex.short < self.cell_px && !tex.full;
        let too_big = shrink_factor(tex.short, self.cell_px) >= 2;
        too_small || too_big
    }

    pub fn view_is_loading(&self) -> bool {
        self.view_loading.is_some()
    }

    pub fn failed(&self, path: &Path) -> bool {
        self.failed.contains(path)
    }

    pub fn request_visible(&mut self, photos: &[Photo]) {
        let generation = self.generation;
        self.wanted
            .extend(photos.iter().map(|photo| photo.path.clone()));
        // Der Cache muss alles Sichtbare halten, sonst verdrängen sich die
        // Miniaturen gegenseitig und flimmern.
        let capacity = (self.wanted.len() + TEXTURE_MARGIN).max(TEXTURE_LIMIT);
        if capacity != self.textures.cap().get() {
            self.textures.resize(NonZeroUsize::new(capacity).unwrap());
        }
        let wanted = &self.wanted;
        self.jobs.retain_thumbs(|path| wanted.contains(path));
        let view_loading = self.view_loading.as_ref();
        self.inflight
            .retain(|path| wanted.contains(path) || view_loading == Some(path));
        self.pending = self.inflight.len();

        for photo in photos {
            if self.failed.contains(&photo.path) {
                continue;
            }
            if self.textures.contains(&photo.path) && !self.needs_reload(&photo.path) {
                continue;
            }
            if self.inflight.contains(&photo.path) {
                continue;
            }
            if jobs::skip_thumb(&photo.path, photo.len) {
                self.failed.insert(photo.path.clone());
                continue;
            }
            let cache_dir = cache::dir_for(&photo.path);
            let heavy = jobs::is_heavy_file(&photo.path, photo.len);
            self.jobs.submit(
                Work::Thumb {
                    path: photo.path.clone(),
                    mtime: photo.mtime,
                    len: photo.len,
                    generation,
                    cache_dir,
                },
                heavy,
            );
            self.inflight.insert(photo.path.clone());
        }
        self.pending = self.inflight.len();
    }

    pub fn request_view(&mut self, photo: &Photo, window_long_edge: u32) {
        if library::is_video_path(&photo.path) {
            if self
                .view
                .as_ref()
                .is_none_or(|(path, _)| path != &photo.path)
            {
                self.view = self
                    .textures
                    .peek(&photo.path)
                    .map(|tex| (photo.path.clone(), tex.handle.clone()));
            }
            self.view_loading = None;
            return;
        }
        if self
            .view
            .as_ref()
            .is_some_and(|(path, _)| path == &photo.path)
        {
            return;
        }
        if self.view_loading.as_ref() == Some(&photo.path) {
            return;
        }
        self.view = self
            .textures
            .peek(&photo.path)
            .map(|tex| (photo.path.clone(), tex.handle.clone()));
        self.view_loading = Some(photo.path.clone());
        self.jobs.cancel_views();
        let target = if library::is_raw_path(&photo.path) {
            window_long_edge.clamp(1600, VIEW_EDGE)
        } else {
            FULL_VIEW_EDGE
        };
        self.jobs.submit(
            Work::View {
                path: photo.path.clone(),
                target,
                generation: self.generation,
            },
            jobs::is_heavy_file(&photo.path, photo.len),
        );
    }

    pub fn request_meta(&self, photo: &Photo) {
        self.jobs.submit(
            Work::Meta {
                photo: photo.clone(),
            },
            jobs::is_heavy_file(&photo.path, photo.len),
        );
    }

    pub fn request_captures(&mut self, photos: &[Photo], around: Option<usize>) {
        const MAX_INFLIGHT: usize = 4;
        const WINDOW: usize = 96;
        if photos.is_empty() || self.pending > 0 || self.capture_inflight.len() >= MAX_INFLIGHT {
            return;
        }
        for photo in around_window(photos, around, WINDOW) {
            if self.capture_inflight.len() >= MAX_INFLIGHT {
                break;
            }
            self.submit_capture(photo);
        }
        self.drain_unknown(photos, MAX_INFLIGHT, true);
    }

    pub fn request_album_reads(&mut self, photos: &[Photo], around: Option<usize>) {
        const MAX_INFLIGHT: usize = 6;
        const WINDOW: usize = 96;
        if photos.is_empty() || self.album_inflight.len() >= MAX_INFLIGHT {
            return;
        }
        for photo in around_window(photos, around, WINDOW) {
            if self.album_inflight.len() >= MAX_INFLIGHT {
                break;
            }
            self.submit_album_read(photo);
        }
        self.drain_unknown(photos, MAX_INFLIGHT, false);
    }

    fn submit_capture(&mut self, photo: &Photo) {
        if photo.capture_known
            || jobs::skip_capture(&photo.path, photo.len)
            || self.capture_inflight.contains(&photo.path)
        {
            return;
        }
        self.capture_inflight.insert(photo.path.clone());
        self.jobs.submit(
            Work::Capture {
                path: photo.path.clone(),
                generation: self.generation,
            },
            jobs::is_heavy_file(&photo.path, photo.len),
        );
    }

    fn submit_album_read(&mut self, photo: &Photo) {
        if photo.albums_known || self.album_inflight.contains(&photo.path) {
            return;
        }
        self.album_inflight.insert(photo.path.clone());
        self.jobs.submit(
            Work::AlbumRead {
                path: photo.path.clone(),
                generation: self.generation,
            },
            false,
        );
    }

    fn drain_unknown(&mut self, photos: &[Photo], max_inflight: usize, captures: bool) {
        if photos.is_empty() {
            return;
        }
        let inflight = if captures {
            self.capture_inflight.len()
        } else {
            self.album_inflight.len()
        };
        if inflight >= max_inflight {
            return;
        }
        let cursor = if captures {
            self.capture_cursor
        } else {
            self.album_cursor
        }
        .min(photos.len() - 1);
        let mut checked = 0usize;
        let mut next = cursor;
        while checked < 256 && checked < photos.len() {
            let full = if captures {
                self.capture_inflight.len() >= max_inflight
            } else {
                self.album_inflight.len() >= max_inflight
            };
            if full {
                break;
            }
            let photo = &photos[next];
            if captures {
                self.submit_capture(photo);
            } else {
                self.submit_album_read(photo);
            }
            next = (next + 1) % photos.len();
            checked += 1;
        }
        if captures {
            self.capture_cursor = next;
        } else {
            self.album_cursor = next;
        }
    }

    pub fn request_album_write(&mut self, path: PathBuf, albums: Vec<String>, covers: Vec<String>) {
        self.album_inflight.insert(path.clone());
        self.jobs.submit(
            Work::AlbumWrite {
                path,
                albums,
                covers,
            },
            false,
        );
    }

    pub fn view_texture(&self, path: &Path) -> Option<TextureHandle> {
        self.view
            .as_ref()
            .and_then(|(stored, tex)| (stored == path).then(|| tex.clone()))
    }
}

fn around_window(photos: &[Photo], around: Option<usize>, window: usize) -> &[Photo] {
    let start = around.unwrap_or(0).min(photos.len().saturating_sub(1));
    let from = start.saturating_sub(window);
    let to = (start + window + 1).min(photos.len());
    &photos[from..to]
}

/// Kürzer als das wird nicht verkleinert: die Miniatur dient im Betrachter
/// als Platzhalter, bis das große Bild da ist, und soll dort nicht matschen.
const MIN_SHORT: u32 = 160;

/// Ganzzahliger Verkleinerungsfaktor, der die Zelle noch abdeckt.
fn shrink_factor(source_short: u32, cell_px: u32) -> usize {
    let by_cell = source_short / cell_px.max(1);
    let by_floor = source_short / MIN_SHORT;
    by_cell.min(by_floor).max(1) as usize
}

/// Ganzzahlige Verkleinerung mit Kastenfilter: jedes Zielpixel ist der
/// Mittelwert eines `factor`×`factor`-Blocks. Schnell genug für den UI-Thread
/// (eine 512er-Miniatur in deutlich unter einer Millisekunde).
fn shrink(image: &ColorImage, factor: usize) -> ColorImage {
    let (w, h) = (image.width(), image.height());
    let (ow, oh) = (w / factor, h / factor);
    if ow == 0 || oh == 0 {
        return image.clone();
    }
    let mut out = Vec::with_capacity(ow * oh);
    let count = (factor * factor) as u32;
    let src = &image.pixels;
    for oy in 0..oh {
        for ox in 0..ow {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for dy in 0..factor {
                let row = (oy * factor + dy) * w + ox * factor;
                for px in &src[row..row + factor] {
                    r += px.r() as u32;
                    g += px.g() as u32;
                    b += px.b() as u32;
                }
            }
            out.push(Color32::from_rgb(
                (r / count) as u8,
                (g / count) as u8,
                (b / count) as u8,
            ));
        }
    }
    ColorImage::new([ow, oh], out)
}

pub fn placeholder_color(time: f64, failed: bool) -> Color32 {
    if failed {
        return Color32::from_rgb(0x4A, 0x2A, 0x2A);
    }
    let pulse = ((time * 2.2).sin() * 4.0) as i32;
    let v = (0x1A + pulse).clamp(0x14, 0x24) as u8;
    Color32::from_rgb(v, v.saturating_sub(2), v.saturating_sub(4))
}
