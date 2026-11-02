use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, PointerButton, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2,
};
use eframe::egui::text::{LayoutJob, TextWrapping};

use crate::albums::{self, AlbumStore};
use crate::library::{self, DirNode, Photo};
use crate::meta::FileInfo;
use crate::theme;
use crate::thumbs::{self, ThumbService};
use crate::video;

const THUMB_MIN: f32 = 72.0;
const THUMB_MAX: f32 = 280.0;
const GAP: f32 = 10.0;
const GRID_PAD: f32 = 24.0;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Prefs {
    thumb_size: f32,
    last_folder: Option<PathBuf>,
    #[serde(default)]
    info_tab: InfoTab,
    #[serde(default)]
    sort: SortBy,
    #[serde(default)]
    sort_desc: bool,
    #[serde(default = "default_true")]
    show_raw: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            thumb_size: 132.0,
            last_folder: None,
            info_tab: InfoTab::Overview,
            sort: SortBy::Name,
            sort_desc: false,
            show_raw: true,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
enum SortBy {
    #[default]
    Name,
    Captured,
    Modified,
    Size,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
enum InfoTab {
    #[default]
    Overview,
    All,
}

enum View {
    Grid,
    Photo { index: usize, zoom: f32, offset: Vec2 },
}

struct FolderTree {
    root: PathBuf,
    root_name: String,
    children: HashMap<PathBuf, Vec<DirNode>>,
    expanded: Vec<PathBuf>,
}

pub struct Lichtkasten {
    prefs: Prefs,
    thumbs: ThumbService,
    all_photos: Vec<Photo>,
    photos: Vec<Photo>,
    selected: Option<usize>,
    view: View,
    tree: Option<FolderTree>,
    current_dir: Option<PathBuf>,
    io_tx: std::sync::mpsc::Sender<ScanMsg>,
    scan_rx: Receiver<ScanMsg>,
    scanning: bool,
    status: String,
    meta: Option<FileInfo>,
    meta_loading: Option<PathBuf>,
    album_store: AlbumStore,
    current_album: Option<String>,
    new_album: String,
    naming_album: bool,
    focus_new_album: bool,
    video: Option<VideoSlot>,
}

enum VideoSlot {
    Loading {
        path: PathBuf,
        rx: std::sync::mpsc::Receiver<Result<video::Player, String>>,
    },
    Live(video::Player),
    Failed {
        path: PathBuf,
        error: String,
    },
}

enum ScanMsg {
    Photos {
        dir: PathBuf,
        photos: Result<Vec<Photo>, String>,
    },
    AlbumPhotos {
        name: String,
        photos: Result<Vec<Photo>, String>,
    },
    Children {
        dir: PathBuf,
        children: Result<Vec<DirNode>, String>,
    },
}

impl Lichtkasten {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let prefs: Prefs = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        let (io_tx, scan_rx) = mpsc::channel();
        let mut app = Self {
            prefs,
            thumbs: ThumbService::new(cc.egui_ctx.clone()),
            all_photos: Vec::new(),
            photos: Vec::new(),
            selected: None,
            view: View::Grid,
            tree: None,
            current_dir: None,
            io_tx,
            scan_rx,
            scanning: false,
            status: String::new(),
            meta: None,
            meta_loading: None,
            album_store: AlbumStore::load(),
            current_album: None,
            new_album: String::new(),
            naming_album: false,
            focus_new_album: false,
            video: None,
        };

        let start = app
            .prefs
            .last_folder
            .clone()
            .or_else(library::pictures_dir);
        if let Some(path) = start {
            app.open_root(path);
        }
        app
    }

    fn open_root(&mut self, path: PathBuf) {
        let root_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.tree = Some(FolderTree {
            root: path.clone(),
            root_name,
            children: HashMap::new(),
            expanded: vec![path.clone()],
        });
        self.prefs.last_folder = Some(path.clone());
        self.load_children(path.clone());
        self.open_dir(path);
    }

    fn open_dir(&mut self, path: PathBuf) {
        self.thumbs.reset_folder();
        self.all_photos.clear();
        self.photos.clear();
        self.selected = None;
        self.meta = None;
        self.meta_loading = None;
        self.view = View::Grid;
        self.video = None;
        self.current_album = None;
        self.current_dir = Some(path.clone());
        self.scanning = true;
        self.status = format!("Lese {} …", path.display());
        self.spawn_scan(path);
    }

    fn spawn_scan(&self, path: PathBuf) {
        let tx = self.io_tx.clone();
        thread::spawn(move || {
            let photos = library::scan_photos(&path).map_err(|e| e.to_string());
            let _ = tx.send(ScanMsg::Photos { dir: path, photos });
        });
    }

    fn load_children(&mut self, path: PathBuf) {
        self.request_children(path);
    }

    fn request_children(&self, path: PathBuf) {
        let tx = self.io_tx.clone();
        thread::spawn(move || {
            let children = library::scan_subdirs(&path).map_err(|e| e.to_string());
            let _ = tx.send(ScanMsg::Children { dir: path, children });
        });
    }

    fn poll_io(&mut self) {
        let mut messages = Vec::new();
        while let Ok(msg) = self.scan_rx.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            match msg {
                ScanMsg::Photos { dir, photos } => {
                    if self.current_album.is_some() || self.current_dir.as_ref() != Some(&dir) {
                        continue;
                    }
                    self.scanning = false;
                    match photos {
                        Ok(photos) => {
                            self.all_photos = photos;
                            self.ingest_album_tags();
                            self.rebuild_photos();
                            self.status = if self.photos.is_empty() {
                                "Keine Medien in diesem Ordner".into()
                            } else {
                                String::new()
                            };
                        }
                        Err(err) => {
                            self.status = format!("Ordner nicht lesbar: {err}");
                            self.all_photos.clear();
                            self.photos.clear();
                        }
                    }
                }
                ScanMsg::AlbumPhotos { name, photos } => {
                    if self.current_album.as_ref() != Some(&name) {
                        continue;
                    }
                    self.scanning = false;
                    match photos {
                        Ok(photos) => {
                            self.all_photos = photos;
                            self.rebuild_photos();
                            self.status = if self.photos.is_empty() {
                                "Keine Medien in diesem Album".into()
                            } else {
                                String::new()
                            };
                        }
                        Err(err) => {
                            self.status = format!("Album nicht lesbar: {err}");
                            self.all_photos.clear();
                            self.photos.clear();
                        }
                    }
                }
                ScanMsg::Children { dir, children } => {
                    if let (Ok(children), Some(tree)) = (children, self.tree.as_mut()) {
                        tree.children.insert(dir, children);
                    }
                }
            }
        }
    }

    fn pick_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Ordner öffnen")
            .pick_folder()
        {
            self.open_root(path);
        }
    }

    fn open_selected_photo(&mut self) {
        if let Some(index) = self.selected
            && index < self.photos.len()
        {
            self.view = View::Photo {
                index,
                zoom: 1.0,
                offset: Vec2::ZERO,
            };
        }
    }

    fn current_photo(&self) -> Option<&Photo> {
        let index = match self.view {
            View::Photo { index, .. } => Some(index),
            View::Grid => self.selected,
        }?;
        self.photos.get(index)
    }

    fn take_meta(&mut self, info: Option<FileInfo>) -> bool {
        let Some(info) = info else {
            return false;
        };
        let mut capture_changed = false;
        if let Some(photo) = self
            .all_photos
            .iter_mut()
            .find(|photo| photo.path == info.path)
        {
            if !photo.capture_known || photo.capture != info.captured {
                photo.capture = info.captured;
                photo.capture_known = true;
                capture_changed = true;
            }
        }
        if let Some(photo) = self.photos.iter_mut().find(|photo| photo.path == info.path) {
            photo.capture = info.captured;
            photo.capture_known = true;
        }
        self.apply_photo_albums(&info.path, &info.albums);
        if self.current_photo().is_some_and(|photo| photo.path == info.path) {
            self.meta_loading = None;
            self.meta = Some(info);
        }
        capture_changed
    }

    fn apply_captures(&mut self, captures: Vec<(PathBuf, Option<u64>)>) -> bool {
        let mut changed = false;
        for (path, stamp) in captures {
            if let Some(photo) = self.all_photos.iter_mut().find(|photo| photo.path == path) {
                if !photo.capture_known || photo.capture != stamp {
                    photo.capture = stamp;
                    photo.capture_known = true;
                    changed = true;
                }
            }
            if let Some(photo) = self.photos.iter_mut().find(|photo| photo.path == path) {
                photo.capture = stamp;
                photo.capture_known = true;
            }
        }
        changed
    }

    fn sort_photos(&mut self) {
        let desc = self.prefs.sort_desc;
        self.photos.sort_by(|a, b| {
            let ord = match self.prefs.sort {
                SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortBy::Captured => a
                    .sort_time()
                    .cmp(&b.sort_time())
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortBy::Modified => a
                    .mtime
                    .cmp(&b.mtime)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortBy::Size => a
                    .len
                    .cmp(&b.len)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            };
            if desc { ord.reverse() } else { ord }
        });
    }

    fn remember_path(&self) -> Option<PathBuf> {
        let index = match self.view {
            View::Photo { index, .. } => Some(index),
            View::Grid => self.selected,
        }?;
        self.photos.get(index).map(|photo| photo.path.clone())
    }

    fn restore_path(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else {
            return;
        };
        let Some(index) = self.photos.iter().position(|photo| photo.path == path) else {
            return;
        };
        self.selected = Some(index);
        if let View::Photo { index: current, .. } = &mut self.view {
            *current = index;
        }
    }

    fn rebuild_photos(&mut self) {
        let keep = self.remember_path();
        self.photos = self
            .all_photos
            .iter()
            .filter(|photo| self.prefs.show_raw || !library::is_raw_path(&photo.path))
            .cloned()
            .collect();
        self.sort_photos();
        if let Some(path) = &keep
            && !self.photos.iter().any(|photo| photo.path == *path)
        {
            self.selected = None;
            if let View::Photo { .. } = self.view {
                self.view = View::Grid;
            }
            self.meta = None;
            self.meta_loading = None;
        } else {
            self.restore_path(keep);
        }
    }

    fn toggle_raw(&mut self) {
        self.prefs.show_raw = !self.prefs.show_raw;
        self.rebuild_photos();
    }

    fn set_sort(&mut self, sort: SortBy) {
        if self.prefs.sort == sort {
            self.prefs.sort_desc = !self.prefs.sort_desc;
        } else {
            self.prefs.sort = sort;
            self.prefs.sort_desc = matches!(sort, SortBy::Captured | SortBy::Modified | SortBy::Size);
        }
        let keep = self.remember_path();
        self.sort_photos();
        self.restore_path(keep);
    }

    fn refresh_meta(&mut self) {
        let Some(photo) = self.current_photo().cloned() else {
            if self.meta.is_some() || self.meta_loading.is_some() {
                self.meta = None;
                self.meta_loading = None;
            }
            return;
        };
        if self.meta.as_ref().is_some_and(|info| info.path == photo.path) {
            return;
        }
        if self.meta_loading.as_ref() == Some(&photo.path) {
            return;
        }
        self.meta = None;
        self.meta_loading = Some(photo.path.clone());
        self.thumbs.request_meta(&photo);
    }

    fn ingest_album_tags(&mut self) {
        let tagged: Vec<(PathBuf, Vec<String>)> = self
            .all_photos
            .iter()
            .filter(|photo| photo.albums_known && !photo.albums.is_empty())
            .map(|photo| (photo.path.clone(), photo.albums.clone()))
            .collect();
        for (path, albums) in tagged {
            self.album_store.remember(&path, &albums);
        }
        self.album_store.save();
    }

    fn apply_albums(&mut self, found: Vec<(PathBuf, Vec<String>)>) {
        if found.is_empty() {
            return;
        }
        for (path, albums) in found {
            self.apply_photo_albums(&path, &albums);
        }
        self.album_store.save();
    }

    fn apply_photo_albums(&mut self, path: &std::path::Path, albums: &[String]) {
        if let Some(photo) = self.all_photos.iter_mut().find(|photo| photo.path == path) {
            photo.albums = albums.to_vec();
            photo.albums_known = true;
        }
        if let Some(photo) = self.photos.iter_mut().find(|photo| photo.path == path) {
            photo.albums = albums.to_vec();
            photo.albums_known = true;
        }
        self.album_store.remember(path, albums);
    }

    fn create_album(&mut self) {
        let name = albums::normalize_name(&self.new_album);
        if name.is_empty() {
            return;
        }
        if self.album_store.ensure(&name) {
            self.album_store.save();
        }
        self.new_album.clear();
        self.naming_album = false;
        self.add_selected_to_album(&name);
    }

    fn add_selected_to_album(&mut self, name: &str) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            if self.album_store.ensure(name) {
                self.album_store.save();
            }
            return;
        }
        for path in &paths {
            self.album_store.add_path(name, path.clone());
            let mut albums = self
                .all_photos
                .iter()
                .find(|photo| photo.path == *path)
                .map(|photo| photo.albums.clone())
                .or_else(|| {
                    self.photos
                        .iter()
                        .find(|photo| photo.path == *path)
                        .map(|photo| photo.albums.clone())
                })
                .unwrap_or_default();
            if !albums.iter().any(|have| have == name) {
                albums.push(name.to_string());
            }
            self.apply_photo_albums(path, &albums);
            self.thumbs.request_album_write(path.clone(), albums);
        }
        self.album_store.save();
        self.status = format!("Zu „{name}“ gelegt");
    }

    fn remove_selected_from_album(&mut self) {
        let Some(name) = self.current_album.clone() else {
            return;
        };
        let paths = self.selected_paths();
        for path in &paths {
            self.album_store.drop_path(&name, path);
            let albums: Vec<String> = self
                .all_photos
                .iter()
                .find(|photo| photo.path == *path)
                .map(|photo| {
                    photo
                        .albums
                        .iter()
                        .filter(|have| *have != &name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            self.apply_photo_albums(path, &albums);
            self.thumbs.request_album_write(path.clone(), albums);
        }
        self.album_store.save();
        if self.current_album.as_deref() == Some(name.as_str()) {
            self.open_album(name);
        }
    }

    fn delete_album(&mut self, name: &str) {
        let paths = self.album_store.remove_album(name);
        for path in paths {
            let albums: Vec<String> = self
                .all_photos
                .iter()
                .find(|photo| photo.path == path)
                .map(|photo| {
                    photo
                        .albums
                        .iter()
                        .filter(|have| *have != name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_else(|| {
                    albums::read(&path)
                        .into_iter()
                        .filter(|have| have != name)
                        .collect()
                });
            self.apply_photo_albums(&path, &albums);
            self.thumbs.request_album_write(path, albums);
        }
        self.album_store.save();
        if self.current_album.as_deref() == Some(name) {
            if let Some(dir) = self.current_dir.clone() {
                self.open_dir(dir);
            } else {
                self.current_album = None;
                self.all_photos.clear();
                self.photos.clear();
            }
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        if let View::Photo { index, .. } = self.view
            && let Some(photo) = self.photos.get(index)
        {
            return vec![photo.path.clone()];
        }
        if let Some(index) = self.selected
            && let Some(photo) = self.photos.get(index)
        {
            return vec![photo.path.clone()];
        }
        Vec::new()
    }

    fn open_album(&mut self, name: String) {
        self.current_album = Some(name.clone());
        self.view = View::Grid;
        self.video = None;
        self.selected = None;
        self.meta = None;
        self.meta_loading = None;
        self.scanning = true;
        self.status = format!("Lese Album {name} …");
        let paths = self.album_store.paths(&name);
        let tx = self.io_tx.clone();
        thread::spawn(move || {
            let photos = paths
                .iter()
                .filter_map(|path| library::photo_from_path(path))
                .collect::<Vec<_>>();
            let _ = tx.send(ScanMsg::AlbumPhotos {
                name,
                photos: Ok(photos),
            });
        });
    }

    fn step_photo(&mut self, delta: isize) {
        let View::Photo { index, zoom, offset } = &mut self.view else {
            return;
        };
        if self.photos.is_empty() {
            return;
        }
        let next = (*index as isize + delta).rem_euclid(self.photos.len() as isize) as usize;
        *index = next;
        *zoom = 1.0;
        *offset = Vec2::ZERO;
        self.selected = Some(next);
        self.video = None;
    }

    fn sync_video(&mut self, ctx: &egui::Context) {
        let want = match &self.view {
            View::Photo { index, .. } => self
                .photos
                .get(*index)
                .filter(|photo| library::is_video_path(&photo.path))
                .map(|photo| photo.path.clone()),
            View::Grid => None,
        };
        let current = self.video.as_ref().map(|slot| match slot {
            VideoSlot::Loading { path, .. }
            | VideoSlot::Live(video::Player { path, .. })
            | VideoSlot::Failed { path, .. } => path.clone(),
        });
        if want != current {
            self.video = want.as_ref().map(|path| VideoSlot::Loading {
                path: path.clone(),
                rx: video::Player::spawn(path.clone()),
            });
        }
        let loaded = if let Some(VideoSlot::Loading { rx, path }) = &mut self.video {
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint();
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Wiedergabe abgebrochen".into()))
                }
            }
            .map(|result| (path.clone(), result))
        } else {
            None
        };
        if let Some((path, result)) = loaded {
            self.video = Some(match result {
                Ok(player) => VideoSlot::Live(player),
                Err(error) => VideoSlot::Failed { path, error },
            });
        }
        if let Some(VideoSlot::Live(player)) = &mut self.video {
            player.tick(ctx);
        }
    }

    fn video_player(&self) -> Option<&video::Player> {
        match &self.video {
            Some(VideoSlot::Live(player)) => Some(player),
            _ => None,
        }
    }

    fn video_player_mut(&mut self) -> Option<&mut video::Player> {
        match &mut self.video {
            Some(VideoSlot::Live(player)) => Some(player),
            _ => None,
        }
    }

    fn video_error(&self) -> Option<&str> {
        match &self.video {
            Some(VideoSlot::Failed { error, .. }) => Some(error),
            Some(VideoSlot::Live(player)) => player.error.as_deref(),
            _ => None,
        }
    }

    fn current_is_video(&self) -> bool {
        self.current_photo()
            .is_some_and(|photo| library::is_video_path(&photo.path))
    }
}

impl eframe::App for Lichtkasten {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.prefs);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_io();
        let out = self.thumbs.poll(ctx);
        let capture_changed = self.apply_captures(out.captures) | self.take_meta(out.meta);
        self.apply_albums(out.albums);
        if capture_changed && self.prefs.sort == SortBy::Captured {
            let keep = self.remember_path();
            self.sort_photos();
            self.restore_path(keep);
        }
        self.refresh_meta();
        self.thumbs.request_captures(&self.photos, self.selected);
        self.thumbs.request_album_reads(&self.photos, self.selected);
        self.sync_video(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        handle_drops(&ctx, self);

        ctx.input(|input| {
            if input.key_pressed(Key::Escape) || input.key_pressed(Key::Backspace) {
                self.view = View::Grid;
                self.video = None;
            }
        });

        match &self.view {
            View::Grid => {
                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                    self.open_selected_photo();
                }
            }
            View::Photo { .. } => {
                let video = self.current_is_video();
                if video && ctx.input(|i| i.key_pressed(Key::Space)) {
                    if let Some(player) = self.video_player_mut() {
                        player.toggle();
                    }
                } else if ctx.input(|i| i.key_pressed(Key::ArrowRight) || (!video && i.key_pressed(Key::Space)))
                {
                    self.step_photo(1);
                }
                if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
                    self.step_photo(-1);
                }
            }
        }

        egui::Panel::top("title")
            .frame(theme::chrome(egui::Margin::symmetric(20, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    ui.label(RichText::new("Lichtkasten").size(15.0).color(theme::INK));
                    if quiet_text(ui, "Ordner").clicked() {
                        self.pick_folder();
                    }
                    if let View::Photo { .. } = self.view
                        && quiet_text(ui, "Raster").clicked()
                    {
                        self.view = View::Grid;
                    }
                    ui.add_space(8.0);
                    sort_chip(ui, self, SortBy::Name, "Name");
                    sort_chip(ui, self, SortBy::Captured, "Aufnahme");
                    sort_chip(ui, self, SortBy::Modified, "Geändert");
                    sort_chip(ui, self, SortBy::Size, "Größe");
                    raw_chip(ui, self);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let trail = if let Some(album) = &self.current_album {
                            album.clone()
                        } else if let Some(dir) = &self.current_dir {
                            dir.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| dir.display().to_string())
                        } else {
                            String::new()
                        };
                        if !trail.is_empty() {
                            ui.label(RichText::new(trail).size(13.0).color(theme::MUTED));
                        }
                    });
                });
                theme::hairline_bottom(ui);
            });

        egui::Panel::bottom("bar")
            .frame(theme::chrome(egui::Margin::symmetric(20, 8)))
            .show(ui, |ui| {
                theme::hairline_top(ui);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(media_count_label(&self.photos)).color(theme::MUTED));
                    if self.thumbs.pending > 0 {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("{} Vorschaubilder", self.thumbs.ready))
                                .color(theme::MUTED),
                        );
                    }
                    if !self.status.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(&self.status).color(theme::MUTED));
                    }
                    if self.selected.is_some() || matches!(self.view, View::Photo { .. }) {
                        ui.separator();
                        ui.menu_button("Zu Album", |ui| {
                            if self.album_store.albums.is_empty() {
                                ui.label(RichText::new("Noch kein Album").color(theme::MUTED));
                            }
                            let names: Vec<String> = self
                                .album_store
                                .albums
                                .iter()
                                .map(|album| album.name.clone())
                                .collect();
                            for name in names {
                                if ui.button(&name).clicked() {
                                    self.add_selected_to_album(&name);
                                    ui.close();
                                }
                            }
                        });
                        if self.current_album.is_some() && ui.button("Aus Album").clicked() {
                            self.remove_selected_from_album();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_sized(
                            [160.0, 18.0],
                            egui::Slider::new(&mut self.prefs.thumb_size, THUMB_MIN..=THUMB_MAX)
                                .show_value(false)
                                .trailing_fill(true),
                        );
                        ui.label(RichText::new("Größe").color(theme::MUTED));
                    });
                });
            });

        egui::Panel::left("sources")
            .resizable(true)
            .default_size(220.0)
            .size_range(168.0..=320.0)
            .frame(theme::side())
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                theme::section(ui, "Mediathek");
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("mediathek-scroll")
                    .auto_shrink([false, true])
                    .max_height((ui.available_height() - 148.0).max(80.0))
                    .hscroll(false)
                    .show(ui, |ui| {
                        draw_tree(ui, self);
                    });
                ui.add_space(16.0);
                draw_albums(ui, self);
            });

        egui::Panel::right("info")
            .resizable(true)
            .default_size(260.0)
            .size_range(200.0..=400.0)
            .frame(theme::side())
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                draw_info(ui, self);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::VOID))
            .show(ui, |ui| match self.view {
                View::Grid => draw_grid(ui, self),
                View::Photo { index, zoom, offset } => {
                    draw_viewer(ui, self, index, zoom, offset);
                }
            });

        if self.thumbs.pending > 0
            || self.scanning
            || self.thumbs.view_is_loading()
            || self.meta_loading.is_some()
            || self.thumbs.more_results
        {
            ctx.request_repaint();
        }
    }
}

