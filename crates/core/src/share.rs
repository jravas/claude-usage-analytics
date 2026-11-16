//! Sharing usage without sharing private text.
//!
//! A *summary* is a small versioned JSON file a person can send to a
//! colleague: per-day totals by model family, per-request metrics, weekday
//! and hour shares and a few group shares. It never contains paths, project
//! names, session ids or titles, branches, prompts or tool arguments.
//! Project rows are left out unless asked for, and then carry a salted hash
//! instead of the name unless the user opts in to names.
//!
//! [`export_summary`] builds one from the index, [`load_summaries`] reads
//! them back and [`compare`] lines people up week by week. [`table`] exports
//! the existing daily and breakdown tables as CSV or JSON for local use (those
//! do contain names; they are not meant for sharing).

use crate::query::{self, DayRow, Dim, Filter, Group, Totals};
use crate::{Error, Result};
use jiff::civil::Date;
use jiff::Span;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Schema id written into every summary. The number after the slash is the
/// major version; readers reject other majors and ignore unknown fields.
pub const SCHEMA: &str = "claude-usage-summary/1";
const SCHEMA_PREFIX: &str = "claude-usage-summary/";
const SCHEMA_MAJOR: u32 = 1;

/// Model families a summary reports. Anything else is "other", so a custom
/// or internal model id never leaves the machine.
pub const FAMILIES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

pub fn family(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    FAMILIES
        .iter()
        .find(|f| m.contains(*f))
        .copied()
        .unwrap_or("other")
}

/// Token counts and per-request ratios. Like [`Totals`], but readable back
/// from a file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Metrics {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub thinking: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub web_search: u64,
    pub web_fetch: u64,
    /// input + output + cache write + cache read.
    pub total: u64,
    pub output_per_request: f64,
    /// Prompt size per request: input + cache write + cache read.
    pub context_per_request: f64,
    /// Share of the prompt served from cache.
    pub cache_hit_rate: f64,
}

impl Metrics {
    pub fn add(&mut self, o: &Metrics) {
        self.requests += o.requests;
        self.input += o.input;
        self.output += o.output;
        self.thinking += o.thinking;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
        self.web_search += o.web_search;
        self.web_fetch += o.web_fetch;
        self.finish();
    }

    /// Recompute `total` and the ratios from the counts.
    pub fn finish(&mut self) {
        let ctx = self.input + self.cache_write + self.cache_read;
        self.total = ctx + self.output;
        let (mut opr, mut cpr, mut hit) = (0.0, 0.0, 0.0);
        if self.requests > 0 {
            opr = self.output as f64 / self.requests as f64;
            cpr = ctx as f64 / self.requests as f64;
        }
        if ctx > 0 {
            hit = self.cache_read as f64 / ctx as f64;
        }
        self.output_per_request = round(opr, 1);
        self.context_per_request = round(cpr, 1);
        self.cache_hit_rate = round(hit, 4);
    }
}

impl From<&Totals> for Metrics {
    fn from(t: &Totals) -> Self {
        let mut m = Metrics {
            requests: t.requests,
            input: t.input,
            output: t.output,
            thinking: t.thinking,
            cache_write: t.cache_write,
            cache_read: t.cache_read,
            web_search: t.web_search,
            web_fetch: t.web_fetch,
            ..Default::default()
        };
        m.finish();
        m
    }
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySummary {
    /// Local calendar day, `YYYY-MM-DD`.
    pub day: String,
    pub totals: Metrics,
    /// Keyed by [`family`].
    #[serde(default)]
    pub by_family: BTreeMap<String, Metrics>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    pub key: String,
    pub requests: u64,
    /// Share of all tokens in the range.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    /// Salted hash of the project, stable across one person's exports.
    pub id: String,
    /// The folder name, only when the user opted in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub totals: Metrics,
    pub share: f64,
}

/// What one person shares. See the module docs for what is left out.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub schema: String,
    pub app_version: String,
    /// UTC, to the second.
    pub generated_at: String,
    /// Display name the user chose, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// First and last day covered, inclusive. Days in between that are
    /// missing from `days` had no usage.
    pub from: String,
    pub to: String,
    pub totals: Metrics,
    /// Days with usage, oldest first.
    pub days: Vec<DaySummary>,
    /// Share of tokens by weekday, Monday first (7 values).
    #[serde(default)]
    pub weekday_share: Vec<f64>,
    /// Share of tokens by local hour, 0 to 23 (24 values).
    #[serde(default)]
    pub hour_share: Vec<f64>,
    /// Group shares by dimension: `family`, `agent`, `effort`, `entrypoint`.
    #[serde(default)]
    pub groups: BTreeMap<String, Vec<Share>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<ProjectSummary>>,
}

