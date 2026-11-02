//! Volltextindex pro Sammlung: `<Heimat>/.lichtkasten/index.sqlite`.
//!
//! Eine SQLite-Datei mit einer FTS5-Tabelle, die mit der Platte wandert.
//! Sie ist ein Cache wie die Miniaturen: fehlt sie oder ist sie kaputt, wird
//! sie neu aufgebaut. Die Wahrheit bleibt der Ordner.
//!
//! Heimat des Index ist der oberste Elternordner (auf demselben Dateisystem),
//! der schon einen Index hat; gibt es keinen, der geöffnete Ordner selbst.
//! Wer einmal `Bilder` öffnet, hat danach in jedem Unterordner sofort den
//! vollen Index. Verschachtelte ältere Indizes werden übernommen und
//! gelöscht, damit je Sammlung genau eine Datei bleibt.
//!
//! Ein einzelner Indexer-Thread besitzt die Verbindung. Er gleicht nur den
//! geöffneten Teilbaum gegen den Index ab (mtime + Länge), erkennt
//! verschobene Dateien ohne Neulesen und liest nur neue oder geänderte
//! Dateien nach. Suchanfragen werden zwischen zwei Blöcken sofort bedient,
//! damit die Suche auch während des Aufbaus antwortet. Jeder Block ist eine
//! Transaktion: Abbruch zu jedem Zeitpunkt lässt eine gültige Datei zurück,
//! der nächste Start macht beim Rest weiter.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};

use crate::cache;
use crate::library::{self, Photo};
use crate::meta;

pub const FILE_NAME: &str = "index.sqlite";
/// Schema-Kennung; passt sie nicht, wird die Datei neu aufgebaut.
const SCHEMA: &str = "2";
const MAX_RESULTS: usize = 4000;
/// So viele Dateien werden am Stück indiziert, bevor Suchanfragen drankommen.
const BATCH: usize = 24;
const MAX_DEPTH: u8 = 32;

pub struct Progress {
    pub running: AtomicBool,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    /// Index liegt im RAM, weil der Ordner nicht beschreibbar ist.
    pub volatile: AtomicBool,
    /// Bitte so schnell wie möglich aufhören (App schließt, Wurzel wechselt).
    stop: AtomicBool,
    /// Thread ist zu Ende.
    exited: AtomicBool,
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
    tx: Option<Sender<Cmd>>,
    pub progress: Arc<Progress>,
}

impl Indexer {
    /// `scope` ist der geöffnete Ordner; die Heimat des Index wird im Thread gesucht.
    pub fn start(scope: PathBuf, on_event: Box<dyn Fn(Event) + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress {
            running: AtomicBool::new(true),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            volatile: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let worker_progress = Arc::clone(&progress);
        let spawned = thread::Builder::new()
            .name("lk-index".into())
            .spawn(move || {
                run(scope, rx, on_event, Arc::clone(&worker_progress));
                worker_progress.running.store(false, Ordering::Relaxed);
                worker_progress.exited.store(true, Ordering::Relaxed);
            });
        if spawned.is_err() {
            progress.running.store(false, Ordering::Relaxed);
            progress.exited.store(true, Ordering::Relaxed);
        }
        let _ = tx.send(Cmd::Rescan);
        Self {
            tx: Some(tx),
            progress,
        }
    }

    pub fn rescan(&self) {
        self.send(Cmd::Rescan);
    }

    pub fn refresh(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.send(Cmd::Refresh(paths));
        }
    }

