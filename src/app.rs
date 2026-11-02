use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, PointerButton, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2,
};
use eframe::egui::text::{LayoutJob, TextWrapping};

use crate::albums::{self, AlbumStore};
use crate::i18n;
use crate::library::{self, DirNode, Photo};
use crate::open_with;
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
    #[serde(default)]
    last_folder: Option<PathBuf>,
    #[serde(skip)]
    info_tab: InfoTab,
    #[serde(default)]
    sort: SortBy,
    #[serde(default)]
    sort_desc: bool,
    #[serde(default = "default_show_raw")]
    show_raw: bool,
    #[serde(skip)]
    source: SourcePane,
}

fn default_show_raw() -> bool {
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
            source: SourcePane::Folders,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
enum SourcePane {
    #[default]
    Folders,
    Albums,
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

/// Pfad, Alben, Deckblatt-Tags, mtime, Länge — aus dem Scan gesammelt.
type TaggedPhoto = (PathBuf, Vec<String>, Vec<String>, u64, u64);

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
    /// Pfad → Index in `all_photos`; spart lineare Suchen bei großen Ordnern.
    all_index: HashMap<PathBuf, usize>,
    /// Pfad → Index in `photos` (gefiltert, sortiert).
    vis_index: HashMap<PathBuf, usize>,
    selected: HashSet<usize>,
    focus: Option<usize>,
    anchor: Option<usize>,
    dragging: bool,
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
    need_folder: bool,
    picking_folder: bool,
    ctx: egui::Context,
    open_with: open_with::SharedCatalog,
    open_with_icons: HashMap<String, egui::TextureHandle>,
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
    RestoreFolder(PathBuf),
    FolderMissing,
    PickedFolder(Option<PathBuf>),
}

impl Lichtkasten {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let mut prefs = Prefs::default();
        if let Some(saved) = cc
            .storage
            .and_then(|storage| eframe::get_value::<Prefs>(storage, eframe::APP_KEY))
        {
            prefs.thumb_size = saved.thumb_size.clamp(THUMB_MIN, THUMB_MAX);
            prefs.last_folder = saved.last_folder;
            prefs.show_raw = saved.show_raw;
            prefs.sort = saved.sort;
            prefs.sort_desc = saved.sort_desc;
        }

        let (io_tx, scan_rx) = mpsc::channel();
        let mut app = Self {
            prefs,
            thumbs: ThumbService::new(cc.egui_ctx.clone()),
            all_photos: Vec::new(),
            photos: Vec::new(),
            all_index: HashMap::new(),
            vis_index: HashMap::new(),
            selected: HashSet::new(),
            focus: None,
            anchor: None,
            dragging: false,
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
            need_folder: false,
            picking_folder: false,
            ctx: cc.egui_ctx.clone(),
            open_with: open_with::spawn_load(),
            open_with_icons: HashMap::new(),
            video: None,
        };

        if let Some(path) = app.prefs.last_folder.clone() {
            app.spawn_restore(path);
        } else {
            app.need_folder = true;
        }
        app
    }

    fn spawn_restore(&self, path: PathBuf) {
        let tx = self.io_tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let msg = if path.is_dir() {
                ScanMsg::RestoreFolder(path)
            } else {
                ScanMsg::FolderMissing
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
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
        self.load_children(path.clone());
        self.open_dir(path);
    }

    fn set_all_photos(&mut self, photos: Vec<Photo>) {
        self.all_photos = photos;
        self.all_index = self
            .all_photos
            .iter()
            .enumerate()
            .map(|(index, photo)| (photo.path.clone(), index))
            .collect();
    }

    fn reindex_visible(&mut self) {
        self.vis_index = self
            .photos
            .iter()
            .enumerate()
            .map(|(index, photo)| (photo.path.clone(), index))
            .collect();
    }

    fn clear_photos(&mut self) {
        self.all_photos.clear();
        self.all_index.clear();
        self.photos.clear();
        self.vis_index.clear();
    }

    fn all_photo(&self, path: &std::path::Path) -> Option<&Photo> {
        self.all_photos.get(*self.all_index.get(path)?)
    }

    fn all_photo_mut(&mut self, path: &std::path::Path) -> Option<&mut Photo> {
        let index = *self.all_index.get(path)?;
        self.all_photos.get_mut(index)
    }

    fn visible_photo_mut(&mut self, path: &std::path::Path) -> Option<&mut Photo> {
        let index = *self.vis_index.get(path)?;
        self.photos.get_mut(index)
    }

    fn visible_index(&self, path: &std::path::Path) -> Option<usize> {
        self.vis_index.get(path).copied()
    }

    fn open_dir(&mut self, path: PathBuf) {
        self.thumbs.reset_folder();
        self.clear_photos();
        self.clear_selection();
        self.meta = None;
        self.meta_loading = None;
        self.view = View::Grid;
        self.video = None;
        self.prefs.source = SourcePane::Folders;
        self.current_album = None;
        self.current_dir = Some(path.clone());
        self.prefs.last_folder = Some(path.clone());
        self.scanning = true;
        self.status = i18n::reading_path(path.display());
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
                            self.set_all_photos(photos);
                            self.ingest_album_tags();
                            self.rebuild_photos();
                            self.status = if self.photos.is_empty() {
                                i18n::t("Keine Medien in diesem Ordner", "No media in this folder")
                                    .into()
                            } else {
                                String::new()
                            };
                        }
                        Err(err) => {
                            self.status = i18n::folder_unreadable(err);
                            self.clear_photos();
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
                            self.set_all_photos(photos);
                            self.rebuild_photos();
                            self.status = if self.photos.is_empty() {
                                i18n::t("Keine Medien in diesem Album", "No media in this album")
                                    .into()
                            } else {
                                String::new()
                            };
                        }
                        Err(err) => {
                            self.status = i18n::album_unreadable(err);
                            self.clear_photos();
                        }
                    }
                }
                ScanMsg::Children { dir, children } => {
                    if let (Ok(children), Some(tree)) = (children, self.tree.as_mut()) {
                        tree.children.insert(dir, children);
                    }
                }
                ScanMsg::RestoreFolder(path) => {
                    self.open_root(path);
                }
                ScanMsg::FolderMissing => {
                    self.need_folder = true;
                }
                ScanMsg::PickedFolder(path) => {
                    self.picking_folder = false;
                    if let Some(path) = path {
                        self.open_root(path);
                    } else {
                        self.status.clear();
                    }
                }
            }
        }
    }

    fn pick_folder(&mut self) {
        if self.picking_folder {
            return;
        }
        self.picking_folder = true;
        self.status = i18n::t("Ordner wählen …", "Choosing folder…").into();
        let tx = self.io_tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .set_title(i18n::t("Ordner öffnen", "Open Folder"))
                .pick_folder();
            let _ = tx.send(ScanMsg::PickedFolder(path));
            ctx.request_repaint();
        });
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.focus = None;
        self.anchor = None;
        self.dragging = false;
    }

    fn select_only(&mut self, index: usize) {
        self.selected.clear();
        if index < self.photos.len() {
            self.selected.insert(index);
            self.focus = Some(index);
            self.anchor = Some(index);
        }
    }

    fn toggle_select(&mut self, index: usize) {
        if !self.selected.remove(&index) {
            self.selected.insert(index);
        }
        self.focus = Some(index);
        self.anchor = Some(index);
    }

    fn select_range(&mut self, index: usize, add: bool) {
        let from = self.anchor.unwrap_or(index);
        if !add {
            self.selected.clear();
        }
        let (start, end) = if from <= index { (from, index) } else { (index, from) };
        for item in start..=end.min(self.photos.len().saturating_sub(1)) {
            self.selected.insert(item);
        }
        self.focus = Some(index);
    }

    fn select_all(&mut self) {
        self.selected = (0..self.photos.len()).collect();
        if self.focus.is_none() {
            self.focus = self.selected.iter().copied().next();
        }
        if self.anchor.is_none() {
            self.anchor = self.focus;
        }
    }

    fn open_selected_photo(&mut self) {
        if let Some(index) = self.focus
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
            View::Grid => self.focus,
        }?;
        self.photos.get(index)
    }

    fn take_meta(&mut self, info: Option<FileInfo>) -> bool {
        let Some(info) = info else {
            return false;
        };
        let mut capture_changed = false;
        if let Some(photo) = self.all_photo_mut(&info.path)
            && (!photo.capture_known || photo.capture != info.captured)
        {
            photo.capture = info.captured;
            photo.capture_known = true;
            capture_changed = true;
        }
        if let Some(photo) = self.visible_photo_mut(&info.path) {
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
            if let Some(photo) = self.all_photo_mut(&path)
                && (!photo.capture_known || photo.capture != stamp)
            {
                photo.capture = stamp;
                photo.capture_known = true;
                changed = true;
            }
            if let Some(photo) = self.visible_photo_mut(&path) {
                photo.capture = stamp;
                photo.capture_known = true;
            }
        }
        changed
    }

    fn sort_photos(&mut self) {
        let desc = self.prefs.sort_desc;
        match self.prefs.sort {
            SortBy::Name => {
                self.photos.sort_by_cached_key(|photo| photo.name.to_lowercase());
                if desc {
                    self.photos.reverse();
                }
            }
            SortBy::Captured => self.photos.sort_by(|a, b| {
                let ord = a.sort_time().cmp(&b.sort_time()).then_with(|| a.name.cmp(&b.name));
                if desc { ord.reverse() } else { ord }
            }),
            SortBy::Modified => self.photos.sort_by(|a, b| {
                let ord = a.mtime.cmp(&b.mtime).then_with(|| a.name.cmp(&b.name));
                if desc { ord.reverse() } else { ord }
            }),
            SortBy::Size => self.photos.sort_by(|a, b| {
                let ord = a.len.cmp(&b.len).then_with(|| a.name.cmp(&b.name));
                if desc { ord.reverse() } else { ord }
            }),
        }
        self.reindex_visible();
    }

    fn remember_path(&self) -> Option<PathBuf> {
        let index = match self.view {
            View::Photo { index, .. } => Some(index),
            View::Grid => self.focus,
        }?;
        self.photos.get(index).map(|photo| photo.path.clone())
    }

    fn remember_selection(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .selected
            .iter()
            .filter_map(|index| self.photos.get(*index).map(|photo| photo.path.clone()))
            .collect();
        paths.sort();
        paths
    }

    fn restore_path(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else {
            return;
        };
        let Some(index) = self.visible_index(&path) else {
            return;
        };
        self.select_only(index);
        if let View::Photo { index: current, .. } = &mut self.view {
            *current = index;
        }
    }

    fn restore_selection(&mut self, focus: Option<PathBuf>, paths: Vec<PathBuf>) {
        self.selected.clear();
        for path in paths {
            if let Some(index) = self.visible_index(&path) {
                self.selected.insert(index);
            }
        }
        self.focus = focus.and_then(|path| self.visible_index(&path));
        if let Some(index) = self.focus {
            self.selected.insert(index);
        }
        self.anchor = self.focus;
        if let Some(index) = self.focus
            && let View::Photo { index: current, .. } = &mut self.view
        {
            *current = index;
        }
    }

    fn rebuild_photos(&mut self) {
        let keep = self.remember_path();
        let marked = self.remember_selection();
        self.photos = self
            .all_photos
            .iter()
            .filter(|photo| self.prefs.show_raw || !library::is_raw_path(&photo.path))
            .cloned()
            .collect();
        self.sort_photos();
        if let Some(path) = &keep
            && self.visible_index(path).is_none()
        {
            self.clear_selection();
            if let View::Photo { .. } = self.view {
                self.view = View::Grid;
            }
            self.meta = None;
            self.meta_loading = None;
        } else {
            self.restore_selection(keep, marked);
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
        let marked = self.remember_selection();
        self.sort_photos();
        self.restore_selection(keep, marked);
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
        let tagged: Vec<TaggedPhoto> = self
            .all_photos
            .iter()
            .filter(|photo| photo.albums_known && (!photo.albums.is_empty() || !photo.covers.is_empty()))
            .map(|photo| {
                (
                    photo.path.clone(),
                    photo.albums.clone(),
                    photo.covers.clone(),
                    photo.mtime,
                    photo.len,
                )
            })
            .collect();
        for (path, albums, covers, mtime, len) in tagged {
            self.album_store.remember(&path, &albums);
            self.album_store.remember_cover(&path, &covers, mtime, len);
            self.album_store.touch_cover_meta(&path, mtime, len);
        }
        self.album_store.save();
    }

    fn apply_albums(&mut self, found: Vec<(PathBuf, Vec<String>, Vec<String>)>) {
        if found.is_empty() {
            return;
        }
        for (path, albums, covers) in found {
            self.apply_photo_albums(&path, &albums);
            if let Some((mtime, len)) = self.all_photo(&path).map(|photo| (photo.mtime, photo.len)) {
                self.album_store.remember_cover(&path, &covers, mtime, len);
                self.album_store.touch_cover_meta(&path, mtime, len);
            } else {
                self.album_store.remember_cover(&path, &covers, 0, 0);
            }
            if let Some(photo) = self.all_photo_mut(&path) {
                photo.covers = covers.clone();
            }
            if let Some(photo) = self.visible_photo_mut(&path) {
                photo.covers = covers;
            }
        }
        self.album_store.save();
    }

    fn persist_album_tags(&mut self, path: PathBuf, albums: Vec<String>) {
        self.apply_photo_albums(&path, &albums);
        let covers = self.album_store.covers_of(&path);
        if let Some(photo) = self.all_photo_mut(&path) {
            photo.covers = covers.clone();
        }
        if let Some(photo) = self.visible_photo_mut(&path) {
            photo.covers = covers.clone();
        }
        self.thumbs.request_album_write(path, albums, covers);
    }

    fn apply_photo_albums(&mut self, path: &std::path::Path, albums: &[String]) {
        if let Some(photo) = self.all_photo_mut(path) {
            photo.albums = albums.to_vec();
            photo.albums_known = true;
        }
        if let Some(photo) = self.visible_photo_mut(path) {
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
            let known = self.all_photo(path).map(|photo| (photo.mtime, photo.len, photo.albums.clone()));
            if let Some((mtime, len, _)) = &known {
                self.album_store.touch_cover_meta(path, *mtime, *len);
            }
            let mut albums = known.map(|(_, _, albums)| albums).unwrap_or_default();
            if !albums.iter().any(|have| have == name) {
                albums.push(name.to_string());
            }
            self.persist_album_tags(path.clone(), albums);
        }
        self.album_store.save();
        self.status = i18n::added_to_album(name);
    }

    fn remove_selected_from_album(&mut self) {
        let Some(name) = self.current_album.clone() else {
            return;
        };
        let paths = self.selected_paths();
        for path in &paths {
            self.album_store.drop_path(&name, path);
            let albums: Vec<String> = self
                .all_photo(path)
                .map(|photo| {
                    photo
                        .albums
                        .iter()
                        .filter(|have| *have != &name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            self.persist_album_tags(path.clone(), albums);
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
                .all_photo(&path)
                .map(|photo| {
                    photo
                        .albums
                        .iter()
                        .filter(|have| *have != name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_else(|| {
                    self.album_store
                        .albums
                        .iter()
                        .filter(|album| album.name != name && album.paths.iter().any(|entry| entry == &path))
                        .map(|album| album.name.clone())
                        .collect()
                });
            self.persist_album_tags(path, albums);
        }
        self.album_store.save();
        if self.current_album.as_deref() == Some(name) {
            if let Some(dir) = self.current_dir.clone() {
                self.open_dir(dir);
            } else {
                self.current_album = None;
                self.clear_photos();
                self.clear_selection();
            }
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        if let View::Photo { index, .. } = self.view
            && let Some(photo) = self.photos.get(index)
        {
            return vec![photo.path.clone()];
        }
        let mut paths: Vec<PathBuf> = self
            .selected
            .iter()
            .filter_map(|index| self.photos.get(*index).map(|photo| photo.path.clone()))
            .collect();
        paths.sort();
        paths
    }

    fn set_album_cover(&mut self, index: usize) {
        let Some(name) = self.current_album.clone() else {
            return;
        };
        let Some(photo) = self.photos.get(index).cloned() else {
            return;
        };
        let previous = self
            .album_store
            .set_cover(&name, photo.path.clone(), photo.mtime, photo.len);
        self.persist_album_tags(photo.path.clone(), photo.albums.clone());
        if let Some(old) = previous {
            let albums = self
                .all_photo(&old)
                .map(|item| item.albums.clone())
                .unwrap_or_else(|| {
                    self.album_store
                        .albums
                        .iter()
                        .filter(|album| album.paths.iter().any(|path| path == &old))
                        .map(|album| album.name.clone())
                        .collect()
                });
            self.persist_album_tags(old, albums);
        }
        self.status = i18n::cover_set(&name);
    }

    fn open_album(&mut self, name: String) {
        self.prefs.source = SourcePane::Albums;
        self.current_album = Some(name.clone());
        self.view = View::Grid;
        self.video = None;
        self.clear_selection();
        self.meta = None;
        self.meta_loading = None;
        self.scanning = true;
        self.status = i18n::reading_album(&name);
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
        self.select_only(next);
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
                    Some(Err(i18n::t("Wiedergabe abgebrochen", "Playback stopped").into()))
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

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (escape, backspace) = ctx.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Backspace)));
        if escape || backspace {
            if matches!(self.view, View::Photo { .. }) {
                self.view = View::Grid;
                self.video = None;
            } else if escape && !self.selected.is_empty() {
                self.clear_selection();
            }
        }

        match &self.view {
            View::Grid => {
                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                    self.open_selected_photo();
                }
                if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::A)) {
                    self.select_all();
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
        self.thumbs.request_captures(&self.photos, self.focus);
        self.thumbs.request_album_reads(&self.photos, self.focus);
        if self.prefs.source == SourcePane::Albums {
            let covers = self.album_store.cover_stubs();
            self.thumbs.request_visible(&covers);
        }
        self.sync_video(ctx);
        if self.need_folder && !self.picking_folder && self.tree.is_none() {
            self.need_folder = false;
            self.pick_folder();
        }
        if self.picking_folder {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        handle_drops(&ctx, self);

        // Tastaturkürzel nur, wenn kein Textfeld (Albumname) tippt.
        let typing = ctx.text_edit_focused();
        if !typing {
            self.handle_shortcuts(&ctx);
        }

        egui::Panel::top("title")
            .frame(theme::chrome(egui::Margin::symmetric(20, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    if quiet_text(ui, i18n::t("Ordner öffnen", "Open Folder")).clicked() {
                        self.pick_folder();
                    }
                    if let View::Photo { .. } = self.view
                        && quiet_text(ui, i18n::t("Raster", "Grid")).clicked()
                    {
                        self.view = View::Grid;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        raw_chip(ui, self);
                        sort_chip(ui, self, SortBy::Size, i18n::t("Größe", "Size"));
                        sort_chip(ui, self, SortBy::Modified, i18n::t("Geändert", "Modified"));
                        sort_chip(ui, self, SortBy::Captured, i18n::t("Aufnahme", "Date Taken"));
                        sort_chip(ui, self, SortBy::Name, i18n::t("Name", "Name"));
                    });
                });
                ui.add_space(8.0);
                theme::hairline_bottom(ui);
            });

        egui::Panel::bottom("bar")
            .frame(theme::chrome(egui::Margin::symmetric(20, 8)))
            .show(ui, |ui| {
                theme::hairline_top(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(media_count_label(&self.photos)).color(theme::MUTED));
                    if self.selected.len() > 1 {
                        ui.separator();
                        ui.label(
                            RichText::new(i18n::selected_count(self.selected.len()))
                                .color(theme::INK),
                        );
                    }
                    if self.thumbs.pending > 0 {
                        ui.separator();
                        ui.label(
                            RichText::new(i18n::thumbnail_count(self.thumbs.ready))
                                .color(theme::MUTED),
                        );
                    }
                    if !self.status.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(&self.status).color(theme::MUTED));
                    }
                    if !self.selected.is_empty() || matches!(self.view, View::Photo { .. }) {
                        ui.separator();
                        ui.menu_button(i18n::t("Zu Album", "Add to Album"), |ui| {
                            if self.album_store.albums.is_empty() {
                                ui.label(
                                    RichText::new(i18n::t("Noch kein Album", "No albums yet"))
                                        .color(theme::MUTED),
                                );
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
                        if self.current_album.is_some()
                            && ui.button(i18n::t("Aus Album", "Remove from Album")).clicked()
                        {
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
                        ui.label(RichText::new(i18n::t("Größe", "Size")).color(theme::MUTED));
                    });
                });
            });

        egui::Panel::left("sources")
            .resizable(true)
            .default_size(248.0)
            .size_range(188.0..=360.0)
            .frame(theme::side())
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    if icon_toggle(
                        ui,
                        self.prefs.source == SourcePane::Folders,
                        paint_folder_icon,
                        i18n::t("Ordner", "Folders"),
                    )
                    .clicked()
                    {
                        self.prefs.source = SourcePane::Folders;
                    }
                    if icon_toggle(
                        ui,
                        self.prefs.source == SourcePane::Albums,
                        paint_album_icon,
                        i18n::t("Alben", "Albums"),
                    )
                    .clicked()
                    {
                        self.prefs.source = SourcePane::Albums;
                    }
                });
                ui.add_space(12.0);
                match self.prefs.source {
                    SourcePane::Folders => {
                        egui::ScrollArea::vertical()
                            .id_salt("mediathek-scroll")
                            .auto_shrink([false, false])
                            .hscroll(false)
                            .show(ui, |ui| {
                                draw_tree(ui, self);
                            });
                    }
                    SourcePane::Albums => draw_albums(ui, self),
                }
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

        if self.dragging && ctx.input(|i| !i.pointer.any_down()) {
            self.dragging = false;
        }

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
            .map(|file| file.path().to_path_buf())
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
    theme::section(ui, i18n::t("Datei", "File"));
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        tab_chip(ui, app, InfoTab::Overview, i18n::t("Übersicht", "Overview"));
        tab_chip(ui, app, InfoTab::All, i18n::t("Metadaten", "Metadata"));
    });
    ui.add_space(16.0);

    if app.current_photo().is_none() {
        ui.add_space(8.0);
        ui.label(
            RichText::new(i18n::t("Kein Foto gewählt", "No photo selected"))
                .size(13.0)
                .color(theme::MUTED),
        );
        return;
    }
    if app.meta_loading.is_some() && app.meta.is_none() {
        ui.label(
            RichText::new(i18n::t("Metadaten werden gelesen…", "Reading metadata…"))
                .color(theme::MUTED),
        );
        return;
    }
    let Some(info) = app.meta.as_ref() else {
        ui.label(
            RichText::new(i18n::t("Keine Dateiinformationen", "No file information"))
                .color(theme::MUTED),
        );
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
                        RichText::new(i18n::t(
                            "Dieses Bild hat kein EXIF, IPTC oder XMP.",
                            "This image has no EXIF, IPTC, or XMP.",
                        ))
                        .color(theme::MUTED),
                    );
                }
            }
        });
}