/// Whether and how project rows go into a summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Projects {
    /// No project rows (default).
    #[default]
    None,
    /// One row per project, identified by a salted hash.
    Hashed { salt: Vec<u8> },
    /// Hash plus the project's folder name (never the full path).
    Named { salt: Vec<u8> },
}

#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// First day, inclusive, `YYYY-MM-DD`.
    pub from: String,
    /// Last day, inclusive.
    pub to: String,
    pub name: Option<String>,
    pub projects: Projects,
}

/// Monday of the week `weeks_back` weeks before the current one, through
/// today: the default export range (`weeks_back` = 4 gives five weeks).
pub fn default_range(today: Date, weeks_back: i64) -> (String, String) {
    let monday = monday_of(today);
    (
        add_days(monday, -7 * weeks_back).to_string(),
        today.to_string(),
    )
}

fn monday_of(d: Date) -> Date {
    add_days(d, -(d.weekday().to_monday_zero_offset() as i64))
}

fn add_days(d: Date, n: i64) -> Date {
    d.checked_add(Span::new().days(n)).unwrap_or(d)
}

fn date(s: &str) -> Result<Date> {
    s.parse()
        .map_err(|e| Error::Other(format!("bad date {s:?}: {e}")))
}

const EFFORTS: [&str; 7] = ["low", "medium", "high", "xhigh", "max", "none", "unknown"];

/// Entrypoints Claude Code is known to write. The field is free text in the
/// transcript, so anything else is reported as "other".
const ENTRYPOINTS: [&str; 12] = [
    "cli",
    "claude-desktop",
    "claude-vscode",
    "claude-jetbrains",
    "sdk-cli",
    "sdk-ts",
    "sdk-py",
    "mcp",
    "remote",
    "github-action",
    "claude-code-github-action",
    "unknown",
];

