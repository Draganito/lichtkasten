//! Volltextindex pro Wurzelordner: `<Wurzel>/.lichtkasten/index.sqlite`.
//!
//! Eine SQLite-Datei mit einer FTS5-Tabelle, die mit der Platte wandert.
//! Sie ist ein Cache wie die Miniaturen: fehlt sie oder ist sie kaputt, wird
//! sie neu aufgebaut. Die Wahrheit bleibt der Ordner.
//!
//! Ein einzelner Indexer-Thread besitzt die Verbindung. Er gleicht den Baum
//! gegen den Index ab (mtime + Länge) und liest nur neue oder geänderte
//! Dateien nach. Suchanfragen werden zwischen zwei Dateien sofort bedient,
//! damit die Suche auch während des Aufbaus antwortet.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use rusqlite::{Connection, OptionalExtension, params};

use crate::cache;
use crate::library::{self, Photo};
use crate::meta;

const FILE_NAME: &str = "index.sqlite";
const MAX_RESULTS: usize = 4000;
/// So viele Dateien werden am Stück indiziert, bevor Suchanfragen drankommen.
const BATCH: usize = 24;

pub struct Progress {
    pub running: AtomicBool,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    /// Index liegt im RAM, weil der Ordner nicht beschreibbar ist.
    pub volatile: AtomicBool,
}

pub enum Event {
    Results { seq: u64, photos: Vec<Photo> },
}

enum Cmd {
    Rescan,
    /// Einzelne Dateien neu lesen (nach Album-Schreibvorgängen).
    Refresh(Vec<PathBuf>),
    Search {
        seq: u64,
        query: String,
    },
}

pub struct Indexer {
    tx: Sender<Cmd>,
    pub progress: Arc<Progress>,
}

impl Indexer {
    pub fn start(root: PathBuf, on_event: Box<dyn Fn(Event) + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress {
            running: AtomicBool::new(true),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            volatile: AtomicBool::new(false),
        });
        let worker_progress = Arc::clone(&progress);
        thread::Builder::new()
            .name("lk-index".into())
            .spawn(move || run(root, rx, on_event, worker_progress))
            .ok();
        let _ = tx.send(Cmd::Rescan);
        Self { tx, progress }
    }

    pub fn rescan(&self) {
        let _ = self.tx.send(Cmd::Rescan);
    }

    pub fn refresh(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            let _ = self.tx.send(Cmd::Refresh(paths));
        }
    }

    pub fn search(&self, seq: u64, query: String) {
        let _ = self.tx.send(Cmd::Search { seq, query });
    }

    pub fn is_running(&self) -> bool {
        self.progress.running.load(Ordering::Relaxed)
    }

    pub fn counts(&self) -> (usize, usize) {
        (
            self.progress.done.load(Ordering::Relaxed),
            self.progress.total.load(Ordering::Relaxed),
        )
    }
}

struct Store {
    root: PathBuf,
    conn: Connection,
}