    pub fn search(&self, seq: u64, query: String) {
        self.send(Cmd::Search { seq, query });
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
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

    /// Beim Beenden: laufenden Block fertig schreiben, aber höchstens `timeout` warten.
    pub fn shutdown(mut self, timeout: Duration) {
        self.progress.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        let until = Instant::now() + timeout;
        while !self.progress.exited.load(Ordering::Relaxed) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Indexer {
    /// Wurzelwechsel: nur stoppen, nicht warten; der Thread schließt selbst sauber ab.
    fn drop(&mut self) {
        self.progress.stop.store(true, Ordering::Relaxed);
    }
}

fn run(
    scope: PathBuf,
    rx: Receiver<Cmd>,
    on_event: Box<dyn Fn(Event) + Send>,
    progress: Arc<Progress>,
) {
    let Some(mut store) = Store::open(scope, Arc::clone(&progress)) else {
        return;
    };
    loop {
        let Ok(first) = rx.recv() else {
            return;
        };
        if progress.stop.load(Ordering::Relaxed) {
            return;
        }
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
            // Während des Abgleichs eingetroffene Rescans laufen danach nochmal.
            loop {
                store.rescan_again = false;
                progress.running.store(true, Ordering::Relaxed);
                let finished = store.reconcile(&rx, &on_event);
                progress.running.store(false, Ordering::Relaxed);
                if !finished {
                    return;
                }
                if !store.rescan_again {
                    break;
                }
            }
        }
    }
}

struct Store {
    /// Ordner, in dem der Index liegt; alle Pfade sind relativ dazu.
    home: PathBuf,
    /// Geöffneter Ordner; nur dieser Teilbaum wird abgeglichen und durchsucht.
    scope: PathBuf,
    /// `scope` relativ zu `home` mit Schrägstrich am Ende, leer wenn gleich.
    prefix: String,
    conn: Connection,
    progress: Arc<Progress>,
    rescan_again: bool,
    /// Indexdateien unterhalb der Heimat, die beim nächsten Abgleich übernommen werden.
    nested: Vec<PathBuf>,
}

enum Copied {
    Done,
    OtherVersion,
    Failed,
}

enum Opened {
    Writable(Connection),
    ReadOnly(Connection),
    Broken,
    Unavailable,
}

impl Store {
    fn open(scope: PathBuf, progress: Arc<Progress>) -> Option<Self> {
        let (home, nested) = locate(&scope);
        let file = home.join(cache::DIR_NAME).join(FILE_NAME);
        let mut volatile = false;
        let conn = match open_file(&file) {
            Opened::Writable(conn) => conn,
            Opened::Broken => {
                remove_index_files(&file);
                match open_file(&file) {
                    Opened::Writable(conn) => conn,
                    _ => {
                        volatile = true;
                        memory_store(None)?
                    }
                }
            }
            Opened::ReadOnly(conn) => {
                // Fremder oder schreibgeschützter Index: Stand übernehmen, im RAM weiterführen.
                volatile = true;
                memory_store(Some(conn))?
            }
            Opened::Unavailable => {
                volatile = true;
                memory_store(None)?
            }
        };
        progress.volatile.store(volatile, Ordering::Relaxed);
        let prefix = match scope.strip_prefix(&home) {
            Ok(rel) if !rel.as_os_str().is_empty() => format!("{}/", rel.to_string_lossy()),
            _ => String::new(),
        };
        Some(Self {
            home,
            scope,
            prefix,
            conn,
            progress,
            rescan_again: false,
            nested,
        })
    }

    /// Gleicht den Teilbaum mit dem Index ab. `false`, wenn Schluss ist.
    fn reconcile(&mut self, rx: &Receiver<Cmd>, on_event: &dyn Fn(Event)) -> bool {
        self.progress.total.store(0, Ordering::Relaxed);
        self.progress.done.store(0, Ordering::Relaxed);

        for dir in std::mem::take(&mut self.nested) {
            self.absorb(&dir);
        }

        let mut known = self.known_in_scope();
        let mut pending: Vec<Photo> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stack = vec![(self.scope.clone(), 0u8)];
        while let Some((dir, depth)) = stack.pop() {
            if dir != self.home && dir.join(cache::DIR_NAME).join(FILE_NAME).is_file() {
                self.absorb(&dir);
                known = self.known_in_scope();
            }
            if let Ok(photos) = library::scan_photos(&dir) {
                for photo in photos {
                    let rel = self.relative(&photo.path);
                    match known.get(&rel) {
                        Some(&(mtime, len)) if mtime == photo.mtime && len == photo.len => {}
                        _ => pending.push(photo),
                    }
                    seen.insert(rel);
                }
            }
            if depth < MAX_DEPTH
                && let Ok(entries) = fs::read_dir(&dir)
            {
                // Symlinks nicht verfolgen: keine Schleifen, keine Doppelungen.
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
            }
            if !self.serve_searches(rx, on_event) {
                return false;
            }
        }

        // Verschwundene Einträge: erst als verschoben wiedererkennen, Rest löschen.
        let mut gone: HashMap<(String, u64, u64), Vec<String>> = HashMap::new();
        for (path, (mtime, len)) in &known {
            if seen.contains(path) {
                continue;
            }
            let name = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            gone.entry((name, *mtime, *len))
                .or_default()
                .push(path.clone());
        }
        if !gone.is_empty() {
            let mut remaining = Vec::with_capacity(pending.len());
            if let Ok(tx) = self.conn.unchecked_transaction() {
                for photo in pending {
                    let key = (photo.name.clone(), photo.mtime, photo.len);
                    let moved = gone.get_mut(&key).and_then(Vec::pop);
                    match moved {
                        Some(old) => {
                            let rel = self.relative(&photo.path);
                            let _ = tx.execute(
                                "UPDATE docs SET path = ?1, folder = ?2 WHERE path = ?3",
                                params![rel, folder_words(&rel), old],
                            );
                        }
                        None => remaining.push(photo),
                    }
                }
                for path in gone.into_values().flatten() {
                    let _ = tx.execute("DELETE FROM docs WHERE path = ?1", params![path]);
                }
                let _ = tx.commit();
            }
            pending = remaining;
        }

        self.progress.total.store(pending.len(), Ordering::Relaxed);
        for chunk in pending.chunks(BATCH) {
            let written = self.index_chunk(chunk);
            self.progress.done.fetch_add(written, Ordering::Relaxed);
            if !self.serve_searches(rx, on_event) {
                return false;
            }
        }
        true
    }

    /// Alle Einträge im geöffneten Teilbaum: Pfad → (mtime, Länge).
    fn known_in_scope(&self) -> HashMap<String, (u64, u64)> {
        let mut known = HashMap::new();
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT path, mtime, len FROM docs WHERE substr(path, 1, ?2) = ?1")
        else {
            return known;
        };
        let rows = stmt.query_map(
            params![self.prefix, self.prefix.chars().count() as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            },
        );
        if let Ok(rows) = rows {
            for (path, mtime, len) in rows.flatten() {
                known.insert(path, (mtime, len));
            }
        }
        known
    }

    /// Liest die Dateien eines Blocks und schreibt sie in einer Transaktion.
    /// Bei Stopp-Wunsch wird nach der aktuellen Datei festgeschrieben.
    fn index_chunk(&mut self, chunk: &[Photo]) -> usize {
        let Ok(tx) = self.conn.unchecked_transaction() else {
            return 0;
        };
        let mut written = 0;
        for photo in chunk {
            if self.progress.stop.load(Ordering::Relaxed) {
                break;
            }
            let fields = meta::index_fields(&photo.path);
            let rel = self.relative(&photo.path);
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
                    folder_words(&rel),
                    fields.albums,
                    fields.meta,
                ],
            );
            written += 1;
        }
        let _ = tx.commit();
        written
    }