fn hash_project(salt: &[u8], project: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update([0u8]);
    h.update(project.as_bytes());
    h.finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn base_name(p: &str) -> String {
    p.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

/// Build a summary of `opts.from..=opts.to` from the index. The result is
/// checked against every path, project, branch, session id and title in the
/// index before it is returned; a match is an error, never a silent leak.
pub fn export_summary(conn: &Connection, opts: &ExportOptions) -> Result<Summary> {
    let (from, to) = (date(&opts.from)?, date(&opts.to)?);
    if from > to {
        return Err(Error::Other(format!("range {from}..{to} is empty")));
    }
    let f = Filter {
        from: Some(from.to_string()),
        to: Some(to.to_string()),
        ..Default::default()
    };
    let private = Private::load(conn)?;

    // Per day and model, folded into families.
    let mut st = conn.prepare(
        "SELECT day, model, COUNT(*), COALESCE(SUM(input),0), COALESCE(SUM(output),0),
                COALESCE(SUM(thinking),0), COALESCE(SUM(cache_write),0), COALESCE(SUM(cache_read),0),
                COALESCE(SUM(web_search),0), COALESCE(SUM(web_fetch),0)
         FROM requests WHERE day >= ?1 AND day <= ?2 GROUP BY day, model ORDER BY day",
    )?;
    let rows = st.query_map([from.to_string(), to.to_string()], |r| {
        let g = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            Metrics {
                requests: g(2)?,
                input: g(3)?,
                output: g(4)?,
                thinking: g(5)?,
                cache_write: g(6)?,
                cache_read: g(7)?,
                web_search: g(8)?,
                web_fetch: g(9)?,
                ..Default::default()
            },
        ))
    })?;
    let mut days: BTreeMap<String, DaySummary> = BTreeMap::new();
    let mut totals = Metrics::default();
    for row in rows {
        let (day, model, m) = row?;
        totals.add(&m);
        let d = days.entry(day.clone()).or_insert_with(|| DaySummary {
            day,
            totals: Metrics::default(),
            by_family: BTreeMap::new(),
        });
        d.totals.add(&m);
        d.by_family
            .entry(family(&model).to_string())
            .or_default()
            .add(&m);
    }
    let all = totals.total.max(1) as f64;

    let mut weekday_share = vec![0.0; 7];
    for g in query::breakdown(conn, &f, Dim::Weekday, 7)? {
        if let Some(x) = g
            .key
            .parse::<usize>()
            .ok()
            .and_then(|i| weekday_share.get_mut(i))
        {
            *x = round(g.share, 4);
        }
    }
    let mut hour_share = vec![0.0; 24];
    for g in query::breakdown(conn, &f, Dim::Hour, 24)? {
        if let Some(x) = g
            .key
            .parse::<usize>()
            .ok()
            .and_then(|i| hour_share.get_mut(i))
        {
            *x = round(g.share, 4);
        }
    }

    let mut groups = BTreeMap::new();
    let mut fam: BTreeMap<String, Metrics> = BTreeMap::new();
    for d in days.values() {
        for (k, m) in &d.by_family {
            fam.entry(k.clone()).or_default().add(m);
        }
    }
    groups.insert("family".to_string(), shares(fam, all));
    let fold = |dim: Dim, clean: &dyn Fn(&str) -> String| -> Result<Vec<Share>> {
        let mut m: BTreeMap<String, Metrics> = BTreeMap::new();
        for g in query::breakdown(conn, &f, dim, 1000)? {
            m.entry(clean(&g.key))
                .or_default()
                .add(&Metrics::from(&g.totals));
        }
        Ok(shares(m, all))
    };
    groups.insert(
        "agent".to_string(),
        fold(Dim::Agent, &|k| {
            if k == "subagent" { "subagent" } else { "main" }.to_string()
        })?,
    );
    groups.insert(
        "effort".to_string(),
        fold(Dim::Effort, &|k| {
            if EFFORTS.contains(&k) { k } else { "other" }.to_string()
        })?,
    );
    groups.insert(
        "entrypoint".to_string(),
        fold(Dim::Entrypoint, &|k| {
            if ENTRYPOINTS.contains(&k) { k } else { "other" }.to_string()
        })?,
    );

    let projects = match &opts.projects {
        Projects::None => None,
        Projects::Hashed { salt } | Projects::Named { salt } => {
            let named = matches!(opts.projects, Projects::Named { .. });
            let mut out: Vec<ProjectSummary> = query::breakdown(conn, &f, Dim::Project, 100_000)?
                .into_iter()
                .map(|g: Group| ProjectSummary {
                    id: hash_project(salt, &g.key),
                    name: named.then(|| base_name(&g.key)),
                    totals: Metrics::from(&g.totals),
                    share: round(g.share, 4),
                })
                .collect();
            out.sort_by_key(|p| std::cmp::Reverse(p.totals.total));
            Some(out)
        }
    };

    let s = Summary {
        schema: SCHEMA.to_string(),
        app_version: crate::version().to_string(),
        generated_at: jiff::Timestamp::now()
            .round(jiff::Unit::Second)
            .unwrap_or_else(|_| jiff::Timestamp::now())
            .to_string(),
        name: opts.name.clone().filter(|n| !n.trim().is_empty()),
        from: from.to_string(),
        to: to.to_string(),
        totals,
        days: days.into_values().collect(),
        weekday_share,
        hour_share,
        groups,
        projects,
    };
    private.check(&s)?;
    Ok(s)
}

fn shares(m: BTreeMap<String, Metrics>, all: f64) -> Vec<Share> {
    let mut v: Vec<Share> = m
        .into_iter()
        .map(|(key, t)| Share {
            key,
            requests: t.requests,
            share: round(t.total as f64 / all, 4),
        })
        .collect();
    v.sort_by(|a, b| b.share.total_cmp(&a.share).then(a.key.cmp(&b.key)));
    v
}

