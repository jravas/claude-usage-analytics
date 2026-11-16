//! Cost model: API-equivalent dollars and cost-weighted tokens (PLAN 3.1).
//!
//! Prices come from `crates/core/prices.json`, embedded at build time, and an
//! optional user override `prices.json` in [`paths::data_dir`]. Unknown models
//! have no price: every cost function returns `None` for them, never a guess.
//!
//! [`check`] verifies the table against Claude Code's own `cost-state`
//! records: it recomputes each session's cost from the per-model token counts
//! in that record and compares with its `costUSD`.

use crate::{paths, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The price table shipped with the app.
pub const EMBEDDED_PRICES: &str = include_str!("../prices.json");

/// File name of the user override inside the app data folder.
pub const OVERRIDE_FILE: &str = "prices.json";

/// Dollars per million tokens for each token category.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// Higher prices for a request whose prompt exceeds `threshold_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LongContext {
    pub threshold_tokens: u64,
    #[serde(flatten)]
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Canonical model id as Claude Code reports it, e.g. `claude-opus-5-5`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(flatten)]
    pub rates: Rates,
    /// Multiplier on every category for `speed: "fast"`. None: no fast mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_multiplier: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<LongContext>,
    /// Earlier prices, for requests made before a price change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<PreviousRates>,
}

/// Rates that applied to requests made before `until` (YYYY-MM-DD, UTC).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviousRates {
    pub until: String,
    #[serde(flatten)]
    pub rates: Rates,
}

impl ModelPrice {
    /// Base rates for a request made on `day` (YYYY-MM-DD, or any string
    /// starting with one, such as an RFC 3339 timestamp). None: current.
    pub fn rates_on(&self, day: Option<&str>) -> Rates {
        let Some(day) = day else {
            return self.rates;
        };
        let day = day.get(..10).unwrap_or(day);
        self.previous
            .iter()
            .filter(|p| day < p.until.as_str())
            .min_by(|a, b| a.until.cmp(&b.until))
            .map(|p| p.rates)
            .unwrap_or(self.rates)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    #[serde(default)]
    pub version: u32,
    /// Where the prices were taken from.
    #[serde(default)]
    pub source: String,
    /// Date the prices were taken, YYYY-MM-DD.
    #[serde(default)]
    pub retrieved: String,
    #[serde(default)]
    pub currency: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Dollars per web search request.
    #[serde(default)]
    pub web_search_per_request: f64,
    /// Multiplier for `inference_geo: "us"`.
    #[serde(default = "one")]
    pub data_residency_us_multiplier: f64,
    pub models: Vec<ModelPrice>,
}

fn one() -> f64 {
    1.0
}

/// A user override: any top-level field may be left out, and `models` adds
/// to or replaces (by id) the embedded entries.
#[derive(Debug, Default, Deserialize)]
struct Override {
    source: Option<String>,
    retrieved: Option<String>,
    web_search_per_request: Option<f64>,
    data_residency_us_multiplier: Option<f64>,
    #[serde(default)]
    models: Vec<ModelPrice>,
}

/// Token counts of one request, or a sum of requests (see [`PriceTable::cost_of_total`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    /// Includes thinking tokens.
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub web_search_requests: u64,
    /// The response reported `speed: "fast"`.
    #[serde(default)]
    pub fast: bool,
    /// The response reported `inference_geo: "us"`.
    #[serde(default)]
    pub geo_us: bool,
}

impl Usage {
    /// Splits a cache-write total into 5m and 1h. Transcripts that predate the
    /// split report only the total; that part is priced as 5m writes.
    pub fn set_cache_write(&mut self, total: u64, write_5m: u64, write_1h: u64) {
        self.cache_write_1h = write_1h;
        self.cache_write_5m = write_5m.max(total.saturating_sub(write_1h));
    }

    /// Input, cache reads and cache writes: what decides long-context pricing.
    pub fn prompt_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens() + self.output
    }
}

impl PriceTable {
    /// The table shipped with the app.
    pub fn embedded() -> PriceTable {
        serde_json::from_str(EMBEDDED_PRICES).expect("embedded prices.json is valid")
    }

    /// Embedded table plus the user override in the app data folder, if any.
    pub fn load() -> Result<PriceTable> {
        Self::load_with_override(&override_path())
    }

