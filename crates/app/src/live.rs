//! Small read-only queries for the menu bar and notifications. They use only
//! the `requests` and `sessions` columns that every schema version has, and
//! stay cheap: recent rows are found by rowid (rows are appended as
//! transcripts grow), and per-session lookups use the session index.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Context drop that counts as a compaction: the next main-thread request
/// is less than half the previous one, and the previous one was at least
/// this large. Detected from sizes, not read from the transcript.
const COMPACTION_MIN_PREV: i64 = 50_000;

/// The session that most recently had a main-thread request indexed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
    pub id: String,
    pub title: String,
    pub project: String,
    /// Unix ms of its latest main-thread request.
    pub last_ts: i64,
    /// Exact prompt size of that request: input + cache write + cache read.
    pub context: u64,
    /// All requests in the session, subagents included.
    pub requests: u64,
    /// Context drops of more than half since local midnight (estimated).
    pub compactions_today: u64,
}

pub fn active_session(
    conn: &Connection,
    day_start_ms: i64,
) -> rusqlite::Result<Option<ActiveSession>> {
    let latest = conn
        .query_row(
            "SELECT session, ts, input + cache_write + cache_read, project
             FROM requests WHERE subagent = 0 ORDER BY rowid DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((id, last_ts, context, project)) = latest else {
        return Ok(None);
    };
    let title: Option<String> = conn
        .query_row("SELECT title FROM sessions WHERE id = ?1", [&id], |r| {
            r.get(0)
        })
        .optional()?;
    let requests: i64 = conn.query_row(
        "SELECT COUNT(*) FROM requests WHERE session = ?1",
        [&id],
        |r| r.get(0),
    )?;
    let compactions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
           SELECT input + cache_write + cache_read AS ctx,
                  LAG(input + cache_write + cache_read) OVER (ORDER BY ts) AS prev
           FROM requests WHERE session = ?1 AND subagent = 0 AND ts >= ?2)
         WHERE prev >= ?3 AND ctx * 2 < prev",
        params![id, day_start_ms, COMPACTION_MIN_PREV],
        |r| r.get(0),
    )?;
    let title = title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| short_project(&project).to_string());
    Ok(Some(ActiveSession {
        id,
        title,
        project,
        last_ts,
        context: context.max(0) as u64,
        requests: requests.max(0) as u64,
        compactions_today: compactions.max(0) as u64,
    }))
}

fn short_project(p: &str) -> &str {
    p.rsplit('/').find(|s| !s.is_empty()).unwrap_or(p)
}

pub fn max_rowid(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT IFNULL(MAX(rowid), 0) FROM requests", [], |r| {
        r.get(0)
    })
}

/// A main-thread request indexed after a given rowid.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRequest {
    pub rowid: i64,
    pub session: String,
    pub ts: i64,
    pub context: u64,
    pub cache_write: u64,
    /// Part of the cache write with the 1 hour lifetime.
    pub cache_write_1h: u64,
    /// Previous main-thread request in the same session, Unix ms.
    pub prev_ts: Option<i64>,
}

pub fn new_requests(conn: &Connection, after_rowid: i64) -> rusqlite::Result<Vec<NewRequest>> {
    let mut st = conn.prepare(
        "SELECT r.rowid, r.session, r.ts, r.input + r.cache_write + r.cache_read,
                r.cache_write, r.cw1h,
                (SELECT MAX(p.ts) FROM requests p
                 WHERE p.session = r.session AND p.subagent = 0 AND p.ts < r.ts)
         FROM requests r WHERE r.rowid > ?1 AND r.subagent = 0 ORDER BY r.rowid",
    )?;
    let rows = st.query_map([after_rowid], |r| {
        Ok(NewRequest {
            rowid: r.get(0)?,
            session: r.get(1)?,
            ts: r.get(2)?,
            context: r.get::<_, i64>(3)?.max(0) as u64,
            cache_write: r.get::<_, i64>(4)?.max(0) as u64,
            cache_write_1h: r.get::<_, i64>(5)?.max(0) as u64,
            prev_ts: r.get(6)?,
        })
    })?;
    rows.collect()
}