/// Strings from the index that must never appear in a summary.
struct Private {
    /// Full values: paths, projects, branches, session ids, titles.
    whole: HashSet<String>,
    /// Single path components, such as the user name in `/Users/<name>`.
    parts: HashSet<String>,
}

impl Private {
    fn load(conn: &Connection) -> Result<Self> {
        let mut whole = HashSet::new();
        for sql in [
            "SELECT DISTINCT project FROM requests",
            "SELECT DISTINCT cwd FROM requests",
            "SELECT DISTINCT branch FROM requests WHERE branch IS NOT NULL",
            "SELECT DISTINCT session FROM requests",
            "SELECT DISTINCT file FROM requests",
            "SELECT path FROM files",
            "SELECT project FROM files WHERE project IS NOT NULL",
            "SELECT title FROM sessions",
            "SELECT id FROM sessions",
        ] {
            let mut st = conn.prepare(sql)?;
            for v in st.query_map([], |r| r.get::<_, String>(0))? {
                let v = v?.trim().to_lowercase();
                if v.len() >= 3 {
                    whole.insert(v);
                }
            }
        }
        let parts = whole
            .iter()
            .filter(|v| v.contains('/'))
            .flat_map(|v| v.split('/'))
            .filter(|p| p.len() >= 3)
            .map(str::to_string)
            .collect();
        Ok(Private { whole, parts })
    }

    fn matches(&self, value: &str) -> bool {
        let v = value.to_lowercase();
        self.parts.contains(&v) || self.whole.iter().any(|w| v.contains(w.as_str()))
    }

    /// Walk every string value of the serialized summary. Values from a
    /// fixed vocabulary, dates, project hashes, the schema and version, the
    /// display name and opted-in project names are known safe; anything else
    /// must not be or contain a private string. This also guards fields
    /// added later.
    fn check(&self, s: &Summary) -> Result<()> {
        let v = serde_json::to_value(s).map_err(|e| Error::Other(e.to_string()))?;
        let mut bad = false;
        walk(&v, "", &mut |path, val| {
            let safe = matches!(path, "schema" | "appVersion" | "generatedAt" | "name")
                || (path == "projects.name" && s.projects.is_some())
                || is_vocab(val)
                || val.parse::<Date>().is_ok()
                || (path == "projects.id" && is_hash(val));
            if !safe && self.matches(val) {
                bad = true;
            }
        });
        if bad {
            return Err(Error::Other(
                "the summary would contain a private string from the index; nothing was written"
                    .into(),
            ));
        }
        Ok(())
    }
}

fn walk(v: &serde_json::Value, path: &str, f: &mut dyn FnMut(&str, &str)) {
    use serde_json::Value as V;
    match v {
        V::String(s) => f(path, s),
        V::Array(a) => a.iter().for_each(|x| walk(x, path, f)),
        V::Object(o) => {
            for (k, x) in o {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(x, &p, f);
            }
        }
        _ => {}
    }
}

fn is_vocab(v: &str) -> bool {
    FAMILIES.contains(&v)
        || EFFORTS.contains(&v)
        || ENTRYPOINTS.contains(&v)
        || matches!(v, "other" | "main" | "subagent")
}

fn is_hash(v: &str) -> bool {
    v.len() == 12
        && v.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Parse one summary file's text, rejecting other schemas.
pub fn parse_summary(text: &str) -> Result<Summary> {
    let s: Summary =
        serde_json::from_str(text).map_err(|e| Error::Other(format!("not a summary: {e}")))?;
    let major = s
        .schema
        .strip_prefix(SCHEMA_PREFIX)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok());
    if major != Some(SCHEMA_MAJOR) {
        return Err(Error::Other(format!(
            "unsupported summary schema {:?}; this app reads {SCHEMA}",
            s.schema
        )));
    }
    date(&s.from)?;
    date(&s.to)?;
    Ok(s)
}