fn handle_drops(ctx: &egui::Context, app: &mut Lichtkasten) {
    let dropped: Vec<PathBuf> = ctx.input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .filter_map(|file| Some(file.path().to_path_buf()))
            .collect()
    });
    if let Some(path) = dropped.into_iter().next() {
        if path.is_dir() {
            app.open_root(path);
        } else if let Some(parent) = path.parent() {
            app.open_root(parent.to_path_buf());
        }
    }
}

fn draw_info(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    theme::section(ui, "Datei");
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        tab_chip(ui, app, InfoTab::Overview, "Übersicht");
        tab_chip(ui, app, InfoTab::All, "Metadaten");
    });
    ui.add_space(16.0);

    if app.current_photo().is_none() {
        ui.add_space(8.0);
        ui.label(RichText::new("Kein Foto gewählt").size(13.0).color(theme::MUTED));
        return;
    }
    if app.meta_loading.is_some() && app.meta.is_none() {
        ui.label(RichText::new("Metadaten werden gelesen…").color(theme::MUTED));
        return;
    }
    let Some(info) = app.meta.as_ref() else {
        ui.label(RichText::new("Keine Dateiinformationen").color(theme::MUTED));
        return;
    };

    egui::ScrollArea::vertical()
        .id_salt("file-info")
        .auto_shrink([false, false])
        .show(ui, |ui| match app.prefs.info_tab {
            InfoTab::Overview => {
                for (key, value) in &info.overview {
                    info_row(ui, key, value);
                }
            }
            InfoTab::All => {
                let mut any = false;
                any |= info_section(ui, "EXIF", &info.exif);
                any |= info_section(ui, "IPTC", &info.iptc);
                any |= info_section(ui, "XMP", &info.xmp);
                if !any {
                    ui.label(
                        RichText::new("Dieses Bild hat kein EXIF, IPTC oder XMP.")
                            .color(theme::MUTED),
                    );
                }
            }
        });
}