fn draw_open_with_menu(ui: &mut egui::Ui, app: &mut Lichtkasten, path: &std::path::Path) {
    ui.label(RichText::new(i18n::t("Öffnen mit", "Open with")).small().color(theme::MUTED));
    let apps = app
        .open_with
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|catalog| catalog.apps_for(path)));
    match apps {
        None => {
            ui.label(
                RichText::new(i18n::t("Programme werden gelesen…", "Reading programs…"))
                    .color(theme::MUTED),
            );
            ui.ctx().request_repaint();
        }
        Some(apps) if apps.is_empty() => {
            ui.label(
                RichText::new(i18n::t("Kein passendes Programm", "No matching program"))
                    .color(theme::MUTED),
            );
        }
        Some(apps) => {
            for program in apps.iter() {
                let label = program.name.clone();
                if !app.open_with_icons.contains_key(&program.id)
                    && let Some(icon) = &program.icon
                {
                    let texture = ui.ctx().load_texture(
                        format!("app-icon:{}", program.id),
                        open_with::color_image(icon),
                        egui::TextureOptions::LINEAR,
                    );
                    app.open_with_icons.insert(program.id.clone(), texture);
                }
                let texture = app.open_with_icons.get(&program.id).cloned();
                let clicked = if let Some(texture) = texture {
                    ui.add(egui::Button::new((
                        egui::Image::from_texture(&texture)
                            .max_width(16.0)
                            .max_height(16.0),
                        label,
                    )))
                    .clicked()
                } else {
                    ui.button(label).clicked()
                };
                if clicked {
                    open_with::launch(program, path);
                    ui.close();
                }
            }
        }
    }
}