pub fn to_json(s: &Summary) -> String {
    serde_json::to_string_pretty(s).expect("summary serializes")
}

/// A summary read from disk with the label it is shown under.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Loaded {
    /// The summary's name, or the file name without extension. Made unique.
    pub label: String,
    pub path: PathBuf,
    pub summary: Summary,
}

pub fn load_summaries<P: AsRef<Path>>(paths: &[P]) -> Result<Vec<Loaded>> {
    let mut out: Vec<Loaded> = Vec::new();
    for p in paths {
        let p = p.as_ref();
        let text = std::fs::read_to_string(p)
            .map_err(|e| Error::Other(format!("{}: {e}", p.display())))?;
        let summary =
            parse_summary(&text).map_err(|e| Error::Other(format!("{}: {e}", p.display())))?;
        let base = summary.name.clone().unwrap_or_else(|| {
            p.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "?".into())
        });
        let mut label = base.clone();
        let mut n = 2;
        while out.iter().any(|l| l.label == label) {
            label = format!("{base} ({n})");
            n += 1;
        }
        out.push(Loaded {
            label,
            path: p.to_path_buf(),
            summary,
        });
    }
    Ok(out)
}

/// Monday to Sunday, inclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Week {
    pub start: String,
    pub end: String,
}

/// The `n` weeks ending with the week that contains `last`, oldest first.
pub fn weeks_ending(last: Date, n: usize) -> Vec<Week> {
    let m = monday_of(last);
    (0..n as i64)
        .rev()
        .map(|i| {
            let s = add_days(m, -7 * i);
            Week {
                start: s.to_string(),
                end: add_days(s, 6).to_string(),
            }
        })
        .collect()
}

/// The `n` weeks ending with the latest day any summary covers.
pub fn default_weeks(people: &[Loaded], n: usize) -> Vec<Week> {
    let last = people
        .iter()
        .filter_map(|l| l.summary.to.parse::<Date>().ok())
        .max()
        .unwrap_or_else(query::today);
    weeks_ending(last, n)
}

#[derive(Debug, Clone, Default)]
pub struct CompareOptions {
    /// Only this model family (see [`FAMILIES`]); all models when None.
    pub family: Option<String>,
}

/// Percent change against the previous week, None when it had nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub requests: Option<f64>,
    pub total: Option<f64>,
    pub output: Option<f64>,
    pub context_per_request: Option<f64>,
    pub output_per_request: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekCell {
    pub week_start: String,
    /// Days of this week the summary covers (0 to 7).
    pub days_covered: u32,
    /// The summary ends or starts inside this week. The change is then
    /// against the same weekdays of the previous week.
    pub partial: bool,
    pub totals: Metrics,
    /// The same days one week earlier, when the summary covers all of them.
    pub previous: Option<Metrics>,
    pub change: Change,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonWeeks {
    pub label: String,
    pub cells: Vec<WeekCell>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub weeks: Vec<Week>,
    pub family: Option<String>,
    pub people: Vec<PersonWeeks>,
}

fn day_metrics<'a>(s: &'a Summary, family: Option<&str>) -> BTreeMap<&'a str, &'a Metrics> {
    s.days
        .iter()
        .filter_map(|d| {
            let m = match family {
                Some(f) => d.by_family.get(f)?,
                None => &d.totals,
            };
            Some((d.day.as_str(), m))
        })
        .collect()
}