fn quiet_text(ui: &mut egui::Ui, text: &str) -> egui::Response {
    quiet_toggle(ui, false, text)
}

fn quiet_toggle(ui: &mut egui::Ui, selected: bool, text: impl Into<String>) -> egui::Response {
    let color = if selected { theme::INK } else { theme::MUTED };
    let response = ui.add(
        egui::Label::new(RichText::new(text.into()).size(13.0).color(color)).sense(Sense::click()),
    );
    if selected {
        let rect = response.rect;
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, theme::INK),
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Default);
    }
    response
}

fn sort_chip(ui: &mut egui::Ui, app: &mut Lichtkasten, sort: SortBy, label: &str) {
    let selected = app.prefs.sort == sort;
    let text = if selected {
        if app.prefs.sort_desc {
            format!("{label} ↓")
        } else {
            format!("{label} ↑")
        }
    } else {
        label.to_string()
    };
    if quiet_toggle(ui, selected, text).clicked() {
        app.set_sort(sort);
    }
}

fn raw_chip(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    let selected = app.prefs.show_raw;
    let response = quiet_toggle(ui, selected, "RAW");
    if response.hovered() {
        response.clone().on_hover_text(if selected {
            "RAW ausblenden"
        } else {
            "RAW einblenden"
        });
    }
    if response.clicked() {
        app.toggle_raw();
    }
}