    /// Einzelne Pfade neu lesen; verschwundene Dateien verlassen den Index.
    fn refresh(&mut self, paths: &[PathBuf]) {
        let mut present = Vec::new();
        for path in paths {
            if !path.starts_with(&self.home) {
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

    /// Übernimmt einen verschachtelten Index unter `dir` und löscht ihn danach.
    /// Reihenfolge: erst festschreiben, dann löschen; ein Abbruch dazwischen
    /// lässt beide Dateien stehen und wird beim nächsten Start wiederholt.
    fn absorb(&mut self, dir: &Path) {
        let file = dir.join(cache::DIR_NAME).join(FILE_NAME);
        let Ok(rel) = dir.strip_prefix(&self.home) else {
            return;
        };
        let rel = rel.to_string_lossy();
        if rel.is_empty() {
            return;
        }
        let prefix = format!("{rel}/");
        let words = rel.replace(['/', '\\'], " ");
        match self.attach_and_copy(&file, &prefix, &words) {
            // Übernommen oder aus einer anderen Programmversion: weg damit.
            Copied::Done | Copied::OtherVersion => remove_index_files(&file),
            // Gesperrt oder nicht lesbar: nur löschen, wenn die Datei kaputt ist.
            Copied::Failed => {
                if !index_is_sane(&file) {
                    remove_index_files(&file);
                }
            }
        }
    }

    fn attach_and_copy(&mut self, file: &Path, prefix: &str, words: &str) -> Copied {
        let attached = self.conn.execute(
            "ATTACH DATABASE ?1 AS old",
            params![file.to_string_lossy().into_owned()],
        );
        if attached.is_err() {
            return Copied::Failed;
        }
        let version: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM old.meta WHERE key = 'schema'",
                [],
                |row| row.get(0),
            )
            .ok();
        let result = if version.as_deref() != Some(SCHEMA) {
            Copied::OtherVersion
        } else {
            let copied = self
                .conn
                .execute_batch("BEGIN IMMEDIATE;")
                .and_then(|_| {
                    self.conn.execute(
                        "DELETE FROM docs WHERE path IN (SELECT ?1 || path FROM old.docs)",
                        params![prefix],
                    )
                })
                .and_then(|_| {
                    self.conn.execute(
                        "INSERT INTO docs(path, mtime, len, captured, name, folder, albums, meta) \
                         SELECT ?1 || path, mtime, len, captured, name, \
                                trim(?2 || ' ' || folder), albums, meta FROM old.docs",
                        params![prefix, words],
                    )
                })
                .and_then(|_| self.conn.execute_batch("COMMIT;"));
            if copied.is_ok() {
                Copied::Done
            } else {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Copied::Failed
            }
        };
        let _ = self.conn.execute_batch("DETACH DATABASE old;");
        result
    }

    /// Beantwortet wartende Suchanfragen; Rescans werden für danach gemerkt.
    fn serve_searches(&mut self, rx: &Receiver<Cmd>, on_event: &dyn Fn(Event)) -> bool {
        if self.progress.stop.load(Ordering::Relaxed) {
            return false;
        }
        let mut latest: Option<(u64, String)> = None;
        let mut refresh: Vec<PathBuf> = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Cmd::Search { seq, query }) => latest = Some((seq, query)),
                Ok(Cmd::Refresh(paths)) => refresh.extend(paths),
                Ok(Cmd::Rescan) => self.rescan_again = true,
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
        let mut stmt = match self.conn.prepare_cached(
            "SELECT path FROM docs WHERE docs MATCH ?1 AND substr(path, 1, ?3) = ?2 \
             ORDER BY rank LIMIT ?4",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(
            params![
                fts,
                self.prefix,
                self.prefix.chars().count() as i64,
                MAX_RESULTS as i64
            ],
            |row| row.get::<_, String>(0),
        );
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|rel| library::photo_from_path(&self.home.join(rel)))
            .collect()
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.home)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
}

/// Ordnerteil eines relativen Pfads als Suchwörter.
fn folder_words(rel: &str) -> String {
    Path::new(rel)
        .parent()
        .map(|p| p.to_string_lossy().replace(['/', '\\'], " "))
        .unwrap_or_default()
}

/// Sucht die Heimat des Index: der oberste Elternordner auf demselben
/// Dateisystem, der schon einen Index hat, sonst `scope` selbst.
/// Zurück kommen außerdem die Indexordner dazwischen, die übernommen werden.
fn locate(scope: &Path) -> (PathBuf, Vec<PathBuf>) {
    use std::os::unix::fs::MetadataExt;
    let device = fs::metadata(scope).ok().map(|m| m.dev());
    let mut found: Vec<PathBuf> = Vec::new();
    let mut dir = Some(scope.to_path_buf());
    while let Some(current) = dir {
        let same_device = fs::metadata(&current).ok().map(|m| m.dev()) == device;
        if !same_device {
            break;
        }
        if current.join(cache::DIR_NAME).join(FILE_NAME).is_file() {
            found.push(current.clone());
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    match found.pop() {
        Some(home) => (home, found),
        None => (scope.to_path_buf(), Vec::new()),
    }
}

fn open_file(file: &Path) -> Opened {
    let Some(dir) = file.parent() else {
        return Opened::Unavailable;
    };
    if fs::create_dir_all(dir).is_err() {
        return if file.is_file() {
            open_read_only(file)
        } else {
            Opened::Unavailable
        };
    }
    let Ok(conn) = Connection::open(file) else {
        return if file.is_file() {
            open_read_only(file)
        } else {
            Opened::Unavailable
        };
    };
    let _ = conn.busy_timeout(Duration::from_secs(5));
    if !is_database(&conn) {
        return Opened::Broken;
    }
    // Schreibgeschützte Datei oder fremder Besitzer: SQLite öffnet dann still nur lesend.
    if conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(true) {
        drop(conn);
        return open_read_only(file);
    }
    match prepare(&conn) {
        Ok(true) => {}
        _ => return Opened::Broken,
    }
    // Echte Schreibprobe: deckt auch einen nicht beschreibbaren Ordner (Journal) auf.
    let probe = conn.execute_batch(
        "BEGIN IMMEDIATE; \
         INSERT OR REPLACE INTO meta(key, value) VALUES ('probe', '1'); \
         DELETE FROM meta WHERE key = 'probe'; \
         COMMIT;",
    );
    if probe.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
        drop(conn);
        return open_read_only(file);
    }
    Opened::Writable(conn)
}

fn open_read_only(file: &Path) -> Opened {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(file, flags) {
        Ok(conn) if is_database(&conn) && schema_matches(&conn) => Opened::ReadOnly(conn),
        Ok(_) => Opened::Unavailable,
        Err(_) => Opened::Unavailable,
    }
}

/// Leerer RAM-Index, optional mit dem Inhalt eines nur lesbaren Index.
fn memory_store(seed: Option<Connection>) -> Option<Connection> {
    let conn = Connection::open_in_memory().ok()?;
    if !prepare(&conn).unwrap_or(false) {
        return None;
    }
    if let Some(seed) = seed
        && let Ok(mut stmt) =
            seed.prepare("SELECT path, mtime, len, captured, name, folder, albums, meta FROM docs")
        && let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        && let Ok(tx) = conn.unchecked_transaction()
    {
        for row in rows.flatten() {
            let _ = tx.execute(
                "INSERT INTO docs(path, mtime, len, captured, name, folder, albums, meta) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7],
            );
        }
        let _ = tx.commit();
    }
    Some(conn)
}

fn is_database(conn: &Connection) -> bool {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .is_ok()
}

fn schema_matches(conn: &Connection) -> bool {
    conn.query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
        row.get::<_, String>(0)
    })
    .map(|v| v == SCHEMA)
    .unwrap_or(false)
}