fn icon_toggle(
    ui: &mut egui::Ui,
    selected: bool,
    paint: fn(&egui::Painter, Rect, Color32),
    tip: &str,
) -> egui::Response {
    let size = Vec2::new(20.0, 16.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let color = if selected || response.hovered() {
        theme::INK
    } else {
        theme::MUTED
    };
    paint(ui.painter(), rect, color);
    if selected {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, theme::INK),
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Default);
    }
    response.on_hover_text(tip)
}

fn paint_folder_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let w = rect.width().max(1.0);
    let h = rect.height().max(1.0);
    let p = |x: f32, y: f32| Pos2::new(rect.left() + x * w, rect.top() + y * h);
    let tab = Rect::from_two_pos(p(0.04, 0.10), p(0.46, 0.42));
    let body = Rect::from_two_pos(p(0.04, 0.30), p(0.96, 0.90));
    painter.rect_filled(tab, CornerRadius::same(1), color);
    painter.rect_filled(body, CornerRadius::same(2), color);
}

fn paint_album_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let w = rect.width().max(1.0);
    let h = rect.height().max(1.0);
    let p = |x: f32, y: f32| Pos2::new(rect.left() + x * w, rect.top() + y * h);
    let back = Rect::from_two_pos(p(0.22, 0.06), p(0.96, 0.66));
    let front = Rect::from_two_pos(p(0.04, 0.30), p(0.78, 0.92));
    painter.rect_stroke(back, CornerRadius::same(2), Stroke::new(1.4, color), StrokeKind::Inside);
    painter.rect_filled(front, CornerRadius::same(2), color);
}