fn tab_chip(ui: &mut egui::Ui, app: &mut Lichtkasten, tab: InfoTab, label: &str) {
    if quiet_toggle(ui, app.prefs.info_tab == tab, label).clicked() {
        app.prefs.info_tab = tab;
    }
}

fn info_section(ui: &mut egui::Ui, title: &str, rows: &[(String, String)]) -> bool {
    if rows.is_empty() {
        return false;
    }
    ui.add_space(6.0);
    ui.label(RichText::new(title).small().strong().color(theme::MUTED));
    ui.add_space(4.0);
    for (key, value) in rows {
        info_row(ui, key, value);
    }
    true
}

fn info_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.add_space(3.0);
    ui.label(RichText::new(key).small().color(theme::MUTED));
    ui.label(RichText::new(value).color(theme::INK));
    ui.add_space(4.0);
}

enum RowAction {
    None,
    Toggle,
    Open,
}

fn draw_albums(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    ui.horizontal(|ui| {
        theme::section(ui, "Alben");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if quiet_text(ui, "Neu").clicked() {
                app.naming_album = true;
                app.new_album.clear();
                app.focus_new_album = true;
            }
        });
    });
    ui.add_space(6.0);
    if app.naming_album {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.new_album)
                    .id(egui::Id::new("new-album-name"))
                    .desired_width(120.0)
                    .hint_text("Name"),
            );
            if app.focus_new_album {
                response.request_focus();
                app.focus_new_album = false;
            }
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if enter || quiet_text(ui, "Anlegen").clicked() {
                app.create_album();
            }
            if quiet_text(ui, "Abbrechen").clicked() {
                app.naming_album = false;
                app.new_album.clear();
            }
        });
    }
    let albums: Vec<(String, usize)> = app
        .album_store
        .albums
        .iter()
        .map(|album| (album.name.clone(), album.paths.len()))
        .collect();
    if albums.is_empty() && !app.naming_album {
        ui.label(RichText::new("Noch keine Alben").small().color(theme::MUTED));
    }
    for (name, count) in albums {
        let selected = app.current_album.as_ref() == Some(&name);
        let row_h = 26.0;
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), row_h),
            Sense::click(),
        );
        if selected || response.hovered() {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(6),
                if selected {
                    theme::SURFACE
                } else {
                    Color32::from_rgba_premultiplied(18, 18, 18, 18)
                },
            );
        }
        let name_color = if selected { Color32::WHITE } else { theme::INK };
        ui.painter().text(
            rect.left_center() + Vec2::new(8.0, 0.0),
            Align2::LEFT_CENTER,
            &name,
            FontId::proportional(14.0),
            name_color,
        );
        ui.painter().text(
            rect.right_center() - Vec2::new(8.0, 0.0),
            Align2::RIGHT_CENTER,
            format!("{count}"),
            FontId::proportional(12.0),
            theme::MUTED,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Default);
        }
        if response.clicked() {
            app.open_album(name.clone());
        }
        response.context_menu(|ui| {
            if ui.button("Album löschen").clicked() {
                app.delete_album(&name);
                ui.close();
            }
        });
    }
}