fn index_is_sane(file: &Path) -> bool {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(file, flags)
        .map(|conn| is_database(&conn))
        .unwrap_or(false)
}

/// Legt das Schema an. `Ok(false)`: Datei stammt aus einer anderen Version
/// oder ist unbrauchbar und muss neu aufgebaut werden.
fn prepare(conn: &Connection) -> rusqlite::Result<bool> {
    // DELETE statt WAL: im Ruhezustand eine einzige Datei, sicher auch auf Netzlaufwerken.
    let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    conn.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);")?;
    let version: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
            row.get(0)
        })
        .ok();
    let has_docs: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'docs'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    match version {
        Some(v) if v == SCHEMA && has_docs => {}
        None if !has_docs => {
            conn.execute_batch(
                "CREATE VIRTUAL TABLE docs USING fts5(\
                    path UNINDEXED, mtime UNINDEXED, len UNINDEXED, captured UNINDEXED, \
                    name, folder, albums, meta, \
                    tokenize = 'unicode61 remove_diacritics 2'\
                );",
            )?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema', ?1)",
                params![SCHEMA],
            )?;
        }
        _ => return Ok(false),
    }
    // Schnelltest, ob die Tabelle lesbar ist.
    Ok(conn
        .query_row("SELECT count(*) FROM docs", [], |row| row.get::<_, i64>(0))
        .is_ok())
}

/// Löscht eine Indexdatei samt Journal-Begleitern. Nur `index.sqlite*`, nie den Ordner.
fn remove_index_files(file: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
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
