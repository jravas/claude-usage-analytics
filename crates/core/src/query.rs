//! Questions asked of the index.

use crate::{Error, Result};
use jiff::civil::Date;
use jiff::{Span, Zoned};
use rusqlite::types::Value;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Filter {
    /// First day, inclusive, `YYYY-MM-DD`.
    pub from: Option<String>,
    /// Last day, inclusive.
    pub to: Option<String>,
    pub weekdays_only: bool,
    /// Substring of the model id, such as "opus".
    pub model: Option<String>,
    pub project: Option<String>,
    pub session: Option<String>,
    pub subagent: Option<bool>,
    /// Only responses before this instant (Unix ms). Lets a partial day be
    /// compared with the same hours a week earlier.
    pub before_ts: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub thinking: u64,
    pub cache_write: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub web_search: u64,
    pub web_fetch: u64,
    /// input + output + cache write + cache read.
    pub total: u64,
    pub output_per_request: f64,
    /// Prompt size the model saw per request: input + cache write + cache read.
    pub context_per_request: f64,
    /// Share of the prompt served from cache.
    pub cache_hit_rate: f64,
    /// API-equivalent dollars, from the price table. Requests on models with
    /// no price add nothing here and are counted in `unpriced`.
    pub cost_usd: f64,
    /// Price-weighted tokens: what the requests cost in input-token units.
    pub weighted: f64,
    /// Requests with no price (unknown model, or not priced yet).
    pub unpriced: u64,
    pub cost_per_request: f64,
}

impl Totals {
    fn finish(mut self) -> Self {
        let ctx = self.input + self.cache_write + self.cache_read;
        self.total = ctx + self.output;
        if self.requests > 0 {
            self.output_per_request = self.output as f64 / self.requests as f64;
            self.context_per_request = ctx as f64 / self.requests as f64;
        }
        if ctx > 0 {
            self.cache_hit_rate = self.cache_read as f64 / ctx as f64;
        }
        let priced = self.requests.saturating_sub(self.unpriced);
        if priced > 0 {
            self.cost_per_request = self.cost_usd / priced as f64;
        }
        self
    }
}

const SUMS: &str = "COUNT(*), COALESCE(SUM(input),0), COALESCE(SUM(output),0), COALESCE(SUM(thinking),0),
 COALESCE(SUM(cache_write),0), COALESCE(SUM(cw5m),0), COALESCE(SUM(cw1h),0), COALESCE(SUM(cache_read),0),
 COALESCE(SUM(web_search),0), COALESCE(SUM(web_fetch),0),
 COALESCE(SUM(cost_usd),0), COALESCE(SUM(weighted),0), COALESCE(SUM(cost_usd IS NULL),0)";

fn totals_at(r: &rusqlite::Row, i: usize) -> rusqlite::Result<Totals> {
    let g = |k: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i + k)?.max(0) as u64) };
    Ok(Totals {
        requests: g(0)?,
        input: g(1)?,
        output: g(2)?,
        thinking: g(3)?,
        cache_write: g(4)?,
        cache_write_5m: g(5)?,
        cache_write_1h: g(6)?,
        cache_read: g(7)?,
        web_search: g(8)?,
        web_fetch: g(9)?,
        cost_usd: r.get::<_, f64>(i + 10)?,
        weighted: r.get::<_, f64>(i + 11)?,
        unpriced: g(12)?,
        ..Default::default()
    }
    .finish())
}

/// Escape `%`, `_` and the escape character itself for `LIKE … ESCAPE '\'`.
fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(ch);
    }
    o
}

