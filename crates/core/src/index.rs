//! The SQLite index. Each transcript file is read once; later refreshes read
//! only the bytes appended since. Rows stay in the index after their
//! transcript is deleted, so history outlives cleanup.
//!
//! Schema changes are migrations (`MIGRATIONS`), applied in order on open,
//! one `user_version` step each. Raw request rows are never dropped by a
//! migration.
//!
//! Concurrency: one writer connection refreshes; queries can use read-only
//! connections from [`Index::open_reader`] so a refresh never blocks them
//! (the database is in WAL mode). A reader can still be asked to refresh:
//! it hands the work to the writer it was given with
//! [`Index::attach_writer`].

use crate::desktop;
use crate::parse::{self, Row};
use crate::{Error, Result};
use jiff::tz::TimeZone;
use rayon::prelude::*;
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The schema version this build writes. Each step in `MIGRATIONS` moves the
/// index up by one.
pub const SCHEMA_VERSION: i64 = 4;

/// Version 2: the first released schema.
const SCHEMA_V2: &str = "
CREATE TABLE files(
  path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime INTEGER NOT NULL, offset INTEGER NOT NULL,
  project TEXT);
CREATE TABLE requests(
  key TEXT PRIMARY KEY, file TEXT NOT NULL, session TEXT NOT NULL, ts INTEGER NOT NULL,
  day TEXT NOT NULL, hour INTEGER NOT NULL, dow INTEGER NOT NULL,
  model TEXT NOT NULL, effort TEXT, speed TEXT, entrypoint TEXT,
  project TEXT NOT NULL, cwd TEXT NOT NULL, branch TEXT, subagent INTEGER NOT NULL,
  input INTEGER NOT NULL, output INTEGER NOT NULL, thinking INTEGER NOT NULL,
  cw5m INTEGER NOT NULL, cw1h INTEGER NOT NULL, cache_write INTEGER NOT NULL, cache_read INTEGER NOT NULL,
  web_search INTEGER NOT NULL, web_fetch INTEGER NOT NULL);
CREATE INDEX requests_day ON requests(day);
CREATE INDEX requests_file ON requests(file);
CREATE INDEX requests_session ON requests(session);
CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT NOT NULL);
";

/// Version 3: more per-request fields, Claude Desktop's session index, and
/// a `meta` table. Every file is read once more from the start so the new
/// columns fill in for transcripts still on disk; rows whose transcript is
/// gone keep NULL there.
const MIGRATE_V3: &str = "
ALTER TABLE requests ADD COLUMN requested_model TEXT;
ALTER TABLE requests ADD COLUMN stop_reason TEXT;
ALTER TABLE requests ADD COLUMN attribution_skill TEXT;
ALTER TABLE requests ADD COLUMN attribution_plugin TEXT;
ALTER TABLE requests ADD COLUMN attribution_mcp_server TEXT;
ALTER TABLE requests ADD COLUMN thinking_ms INTEGER;
ALTER TABLE requests ADD COLUMN inference_geo TEXT;
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE desktop_files(path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime INTEGER NOT NULL);
CREATE TABLE desktop_sessions(
  id TEXT PRIMARY KEY, cli_session_id TEXT, title TEXT, title_source TEXT,
  model TEXT, effort TEXT, permission_mode TEXT, archived INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER, last_activity_at INTEGER, completed_turns INTEGER,
  branch TEXT, source_branch TEXT, origin_cwd TEXT, mcp_servers TEXT NOT NULL DEFAULT '[]');
CREATE INDEX desktop_sessions_cli ON desktop_sessions(cli_session_id);
CREATE TABLE session_links(
  cli_session_id TEXT PRIMARY KEY, canonical TEXT NOT NULL, desktop_id TEXT NOT NULL);
CREATE INDEX session_links_canonical ON session_links(canonical);
CREATE TABLE mcp_names(uuid TEXT PRIMARY KEY, name TEXT NOT NULL);
UPDATE files SET offset = 0, size = -1;
";