    /// Embedded table plus the override at `path`. A missing file is not an
    /// error; a malformed one is.
    pub fn load_with_override(path: &Path) -> Result<PriceTable> {
        let mut t = Self::embedded();
        match std::fs::read_to_string(path) {
            Ok(s) => t.apply_override(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(t)
    }

    fn apply_override(&mut self, json: &str) -> Result<()> {
        let o: Override =
            serde_json::from_str(json).map_err(|e| Error::Other(format!("price override: {e}")))?;
        if let Some(v) = o.source {
            self.source = v;
        }
        if let Some(v) = o.retrieved {
            self.retrieved = v;
        }
        if let Some(v) = o.web_search_per_request {
            self.web_search_per_request = v;
        }
        if let Some(v) = o.data_residency_us_multiplier {
            self.data_residency_us_multiplier = v;
        }
        for m in o.models {
            match self.models.iter_mut().find(|x| x.id == m.id) {
                Some(x) => *x = m,
                None => self.models.push(m),
            }
        }
        Ok(())
    }

    /// Price entry for a model id as it appears in transcripts. Accepts
    /// date suffixes (`-20251001`), context tags (`[1m]`), platform prefixes
    /// (`us.anthropic.`) and version suffixes (`@…`, `-v1:0`). No fuzzy
    /// matching: a model that is not in the table is unknown.
    pub fn price(&self, model: &str) -> Option<&ModelPrice> {
        let id = normalize_model(model);
        self.models
            .iter()
            .find(|m| m.id == id || m.aliases.contains(&id))
    }

    /// Dollars for one request. Applies long-context, fast-mode and
    /// data-residency pricing. None for an unknown model.
    pub fn cost_of(&self, model: &str, u: &Usage) -> Option<f64> {
        self.cost_of_on(model, u, None)
    }

    /// Like [`PriceTable::cost_of`], at the prices in effect on `day`
    /// (YYYY-MM-DD or an RFC 3339 timestamp). Use this for stored history.
    pub fn cost_of_on(&self, model: &str, u: &Usage, day: Option<&str>) -> Option<f64> {
        let p = self.price(model)?;
        let rates = request_rates(p, u, day);
        Some(self.priced(p, &rates, u))
    }

    /// Dollars for a sum over many requests. Long-context pricing is per
    /// request and cannot be decided from a sum, so base prices are used.
    pub fn cost_of_total(&self, model: &str, u: &Usage) -> Option<f64> {
        let p = self.price(model)?;
        Some(self.priced(p, &p.rates, u))
    }

    fn priced(&self, p: &ModelPrice, r: &Rates, u: &Usage) -> f64 {
        let tokens = u.input as f64 * r.input
            + u.output as f64 * r.output
            + u.cache_write_5m as f64 * r.cache_write_5m
            + u.cache_write_1h as f64 * r.cache_write_1h
            + u.cache_read as f64 * r.cache_read;
        let mut mult = 1.0;
        if u.fast {
            mult *= p.fast_multiplier.unwrap_or(1.0);
        }
        if u.geo_us {
            mult *= self.data_residency_us_multiplier;
        }
        tokens / 1e6 * mult + u.web_search_requests as f64 * self.web_search_per_request
    }

    /// Tokens weighted by their price relative to the same model's input
    /// price (input = 1, so a 5m cache write on most models is 1.25 and a
    /// cache read 0.1 or less). Web searches are not tokens and are left
    /// out. None for an unknown model.
    pub fn weighted_tokens(&self, model: &str, u: &Usage) -> Option<f64> {
        let p = self.price(model)?;
        let r = request_rates(p, u, None);
        let base = p.rates.input;
        if base <= 0.0 {
            return None;
        }
        let mut mult = 1.0;
        if u.fast {
            mult *= p.fast_multiplier.unwrap_or(1.0);
        }
        Some(
            (u.input as f64 * r.input
                + u.output as f64 * r.output
                + u.cache_write_5m as f64 * r.cache_write_5m
                + u.cache_write_1h as f64 * r.cache_write_1h
                + u.cache_read as f64 * r.cache_read)
                / base
                * mult,
        )
    }

    /// The value of `u` in `unit`. Raw tokens are known for every model.
    pub fn value(&self, unit: Unit, model: &str, u: &Usage) -> Option<f64> {
        match unit {
            Unit::Cost => self.cost_of(model, u),
            Unit::Weighted => self.weighted_tokens(model, u),
            Unit::Tokens => Some(u.total_tokens() as f64),
        }
    }
}

/// Rates for one request: dated base prices, then the long-context tier.
fn request_rates(p: &ModelPrice, u: &Usage, day: Option<&str>) -> Rates {
    match p.long_context {
        Some(lc) if u.prompt_tokens() > lc.threshold_tokens => lc.rates,
        _ => p.rates_on(day),
    }
}

/// Path of the user's price override file.
pub fn override_path() -> PathBuf {
    paths::data_dir().join(OVERRIDE_FILE)
}

/// Canonical model id: lower case, no platform prefix, context tag, version
/// or date suffix.
pub fn normalize_model(model: &str) -> String {
    let mut s = model.trim().to_ascii_lowercase();
    if let Some(i) = s.rfind("anthropic.") {
        s = s[i + "anthropic.".len()..].to_string();
    }
    for cut in ['[', '@', ':'] {
        if let Some(i) = s.find(cut) {
            s.truncate(i);
        }
    }
    // Bedrock `-v1` (after `:0` was cut above).
    if let Some(i) = s.rfind("-v") {
        if s[i + 2..].chars().all(|c| c.is_ascii_digit()) && i + 2 < s.len() {
            s.truncate(i);
        }
    }
    // Date snapshot `-20251001`.
    if let Some(i) = s.rfind('-') {
        let tail = &s[i + 1..];
        if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) {
            s.truncate(i);
        }
    }
    s
}

/// Display unit for totals, switchable in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    /// API-equivalent dollars ("at API prices", not a bill).
    #[default]
    Cost,
    /// Tokens weighted by price, input = 1.
    Weighted,
    /// Raw token counts.
    Tokens,
}

