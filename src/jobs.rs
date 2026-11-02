use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::{ColorImage, Context};

use crate::albums;
use crate::decode;
use crate::library::{self, Photo};
use crate::meta::{self, FileInfo};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prio {
    View = 0,
    Meta = 1,
    Thumb = 2,
    Capture = 3,
    Album = 4,
}

pub enum Work {
    Thumb {
        path: PathBuf,
        mtime: u64,
        len: u64,
        generation: u64,
        cache_dir: PathBuf,
    },
    View {
        path: PathBuf,
        target: u32,
        generation: u64,
    },
    Meta {
        photo: Photo,
    },
    Capture {
        path: PathBuf,
        generation: u64,
    },
    AlbumRead {
        path: PathBuf,
        generation: u64,
    },
    AlbumWrite {
        path: PathBuf,
        albums: Vec<String>,
        covers: Vec<String>,
    },
}

pub enum Done {
    Thumb {
        path: PathBuf,
        generation: u64,
        image: Result<ColorImage, String>,
    },
    View {
        path: PathBuf,
        generation: u64,
        image: Result<ColorImage, String>,
    },
    Meta(FileInfo),
    Capture {
        path: PathBuf,
        generation: u64,
        stamp: Option<u64>,
    },
    Albums {
        path: PathBuf,
        generation: u64,
        albums: Vec<String>,
        covers: Vec<String>,
    },
}

struct Queued {
    prio: Prio,
    seq: u64,
    heavy: bool,
    work: Work,
}

struct Shared {
    queue: Mutex<VecDeque<Queued>>,
    cond: Condvar,
    seq: Mutex<u64>,
}

#[derive(Clone)]
pub struct JobQueue {
    shared: Arc<Shared>,
}

pub struct JobJoin {
    pub rx: Receiver<Done>,
}

impl JobQueue {
    pub fn start(ctx: Context) -> (Self, JobJoin) {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cond: Condvar::new(),
            seq: Mutex::new(0),
        });
        let queue = Self {
            shared: Arc::clone(&shared),
        };

        let light = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 4);
        for i in 0..light {
            spawn_worker(
                format!("lk-light-{i}"),
                Arc::clone(&shared),
                tx.clone(),
                ctx.clone(),
                false,
            );
        }
        spawn_worker(
            "lk-heavy".into(),
            Arc::clone(&shared),
            tx,
            ctx,
            true,
        );

        (queue, JobJoin { rx })
    }

    pub fn submit(&self, work: Work, heavy: bool) {
        let prio = match &work {
            Work::View { .. } => Prio::View,
            Work::Meta { .. } => Prio::Meta,
            Work::Thumb { .. } => Prio::Thumb,
            Work::Capture { .. } => Prio::Capture,
            Work::AlbumRead { .. } | Work::AlbumWrite { .. } => Prio::Album,
        };
        let mut seq = self.shared.seq.lock().unwrap_or_else(|e| e.into_inner());
        *seq += 1;
        let item = Queued {
            prio,
            seq: *seq,
            heavy,
            work,
        };
        drop(seq);
        if let Ok(mut queue) = self.shared.queue.try_lock() {
            queue.push_back(item);
            self.shared.cond.notify_all();
        } else if let Ok(mut queue) = self.shared.queue.lock() {
            queue.push_back(item);
            self.shared.cond.notify_all();
        }
    }

    pub fn retain_thumbs(&self, keep: impl Fn(&PathBuf) -> bool) {
        if let Ok(mut queue) = self.shared.queue.try_lock() {
            queue.retain(|item| match &item.work {
                Work::Thumb { path, .. } => keep(path),
                Work::View { .. }
                | Work::Meta { .. }
                | Work::Capture { .. }
                | Work::AlbumRead { .. }
                | Work::AlbumWrite { .. } => true,
            });
        }
    }

    pub fn cancel_views(&self) {
        if let Ok(mut queue) = self.shared.queue.try_lock() {
            queue.retain(|item| !matches!(item.work, Work::View { .. }));
        }
    }

    pub fn clear_generation(&self, live: u64) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            queue.retain(|item| match &item.work {
                Work::Thumb { generation, .. }
                | Work::View { generation, .. }
                | Work::Capture { generation, .. }
                | Work::AlbumRead { generation, .. } => *generation == live,
                Work::Meta { .. } => false,
                Work::AlbumWrite { .. } => true,
            });
        }
        self.shared.cond.notify_all();
    }
}

fn spawn_worker(
    name: String,
    shared: Arc<Shared>,
    tx: Sender<Done>,
    ctx: Context,
    heavy_lane: bool,
) {
    thread::Builder::new()
        .name(name)
        .spawn(move || worker_loop(shared, tx, ctx, heavy_lane))
        .expect("worker");
}

fn worker_loop(shared: Arc<Shared>, tx: Sender<Done>, ctx: Context, heavy_lane: bool) {
    loop {
        let work = {
            let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(index) = pick(&queue, heavy_lane) {
                    break queue.remove(index).unwrap().work;
                }
                queue = shared.cond.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };

        let done = match work {
            Work::Thumb {
                path,
                mtime,
                len,
                generation,
                cache_dir,
            } => Done::Thumb {
                image: decode::load_or_cache_thumb(&cache_dir, &path, mtime, len),
                path,
                generation,
            },
            Work::View {
                path,
                target,
                generation,
            } => Done::View {
                image: decode::load_view(&path, target),
                path,
                generation,
            },
            Work::Meta { photo } => Done::Meta(meta::read_file_info(&photo)),
            Work::Capture { path, generation } => Done::Capture {
                stamp: meta::read_capture_stamp(&path),
                path,
                generation,
            },
            Work::AlbumRead { path, generation } => {
                let tags = albums::read_tags(&path);
                Done::Albums {
                    albums: tags.albums,
                    covers: tags.covers,
                    path,
                    generation,
                }
            }
            Work::AlbumWrite {
                path,
                albums,
                covers,
            } => {
                let tags = albums::write(&path, &albums, &covers).unwrap_or(albums::Tags {
                    albums,
                    covers,
                });
                Done::Albums {
                    albums: tags.albums,
                    covers: tags.covers,
                    path,
                    generation: 0,
                }
            }
        };

        if tx.send(done).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

fn pick(queue: &VecDeque<Queued>, heavy_lane: bool) -> Option<usize> {
    queue
        .iter()
        .enumerate()
        .filter(|(_, job)| job.heavy == heavy_lane)
        .min_by_key(|(_, job)| (job.prio, job.seq))
        .map(|(i, _)| i)
}

pub fn budget(max: Duration) -> Instant {
    Instant::now() + max
}

pub fn within(deadline: Instant) -> bool {
    Instant::now() < deadline
}

pub fn is_tiff(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
}

pub fn is_heavy_file(path: &std::path::Path, len: u64) -> bool {
    if library::is_raw_path(path) || library::is_video_path(path) {
        return false;
    }
    is_tiff(path) || len > 32 * 1024 * 1024
}

pub fn skip_thumb(path: &std::path::Path, len: u64) -> bool {
    if library::is_raw_path(path) || library::is_video_path(path) {
        return false;
    }
    len > 200 * 1024 * 1024
}

pub fn skip_capture(path: &std::path::Path, len: u64) -> bool {
    if library::is_video_path(path) {
        return true;
    }
    if library::is_raw_path(path) {
        return len > 150 * 1024 * 1024;
    }
    skip_thumb(path, len) || (is_tiff(path) && len > 32 * 1024 * 1024)
}