fn draw_tree(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    let Some(tree) = app.tree.as_ref() else {
        ui.add_space(12.0);
        ui.label("Öffne einen Ordner oder ziehe ihn hierher.");
        return;
    };
    let root = tree.root.clone();
    let root_name = tree.root_name.clone();

    egui::ScrollArea::vertical()
        .id_salt("folder-tree")
        .auto_shrink([false, false])
        .hscroll(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            draw_dir_branch(ui, app, &root, &root_name, 0, true);
        });
}

fn draw_dir_branch(
    ui: &mut egui::Ui,
    app: &mut Lichtkasten,
    path: &PathBuf,
    name: &str,
    depth: u8,
    has_subdirs: bool,
) {
    let expanded = app
        .tree
        .as_ref()
        .is_some_and(|tree| tree.expanded.iter().any(|p| p == path));

    match draw_dir_row(ui, app, path, name, depth, has_subdirs, expanded) {
        RowAction::Toggle => toggle_dir(app, path.clone()),
        RowAction::Open => {
            if app.current_album.is_some() {
                app.open_dir(path.clone());
            } else if app.current_dir.as_ref() == Some(path) {
                app.view = View::Grid;
            } else {
                app.open_dir(path.clone());
            }
        }
        RowAction::None => {}
    }

    if expanded {
        let children = app
            .tree
            .as_ref()
            .and_then(|tree| tree.children.get(path))
            .cloned()
            .unwrap_or_default();
        for child in children {
            draw_dir_branch(ui, app, &child.path, &child.name, depth + 1, child.has_subdirs);
        }
    }
}