impl Unit {
    pub const ALL: [Unit; 3] = [Unit::Cost, Unit::Weighted, Unit::Tokens];

    pub fn parse(s: &str) -> Option<Unit> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cost" | "usd" | "$" => Some(Unit::Cost),
            "weighted" | "weighted-tokens" => Some(Unit::Weighted),
            "tokens" | "raw" => Some(Unit::Tokens),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Cost => "cost",
            Unit::Weighted => "weighted",
            Unit::Tokens => "tokens",
        }
    }

    /// Label for a toggle or column header.
    pub fn label(self) -> &'static str {
        match self {
            Unit::Cost => "Cost (at API prices)",
            Unit::Weighted => "Weighted tokens",
            Unit::Tokens => "Tokens",
        }
    }

    /// Short human form: `$12.34`, `$0.042`, `1.2M`.
    pub fn format(self, v: f64) -> String {
        match self {
            Unit::Cost => format_usd(v),
            Unit::Weighted | Unit::Tokens => crate::fmt::tokens(v.max(0.0).round() as u64),
        }
    }

    /// Like [`Unit::format`], with "no price" for an unknown value.
    pub fn format_opt(self, v: Option<f64>) -> String {
        v.map(|v| self.format(v))
            .unwrap_or_else(|| "no price".into())
    }
}

/// `$1,234`, `$12.34`, `$0.042`, `$0`.
pub fn format_usd(v: f64) -> String {
    let sign = if v < 0.0 { "-" } else { "" };
    let a = v.abs();
    if a == 0.0 {
        return "$0".into();
    }
    if a >= 1000.0 {
        let whole = a.round() as u64;
        let s = whole.to_string();
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
        }
        return format!("{sign}${out}");
    }
    if a >= 1.0 {
        format!("{sign}${a:.2}")
    } else if a >= 0.01 {
        format!("{sign}${a:.3}")
    } else {
        format!("{sign}${a:.4}")
    }
}

// ---------------------------------------------------------------------------
// Verification against Claude Code's `cost-state`.

/// Default tolerance for "agrees with Claude Code": 10%.
pub const TOLERANCE: f64 = 0.10;

/// The cache-read multiplier most models use (0.1x input). Claude Code up to
/// 2.1.309 priced cache reads at this rate for every model, including Opus
/// 5.5, Sonnet 5.5 (after its 2026-10-07 price cut) and Fable 5.1, whose
/// cache reads cost less. Used only to explain old `cost-state` records.
pub const STANDARD_CACHE_READ_MULTIPLIER: f64 = 0.1;