/// Version 4: API-equivalent cost and price-weighted tokens per request,
/// filled by [`Index::price_rows`] after each refresh. `priced` is 1 once a
/// row has been through the current price table (its cost stays NULL when
/// the model has no price).
const MIGRATE_V4: &str = "
ALTER TABLE requests ADD COLUMN cost_usd REAL;
ALTER TABLE requests ADD COLUMN weighted REAL;
ALTER TABLE requests ADD COLUMN priced INTEGER NOT NULL DEFAULT 0;
CREATE INDEX requests_priced ON requests(priced);
";

/// `MIGRATIONS[i]` takes the index from version `i + 2` to `i + 3`.
const MIGRATIONS: &[&str] = &[MIGRATE_V3, MIGRATE_V4];

const META_PRICE_TABLE: &str = "price_table";

/// key, model, day, [input, output, cw5m, cw1h, cache_write, cache_read,
/// web_search], speed, inference_geo.
type PriceRow = (
    String,
    String,
    String,
    [i64; 7],
    Option<String>,
    Option<String>,
);

const META_TIME_ZONE: &str = "time_zone";

const INSERT: &str = "INSERT INTO requests(
  key, file, session, ts, day, hour, dow, model, effort, speed, entrypoint, project, cwd, branch,
  subagent, input, output, thinking, cw5m, cw1h, cache_write, cache_read, web_search, web_fetch,
  requested_model, stop_reason, attribution_skill, attribution_plugin, attribution_mcp_server,
  thinking_ms, inference_geo)
 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,
  ?25,?26,?27,?28,?29,?30,?31)
 ON CONFLICT(key) DO UPDATE SET
  requested_model = COALESCE(requested_model, excluded.requested_model),
  stop_reason = COALESCE(stop_reason, excluded.stop_reason),
  attribution_skill = COALESCE(attribution_skill, excluded.attribution_skill),
  attribution_plugin = COALESCE(attribution_plugin, excluded.attribution_plugin),
  attribution_mcp_server = COALESCE(attribution_mcp_server, excluded.attribution_mcp_server),
  thinking_ms = COALESCE(thinking_ms, excluded.thinking_ms),
  inference_geo = COALESCE(inference_geo, excluded.inference_geo)
 WHERE (requested_model IS NULL AND excluded.requested_model IS NOT NULL)
    OR (stop_reason IS NULL AND excluded.stop_reason IS NOT NULL)
    OR (attribution_skill IS NULL AND excluded.attribution_skill IS NOT NULL)
    OR (attribution_plugin IS NULL AND excluded.attribution_plugin IS NOT NULL)
    OR (attribution_mcp_server IS NULL AND excluded.attribution_mcp_server IS NOT NULL)
    OR (thinking_ms IS NULL AND excluded.thinking_ms IS NOT NULL)
    OR (inference_geo IS NULL AND excluded.inference_geo IS NOT NULL)";

pub struct Index {
    /// Read-write for an index from [`Index::open`]; read-only for one from
    /// [`Index::open_reader`].
    pub conn: Connection,
    path: Option<PathBuf>,
    read_only: bool,
    writer: Option<Arc<Mutex<Index>>>,
    desktop_root: Option<PathBuf>,
    tz: Option<TimeZone>,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshStats {
    pub files_seen: usize,
    pub files_read: usize,
    pub new_requests: usize,
    pub errors: usize,
    pub ms: u64,
    /// Claude Desktop session files read (changed since the last refresh).
    pub desktop_files_read: usize,
    /// Rows whose day and hour were re-derived because the time zone changed.
    pub rezoned: usize,
    /// Rows given a cost by the price table in this refresh.
    pub priced: usize,
}

struct Todo {
    path: PathBuf,
    key: String,
    size: i64,
    mtime: i64,
    offset: u64,
    reset: bool,
    project: Option<String>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") => out.push(p),
            _ => {}
        }
    }
}

pub(crate) fn size_mtime(p: &Path) -> Option<(i64, i64)> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos() as i64;
    Some((m.len() as i64, mtime))
}