fn toggle_dir(app: &mut Lichtkasten, path: PathBuf) {
    let expanding = {
        let Some(tree) = app.tree.as_mut() else {
            return;
        };
        if tree.expanded.iter().any(|p| p == &path) {
            tree.expanded.retain(|p| p != &path);
            false
        } else {
            tree.expanded.push(path.clone());
            true
        }
    };
    if expanding {
        app.request_children(path);
    }
}

fn draw_dir_row(
    ui: &mut egui::Ui,
    app: &Lichtkasten,
    path: &PathBuf,
    name: &str,
    depth: u8,
    has_subdirs: bool,
    expanded: bool,
) -> RowAction {
    let selected = app.current_album.is_none() && app.current_dir.as_ref() == Some(path);
    let row_h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());

    let fill = if selected {
        theme::SURFACE
    } else if resp.hovered() {
        Color32::from_rgba_premultiplied(18, 18, 18, 18)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), fill);
    }

    let text = if selected {
        Color32::WHITE
    } else {
        theme::INK
    };
    let indent = 4.0 + depth as f32 * 14.0;
    let chevron_rect = Rect::from_min_size(rect.min + Vec2::new(indent, 0.0), Vec2::new(18.0, row_h));
    let mark = if !has_subdirs {
        ""
    } else if expanded {
        "▾"
    } else {
        "▸"
    };
    if !mark.is_empty() {
        ui.painter().text(
            chevron_rect.center(),
            Align2::CENTER_CENTER,
            mark,
            FontId::proportional(13.0),
            text,
        );
    }
    let text_left = chevron_rect.right() + 2.0;
    let text_max_w = (rect.right() - 8.0 - text_left).max(12.0);
    let mut job = LayoutJob::simple(name.to_owned(), FontId::proportional(14.0), text, text_max_w);
    job.wrap = TextWrapping {
        max_width: text_max_w,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let truncated = galley.elided;
    let text_pos = Pos2::new(text_left, rect.center().y - galley.size().y * 0.5);
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(text_pos, galley, text);

    let resp = if truncated {
        resp.on_hover_text(name)
    } else {
        resp
    };
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Default);
    }

    if resp.clicked() {
        let on_chevron = resp.interact_pointer_pos().is_some_and(|pos| {
            has_subdirs && chevron_rect.expand2(Vec2::new(4.0, 2.0)).contains(pos)
        });
        if on_chevron {
            RowAction::Toggle
        } else {
            RowAction::Open
        }
    } else {
        RowAction::None
    }
}