fn where_clause(f: &Filter) -> (String, Vec<Value>) {
    let mut c = Vec::new();
    let mut v = Vec::new();
    if let Some(x) = &f.from {
        c.push("day >= ?");
        v.push(Value::Text(x.clone()));
    }
    if let Some(x) = &f.to {
        c.push("day <= ?");
        v.push(Value::Text(x.clone()));
    }
    if f.weekdays_only {
        c.push("dow < 5");
    }
    if let Some(x) = f.model.as_ref().filter(|s| !s.is_empty()) {
        c.push("model LIKE '%' || ? || '%' ESCAPE '\\'");
        v.push(Value::Text(like_escape(x)));
    }
    if let Some(x) = f.project.as_ref().filter(|s| !s.is_empty()) {
        c.push("project = ?");
        v.push(Value::Text(x.clone()));
    }
    if let Some(x) = f.session.as_ref().filter(|s| !s.is_empty()) {
        // A resumed Desktop session is one session: its earlier CLI ids too.
        c.push(
            "(session = ? OR session IN (SELECT cli_session_id FROM session_links WHERE canonical = ?))",
        );
        v.push(Value::Text(x.clone()));
        v.push(Value::Text(x.clone()));
    }
    if let Some(x) = f.subagent {
        c.push("subagent = ?");
        v.push(Value::Integer(x as i64));
    }
    if let Some(x) = f.before_ts {
        c.push("ts < ?");
        v.push(Value::Integer(x));
    }
    let w = if c.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", c.join(" AND "))
    };
    (w, v)
}

pub fn totals(conn: &Connection, f: &Filter) -> Result<Totals> {
    let (w, v) = where_clause(f);
    let sql = format!("SELECT {SUMS} FROM requests{w}");
    Ok(conn.query_row(&sql, rusqlite::params_from_iter(v), |r| totals_at(r, 0))?)
}

pub fn today() -> Date {
    Zoned::now().date()
}

fn date(s: &str) -> Result<Date> {
    s.parse()
        .map_err(|e| Error::Other(format!("bad date {s:?}: {e}")))
}