/// Line people up week by week: totals, context per request and output per
/// request, each with the change against the previous week.
pub fn compare(people: &[Loaded], weeks: &[Week], opts: &CompareOptions) -> Result<Comparison> {
    let family = opts.family.as_deref().map(str::to_lowercase);
    let mut out = Vec::new();
    for l in people {
        let s = &l.summary;
        let (from, to) = (date(&s.from)?, date(&s.to)?);
        let by_day = day_metrics(s, family.as_deref());
        let sum = |days: &[Date]| -> Metrics {
            let mut m = Metrics::default();
            for d in days {
                if let Some(x) = by_day.get(d.to_string().as_str()) {
                    m.add(x);
                }
            }
            m
        };
        let mut cells = Vec::new();
        for w in weeks {
            let start = date(&w.start)?;
            let covered: Vec<Date> = (0..7)
                .map(|i| add_days(start, i))
                .filter(|d| *d >= from && *d <= to)
                .collect();
            let prev_days: Vec<Date> = covered.iter().map(|d| add_days(*d, -7)).collect();
            let totals = sum(&covered);
            let previous = (!covered.is_empty() && prev_days.iter().all(|d| *d >= from))
                .then(|| sum(&prev_days));
            let change = previous
                .as_ref()
                .map(|p| Change {
                    requests: pct(p.requests as f64, totals.requests as f64),
                    total: pct(p.total as f64, totals.total as f64),
                    output: pct(p.output as f64, totals.output as f64),
                    context_per_request: pct(p.context_per_request, totals.context_per_request),
                    output_per_request: pct(p.output_per_request, totals.output_per_request),
                })
                .unwrap_or_default();
            cells.push(WeekCell {
                week_start: w.start.clone(),
                days_covered: covered.len() as u32,
                partial: !covered.is_empty() && covered.len() < 7,
                totals,
                previous,
                change,
            });
        }
        out.push(PersonWeeks {
            label: l.label.clone(),
            cells,
        });
    }
    Ok(Comparison {
        weeks: weeks.to_vec(),
        family,
        people: out,
    })
}

fn pct(before: f64, after: f64) -> Option<f64> {
    crate::fmt::change(before, after).map(|p| round(p, 1))
}

/// Read the per-user salt for project hashes, creating it on first use.
/// The same salt keeps one person's project ids stable across exports while
/// making them useless to anyone guessing names.
pub fn load_or_create_salt(path: &Path) -> Result<Vec<u8>> {
    if let Ok(s) = std::fs::read_to_string(path) {
        if let Some(b) = from_hex(s.trim()).filter(|b| b.len() >= 16) {
            return Ok(b);
        }
    }
    let mut b = vec![0u8; 32];
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")?;
        f.read_exact(&mut b)?;
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    std::fs::write(path, hex)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(b)
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A table of the app, for `usage export --csv`.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Table {
    Daily(Vec<DayRow>),
    Groups(Vec<Group>),
}

/// `daily` or `by-<dim>` (`by-model`, `by-project`, …).
pub fn table(conn: &Connection, f: &Filter, name: &str, limit: usize) -> Result<Table> {
    if name == "daily" {
        return Ok(Table::Daily(query::daily(conn, f)?));
    }
    let dim = name
        .strip_prefix("by-")
        .and_then(Dim::parse)
        .ok_or_else(|| Error::Other(format!("unknown table {name:?}: use daily or by-<dim>")))?;
    Ok(Table::Groups(query::breakdown(conn, f, dim, limit)?))
}

const TOTAL_COLS: &str = "requests,input,output,thinking,cache_write,cache_write_5m,cache_write_1h,cache_read,web_search,web_fetch,total,output_per_request,context_per_request,cache_hit_rate";

fn totals_cols(t: &Totals) -> String {
    format!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        t.requests,
        t.input,
        t.output,
        t.thinking,
        t.cache_write,
        t.cache_write_5m,
        t.cache_write_1h,
        t.cache_read,
        t.web_search,
        t.web_fetch,
        t.total,
        round(t.output_per_request, 2),
        round(t.context_per_request, 2),
        round(t.cache_hit_rate, 6)
    )
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

impl Table {
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        match self {
            Table::Daily(rows) => {
                out += &format!("day,weekday,{TOTAL_COLS}\n");
                for r in rows {
                    out += &format!("{},{},{}\n", r.day, r.weekday, totals_cols(&r.totals));
                }
            }
            Table::Groups(rows) => {
                out += &format!("key,label,detail,share,{TOTAL_COLS}\n");
                for g in rows {
                    out += &format!(
                        "{},{},{},{},{}\n",
                        csv_field(&g.key),
                        csv_field(&g.label),
                        csv_field(g.detail.as_deref().unwrap_or("")),
                        round(g.share, 6),
                        totals_cols(&g.totals)
                    );
                }
            }
        }
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("table serializes")
    }
}