fn quiet_text(ui: &mut egui::Ui, text: &str) -> egui::Response {
    quiet_toggle(ui, false, text)
}

fn quiet_toggle(ui: &mut egui::Ui, selected: bool, text: impl Into<String>) -> egui::Response {
    let color = if selected { theme::INK } else { theme::MUTED };
    let response = ui.add(
        egui::Button::new(RichText::new(text.into()).size(13.0).color(color))
            .selected(selected)
            .frame(true),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
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
            i18n::t("RAW ausblenden", "Hide RAW")
        } else {
            i18n::t("RAW einblenden", "Show RAW")
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
        if quiet_text(ui, i18n::t("Neu", "New")).clicked() {
            app.naming_album = true;
            app.new_album.clear();
            app.focus_new_album = true;
        }
    });
    if app.naming_album {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.new_album)
                    .id_salt("new-album-name")
                    .desired_width(120.0)
                    .hint_text(i18n::t("Name", "Name")),
            );
            if app.focus_new_album {
                response.request_focus();
                app.focus_new_album = false;
            }
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if enter || quiet_text(ui, i18n::t("Anlegen", "Create")).clicked() {
                app.create_album();
            }
            if quiet_text(ui, i18n::t("Abbrechen", "Cancel")).clicked() {
                app.naming_album = false;
                app.new_album.clear();
            }
        });
    }
    ui.add_space(8.0);
    let albums: Vec<(String, usize, Option<PathBuf>)> = app
        .album_store
        .albums
        .iter()
        .map(|album| (album.name.clone(), album.paths.len(), album.cover.clone()))
        .collect();
    if albums.is_empty() && !app.naming_album {
        ui.label(
            RichText::new(i18n::t("Noch keine Alben", "No albums yet")).small().color(theme::MUTED),
        );
        return;
    }
    const COVER: f32 = 72.0;
    const ROW: f32 = 80.0;
    egui::ScrollArea::vertical()
        .id_salt("album-covers")
        .auto_shrink([false, false])
        .hscroll(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for (name, count, cover) in albums {
                let selected = app.current_album.as_ref() == Some(&name);
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), ROW),
                    Sense::click(),
                );
                let drop_over = app.dragging && response.contains_pointer();
                if selected || response.hovered() || drop_over {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(6),
                        if selected || drop_over {
                            theme::SURFACE
                        } else {
                            Color32::from_rgba_premultiplied(18, 18, 18, 18)
                        },
                    );
                }
                let cover_rect = Rect::from_min_size(
                    rect.min + Vec2::new(4.0, (ROW - COVER) * 0.5),
                    Vec2::splat(COVER),
                );
                ui.painter().rect_filled(
                    cover_rect,
                    CornerRadius::same(2),
                    thumbs::placeholder_color(0.0, false),
                );
                if let Some(path) = &cover
                    && let Some(texture) = app.thumbs.texture(path)
                {
                    ui.painter().image(
                        texture.id(),
                        cover_rect,
                        cover_uv(texture.size_vec2()),
                        Color32::WHITE,
                    );
                }
                let text_left = cover_rect.right() + 10.0;
                let name_color = if selected { Color32::WHITE } else { theme::INK };
                ui.painter().text(
                    Pos2::new(text_left, rect.center().y - 8.0),
                    Align2::LEFT_CENTER,
                    &name,
                    FontId::proportional(14.0),
                    name_color,
                );
                ui.painter().text(
                    Pos2::new(text_left, rect.center().y + 10.0),
                    Align2::LEFT_CENTER,
                    format!("{count}"),
                    FontId::proportional(12.0),
                    theme::MUTED,
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::Default);
                }
                if drop_over && ui.input(|i| i.pointer.any_released()) && !app.selected.is_empty() {
                    app.add_selected_to_album(&name);
                    app.dragging = false;
                } else if response.clicked() {
                    app.open_album(name.clone());
                }
                response.context_menu(|ui| {
                    if ui.button(i18n::t("Album löschen", "Delete album")).clicked() {
                        app.delete_album(&name);
                        ui.close();
                    }
                });
            }
        });
}