#[derive(Debug, Clone, Serialize)]
pub struct ModelCheck {
    pub model: String,
    pub reported_usd: f64,
    /// None when the model has no price.
    pub computed_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheck {
    pub session_id: String,
    pub reported_usd: f64,
    /// None when any model of the session has no price.
    pub computed_usd: Option<f64>,
    /// (computed - reported) / reported. None without a computed cost or
    /// with a zero reported cost.
    pub diff: Option<f64>,
    /// The record has no 5m/1h cache-write split: written by an older Claude
    /// Code (up to 2.1.309) whose price table was out of date.
    pub legacy_format: bool,
    /// For legacy records: the difference when cache reads are priced the way
    /// that Claude Code did, at 0.1x input.
    pub legacy_diff: Option<f64>,
    pub models: Vec<ModelCheck>,
}

impl SessionCheck {
    /// Within tolerance at current prices, or, for a legacy record, at the
    /// cache-read rate that Claude Code used then.
    pub fn explained(&self, tolerance: f64) -> bool {
        let ok = |d: Option<f64>| d.is_some_and(|d| d.abs() <= tolerance);
        ok(self.diff) || (self.legacy_format && ok(self.legacy_diff))
    }
}

/// How many of a group of sessions agree within tolerance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Share {
    pub compared: usize,
    pub within: usize,
    /// within / compared, 0 when nothing was compared.
    pub share: f64,
}