/// A stable name for a time zone, stored to notice when it changes.
fn tz_key(tz: &TimeZone) -> String {
    tz.iana_name()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{tz:?}"))
}

fn count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))
}

/// Bring the schema to `SCHEMA_VERSION`. Versions before 2 predate any
/// release and are rebuilt; later ones migrate in place.
fn migrate(conn: &mut Connection) -> Result<()> {
    let mut v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "the index at schema version {v} was written by a newer version of this app \
             (this one reads up to {SCHEMA_VERSION}); update the app"
        )));
    }
    if v < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS requests; DROP TABLE IF EXISTS sessions;",
        )?;
        tx.execute_batch(SCHEMA_V2)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        v = 2;
    }
    while v < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        tx.execute_batch(MIGRATIONS[(v - 2) as usize])?;
        if v + 1 == 3 {
            // Rows so far were split into days in the zone they were read in.
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![META_TIME_ZONE, tz_key(&TimeZone::system())],
            )?;
        }
        tx.pragma_update(None, "user_version", v + 1)?;
        tx.commit()?;
        v += 1;
    }
    Ok(())
}

static MEM_DB: AtomicUsize = AtomicUsize::new(0);

impl Index {
    /// Open (creating or migrating) the index at `path` for reading and
    /// writing. Claude Desktop's session index is read on refresh.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut i = Self::init(Connection::open(path)?, Some(path.to_path_buf()))?;
        i.desktop_root = Some(crate::paths::desktop_sessions_dir());
        Ok(i)
    }

    /// A private in-memory index, for tests. It does not read Claude
    /// Desktop's files unless [`Index::set_desktop_root`] is called.
    /// [`Index::reader`] works on it too.
    pub fn open_in_memory() -> Result<Self> {
        let n = MEM_DB.fetch_add(1, Ordering::Relaxed);
        let uri = format!(
            "file:usage-mem-{}-{n}?mode=memory&cache=shared",
            std::process::id()
        );
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::init(conn, Some(PathBuf::from(uri)))
    }

    /// A read-only connection to an existing index. Queries through it are
    /// not blocked by a refresh on another connection. Open the index once
    /// with [`Index::open`] first, so it exists and is migrated.
    pub fn open_reader(path: &Path) -> Result<Self> {
        let s = path.to_string_lossy();
        let mut flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if s.starts_with("file:") {
            flags |= OpenFlags::SQLITE_OPEN_URI;
        }
        let conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "query_only", true)?;
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v != SCHEMA_VERSION {
            return Err(Error::Other(format!(
                "index is at schema version {v}, expected {SCHEMA_VERSION}; open it for writing first"
            )));
        }
        Ok(Index {
            conn,
            path: Some(path.to_path_buf()),
            read_only: true,
            writer: None,
            desktop_root: None,
            tz: None,
        })
    }

    /// Another, read-only connection to this index.
    pub fn reader(&self) -> Result<Self> {
        let p = self
            .path
            .as_ref()
            .ok_or_else(|| Error::Other("index has no path".into()))?;
        Self::open_reader(p)
    }

    /// Let a read-only index hand writes (refresh, inserts) to `writer`.
    pub fn attach_writer(mut self, writer: Arc<Mutex<Index>>) -> Self {
        self.writer = Some(writer);
        self
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Where Claude Desktop's session index is read from on refresh; `None`
    /// to skip it.
    pub fn set_desktop_root(&mut self, root: Option<PathBuf>) {
        self.desktop_root = root;
    }

    /// Split days and hours in `tz` instead of the system time zone.
    pub fn set_time_zone(&mut self, tz: Option<TimeZone>) {
        self.tz = tz;
    }

    fn init(mut conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Index {
            conn,
            path,
            read_only: false,
            writer: None,
            desktop_root: None,
            tz: None,
        })
    }

    /// Run `f` on the index that can write: this one, or the writer a
    /// read-only index was attached to. Methods that write go through here.
    pub fn with_writer<T>(&mut self, f: impl FnOnce(&mut Index) -> Result<T>) -> Result<T> {
        if !self.read_only {
            return f(self);
        }
        let w = self
            .writer
            .clone()
            .ok_or_else(|| Error::Other("read-only index has no writer attached".into()))?;
        let mut g = w.lock().map_err(|e| Error::Other(e.to_string()))?;
        f(&mut g)
    }

    /// Re-derive every row's day, hour and weekday if the index was last
    /// split in another time zone. Returns the number of rows changed.
    pub fn sync_time_zone(&mut self, tz: &TimeZone) -> Result<usize> {
        self.with_writer(|w| w.sync_time_zone_inner(tz))
    }

    fn sync_time_zone_inner(&mut self, tz: &TimeZone) -> Result<usize> {
        let key = tz_key(tz);
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [META_TIME_ZONE],
                |r| r.get(0),
            )
            .ok();
        if stored.as_deref() == Some(key.as_str()) {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut sel = tx.prepare("SELECT key, ts, day, hour, dow FROM requests")?;
            let mut upd =
                tx.prepare("UPDATE requests SET day = ?2, hour = ?3, dow = ?4 WHERE key = ?1")?;
            let rows = sel.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u8>(3)?,
                    r.get::<_, u8>(4)?,
                ))
            })?;
            for row in rows {
                let (k, ts, day, hour, dow) = row?;
                let Ok(t) = jiff::Timestamp::from_millisecond(ts) else {
                    continue;
                };
                let (d, h, w) = parse::local_parts(t, tz);
                if d != day || h != hour || w != dow {
                    upd.execute(params![k, d, h, w])?;
                    n += 1;
                }
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![META_TIME_ZONE, key],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Read new transcript bytes under `root` into the index, then Claude
    /// Desktop's session index. `progress` gets (files done, files to read)
    /// and may be called from several threads.
    pub fn refresh(
        &mut self,
        root: &Path,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<RefreshStats> {
        self.with_writer(|w| {
            let mut s = w.refresh_inner(root, progress)?;
            let table = crate::cost::PriceTable::load()
                .unwrap_or_else(|_| crate::cost::PriceTable::embedded());
            s.priced = w.price_rows_inner(&table)?;
            Ok(s)
        })
    }

    /// Fill `cost_usd` and `weighted` for rows not yet priced with `table`.
    /// A different table (edited override, new app version) re-prices every
    /// row. Returns the number of rows priced.
    pub fn price_rows(&mut self, table: &crate::cost::PriceTable) -> Result<usize> {
        self.with_writer(|w| w.price_rows_inner(table))
    }

    fn price_rows_inner(&mut self, table: &crate::cost::PriceTable) -> Result<usize> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(table)
            .unwrap_or_default()
            .hash(&mut h);
        let fp = format!("{:016x}", h.finish());
        let tx = self.conn.transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [META_PRICE_TABLE],
                |r| r.get(0),
            )
            .ok();
        if old.as_deref() != Some(fp.as_str()) {
            tx.execute("UPDATE requests SET priced = 0 WHERE priced = 1", [])?;
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![META_PRICE_TABLE, fp],
            )?;
        }
        let rows: Vec<PriceRow> = {
            let mut st = tx.prepare(
                "SELECT key, model, day, input, output, cw5m, cw1h, cache_write, cache_read,
                        web_search, speed, inference_geo
                 FROM requests WHERE priced = 0",
            )?;
            let it = st.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    [
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ],
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?;
            it.collect::<rusqlite::Result<_>>()?
        };
        {
            let mut up = tx.prepare(
                "UPDATE requests SET cost_usd = ?2, weighted = ?3, priced = 1 WHERE key = ?1",
            )?;
            for (key, model, day, n, speed, geo) in &rows {
                let g = |i: usize| n[i].max(0) as u64;
                let mut u = crate::cost::Usage {
                    input: g(0),
                    output: g(1),
                    cache_read: g(5),
                    web_search_requests: g(6),
                    fast: speed.as_deref() == Some("fast"),
                    geo_us: geo.as_deref() == Some("us"),
                    ..Default::default()
                };
                u.set_cache_write(g(4), g(2), g(3));
                let cost = table.cost_of_on(model, &u, Some(day));
                let weighted = table.weighted_tokens(model, &u);
                up.execute(params![key, cost, weighted])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    fn refresh_inner(
        &mut self,
        root: &Path,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<RefreshStats> {
        let started = Instant::now();
        let tz = self.tz.clone().unwrap_or_else(TimeZone::system);
        let rezoned = self.sync_time_zone_inner(&tz)?;
        let mut files = Vec::new();
        walk(root, &mut files);

        type Known = (i64, i64, i64, Option<String>);
        let known: HashMap<String, Known> = {
            let mut st = self
                .conn
                .prepare("SELECT path, size, mtime, offset, project FROM files")?;
            let rows = st.query_map([], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let todo: Vec<Todo> = files
            .iter()
            .filter_map(|p| {
                let (size, mtime) = size_mtime(p)?;
                let key = p.to_string_lossy().into_owned();
                let (offset, reset, project) = match known.get(&key) {
                    Some((s, t, _, _)) if *s == size && *t == mtime => return None,
                    Some((_, _, o, p)) if size >= *o => (*o as u64, false, p.clone()),
                    Some(_) => (0, true, None), // file shrank: rewritten, read it again
                    None => (0, false, None),
                };
                Some(Todo {
                    path: p.clone(),
                    key,
                    size,
                    mtime,
                    offset,
                    reset,
                    project,
                })
            })
            .collect();

        let total = todo.len();
        let done = AtomicUsize::new(0);
        progress(0, total);
        let parsed: Vec<_> = todo
            .par_iter()
            .map(|t| {
                let r = parse::parse_file(&t.path, t.offset, t.project.clone(), &tz);
                progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                r
            })
            .collect();

        let mut stats = RefreshStats {
            files_seen: files.len(),
            files_read: total,
            rezoned,
            ..Default::default()
        };
        let tx = self.conn.transaction()?;
        let before = count(&tx)?;
        let mut deleted = 0i64;
        {
            let mut ins = tx.prepare(INSERT)?;
            let mut title = tx.prepare(
                "INSERT INTO sessions(id, title) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title
                 WHERE NOT EXISTS (
                   SELECT 1 FROM session_links l JOIN desktop_sessions d ON d.id = l.desktop_id
                   WHERE l.cli_session_id = excluded.id AND COALESCE(d.title, '') <> '')",
            )?;
            let mut file = tx.prepare(
                "INSERT INTO files(path, size, mtime, offset, project) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime,
                   offset = excluded.offset, project = excluded.project",
            )?;
            for (t, r) in todo.iter().zip(parsed) {
                let Ok(fp) = r else {
                    stats.errors += 1;
                    continue;
                };
                if t.reset {
                    deleted += tx.execute("DELETE FROM requests WHERE file = ?1", [&t.key])? as i64;
                }
                for row in &fp.rows {
                    insert(&mut ins, &t.key, row)?;
                }
                for (s, ti) in &fp.titles {
                    title.execute(params![s, ti])?;
                }
                if fp.relocated {
                    tx.execute(
                        "UPDATE requests SET project = ?1 WHERE file = ?2",
                        params![fp.project, t.key],
                    )?;
                }
                file.execute(params![
                    t.key,
                    t.size,
                    t.mtime,
                    fp.offset as i64,
                    fp.project
                ])?;
            }
        }
        stats.new_requests = (count(&tx)? - before + deleted).max(0) as usize;
        // Subagent files take their parent session's project, so a session
        // that moved folders stays in one project.
        if stats.new_requests > 0 {
            tx.execute_batch(
                "UPDATE requests SET project = f.project
                 FROM files f
                 WHERE requests.subagent = 1
                   AND f.path = substr(requests.file, 1, instr(requests.file, '/subagents/') - 1) || '.jsonl'
                   AND f.project IS NOT NULL AND requests.project <> f.project;",
            )?;
        }
        tx.commit()?;
        if let Some(d) = self.desktop_root.clone() {
            match self.refresh_desktop_inner(&d) {
                Ok(n) => stats.desktop_files_read = n,
                Err(_) => stats.errors += 1,
            }
        }
        stats.ms = started.elapsed().as_millis() as u64;
        Ok(stats)
    }

    /// Read Claude Desktop's session files under `root` that changed since
    /// the last read. Returns the number of files read.
    pub fn refresh_desktop(&mut self, root: &Path) -> Result<usize> {
        self.with_writer(|w| w.refresh_desktop_inner(root))
    }

    fn refresh_desktop_inner(&mut self, root: &Path) -> Result<usize> {
        let files = desktop::session_files(root);
        let known: HashMap<String, (i64, i64)> = {
            let mut st = self
                .conn
                .prepare("SELECT path, size, mtime FROM desktop_files")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let todo: Vec<(PathBuf, String, i64, i64)> = files
            .into_iter()
            .filter_map(|p| {
                let (size, mtime) = size_mtime(&p)?;
                let key = p.to_string_lossy().into_owned();
                if known.get(&key) == Some(&(size, mtime)) {
                    return None;
                }
                Some((p, key, size, mtime))
            })
            .collect();
        if todo.is_empty() {
            return Ok(0);
        }
        let parsed: Vec<_> = todo
            .par_iter()
            .map(|(p, ..)| desktop::parse_session_file(p))
            .collect();
        let tx = self.conn.transaction()?;
        {
            let mut seen = tx.prepare(
                "INSERT INTO desktop_files(path, size, mtime) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime",
            )?;
            for ((_, key, size, mtime), s) in todo.iter().zip(parsed) {
                // A file being written may not parse yet; try again next time.
                let Ok(s) = s else { continue };
                desktop::store(&tx, &s)?;
                seen.execute(params![key, size, mtime])?;
            }
        }
        tx.commit()?;
        Ok(todo.len())
    }

    /// Insert rows directly, for tests and imports. Returns the number of
    /// new rows; a row already present only gains fields it lacked.
    pub fn insert_rows(&mut self, file: &str, rows: &[Row]) -> Result<usize> {
        self.with_writer(|w| {
            let tx = w.conn.transaction()?;
            let before = count(&tx)?;
            {
                let mut ins = tx.prepare(INSERT)?;
                for r in rows {
                    insert(&mut ins, file, r)?;
                }
            }
            let n = (count(&tx)? - before) as usize;
            tx.commit()?;
            Ok(n)
        })
    }
}

fn insert(st: &mut rusqlite::Statement, file: &str, r: &Row) -> rusqlite::Result<usize> {
    st.execute(params![
        r.key,
        file,
        r.session,
        r.ts,
        r.day,
        r.hour,
        r.dow,
        r.model,
        r.effort,
        r.speed,
        r.entrypoint,
        r.project,
        r.cwd,
        r.branch,
        r.subagent,
        r.input as i64,
        r.output as i64,
        r.thinking as i64,
        r.cw5m as i64,
        r.cw1h as i64,
        r.cache_write as i64,
        r.cache_read as i64,
        r.web_search as i64,
        r.web_fetch as i64,
        r.requested_model,
        r.stop_reason,
        r.attribution_skill,
        r.attribution_plugin,
        r.attribution_mcp_server,
        r.thinking_ms.map(|x| x as i64),
        r.inference_geo,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database as version 2 of the app left it.
    fn v2_fixture(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(SCHEMA_V2).unwrap();
        c.pragma_update(None, "user_version", 2).unwrap();
        for (i, day) in ["2026-11-09", "2026-11-10", "2026-11-16"]
            .iter()
            .enumerate()
        {
            c.execute(
                "INSERT INTO requests VALUES (?1,'/gone/s1.jsonl','s1',?2,?3,10,0,'claude-opus-5-5',
                 'medium',NULL,NULL,'/r/app','/r/app','main',0,1,100,5,0,10,10,1000,0,0)",
                params![format!("k{i}"), 1_789_000_000_000i64 + i as i64, day],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO files VALUES ('/gone/s1.jsonl', 10, 1, 10, '/r/app')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO sessions VALUES ('s1', 'Old title')", [])
            .unwrap();
    }

    #[test]
    fn migrates_v2_without_losing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.sqlite");
        v2_fixture(&p);
        let mut idx = Index::open(&p).unwrap();
        idx.set_desktop_root(None);
        let v: i64 = idx
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(count(&idx.conn).unwrap(), 3);
        let (title, off): (String, i64) = idx
            .conn
            .query_row(
                "SELECT s.title, f.offset FROM sessions s, files f WHERE s.id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "Old title");
        assert_eq!(off, 0, "files are re-read once to fill the new columns");
        let rm: Option<String> = idx
            .conn
            .query_row("SELECT requested_model FROM requests LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rm, None);
        // The transcript is gone: a refresh keeps the rows.
        idx.refresh(dir.path(), &|_, _| {}).unwrap();
        assert_eq!(count(&idx.conn).unwrap(), 3);
        drop(idx);
        // Opening again is a no-op.
        let idx = Index::open(&p).unwrap();
        assert_eq!(count(&idx.conn).unwrap(), 3);
    }

    #[test]
    fn refuses_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.sqlite");
        Connection::open(&p)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(Index::open(&p).is_err());
    }

    fn row(key: &str, ts: i64) -> Row {
        let t = jiff::Timestamp::from_millisecond(ts).unwrap();
        let (day, hour, dow) = parse::local_parts(t, &TimeZone::UTC);
        Row {
            key: key.into(),
            session: "s".into(),
            ts,
            day,
            hour,
            dow,
            model: "m".into(),
            project: "/p".into(),
            cwd: "/p".into(),
            ..Default::default()
        }
    }

    #[test]
    fn time_zone_change_rederives_days() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.sync_time_zone(&TimeZone::UTC).unwrap();
        // 2026-11-15 23:30 UTC is Monday 00:30 in Zagreb.
        let ts = jiff::Timestamp::from_second(1_794_785_400).unwrap();
        idx.insert_rows("f", &[row("a", ts.as_millisecond())])
            .unwrap();
        let get = |i: &Index| -> (String, u8, u8) {
            i.conn
                .query_row("SELECT day, hour, dow FROM requests", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap()
        };
        assert_eq!(get(&idx), ("2026-11-15".into(), 23, 6));
        let zg = TimeZone::get("Europe/Zagreb").unwrap();
        assert_eq!(idx.sync_time_zone(&zg).unwrap(), 1);
        assert_eq!(get(&idx), ("2026-11-16".into(), 0, 0));
        assert_eq!(idx.sync_time_zone(&zg).unwrap(), 0);
    }

    #[test]
    fn reader_sees_writes_and_delegates_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.sqlite");
        let w = Index::open(&p).unwrap();
        let mut r = w.reader().unwrap();
        assert!(r.is_read_only());
        assert!(r.conn.execute("DELETE FROM requests", []).is_err());
        assert!(r.insert_rows("f", &[row("a", 0)]).is_err(), "no writer");
        let w = Arc::new(Mutex::new(w));
        let mut r = r.attach_writer(w.clone());
        w.lock().unwrap().set_desktop_root(None);
        assert_eq!(r.insert_rows("f", &[row("a", 0)]).unwrap(), 1);
        assert_eq!(count(&r.conn).unwrap(), 1);
        r.refresh(dir.path(), &|_, _| {}).unwrap();
    }

    #[test]
    fn upsert_fills_missing_fields_only() {
        let mut idx = Index::open_in_memory().unwrap();
        let a = row("a", 0);
        assert_eq!(idx.insert_rows("f", std::slice::from_ref(&a)).unwrap(), 1);
        let mut b = a.clone();
        b.output = 999;
        b.stop_reason = Some("end_turn".into());
        assert_eq!(idx.insert_rows("f", &[b]).unwrap(), 0);
        let (out, sr): (i64, Option<String>) = idx
            .conn
            .query_row("SELECT output, stop_reason FROM requests", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((out, sr.as_deref()), (0, Some("end_turn")));
    }
}