fn draw_tree(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    let Some(tree) = app.tree.as_ref() else {
        ui.add_space(12.0);
        ui.label(i18n::t(
            "Öffne einen Ordner oder ziehe ihn hierher.",
            "Open a folder or drop one here.",
        ));
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
                i18n::t("Ordner wird gelesen…", "Reading folder…")
            } else if app.tree.is_none() {
                i18n::t("Ordner öffnen oder hierher ziehen", "Open a folder or drop one here")
            } else if app.current_album.is_some() {
                i18n::t("Keine Medien in diesem Album", "No media in this album")
            } else {
                i18n::t("Keine Medien in diesem Ordner", "No media in this folder")
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

    let mut vis_start = usize::MAX;
    let mut vis_end = 0usize;
    let time = ui.input(|i| i.time);
    let mut activate: Option<usize> = None;
    let mut select: Option<usize> = None;
    let mut cover: Option<usize> = None;
    let mut arrow: Option<isize> = None;

    if !ui.ctx().text_edit_focused() {
        if ui.input(|i| i.key_pressed(Key::ArrowRight)) {
            arrow = Some(1);
        } else if ui.input(|i| i.key_pressed(Key::ArrowLeft)) {
            arrow = Some(-1);
        } else if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            arrow = Some(cols as isize);
        } else if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
            arrow = Some(-(cols as isize));
        }
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
                        vis_start = vis_start.min(index);
                        vis_end = vis_end.max(index + 1);
                        let selected = app.selected.contains(&index);
                        let is_video = library::is_video_path(&app.photos[index].path);
                        let name = app.photos[index].name.clone();
                        let failed = app.thumbs.failed(&app.photos[index].path);
                        let texture = app.thumbs.texture(&app.photos[index].path);

                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(thumb), Sense::click_and_drag());
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
                        if is_video {
                            painter.text(
                                rect.left_bottom() + Vec2::new(8.0, -7.0),
                                Align2::LEFT_BOTTOM,
                                "▶",
                                FontId::proportional(12.0),
                                Color32::WHITE,
                            );
                        }
                        let path = app.photos[index].path.clone();
                        let resp = resp.on_hover_text(name);
                        resp.context_menu(|ui| {
                            if app.current_album.is_some() {
                                if ui.button(i18n::t("Als Deckblatt", "Set as cover")).clicked() {
                                    cover = Some(index);
                                    ui.close();
                                }
                                ui.separator();
                            }
                            draw_open_with_menu(ui, app, &path);
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(if app.dragging {
                                CursorIcon::Grabbing
                            } else {
                                CursorIcon::PointingHand
                            });
                        }
                        if resp.drag_started() {
                            select = Some(index);
                            app.dragging = true;
                            app.prefs.source = SourcePane::Albums;
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

    if vis_start < vis_end {
        let from = vis_start.saturating_sub(cols);
        let to = (vis_end + cols).min(app.photos.len());
        app.thumbs.request_visible(&app.photos[from..to]);
    }

    let mods = ui.input(|i| i.modifiers);
    if let Some(index) = select {
        if mods.shift {
            app.select_range(index, mods.command);
        } else if mods.command && !app.dragging {
            app.toggle_select(index);
        } else if app.dragging && app.selected.contains(&index) {
            app.focus = Some(index);
        } else {
            app.select_only(index);
        }
    }
    if let Some(index) = cover {
        app.select_only(index);
        app.set_album_cover(index);
    }
    if let Some(index) = activate {
        app.select_only(index);
        app.open_selected_photo();
    }
    if let Some(delta) = arrow {
        let current = app.focus.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, app.photos.len() as isize - 1) as usize;
        if mods.shift {
            app.select_range(next, true);
        } else {
            app.select_only(next);
        }
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
        let strip_h = app.prefs.thumb_size + 14.0;
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
                if ui.button(i18n::t("Einpassen", "Fit")).clicked() {
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
            let max_zoom = (zoom_100 * 4.0).clamp(8.0, 32.0);
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
                    .unwrap_or(i18n::t("Video wird geladen…", "Loading video…"))
                    .to_string()
            } else {
                i18n::t("Bild wird geladen…", "Loading image…").into()
            };
            ui.painter().text(
                stage.center(),
                egui::Align2::CENTER_CENTER,
                waiting,
                egui::FontId::proportional(18.0),
                theme::MUTED,
            );
        }

        if is_video && resp.clicked()
            && let Some(player) = app.video_player_mut() {
                player.toggle();
            }

        resp.context_menu(|ui| {
            draw_open_with_menu(ui, app, &photo.path);
        });

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
    let cell = app.prefs.thumb_size;
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
                    let path = &app.photos[index].path;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(cell), Sense::click());
                    let failed = app.thumbs.failed(path);
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(3),
                        thumbs::placeholder_color(0.0, failed),
                    );
                    if let Some(texture) = app.thumbs.texture(path) {
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
                    app.select_only(index);
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
    i18n::media_count(stills, videos)
}

fn draw_video_controls(ui: &mut egui::Ui, app: &mut Lichtkasten) {
    let Some(player) = app.video_player_mut() else {
        ui.label(RichText::new(i18n::t("Video", "Video")).color(theme::MUTED));
        return;
    };
    ui.separator();
    let label = if player.playing {
        i18n::t("Pause", "Pause")
    } else {
        i18n::t("Abspielen", "Play")
    };
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
    let mute = if player.muted {
        i18n::t("Ton an", "Unmute")
    } else {
        i18n::t("Stumm", "Mute")
    };
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