impl Share {
    fn of<'a>(
        it: impl Iterator<Item = &'a SessionCheck>,
        ok: impl Fn(&SessionCheck) -> bool,
    ) -> Share {
        let (mut compared, mut within) = (0, 0);
        for s in it.filter(|s| s.diff.is_some()) {
            compared += 1;
            within += ok(s) as usize;
        }
        Share {
            compared,
            within,
            share: if compared > 0 {
                within as f64 / compared as f64
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CostCheck {
    pub tolerance: f64,
    pub sessions: usize,
    /// Every compared session, at current prices.
    pub all: Share,
    /// Records written by a Claude Code with the current price table (they
    /// carry the 5m/1h cache-write split). This is the regression test for
    /// prices.json.
    pub current_format: Share,
    /// Records written by older Claude Code versions.
    pub legacy_format: Share,
    /// Sessions within tolerance at current prices or, for legacy records,
    /// at Claude Code's old cache-read rate.
    pub explained: Share,
    /// Sessions skipped because a model has no price.
    pub unpriced: usize,
    /// Distinct models without a price.
    pub unknown_models: Vec<String>,
    /// Per-session results, worst disagreement first.
    pub results: Vec<SessionCheck>,
}

/// Relative difference of `computed` from `reported`. None when reported is 0
/// (a 0-vs-0 session agrees exactly and is reported as 0).
pub fn relative_diff(reported: f64, computed: f64) -> Option<f64> {
    if reported > 0.0 {
        Some((computed - reported) / reported)
    } else if computed.abs() < 1e-9 {
        Some(0.0)
    } else {
        None
    }
}

/// Recomputes one `cost-state` record. `record` is the parsed JSON line.
pub fn check_record(t: &PriceTable, record: &serde_json::Value) -> Option<SessionCheck> {
    let id = record.get("sessionId")?.as_str()?.to_string();
    let mu = record.get("modelUsage")?.as_object()?;
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let mut models = Vec::new();
    let mut computed = Some(0.0);
    let mut legacy_computed = Some(0.0);
    let mut legacy_format = false;
    for (model, v) in mu {
        let mut u = Usage {
            input: num(v, "inputTokens") as u64,
            output: num(v, "outputTokens") as u64,
            cache_read: num(v, "cacheReadInputTokens") as u64,
            web_search_requests: num(v, "webSearchRequests") as u64,
            ..Usage::default()
        };
        let total_write = num(v, "cacheCreationInputTokens") as u64;
        if v.get("cacheCreationEphemeral1hInputTokens").is_some() {
            u.set_cache_write(
                total_write,
                num(v, "cacheCreationEphemeral5mInputTokens") as u64,
                num(v, "cacheCreationEphemeral1hInputTokens") as u64,
            );
        } else {
            // Older records drop the split. Claude Code writes its prompt
            // cache with the 1-hour TTL, and pricing these writes as 1h is
            // what reproduces its costUSD.
            legacy_format = true;
            u.cache_write_1h = total_write;
        }
        let c = t.cost_of_total(model, &u);
        computed = computed.zip(c).map(|(a, b)| a + b);
        let lc = t.price(model).map(|p| {
            let mut r = p.rates;
            r.cache_read = r.input * STANDARD_CACHE_READ_MULTIPLIER;
            t.priced(p, &r, &u)
        });
        legacy_computed = legacy_computed.zip(lc).map(|(a, b)| a + b);
        models.push(ModelCheck {
            model: model.clone(),
            reported_usd: num(v, "costUSD"),
            computed_usd: c,
        });
    }
    let reported = record
        .get("totalCostUSD")
        .and_then(|x| x.as_f64())
        .unwrap_or_else(|| models.iter().map(|m| m.reported_usd).sum());
    Some(SessionCheck {
        session_id: id,
        reported_usd: reported,
        diff: computed.and_then(|c| relative_diff(reported, c)),
        computed_usd: computed,
        legacy_format,
        legacy_diff: if legacy_format {
            legacy_computed.and_then(|c| relative_diff(reported, c))
        } else {
            None
        },
        models,
    })
}

/// Summarizes per-session checks: shares within `tolerance`, worst first.
pub fn summarize(mut results: Vec<SessionCheck>, tolerance: f64) -> CostCheck {
    let mut unknown: Vec<String> = results
        .iter()
        .flat_map(|s| s.models.iter())
        .filter(|m| m.computed_usd.is_none())
        .map(|m| m.model.clone())
        .collect();
    unknown.sort();
    unknown.dedup();
    let unpriced = results.iter().filter(|s| s.computed_usd.is_none()).count();
    let within = |s: &SessionCheck| s.diff.is_some_and(|d| d.abs() <= tolerance);
    let all = Share::of(results.iter(), within);
    let current_format = Share::of(results.iter().filter(|s| !s.legacy_format), within);
    let legacy_format = Share::of(results.iter().filter(|s| s.legacy_format), within);
    let explained = Share::of(results.iter(), |s| s.explained(tolerance));
    results.sort_by(|a, b| {
        let k = |s: &SessionCheck| s.diff.map(f64::abs).unwrap_or(f64::INFINITY);
        k(b).total_cmp(&k(a))
    });
    CostCheck {
        tolerance,
        sessions: results.len(),
        all,
        current_format,
        legacy_format,
        explained,
        unpriced,
        unknown_models: unknown,
        results,
    }
}

/// Reads the last `cost-state` record of every transcript under
/// `projects_dir` (one per session; the latest record is cumulative) and
/// checks it against `t`.
pub fn check(t: &PriceTable, projects_dir: &Path) -> Result<CostCheck> {
    use rayon::prelude::*;
    let mut files = Vec::new();
    collect_jsonl(projects_dir, &mut files);
    let records: Vec<serde_json::Value> = files
        .par_iter()
        .filter_map(|f| last_cost_state(f).ok().flatten())
        .collect();
    // A session can span files (resume); keep its largest running total.
    let mut by_session: HashMap<String, SessionCheck> = HashMap::new();
    for r in &records {
        if let Some(c) = check_record(t, r) {
            match by_session.get(&c.session_id) {
                Some(prev) if prev.reported_usd >= c.reported_usd => {}
                _ => {
                    by_session.insert(c.session_id.clone(), c);
                }
            }
        }
    }
    Ok(summarize(by_session.into_values().collect(), TOLERANCE))
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(ft) if ft.is_dir() => collect_jsonl(&p, out),
            Ok(ft) if ft.is_file() && p.extension().is_some_and(|x| x == "jsonl") => out.push(p),
            _ => {}
        }
    }
}

fn last_cost_state(path: &Path) -> Result<Option<serde_json::Value>> {
    let finder = memchr::memmem::Finder::new(b"\"cost-state\"");
    let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut line = Vec::new();
    let mut last = None;
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        // Cheap filter: cost-state lines are small; skip large lines fast.
        if line.len() > 64 * 1024 || finder.find(&line).is_none() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
            if v.get("type").and_then(|t| t.as_str()) == Some("cost-state") {
                last = Some(v);
            }
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn embedded_table_parses_and_has_source() {
        let t = PriceTable::embedded();
        assert!(t.source.starts_with("https://"));
        assert_eq!(t.retrieved.len(), 10);
        assert!(t.models.len() >= 10);
        for m in &t.models {
            assert!(
                m.rates.input > 0.0 && m.rates.output > m.rates.input,
                "{}",
                m.id
            );
            assert!(m.rates.cache_read < m.rates.input, "{}", m.id);
        }
    }

    #[test]
    fn prices_a_request() {
        let t = PriceTable::embedded();
        // Real cost-state line: claude-opus-5-5, $16.6609276.
        let mut u = Usage {
            input: 382,
            output: 165_244,
            cache_read: 43_013_648,
            ..Default::default()
        };
        u.set_cache_write(746_181, 405_886, 340_295);
        let c = t.cost_of("claude-opus-5-5", &u).unwrap();
        assert!(close(c, 16.6609276), "{c}");
        // Fast mode doubles tokens; web search is flat.
        u.fast = true;
        u.web_search_requests = 3;
        assert!(close(
            t.cost_of("claude-opus-5-5", &u).unwrap(),
            2.0 * 16.6609276 + 0.03
        ));
        // A model without fast mode ignores the flag.
        let one = Usage {
            output: 1_000_000,
            fast: true,
            ..Default::default()
        };
        assert!(close(t.cost_of("claude-sonnet-5-5", &one).unwrap(), 10.0));
        // Data residency.
        let geo = Usage {
            input: 1_000_000,
            geo_us: true,
            ..Default::default()
        };
        assert!(close(t.cost_of("claude-opus-5-5", &geo).unwrap(), 4.4));
    }

    #[test]
    fn long_context_is_per_request() {
        let t = PriceTable::embedded();
        let small = Usage {
            input: 100_000,
            output: 1_000,
            ..Default::default()
        };
        let big = Usage {
            input: 1_000,
            cache_read: 100_000,
            output: 1_000,
            ..Default::default()
        };
        let s = t.cost_of("claude-haiku-5-5", &small).unwrap();
        assert!(close(s, 0.1 * 0.1 + 0.001 * 0.5));
        let b = t.cost_of("claude-haiku-5-5", &big).unwrap();
        assert!(close(b, 0.001 * 0.5 + 0.1 * 0.05 + 0.001 * 2.5));
        // Totals never use the long-context tier.
        let tb = t.cost_of_total("claude-haiku-5-5", &big).unwrap();
        assert!(close(tb, 0.001 * 0.1 + 0.1 * 0.01 + 0.001 * 0.5));
    }

    #[test]
    fn model_ids_normalize() {
        let t = PriceTable::embedded();
        for id in [
            "claude-opus-5-5",
            "claude-opus-5-5[1m]",
            "Claude-Opus-5-5",
            "us.anthropic.claude-opus-5-5",
            "anthropic.claude-opus-5-5-v1:0",
            "claude-opus-5-5@20260401",
            "claude-opus-5-5-20260401",
        ] {
            assert_eq!(
                t.price(id).map(|p| p.id.as_str()),
                Some("claude-opus-5-5"),
                "{id}"
            );
        }
        assert_eq!(
            t.price("claude-haiku-4-5-20251001").unwrap().id,
            "claude-haiku-4-5"
        );
        assert_eq!(t.price("claude-mythos-5-2").unwrap().id, "claude-fable-5-2");
        // claude-opus-5 must not be picked for an unknown claude-opus-5-7.
        assert_eq!(normalize_model("claude-opus-5"), "claude-opus-5");
    }

    #[test]
    fn unknown_models_have_no_price() {
        let t = PriceTable::embedded();
        let u = Usage {
            input: 10,
            ..Default::default()
        };
        assert!(t.price("claude-opus-5-7").is_none());
        assert!(t.cost_of("gpt-5", &u).is_none());
        assert!(t.weighted_tokens("<synthetic>", &u).is_none());
        assert_eq!(t.value(Unit::Tokens, "gpt-5", &u), Some(10.0));
        assert_eq!(Unit::Cost.format_opt(None), "no price");
    }

    #[test]
    fn weighted_tokens_use_input_as_one() {
        let t = PriceTable::embedded();
        let u = Usage {
            input: 1000,
            output: 1000,
            cache_write_5m: 1000,
            cache_write_1h: 1000,
            cache_read: 1000,
            ..Default::default()
        };
        // Opus 5.5: 1 + 5 + 1.25 + 2 + 0.05.
        assert!(close(
            t.weighted_tokens("claude-opus-5-5", &u).unwrap(),
            9300.0
        ));
    }

    #[test]
    fn override_replaces_and_adds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prices.json");
        // Missing override: embedded.
        assert_eq!(
            PriceTable::load_with_override(&p).unwrap(),
            PriceTable::embedded()
        );
        std::fs::write(
            &p,
            r#"{"retrieved":"2030-01-01","web_search_per_request":0.02,"models":[
                {"id":"claude-opus-5-5","input":1,"output":2,"cache_write_5m":3,"cache_write_1h":4,"cache_read":0.5},
                {"id":"claude-new-1","aliases":["claude-new-one"],"input":1,"output":1,"cache_write_5m":1,"cache_write_1h":1,"cache_read":1}
            ]}"#,
        )
        .unwrap();
        let t = PriceTable::load_with_override(&p).unwrap();
        assert_eq!(t.retrieved, "2030-01-01");
        assert_eq!(t.source, PriceTable::embedded().source);
        assert_eq!(t.web_search_per_request, 0.02);
        assert_eq!(t.price("claude-opus-5-5").unwrap().rates.input, 1.0);
        assert!(t
            .price("claude-opus-5-5")
            .unwrap()
            .fast_multiplier
            .is_none());
        assert!(t.price("claude-new-one").is_some());
        assert_eq!(t.models.len(), PriceTable::embedded().models.len() + 1);
        std::fs::write(&p, "{ not json").unwrap();
        assert!(PriceTable::load_with_override(&p).is_err());
    }

    #[test]
    fn units_format_and_serialize() {
        assert_eq!(
            serde_json::to_string(&Unit::Weighted).unwrap(),
            "\"weighted\""
        );
        assert_eq!(
            serde_json::from_str::<Unit>("\"cost\"").unwrap(),
            Unit::Cost
        );
        assert_eq!(Unit::parse("tokens"), Some(Unit::Tokens));
        assert_eq!(Unit::parse("nope"), None);
        assert_eq!(Unit::Cost.format(1234.4), "$1,234");
        assert_eq!(Unit::Cost.format(12.345), "$12.35");
        assert_eq!(Unit::Cost.format(0.0421), "$0.042");
        assert_eq!(Unit::Cost.format(0.0012), "$0.0012");
        assert_eq!(Unit::Cost.format(0.0), "$0");
        assert_eq!(Unit::Weighted.format(3_700_000.0), "3.7M");
        assert_eq!(Unit::Tokens.format(999.0), "999");
    }

    #[test]
    fn verification_math() {
        assert!(close(relative_diff(10.0, 11.0).unwrap(), 0.1));
        assert!(close(relative_diff(10.0, 8.0).unwrap(), -0.2));
        assert_eq!(relative_diff(0.0, 0.0), Some(0.0));
        assert_eq!(relative_diff(0.0, 1.0), None);

        let t = PriceTable::embedded();
        let rec = |id: &str, model: &str, out: u64, usd: f64| {
            json!({"type":"cost-state","sessionId":id,"totalCostUSD":usd,
                   "modelUsage":{model:{"inputTokens":0,"outputTokens":out,
                   "cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":usd}}})
        };
        // 1M Opus 5.5 output = $20.
        let exact = check_record(&t, &rec("a", "claude-opus-5-5", 1_000_000, 20.0)).unwrap();
        assert!(close(exact.diff.unwrap(), 0.0));
        let near = check_record(&t, &rec("b", "claude-opus-5-5", 1_000_000, 19.0)).unwrap();
        let far = check_record(&t, &rec("c", "claude-opus-5-5", 1_000_000, 10.0)).unwrap();
        assert!(close(far.diff.unwrap(), 1.0));
        let unknown = check_record(&t, &rec("d", "claude-x-9", 10, 1.0)).unwrap();
        assert!(unknown.computed_usd.is_none() && unknown.diff.is_none());

        let s = summarize(vec![exact, near, far, unknown], TOLERANCE);
        assert_eq!(
            (s.sessions, s.all.compared, s.all.within, s.unpriced),
            (4, 3, 2, 1)
        );
        assert!(close(s.all.share, 2.0 / 3.0));
        // These records have no 5m/1h split: legacy format.
        assert_eq!(s.legacy_format.compared, 3);
        assert_eq!(s.current_format.compared, 0);
        assert_eq!(s.unknown_models, vec!["claude-x-9".to_string()]);
        // Unpriced sorts first (infinite), then the worst disagreement.
        assert_eq!(s.results[0].session_id, "d");
        assert_eq!(s.results[1].session_id, "c");
    }

    #[test]
    fn dated_prices() {
        let t = PriceTable::embedded();
        let u = Usage {
            cache_read: 1_000_000,
            ..Default::default()
        };
        let m = "claude-sonnet-5-5";
        assert!(close(t.cost_of(m, &u).unwrap(), 0.1));
        assert!(close(
            t.cost_of_on(m, &u, Some("2026-10-06T23:59:00Z")).unwrap(),
            0.2
        ));
        assert!(close(t.cost_of_on(m, &u, Some("2026-10-07")).unwrap(), 0.1));
        assert!(close(t.cost_of_on(m, &u, None).unwrap(), 0.1));
        // Models without history ignore the date.
        assert!(close(
            t.cost_of_on("claude-opus-5-5", &u, Some("2020-01-01"))
                .unwrap(),
            0.2
        ));
    }

    #[test]
    fn legacy_records_price_writes_as_1h_and_explain_old_cache_reads() {
        let t = PriceTable::embedded();
        // Real record shape from Claude Code 2.1.284: no 5m/1h split, and a
        // cost computed with cache reads at 0.1x input ($0.20 on Sonnet 5.5).
        let r = json!({"type":"cost-state","sessionId":"old","totalCostUSD":0.8781468,
            "modelUsage":{"claude-sonnet-5-5":{"inputTokens":2570,"outputTokens":4803,
            "cacheReadInputTokens":2600664,"cacheCreationInputTokens":76211,"costUSD":0.8781468}}});
        let c = check_record(&t, &r).unwrap();
        assert!(c.legacy_format);
        assert!(close(c.computed_usd.unwrap(), 0.6180804));
        assert!(c.diff.unwrap() < -0.25);
        assert!(close(c.legacy_diff.unwrap(), 0.0));
        assert!(c.explained(TOLERANCE));
        // Current records carry the split and are never "explained" by the
        // old rate.
        let r = json!({"type":"cost-state","sessionId":"new","totalCostUSD":0.8781468,
            "modelUsage":{"claude-sonnet-5-5":{"inputTokens":2570,"outputTokens":4803,
            "cacheReadInputTokens":2600664,"cacheCreationInputTokens":76211,
            "cacheCreationEphemeral5mInputTokens":0,"cacheCreationEphemeral1hInputTokens":76211,
            "costUSD":0.8781468}}});
        let c = check_record(&t, &r).unwrap();
        assert!(!c.legacy_format && c.legacy_diff.is_none());
        assert!(!c.explained(TOLERANCE));
        let s = summarize(vec![c], TOLERANCE);
        assert_eq!(s.current_format.compared, 1);
        assert_eq!(s.current_format.within, 0);
    }

    #[test]
    fn reads_last_cost_state_from_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("-p");
        std::fs::create_dir_all(proj.join("s1/subagents")).unwrap();
        let line = |usd: f64, out: u64| {
            json!({"type":"cost-state","sessionId":"s1","totalCostUSD":usd,
                   "modelUsage":{"claude-sonnet-5-5":{"outputTokens":out,"costUSD":usd}}})
            .to_string()
        };
        let body = [
            json!({"type":"user","message":{"content":"cost-state is mentioned here"}}).to_string(),
            line(1.0, 100_000),
            json!({"type":"assistant"}).to_string(),
            line(2.0, 200_000),
        ]
        .join("\n");
        std::fs::write(proj.join("s1.jsonl"), body).unwrap();
        let r = check(&PriceTable::embedded(), dir.path()).unwrap();
        assert_eq!(r.sessions, 1);
        assert!(close(r.results[0].reported_usd, 2.0));
        assert!(close(r.results[0].computed_usd.unwrap(), 2.0));
        assert_eq!(r.all.within, 1);
    }
}