fn draw_grid(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    if app.photos.is_empty() {
        ui.centered_and_justified(|ui| {
            let text = if app.scanning {
                "Ordner wird gelesen…"
            } else if app.tree.is_none() {
                "Ordner öffnen oder hierher ziehen"
            } else {
                "Keine Medien in diesem Ordner"
            };
            ui.label(RichText::new(text).size(15.0).color(theme::MUTED));
        });
        return;
    }

    let thumb = app.prefs.thumb_size;
    ui.add_space(GRID_PAD);
    let inner_width = (ui.available_width() - GRID_PAD).max(thumb);
    let cols = ((inner_width + GAP) / (thumb + GAP)).floor().max(1.0) as usize;
    let rows = app.photos.len().div_ceil(cols);
    let row_h = thumb;

    ui.spacing_mut().item_spacing = Vec2::new(GAP, GAP);

    let mut visible: Vec<Photo> = Vec::new();
    let time = ui.input(|i| i.time);
    let mut activate: Option<usize> = None;
    let mut select: Option<usize> = None;
    let mut arrow: Option<isize> = None;

    if ui.input(|i| i.key_pressed(Key::ArrowRight)) {
        arrow = Some(1);
    } else if ui.input(|i| i.key_pressed(Key::ArrowLeft)) {
        arrow = Some(-1);
    } else if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
        arrow = Some(cols as isize);
    } else if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
        arrow = Some(-(cols as isize));
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("photo-grid")
        .show_rows(ui, row_h, rows, |ui, row_range| {
            for row in row_range {
                ui.horizontal(|ui| {
                    ui.add_space(GRID_PAD);
                    for col in 0..cols {
                        let index = row * cols + col;
                        if index >= app.photos.len() {
                            break;
                        }
                        let photo = app.photos[index].clone();
                        visible.push(photo.clone());
                        let selected = app.selected == Some(index);
                        let failed = app.thumbs.failed(&photo.path);
                        let texture = app.thumbs.texture(&photo.path);

                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(thumb), Sense::click());
                        let painter = ui.painter();
                        painter.rect_filled(rect, CornerRadius::same(2), thumbs::placeholder_color(time, failed));
                        if let Some(texture) = texture {
                            painter.image(
                                texture.id(),
                                rect,
                                cover_uv(texture.size_vec2()),
                                Color32::WHITE,
                            );
                        }
                        if selected {
                            painter.rect_stroke(
                                rect,
                                CornerRadius::same(2),
                                Stroke::new(1.0, theme::INK),
                                StrokeKind::Outside,
                            );
                        }
                        if library::is_video_path(&photo.path) {
                            painter.text(
                                rect.left_bottom() + Vec2::new(8.0, -7.0),
                                Align2::LEFT_BOTTOM,
                                "▶",
                                FontId::proportional(12.0),
                                Color32::WHITE,
                            );
                        }
                        let resp = resp.on_hover_text(&photo.name);
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            select = Some(index);
                        }
                        if resp.double_clicked() {
                            activate = Some(index);
                        }
                    }
                });
            }
        });

    if !visible.is_empty() {
        // Prefetch one extra row around the visible window.
        if let Some(first) = visible.first()
            && let Some(start) = app.photos.iter().position(|p| p.path == first.path)
        {
            let from = start.saturating_sub(cols);
            let to = (start + visible.len() + cols).min(app.photos.len());
            app.thumbs.request_visible(&app.photos[from..to]);
        } else {
            app.thumbs.request_visible(&visible);
        }
    }

    if let Some(index) = select {
        app.selected = Some(index);
    }
    if let Some(index) = activate {
        app.selected = Some(index);
        app.open_selected_photo();
    }
    if let Some(delta) = arrow {
        let current = app.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, app.photos.len() as isize - 1) as usize;
        app.selected = Some(next);
    }
}

fn draw_viewer(
    ui: &mut egui::Ui,
    app: &mut Lichtkasten,
    index: usize,
    zoom: f32,
    offset: Vec2,
) {
    if index >= app.photos.len() {
        app.view = View::Grid;
        return;
    }
    let photo = app.photos[index].clone();
    let is_video = library::is_video_path(&photo.path);
    let ppp = ui.ctx().pixels_per_point();
    let available = ui.available_size();
    let target = (available.x.max(available.y) * ppp) as u32;
    if !is_video {
        app.thumbs.request_view(&photo, target);
    }

    ui.vertical(|ui| {
        let strip_h = 78.0;
        let name_row = 28.0;
        let stage_w = ui.available_width();
        let stage_h = (ui.available_height() - strip_h - name_row - 16.0).max(80.0);

        let video_tex = if is_video {
            app.video_player()
                .and_then(|player| player.texture().cloned())
        } else {
            None
        };
        let texture = video_tex.or_else(|| {
            app.thumbs
                .view_texture(&photo.path)
                .or_else(|| app.thumbs.texture(&photo.path))
        });

        let mut new_zoom = zoom;
        let mut new_offset = offset;
        let mut zoom_100 = 1.0;
        let mut percent = 0.0;
        if let Some(texture) = &texture {
            let size = texture.size_vec2();
            let fit = (stage_w / size.x).min(stage_h / size.y).max(0.0001);
            zoom_100 = 1.0 / (fit * ppp);
            percent = fit * zoom * ppp * 100.0;
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new(&photo.name).size(16.0).strong());
            ui.label(
                RichText::new(format!("{} / {}", index + 1, app.photos.len())).color(theme::MUTED),
            );
            if is_video {
                draw_video_controls(ui, app);
            } else if texture.is_some() {
                ui.separator();
                ui.label(RichText::new(format!("{percent:.0} %")).color(theme::MUTED));
                if ui.button("Einpassen").clicked() {
                    new_zoom = 1.0;
                    new_offset = Vec2::ZERO;
                }
                if ui.button("100 %").clicked() {
                    new_zoom = zoom_100;
                    new_offset = Vec2::ZERO;
                }
            }
        });

        let (stage, resp) = ui.allocate_exact_size(Vec2::new(stage_w, stage_h), Sense::click_and_drag());

        if let Some(texture) = texture {
            let size = texture.size_vec2();
            let fit = (stage.width() / size.x).min(stage.height() / size.y).max(0.0001);
            zoom_100 = 1.0 / (fit * ppp);
            let shown = size * fit * new_zoom;
            let center = stage.center() + new_offset;
            let rect = Rect::from_center_size(center, shown);
            ui.painter().with_clip_rect(stage).image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

            let min_zoom = zoom_100.min(1.0);
            let max_zoom = (zoom_100 * 4.0).max(8.0).min(32.0);
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let before = new_zoom;
                    new_zoom = (new_zoom * (1.0 + scroll * 0.0015)).clamp(min_zoom, max_zoom);
                    if (new_zoom - 1.0).abs() < 0.001 {
                        new_zoom = 1.0;
                        new_offset = Vec2::ZERO;
                    } else if let Some(pointer) = resp.hover_pos() {
                        let factor = new_zoom / before;
                        new_offset += (pointer - stage.center() - new_offset) * (1.0 - factor);
                    }
                }
            }
            if new_zoom > 1.001 && resp.dragged_by(PointerButton::Primary) {
                new_offset += resp.drag_delta();
            }
            if resp.double_clicked() {
                if (percent - 100.0).abs() < 8.0 {
                    new_zoom = 1.0;
                    new_offset = Vec2::ZERO;
                } else {
                    new_zoom = zoom_100;
                    new_offset = Vec2::ZERO;
                }
            }
        } else {
            let waiting = if is_video {
                app.video_error()
                    .unwrap_or("Video wird geladen…")
                    .to_string()
            } else {
                "Bild wird geladen…".into()
            };
            ui.painter().text(
                stage.center(),
                egui::Align2::CENTER_CENTER,
                waiting,
                egui::FontId::proportional(18.0),
                theme::MUTED,
            );
        }

        if is_video && resp.clicked() {
            if let Some(player) = app.video_player_mut() {
                player.toggle();
            }
        }

        theme::chrome(egui::Margin::symmetric(0, 6)).show(ui, |ui| {
            theme::hairline_top(ui);
            draw_filmstrip(ui, app, index);
        });

        if let View::Photo {
            index: i,
            zoom: z,
            offset: o,
        } = &mut app.view
            && *i == index
        {
            *z = new_zoom;
            *o = new_offset;
        }
    });
}