fn add_days(d: Date, n: i64) -> Date {
    d.checked_add(Span::new().days(n)).unwrap_or(d)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayRow {
    pub day: String,
    /// "Mon" … "Sun".
    pub weekday: String,
    pub totals: Totals,
}

const WD: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// One row per day from `from` to `to`, including days with no usage.
/// Without `from`/`to` it covers the 14 days ending today.
pub fn daily(conn: &Connection, f: &Filter) -> Result<Vec<DayRow>> {
    let to = match &f.to {
        Some(s) => date(s)?,
        None => today(),
    };
    let from = match &f.from {
        Some(s) => date(s)?,
        None => add_days(to, -13),
    };
    let mut f = f.clone();
    f.from = Some(from.to_string());
    f.to = Some(to.to_string());
    let (w, v) = where_clause(&f);
    let sql = format!("SELECT day, {SUMS} FROM requests{w} GROUP BY day");
    let mut st = conn.prepare(&sql)?;
    let found: std::collections::HashMap<String, Totals> = st
        .query_map(rusqlite::params_from_iter(v), |r| {
            Ok((r.get::<_, String>(0)?, totals_at(r, 1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    let mut d = from;
    while d <= to {
        let dow = d.weekday().to_monday_zero_offset() as usize;
        if !(f.weekdays_only && dow >= 5) {
            let key = d.to_string();
            out.push(DayRow {
                totals: found.get(&key).cloned().unwrap_or_default(),
                day: key,
                weekday: WD[dow].to_string(),
            });
        }
        d = add_days(d, 1);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Dim {
    Model,
    Project,
    /// A resumed Claude Desktop session (several CLI session ids) is one
    /// group, keyed by its current id.
    Session,
    Effort,
    /// Main thread or subagent.
    Agent,
    Hour,
    Weekday,
    Entrypoint,
    /// Skill the response ran under (`attributionSkill`); key "" for none.
    Skill,
    /// Plugin the skill came from (`attributionPlugin`); key "" for none.
    Plugin,
    /// Git branch within its project. Key is `project` + U+001F + `branch`
    /// (branch "" when unknown); label is the branch, detail the project.
    Branch,
    /// Requested model against the one that answered. Key "same",
    /// "unknown" (older transcripts), or `requested → actual`.
    #[serde(rename = "modelFallback", alias = "modelfallback", alias = "fallback")]
    ModelFallback,
}

/// Separates project and branch in a `Dim::Branch` key.
pub const BRANCH_SEP: char = '\u{1f}';

impl Dim {
    fn expr(self) -> &'static str {
        match self {
            Dim::Model => "model",
            Dim::Project => "project",
            Dim::Session => "session",
            Dim::Effort => "COALESCE(effort, 'unknown')",
            Dim::Agent => "CASE subagent WHEN 1 THEN 'subagent' ELSE 'main' END",
            Dim::Hour => "printf('%02d', hour)",
            Dim::Weekday => "CAST(dow AS TEXT)",
            Dim::Entrypoint => "COALESCE(entrypoint, 'unknown')",
            Dim::Skill => "COALESCE(attribution_skill, '')",
            Dim::Plugin => "COALESCE(attribution_plugin, '')",
            Dim::Branch => "project || char(31) || COALESCE(branch, '')",
            // `[1m]`-style suffixes on the requested id are not a fallback.
            Dim::ModelFallback => {
                "CASE WHEN requested_model IS NULL THEN 'unknown'
                      WHEN requested_model = model OR requested_model LIKE model || '[%' THEN 'same'
                      ELSE requested_model || ' → ' || model END"
            }
        }
    }

    fn ordered_by_key(self) -> bool {
        matches!(self, Dim::Hour | Dim::Weekday)
    }

    pub fn parse(s: &str) -> Option<Dim> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub key: String,
    pub label: String,
    /// Secondary line: the full project path, or a session's project.
    pub detail: Option<String>,
    pub totals: Totals,
    /// Share of all tokens in the filtered range.
    pub share: f64,
}

fn base_name(p: &str) -> String {
    p.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

pub fn breakdown(conn: &Connection, f: &Filter, dim: Dim, limit: usize) -> Result<Vec<Group>> {
    let (w, v) = where_clause(f);
    let order = if dim.ordered_by_key() {
        "k ASC".to_string()
    } else {
        "SUM(input) + SUM(output) + SUM(cache_write) + SUM(cache_read) DESC".to_string()
    };
    let (e, links) = if dim == Dim::Session {
        (
            "COALESCE(session_links.canonical, requests.session)",
            " LEFT JOIN session_links ON session_links.cli_session_id = requests.session",
        )
    } else {
        (dim.expr(), "")
    };
    let sql = format!(
        "SELECT {e} AS k, MAX(project), MAX(sessions.title), {SUMS}
         FROM requests LEFT JOIN sessions ON sessions.id = requests.session{links}{w}
         GROUP BY k ORDER BY {order} LIMIT {limit}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(v), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            totals_at(r, 3)?,
        ))
    })?;
    let all = totals(conn, f)?.total.max(1) as f64;
    let mut out = Vec::new();
    for row in rows {
        let (key, project, title, t) = row?;
        let (label, detail) = match dim {
            Dim::Project => (base_name(&key), Some(key.clone())),
            Dim::Session => (
                title.unwrap_or_else(|| key.chars().take(8).collect()),
                project.map(|p| base_name(&p)),
            ),
            Dim::Hour => (format!("{key}:00"), None),
            Dim::Weekday => (
                key.parse::<usize>()
                    .ok()
                    .and_then(|i| WD.get(i))
                    .unwrap_or(&"?")
                    .to_string(),
                None,
            ),
            Dim::Skill | Dim::Plugin if key.is_empty() => ("(none)".to_string(), None),
            Dim::ModelFallback if key == "same" => ("as requested".to_string(), None),
            Dim::ModelFallback if key == "unknown" => ("not recorded".to_string(), None),
            Dim::Branch => {
                let (p, b) = key.split_once(BRANCH_SEP).unwrap_or((&key, ""));
                let b = if b.is_empty() { "(no branch)" } else { b };
                (b.to_string(), Some(base_name(p)))
            }
            _ => (key.clone(), None),
        };
        out.push(Group {
            share: t.total as f64 / all,
            key,
            label,
            detail,
            totals: t,
        });
    }
    Ok(out)
}

/// Models seen, most used first.
pub fn models(conn: &Connection) -> Result<Vec<String>> {
    let mut st =
        conn.prepare("SELECT model FROM requests GROUP BY model ORDER BY COUNT(*) DESC")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// First and last day in the index.
pub fn span(conn: &Connection) -> Result<Option<(String, String)>> {
    Ok(
        conn.query_row("SELECT MIN(day), MAX(day) FROM requests", [], |r| {
            Ok(
                match (
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                },
            )
        })?,
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub today: String,
    /// Local time the comparison is cut at, `HH:MM`.
    pub as_of: String,
    pub today_totals: Totals,
    /// Same weekday a week ago, up to the same time of day.
    pub same_day_last_week: Totals,
    /// Monday through now.
    pub this_week: Totals,
    /// Last Monday through the same weekday and time of day, last week. The
    /// fair comparison for a week still in progress.
    pub last_week_to_date: Totals,
    /// All of last week.
    pub last_week: Totals,
    pub week_start: String,
}

/// This week against last week as of `now`, using `f`'s model, project and
/// weekday filters; its dates are ignored.
pub fn overview(conn: &Connection, f: &Filter, now: &Zoned) -> Result<Overview> {
    let today = now.date();
    let dow = today.weekday().to_monday_zero_offset() as i64;
    let monday = add_days(today, -dow);
    let week_ago = now
        .checked_sub(Span::new().days(7))
        .map_err(|e| Error::Other(e.to_string()))?
        .timestamp()
        .as_millisecond();
    let range = |a: Date, b: Date, before: Option<i64>| -> Result<Totals> {
        let mut g = f.clone();
        g.from = Some(a.to_string());
        g.to = Some(b.to_string());
        g.before_ts = before;
        totals(conn, &g)
    };
    let (lw_today, lw_monday) = (add_days(today, -7), add_days(monday, -7));
    Ok(Overview {
        today: today.to_string(),
        as_of: format!("{:02}:{:02}", now.hour(), now.minute()),
        today_totals: range(today, today, None)?,
        same_day_last_week: range(lw_today, lw_today, Some(week_ago))?,
        this_week: range(monday, today, None)?,
        last_week_to_date: range(lw_monday, lw_today, Some(week_ago))?,
        last_week: range(lw_monday, add_days(monday, -1), None)?,
        week_start: monday.to_string(),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeBucket {
    pub label: String,
    /// Lower bound, inclusive.
    pub min: u64,
    /// Upper bound, inclusive; `None` for the last bucket.
    pub max: Option<u64>,
    pub sessions: u64,
    pub totals: Totals,
    /// Share of the sessions in range.
    pub session_share: f64,
    /// Share of all tokens in range.
    pub share: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSizes {
    pub sessions: u64,
    /// Bucketed by requests per session.
    pub by_requests: Vec<SizeBucket>,
    /// Bucketed by the largest context (input + cache write + cache read) a
    /// main-thread request in the session reached.
    pub by_peak_context: Vec<SizeBucket>,
}

/// Upper bounds of the request-count buckets.
pub const REQUEST_BUCKETS: &[u64] = &[10, 50, 200, 500, 1000];
/// Upper bounds of the peak-context buckets.
pub const PEAK_CONTEXT_BUCKETS: &[u64] = &[50_000, 100_000, 200_000, 500_000, 1_000_000];

/// 50_000 → "50k", 1_000_000 → "1M".
fn round_tokens(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 && n.is_multiple_of(1_000) {
        format!("{}k", n / 1_000)
    } else {
        crate::fmt::tokens(n)
    }
}

/// Buckets with upper bounds `bounds` and one open-ended bucket after them.
/// Request buckets are inclusive ("1–10", "11–50", …); token buckets are
/// half-open ("under 50k", "50k–100k", …).
fn buckets(bounds: &[u64], tokens: bool) -> Vec<SizeBucket> {
    let mut out = Vec::new();
    let mut lo = if tokens { 0 } else { 1 };
    for hi in bounds.iter().copied().map(Some).chain([None]) {
        let max = match (hi, tokens) {
            (Some(h), true) => h - 1,
            (Some(h), false) => h,
            (None, _) => u64::MAX,
        };
        let label = match (hi, tokens) {
            (Some(h), true) if lo == 0 => format!("under {}", round_tokens(h)),
            (Some(h), true) => format!("{}–{}", round_tokens(lo), round_tokens(h)),
            (Some(h), false) => format!("{lo}–{h}"),
            (None, true) => format!("{}+", round_tokens(lo)),
            (None, false) => format!("{lo}+"),
        };
        out.push(SizeBucket {
            label,
            min: lo,
            max: hi.map(|_| max),
            sessions: 0,
            totals: Totals::default(),
            session_share: 0.0,
            share: 0.0,
        });
        lo = max.saturating_add(1);
    }
    out
}

fn add_totals(a: &mut Totals, b: &Totals) {
    a.requests += b.requests;
    a.input += b.input;
    a.output += b.output;
    a.thinking += b.thinking;
    a.cache_write += b.cache_write;
    a.cache_write_5m += b.cache_write_5m;
    a.cache_write_1h += b.cache_write_1h;
    a.cache_read += b.cache_read;
    a.web_search += b.web_search;
    a.web_fetch += b.web_fetch;
}

/// How much of the usage in range comes from small and large sessions.
/// Sessions are counted within the filtered range only, with subagent
/// requests included; a resumed Desktop session counts once.
pub fn session_size_buckets(conn: &Connection, f: &Filter) -> Result<SessionSizes> {
    let (w, v) = where_clause(f);
    let sql = format!(
        "SELECT COALESCE(MAX(CASE WHEN subagent = 0 THEN input + cache_write + cache_read END),
                         MAX(input + cache_write + cache_read)), {SUMS}
         FROM requests LEFT JOIN session_links ON session_links.cli_session_id = requests.session{w}
         GROUP BY COALESCE(session_links.canonical, requests.session)"
    );
    let mut by_req = buckets(REQUEST_BUCKETS, false);
    let mut by_ctx = buckets(PEAK_CONTEXT_BUCKETS, true);
    let mut n = 0u64;
    let mut all = Totals::default();
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(v), |r| {
        Ok((r.get::<_, i64>(0)?.max(0) as u64, totals_at(r, 1)?))
    })?;
    let place = |bs: &mut Vec<SizeBucket>, x: u64, t: &Totals| {
        if let Some(b) = bs.iter_mut().find(|b| b.max.is_none_or(|m| x <= m)) {
            b.sessions += 1;
            add_totals(&mut b.totals, t);
        }
    };
    for r in rows {
        let (peak, t) = r?;
        n += 1;
        place(&mut by_req, t.requests, &t);
        place(&mut by_ctx, peak, &t);
        add_totals(&mut all, &t);
    }
    let total = all.finish().total.max(1) as f64;
    for b in by_req.iter_mut().chain(by_ctx.iter_mut()) {
        b.totals = std::mem::take(&mut b.totals).finish();
        b.share = b.totals.total as f64 / total;
        b.session_share = b.sessions as f64 / n.max(1) as f64;
    }
    Ok(SessionSizes {
        sessions: n,
        by_requests: by_req,
        by_peak_context: by_ctx,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheDay {
    pub day: String,
    pub requests: u64,
    /// Uncached input tokens.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub write_5m: u64,
    pub write_1h: u64,
    /// Cache writes not split by lifetime (older transcripts).
    pub write_unsplit: u64,
    /// cache read / (input + cache write + cache read).
    pub hit_rate: f64,
    /// Share of split cache writes that used the 1-hour lifetime.
    pub share_1h: f64,
}

impl CacheDay {
    fn from_totals(day: String, t: &Totals) -> Self {
        let split = t.cache_write_5m + t.cache_write_1h;
        CacheDay {
            day,
            requests: t.requests,
            input: t.input,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            write_5m: t.cache_write_5m,
            write_1h: t.cache_write_1h,
            write_unsplit: t.cache_write.saturating_sub(split),
            hit_rate: t.cache_hit_rate,
            share_1h: if split > 0 {
                t.cache_write_1h as f64 / split as f64
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    /// One row per day, as `daily` returns them.
    pub days: Vec<CacheDay>,
    /// The whole range; `day` is "".
    pub total: CacheDay,
}

/// Cache hit rate and 5-minute against 1-hour writes, per day.
pub fn cache_stats(conn: &Connection, f: &Filter) -> Result<CacheStats> {
    let rows = daily(conn, f)?;
    let mut all = Totals::default();
    let days = rows
        .iter()
        .map(|r| {
            add_totals(&mut all, &r.totals);
            CacheDay::from_totals(r.day.clone(), &r.totals)
        })
        .collect();
    Ok(CacheStats {
        days,
        total: CacheDay::from_totals(String::new(), &all.finish()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Row;
    use crate::Index;

    fn row(key: &str, day: &str, dow: u8, model: &str, out: u64, read: u64) -> Row {
        Row {
            key: key.into(),
            session: "s".into(),
            day: day.into(),
            dow,
            model: model.into(),
            project: "/r/app".into(),
            cwd: "/r/app".into(),
            output: out,
            cache_read: read,
            input: 1,
            ..Default::default()
        }
    }

    fn idx() -> Index {
        let mut i = Index::open_in_memory().unwrap();
        i.insert_rows(
            "f",
            &[
                row("a", "2026-11-09", 0, "claude-opus-5-5", 100, 1000), // Mon last week
                row("b", "2026-11-14", 5, "claude-opus-5-5", 50, 500),   // Sat
                row("c", "2026-11-16", 0, "claude-opus-5-5", 10, 100),   // Mon this week
                row("d", "2026-11-16", 0, "claude-sonnet-5-5", 7, 70),
                row("d", "2026-11-16", 0, "claude-sonnet-5-5", 7, 70), // duplicate key
            ],
        )
        .unwrap();
        i
    }

    #[test]
    fn totals_and_filters() {
        let i = idx();
        let all = totals(&i.conn, &Filter::default()).unwrap();
        assert_eq!(all.requests, 4);
        let opus = Filter {
            model: Some("opus".into()),
            weekdays_only: true,
            ..Default::default()
        };
        let t = totals(&i.conn, &opus).unwrap();
        assert_eq!(t.requests, 2);
        assert_eq!(t.output, 110);
        assert_eq!(t.total, 110 + 1100 + 2);
    }

    #[test]
    fn daily_fills_gaps_and_skips_weekends() {
        let i = idx();
        let f = Filter {
            from: Some("2026-11-09".into()),
            to: Some("2026-11-16".into()),
            weekdays_only: true,
            ..Default::default()
        };
        let d = daily(&i.conn, &f).unwrap();
        assert_eq!(d.len(), 6);
        assert_eq!(d[0].totals.requests, 1);
        assert_eq!(d[1].totals.requests, 0);
        assert_eq!(d[5].day, "2026-11-16");
        assert_eq!(d[5].weekday, "Mon");
    }

    #[test]
    fn breakdown_by_model() {
        let i = idx();
        let g = breakdown(&i.conn, &Filter::default(), Dim::Model, 10).unwrap();
        assert_eq!(g[0].key, "claude-opus-5-5");
        assert!((g.iter().map(|x| x.share).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn overview_compares_weeks() {
        let i = idx();
        let now: Zoned = "2026-11-16T23:00[UTC]".parse().unwrap();
        let o = overview(&i.conn, &Filter::default(), &now).unwrap();
        assert_eq!(o.week_start, "2026-11-16");
        assert_eq!(o.today_totals.requests, 2);
        assert_eq!(o.same_day_last_week.requests, 1);
        assert_eq!(o.last_week.requests, 2);
        assert_eq!(o.last_week_to_date.requests, 1);
    }

    #[test]
    fn model_filter_escapes_like_wildcards() {
        let mut i = Index::open_in_memory().unwrap();
        i.insert_rows(
            "f",
            &[
                row("a", "2026-11-16", 0, "claude-opus-5-5", 1, 1),
                row("b", "2026-11-16", 0, "odd_model%x", 1, 1),
            ],
        )
        .unwrap();
        let t = |m: &str| {
            let f = Filter {
                model: Some(m.into()),
                ..Default::default()
            };
            totals(&i.conn, &f).unwrap().requests
        };
        assert_eq!(t("_"), 1, "underscore matches itself only");
        assert_eq!(t("%"), 1);
        assert_eq!(t("d_m"), 1);
        assert_eq!(t("opus"), 1);
        assert_eq!(t("o_us"), 0);
        assert_eq!(t("\\"), 0);
    }

    fn rich(key: &str, session: &str, n: u64) -> Row {
        Row {
            session: session.into(),
            ..row(key, "2026-11-16", 0, "claude-opus-5-5", n, n * 10)
        }
    }

    #[test]
    fn skill_plugin_branch_and_fallback_dims() {
        let mut i = Index::open_in_memory().unwrap();
        let mut a = rich("a", "s1", 10);
        a.attribution_skill = Some("anthropic-skills:pdf".into());
        a.attribution_plugin = Some("anthropic-skills".into());
        a.branch = Some("feat/x".into());
        a.requested_model = Some("claude-opus-5-5[1m]".into());
        let mut b = rich("b", "s1", 20);
        b.branch = Some("main".into());
        b.requested_model = Some("claude-opus-5-5".into());
        b.model = "claude-sonnet-5-5".into();
        let mut c = rich("c", "s2", 30);
        c.project = "/r/other".into();
        c.branch = Some("main".into());
        i.insert_rows("f", &[a, b, c]).unwrap();
        let f = Filter::default();

        let g = breakdown(&i.conn, &f, Dim::Skill, 10).unwrap();
        let pdf = g.iter().find(|x| x.key == "anthropic-skills:pdf").unwrap();
        assert_eq!(pdf.totals.requests, 1);
        let none = g.iter().find(|x| x.key.is_empty()).unwrap();
        assert_eq!((none.label.as_str(), none.totals.requests), ("(none)", 2));

        let g = breakdown(&i.conn, &f, Dim::Plugin, 10).unwrap();
        assert_eq!(g.len(), 2);
        assert!(g.iter().any(|x| x.key == "anthropic-skills"));

        // "main" in two projects stays two groups.
        let g = breakdown(&i.conn, &f, Dim::Branch, 10).unwrap();
        assert_eq!(g.len(), 3);
        let m: Vec<_> = g.iter().filter(|x| x.label == "main").collect();
        assert_eq!(m.len(), 2);
        assert_eq!(g[0].detail.as_deref(), Some("other"));
        assert_eq!(g[0].key, format!("/r/other{BRANCH_SEP}main"));

        let g = breakdown(&i.conn, &f, Dim::ModelFallback, 10).unwrap();
        let keys: Vec<_> = g
            .iter()
            .map(|x| (x.key.as_str(), x.totals.requests))
            .collect();
        assert!(keys.contains(&("unknown", 1)));
        assert!(keys.contains(&("same", 1)), "[1m] suffix is not a fallback");
        assert!(keys.contains(&("claude-opus-5-5 → claude-sonnet-5-5", 1)));
        assert_eq!(Dim::parse("modelFallback"), Some(Dim::ModelFallback));
        assert_eq!(Dim::parse("branch"), Some(Dim::Branch));
        assert_eq!(
            serde_json::to_string(&Dim::ModelFallback).unwrap(),
            "\"modelFallback\""
        );
    }

    #[test]
    fn resumed_session_is_one_group() {
        let mut i = Index::open_in_memory().unwrap();
        i.insert_rows("f", &[rich("a", "old", 1), rich("b", "new", 2)])
            .unwrap();
        i.conn
            .execute_batch(
                "INSERT INTO session_links VALUES ('new','new','local_1'), ('old','new','local_1');
                 INSERT INTO sessions VALUES ('new','Desktop title'), ('old','Desktop title');",
            )
            .unwrap();
        let g = breakdown(&i.conn, &Filter::default(), Dim::Session, 10).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(
            (g[0].key.as_str(), g[0].label.as_str(), g[0].totals.requests),
            ("new", "Desktop title", 2)
        );
        let f = Filter {
            session: Some("new".into()),
            ..Default::default()
        };
        assert_eq!(totals(&i.conn, &f).unwrap().requests, 2);
    }

    #[test]
    fn session_size_buckets_split_cost() {
        let mut i = Index::open_in_memory().unwrap();
        let mut rows = Vec::new();
        // s-big: 60 requests at 300k context; s-small: 2 requests at 20k.
        for n in 0..60 {
            let mut r = rich(&format!("b{n}"), "s-big", 100);
            r.cache_read = 300_000;
            rows.push(r);
        }
        for n in 0..2 {
            let mut r = rich(&format!("s{n}"), "s-small", 100);
            r.cache_read = 20_000;
            rows.push(r);
        }
        // A subagent with a larger context does not set the peak.
        let mut r = rich("sub", "s-small", 1);
        r.subagent = true;
        r.cache_read = 900_000;
        rows.push(r);
        i.insert_rows("f", &rows).unwrap();

        let s = session_size_buckets(&i.conn, &Filter::default()).unwrap();
        assert_eq!(s.sessions, 2);
        let labels: Vec<_> = s.by_requests.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(
            labels,
            ["1–10", "11–50", "51–200", "201–500", "501–1000", "1001+"]
        );
        let labels: Vec<_> = s.by_peak_context.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "under 50k",
                "50k–100k",
                "100k–200k",
                "200k–500k",
                "500k–1M",
                "1M+"
            ]
        );
        assert_eq!(s.by_requests[0].sessions, 1);
        assert_eq!(s.by_requests[0].totals.requests, 3);
        assert_eq!(s.by_requests[2].sessions, 1);
        assert_eq!(s.by_peak_context[0].sessions, 1, "subagent ignored");
        assert_eq!(s.by_peak_context[3].sessions, 1);
        let share: f64 = s.by_requests.iter().map(|b| b.share).sum();
        assert!((share - 1.0).abs() < 1e-9);
        assert!(s.by_requests[2].share > 0.9);
        assert!((s.by_requests[2].session_share - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cache_stats_per_day() {
        let mut i = Index::open_in_memory().unwrap();
        let mut a = row("a", "2026-11-15", 6, "m", 1, 300);
        a.cache_write = 100;
        a.cw5m = 25;
        a.cw1h = 75;
        let mut b = row("b", "2026-11-16", 0, "m", 1, 0);
        b.cache_write = 40; // not split by lifetime
        i.insert_rows("f", &[a, b]).unwrap();
        let f = Filter {
            from: Some("2026-11-14".into()),
            to: Some("2026-11-16".into()),
            ..Default::default()
        };
        let c = cache_stats(&i.conn, &f).unwrap();
        assert_eq!(c.days.len(), 3);
        assert_eq!(c.days[0].requests, 0);
        let d = &c.days[1];
        assert_eq!((d.write_5m, d.write_1h, d.write_unsplit), (25, 75, 0));
        assert!((d.hit_rate - 300.0 / 401.0).abs() < 1e-9);
        assert!((d.share_1h - 0.75).abs() < 1e-9);
        assert_eq!(c.days[2].write_unsplit, 40);
        assert_eq!(c.total.cache_write, 140);
        assert_eq!(c.total.requests, 2);
        assert!((c.total.hit_rate - 300.0 / 442.0).abs() < 1e-9);
    }

    #[test]
    fn reader_answers_queries() {
        let i = idx();
        let r = i.reader().unwrap();
        assert_eq!(totals(&r.conn, &Filter::default()).unwrap().requests, 4);
    }
}