fn run(
    root: PathBuf,
    rx: Receiver<Cmd>,
    on_event: Box<dyn Fn(Event) + Send>,
    progress: Arc<Progress>,
) {
    let Some(mut store) = Store::open(&root, &progress) else {
        progress.running.store(false, Ordering::Relaxed);
        return;
    };
    loop {
        let Ok(first) = rx.recv() else {
            return;
        };
        // Alles Wartende einsammeln: Suchen sofort, Rescans zu einem zusammenfassen.
        let mut rescan = false;
        let mut refresh: Vec<PathBuf> = Vec::new();
        let mut search: Option<(u64, String)> = None;
        let mut cmds = vec![first];
        loop {
            match rx.try_recv() {
                Ok(cmd) => cmds.push(cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        for cmd in cmds {
            match cmd {
                Cmd::Rescan => rescan = true,
                Cmd::Refresh(paths) => refresh.extend(paths),
                Cmd::Search { seq, query } => search = Some((seq, query)),
            }
        }
        if let Some((seq, query)) = search {
            let photos = store.search(&query);
            on_event(Event::Results { seq, photos });
        }
        if !refresh.is_empty() {
            store.refresh(&refresh);
        }
        if rescan {
            progress.running.store(true, Ordering::Relaxed);
            let finished = store.reconcile(&rx, &on_event, &progress);
            progress.running.store(false, Ordering::Relaxed);
            if !finished {
                return;
            }
        }
    }
}

impl Store {
    fn open(root: &Path, progress: &Progress) -> Option<Self> {
        let dir = root.join(cache::DIR_NAME);
        let conn = fs::create_dir_all(&dir)
            .ok()
            .and_then(|_| Connection::open(dir.join(FILE_NAME)).ok())
            .and_then(|conn| prepare(&conn).ok().map(|_| conn))
            .or_else(|| {
                // Schreibgeschütztes Medium: Index nur im RAM dieser Sitzung.
                progress.volatile.store(true, Ordering::Relaxed);
                let conn = Connection::open_in_memory().ok()?;
                prepare(&conn).ok()?;
                Some(conn)
            })?;
        Some(Self {
            root: root.to_path_buf(),
            conn,
        })
    }

    /// Gleicht den Baum mit dem Index ab. `false`, wenn der Empfänger weg ist.
    fn reconcile(
        &mut self,
        rx: &Receiver<Cmd>,
        on_event: &dyn Fn(Event),
        progress: &Progress,
    ) -> bool {
        let mut known: HashMap<String, (u64, u64)> = HashMap::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT path, mtime, len FROM docs") {
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            });
            if let Ok(rows) = rows {
                for (path, mtime, len) in rows.flatten() {
                    known.insert(path, (mtime, len));
                }
            }
        }

        let mut pending: Vec<Photo> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut stack = vec![(self.root.clone(), 0u8)];
        while let Some((dir, depth)) = stack.pop() {
            if let Ok(photos) = library::scan_photos(&dir) {
                for photo in photos {
                    let rel = self.relative(&photo.path);
                    match known.get(&rel) {
                        Some(&(mtime, len)) if mtime == photo.mtime && len == photo.len => {}
                        _ => pending.push(photo),
                    }
                    seen.push(rel);
                }
            }
            if depth >= 32 {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
                    continue;
                }
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with('.'))
                {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            }
            if !self.serve_searches(rx, on_event) {
                return false;
            }
        }

        // Verschwundene Dateien raus.
        let seen_set: std::collections::HashSet<&str> = seen.iter().map(String::as_str).collect();
        let gone: Vec<&String> = known
            .keys()
            .filter(|path| !seen_set.contains(path.as_str()))
            .collect();
        if !gone.is_empty()
            && let Ok(tx) = self.conn.unchecked_transaction()
        {
            for path in gone {
                let _ = tx.execute("DELETE FROM docs WHERE path = ?1", params![path]);
            }
            let _ = tx.commit();
        }

        progress.total.store(pending.len(), Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);

        for chunk in pending.chunks(BATCH) {
            self.index_chunk(chunk);
            progress.done.fetch_add(chunk.len(), Ordering::Relaxed);
            if !self.serve_searches(rx, on_event) {
                return false;
            }
        }
        true
    }

    /// Liest die Dateien eines Blocks und schreibt sie in einer Transaktion.
    fn index_chunk(&mut self, chunk: &[Photo]) {
        let Ok(tx) = self.conn.unchecked_transaction() else {
            return;
        };
        for photo in chunk {
            let fields = meta::index_fields(&photo.path);
            let rel = self.relative(&photo.path);
            let folder = Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().replace(['/', '\\'], " "))
                .unwrap_or_default();
            let _ = tx.execute("DELETE FROM docs WHERE path = ?1", params![rel]);
            let _ = tx.execute(
                "INSERT INTO docs(path, mtime, len, captured, name, folder, albums, meta) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    rel,
                    photo.mtime as i64,
                    photo.len as i64,
                    fields.captured.map(|c| c as i64),
                    photo.name,
                    folder,
                    fields.albums,
                    fields.meta,
                ],
            );
        }
        let _ = tx.commit();
    }

    /// Einzelne Pfade neu lesen; verschwundene Dateien verlassen den Index.
    fn refresh(&mut self, paths: &[PathBuf]) {
        let mut present = Vec::new();
        for path in paths {
            if !path.starts_with(&self.root) {
                continue;
            }
            match library::photo_from_path(path) {
                Some(photo) => present.push(photo),
                None => {
                    let rel = self.relative(path);
                    let _ = self
                        .conn
                        .execute("DELETE FROM docs WHERE path = ?1", params![rel]);
                }
            }
        }
        for chunk in present.chunks(BATCH) {
            self.index_chunk(chunk);
        }
    }

    /// Beantwortet wartende Suchanfragen; weitere Rescans werden zusammengefasst.
    fn serve_searches(&mut self, rx: &Receiver<Cmd>, on_event: &dyn Fn(Event)) -> bool {
        let mut latest: Option<(u64, String)> = None;
        let mut refresh: Vec<PathBuf> = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Cmd::Search { seq, query }) => latest = Some((seq, query)),
                Ok(Cmd::Refresh(paths)) => refresh.extend(paths),
                Ok(Cmd::Rescan) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        if !refresh.is_empty() {
            self.refresh(&refresh);
        }
        if let Some((seq, query)) = latest {
            let photos = self.search(&query);
            on_event(Event::Results { seq, photos });
        }
        true
    }

    fn search(&self, query: &str) -> Vec<Photo> {
        let Some(fts) = fts_query(query) else {
            return Vec::new();
        };
        let mut stmt = match self
            .conn
            .prepare_cached("SELECT path FROM docs WHERE docs MATCH ?1 ORDER BY rank LIMIT ?2")
        {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![fts, MAX_RESULTS as i64], |row| {
            row.get::<_, String>(0)
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|rel| library::photo_from_path(&self.root.join(rel)))
            .collect()
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
}

fn prepare(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS docs USING fts5(\
            path UNINDEXED, mtime UNINDEXED, len UNINDEXED, captured UNINDEXED, \
            name, folder, albums, meta, \
            tokenize = 'unicode61 remove_diacritics 2'\
        );",
    )?;
    // Schnelltest, ob die Tabelle lesbar ist (kaputte Datei → neu anlegen).
    let probe: Option<i64> = conn
        .query_row("SELECT count(*) FROM docs", [], |row| row.get(0))
        .optional()?;
    if probe.is_none() {
        conn.execute_batch("DROP TABLE IF EXISTS docs;")?;
        return prepare(conn);
    }
    Ok(())
}

/// Nutzertext → FTS5-Ausdruck: jedes Wort als Präfix, alle müssen vorkommen.
fn fts_query(text: &str) -> Option<String> {
    let mut parts = Vec::new();
    for word in text.split_whitespace() {
        let clean: String = word.chars().filter(|c| *c != '"').collect();
        if clean.is_empty() {
            continue;
        }
        parts.push(format!("\"{clean}\"*"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}