fn draw_filmstrip(ui: &mut egui::Ui, app: &mut Lichtkasten, current: usize) {
    let cell = 64.0;
    let window = 48usize;
    let start = current.saturating_sub(window / 2);
    let end = (start + window).min(app.photos.len());
    let start = end.saturating_sub(window.min(app.photos.len()));
    ui.add_space(8.0);
    egui::ScrollArea::horizontal()
        .id_salt("filmstrip")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let mut jump = None;
                for index in start..end {
                    let photo = app.photos[index].clone();
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(cell), Sense::click());
                    let failed = app.thumbs.failed(&photo.path);
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(3),
                        thumbs::placeholder_color(0.0, failed),
                    );
                    if let Some(texture) = app.thumbs.texture(&photo.path) {
                        ui.painter().image(
                            texture.id(),
                            rect,
                            cover_uv(texture.size_vec2()),
                            Color32::WHITE,
                        );
                    }
                    if index == current {
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::same(3),
                            Stroke::new(1.5, theme::HAIRLINE_STRONG),
                            StrokeKind::Outside,
                        );
                    }
                    if resp.clicked() {
                        jump = Some(index);
                    }
                }
                if let Some(index) = jump {
                    app.selected = Some(index);
                    app.view = View::Photo {
                        index,
                        zoom: 1.0,
                        offset: Vec2::ZERO,
                    };
                }
            });
        });
    if start < end {
        app.thumbs.request_visible(&app.photos[start..end]);
    }
}

fn media_count_label(photos: &[Photo]) -> String {
    let videos = photos
        .iter()
        .filter(|photo| library::is_video_path(&photo.path))
        .count();
    let stills = photos.len().saturating_sub(videos);
    match (stills, videos) {
        (0, 0) => "Keine Medien".into(),
        (1, 0) => "1 Foto".into(),
        (n, 0) => format!("{n} Fotos"),
        (0, 1) => "1 Video".into(),
        (0, n) => format!("{n} Videos"),
        (1, 1) => "1 Foto · 1 Video".into(),
        (s, 1) => format!("{s} Fotos · 1 Video"),
        (1, v) => format!("1 Foto · {v} Videos"),
        (s, v) => format!("{s} Fotos · {v} Videos"),
    }
}

fn draw_video_controls(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    let Some(player) = app.video_player_mut() else {
        ui.label(RichText::new("Video").color(theme::MUTED));
        return;
    };
    ui.separator();
    let label = if player.playing { "Pause" } else { "Abspielen" };
    if quiet_text(ui, label).clicked() {
        player.toggle();
    }
    let duration = player.duration.max(0.0);
    let mut position = player.position.clamp(0.0, duration.max(0.01));
    ui.label(
        RichText::new(format!(
            "{} / {}",
            video::format_time(position),
            video::format_time(duration)
        ))
        .color(theme::MUTED),
    );
    if duration > 0.0 {
        let slider = ui.add_sized(
            [180.0, 18.0],
            egui::Slider::new(&mut position, 0.0..=duration)
                .show_value(false)
                .trailing_fill(true),
        );
        if slider.changed() {
            player.seek(position);
        }
    }
    let mute = if player.muted { "Ton an" } else { "Stumm" };
    if quiet_text(ui, mute).clicked() {
        let next = !player.muted;
        player.set_mute(next);
    }
}

/// Center-crop a texture into a square cell (iPhoto grid), without changing the file.
fn cover_uv(size: Vec2) -> Rect {
    let w = size.x.max(1.0);
    let h = size.y.max(1.0);
    if w > h {
        let x = (1.0 - h / w) * 0.5;
        Rect::from_min_max(Pos2::new(x, 0.0), Pos2::new(1.0 - x, 1.0))
    } else if h > w {
        let y = (1.0 - w / h) * 0.5;
        Rect::from_min_max(Pos2::new(0.0, y), Pos2::new(1.0, 1.0 - y))
    } else {
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
    }
}