pub fn session_title(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .filter(|t| !t.trim().is_empty())
    .unwrap_or_else(|| {
        conn.query_row(
            "SELECT project FROM requests WHERE session = ?1 LIMIT 1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .map(|p| short_project(&p).to_string())
        .unwrap_or_else(|_| id.chars().take(8).collect())
    })
}

/// Median daily total (input + output + cache write + cache read) over the
/// days in `[from_ms, to_ms)` that had any usage, with `offset_s` the local
/// UTC offset used to cut days. None with fewer than 7 such days.
pub fn median_daily_total(
    conn: &Connection,
    from_ms: i64,
    to_ms: i64,
    offset_s: i64,
) -> rusqlite::Result<Option<u64>> {
    let mut st = conn.prepare(
        "SELECT SUM(input + output + cache_write + cache_read) AS t
         FROM requests WHERE ts >= ?1 AND ts < ?2
         GROUP BY (ts / 1000 + ?3) / 86400 ORDER BY t",
    )?;
    let v: Vec<i64> = st
        .query_map(params![from_ms, to_ms, offset_s], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(median(&v).map(|m| m.max(0) as u64))
}

fn median(sorted: &[i64]) -> Option<i64> {
    if sorted.len() < 7 {
        return None;
    }
    let n = sorted.len();
    Some(if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Only the columns these queries read, so the test does not depend on
    /// the full index schema.
    pub fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE requests(key TEXT PRIMARY KEY, session TEXT, ts INTEGER, project TEXT,
               subagent INTEGER, input INTEGER, output INTEGER, cache_write INTEGER,
               cw1h INTEGER, cache_read INTEGER);
             CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT NOT NULL);",
        )
        .unwrap();
        c
    }

    pub fn add(c: &Connection, session: &str, ts: i64, sub: bool, cw: i64, cr: i64) {
        let key = format!("{session}:{ts}:{sub}");
        c.execute(
            "INSERT INTO requests VALUES (?1, ?2, ?3, '/x/proj', ?4, 10, 5, ?5, 0, ?6)",
            params![key, session, ts, sub as i64, cw, cr],
        )
        .unwrap();
    }

    #[test]
    fn active_session_and_compactions() {
        let c = db();
        assert_eq!(active_session(&c, 0).unwrap(), None);
        add(&c, "a", 1_000, false, 0, 100);
        add(&c, "b", 2_000, false, 0, 400_000);
        add(&c, "b", 3_000, false, 0, 600_000);
        add(&c, "b", 4_000, false, 50_000, 0); // compaction
        add(&c, "b", 5_000, true, 0, 900_000); // subagent, ignored for context
        c.execute("INSERT INTO sessions VALUES ('b', 'Fix the tray')", [])
            .unwrap();
        let s = active_session(&c, 0).unwrap().unwrap();
        assert_eq!(s.id, "b");
        assert_eq!(s.title, "Fix the tray");
        assert_eq!(s.context, 50_010);
        assert_eq!(s.requests, 4);
        assert_eq!(s.compactions_today, 1);
        assert_eq!(
            active_session(&c, 3_500)
                .unwrap()
                .unwrap()
                .compactions_today,
            0
        );
        assert_eq!(session_title(&c, "a"), "proj");
    }

    #[test]
    fn new_requests_carry_previous_ts() {
        let c = db();
        add(&c, "a", 1_000, false, 0, 1);
        let mark = max_rowid(&c).unwrap();
        add(&c, "a", 900_000, false, 300_000, 0);
        add(&c, "a", 900_500, true, 0, 0);
        let v = new_requests(&c, mark).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].prev_ts, Some(1_000));
        assert_eq!(v[0].cache_write, 300_000);
    }

    #[test]
    fn median_needs_a_week() {
        let c = db();
        let day = 86_400_000;
        for d in 0..6 {
            add(&c, "a", d * day + 1, false, 0, 100 * (d + 1));
        }
        assert_eq!(median_daily_total(&c, 0, 30 * day, 0).unwrap(), None);
        add(&c, "a", 6 * day + 1, false, 0, 700);
        // Day totals are 115, 215, ..., 715; median is the 4th.
        assert_eq!(median_daily_total(&c, 0, 30 * day, 0).unwrap(), Some(415));
    }
}
