//! Why usage changed between two periods (PLAN 4.D.1 and 4.D.2).
//!
//! A period's amount is split into a context part and an output part:
//!
//! ```text
//! context = requests × context per request × model-mix price × cache price
//! output  = requests × output per request  × model-mix price
//! ```
//!
//! Each part is a product, so its change between periods is decomposed with
//! the log-mean Divisia index (LMDI): factor `x` contributes
//! `L(after, before) × ln(x_after / x_before)`, where `L` is the logarithmic
//! mean. The contributions add up exactly to the change, and the result does
//! not depend on the order the factors are taken in.
//!
//! The amount is measured in a [`Unit`]. In [`Tokens`] every token weighs 1,
//! so cache hit rate and model mix cannot change the amount and contribute
//! nothing; they are still reported so the sentence can mention them. A cost
//! unit (being built in parallel) only has to implement [`Unit`].
//!
//! Only request rows are read, through `query::breakdown` by model.

use crate::query::{self, Dim, Filter, Totals};
use crate::{fmt, Error, Result};
use jiff::civil::Date;
use jiff::{Span, Zoned};
use rusqlite::Connection;
use serde::Serialize;

/// Relative weight of one token of each type for one model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Weights {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl Weights {
    pub const ONES: Weights = Weights {
        input: 1.0,
        output: 1.0,
        cache_write_5m: 1.0,
        cache_write_1h: 1.0,
        cache_read: 1.0,
    };
}

/// What an amount is measured in.
pub trait Unit {
    /// Short id, such as "tokens" or "usd".
    fn id(&self) -> &str;
    /// Weights for a model. `None` (no price known) falls back to 1 per token.
    fn weights(&self, model: &str) -> Option<Weights>;
    /// Format an amount for a sentence.
    fn format(&self, amount: f64) -> String;
}

/// Raw tokens: every token weighs 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokens;

impl Unit for Tokens {
    fn id(&self) -> &str {
        "tokens"
    }
    fn weights(&self, _model: &str) -> Option<Weights> {
        Some(Weights::ONES)
    }
    fn format(&self, amount: f64) -> String {
        fmt::tokens(amount.max(0.0).round() as u64)
    }
}

/// One side of a comparison: whole days `from..=to`, optionally cut at an
/// instant so a day in progress is compared with the same hours earlier.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    pub from: String,
    pub to: String,
    /// Only responses before this instant (Unix ms).
    pub before_ts: Option<i64>,
}

impl Period {
    fn filter(&self, base: &Filter) -> Filter {
        let mut f = base.clone();
        f.from = Some(self.from.clone());
        f.to = Some(self.to.clone());
        f.before_ts = self.before_ts;
        f
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Periods {
    pub before: Period,
    pub after: Period,
    /// "this week vs last week", "last 7 days vs the 7 before".
    pub label: String,
}

fn add_days(d: Date, n: i64) -> Date {
    d.checked_add(Span::new().days(n)).unwrap_or(d)
}

fn week_ago_ms(now: &Zoned, days: i64) -> Result<i64> {
    Ok(now
        .checked_sub(Span::new().days(days))
        .map_err(|e| Error::Other(e.to_string()))?
        .timestamp()
        .as_millisecond())
}

impl Periods {
    /// Monday through now, against last Monday through the same weekday and
    /// time of day. The same comparison as `query::overview`.
    pub fn week_to_date(now: &Zoned) -> Result<Periods> {
        let today = now.date();
        let monday = add_days(today, -(today.weekday().to_monday_zero_offset() as i64));
        Ok(Periods {
            after: Period {
                from: monday.to_string(),
                to: today.to_string(),
                before_ts: None,
            },
            before: Period {
                from: add_days(monday, -7).to_string(),
                to: add_days(today, -7).to_string(),
                before_ts: Some(week_ago_ms(now, 7)?),
            },
            label: "this week vs last week to the same time".into(),
        })
    }

    /// The last `days` days including today, against the `days` days before,
    /// cut at the same time of day. Steadier than [`Self::week_to_date`] early
    /// in the week.
    pub fn trailing(now: &Zoned, days: i64) -> Result<Periods> {
        let days = days.max(1);
        let today = now.date();
        Ok(Periods {
            after: Period {
                from: add_days(today, -(days - 1)).to_string(),
                to: today.to_string(),
                before_ts: None,
            },
            before: Period {
                from: add_days(today, -(2 * days - 1)).to_string(),
                to: add_days(today, -days).to_string(),
                before_ts: Some(week_ago_ms(now, days)?),
            },
            label: format!("last {days} days vs the {days} before"),
        })
    }
}

/// The five drivers, in waterfall order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FactorId {
    Requests,
    ContextPerRequest,
    OutputPerRequest,
    CacheHitRate,
    ModelMix,
}

impl FactorId {
    pub fn label(self) -> &'static str {
        match self {
            FactorId::Requests => "Requests",
            FactorId::ContextPerRequest => "Context per request",
            FactorId::OutputPerRequest => "Output per request",
            FactorId::CacheHitRate => "Cache hit rate",
            FactorId::ModelMix => "Model mix",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Factor {
    pub id: FactorId,
    pub label: String,
    /// The driver's own value in each period: a count, tokens per request, a
    /// rate (0..1), or for model mix the unit's average price index.
    pub before: f64,
    pub after: f64,
    /// Percent change of the driver; for cache hit rate, the change in
    /// percentage points.
    pub change: Option<f64>,
    /// Part of the amount's change caused by this driver, in the unit.
    pub contribution: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub unit: String,
    pub periods: Periods,
    pub before_totals: Totals,
    pub after_totals: Totals,
    pub before: f64,
    pub after: f64,
    pub delta: f64,
    /// Percent change of the amount, None when `before` is 0.
    pub change: Option<f64>,
    /// Percent change of the amount per request.
    pub per_request_change: Option<f64>,
    /// Waterfall steps from `before` to `after`; contributions sum to `delta`.
    pub factors: Vec<Factor>,
    /// One plain-language sentence.
    pub explanation: String,
}

/// Token counts of one period for one model.
#[derive(Debug, Clone, Default)]
pub struct ModelTotals {
    pub model: String,
    pub totals: Totals,
}

/// What the decomposition needs from one period.
#[derive(Debug, Clone, Default)]
struct Side {
    requests: f64,
    ctx: f64,
    out: f64,
    ctx_amount: f64,
    out_amount: f64,
    /// Context tokens by type, all models: input, cw5m, cw1h, cache read.
    mix: [f64; 4],
    /// (context tokens, output tokens, weights) per model.
    models: Vec<(f64, f64, Weights)>,
    hit: f64,
}

/// cw5m + cw1h can be less than cache_write on old rows; count the rest as 5m.
fn ctx_types(t: &Totals) -> [f64; 4] {
    let cw1h = t.cache_write_1h.min(t.cache_write);
    [
        t.input as f64,
        (t.cache_write - cw1h) as f64,
        cw1h as f64,
        t.cache_read as f64,
    ]
}

fn ctx_price(w: &Weights, types: &[f64; 4]) -> f64 {
    let n: f64 = types.iter().sum();
    if n <= 0.0 {
        return 0.0;
    }
    (types[0] * w.input
        + types[1] * w.cache_write_5m
        + types[2] * w.cache_write_1h
        + types[3] * w.cache_read)
        / n
}

fn side(rows: &[ModelTotals], unit: &dyn Unit) -> Side {
    let mut s = Side::default();
    for r in rows {
        let t = &r.totals;
        let w = unit.weights(&r.model).unwrap_or(Weights::ONES);
        let types = ctx_types(t);
        let ctx: f64 = types.iter().sum();
        s.requests += t.requests as f64;
        s.ctx += ctx;
        s.out += t.output as f64;
        s.ctx_amount += ctx * ctx_price(&w, &types);
        s.out_amount += t.output as f64 * w.output;
        for (a, b) in s.mix.iter_mut().zip(types) {
            *a += b;
        }
        s.models.push((ctx, t.output as f64, w));
    }
    if s.ctx > 0.0 {
        s.hit = s.mix[3] / s.ctx;
    }
    s
}

fn sum_totals(rows: &[ModelTotals]) -> Totals {
    let mut a = Totals::default();
    for r in rows {
        let t = &r.totals;
        a.requests += t.requests;
        a.input += t.input;
        a.output += t.output;
        a.thinking += t.thinking;
        a.cache_write += t.cache_write;
        a.cache_write_5m += t.cache_write_5m;
        a.cache_write_1h += t.cache_write_1h;
        a.cache_read += t.cache_read;
        a.cost_usd += t.cost_usd;
        a.weighted += t.weighted;
        a.unpriced += t.unpriced;
        a.web_search += t.web_search;
        a.web_fetch += t.web_fetch;
    }
    let ctx = a.input + a.cache_write + a.cache_read;
    a.total = ctx + a.output;
    if a.requests > 0 {
        a.output_per_request = a.output as f64 / a.requests as f64;
        a.context_per_request = ctx as f64 / a.requests as f64;
    }
    if ctx > 0 {
        a.cache_hit_rate = a.cache_read as f64 / ctx as f64;
    }
    a
}

/// Logarithmic mean.
fn logmean(a: f64, b: f64) -> f64 {
    if (a - b).abs() <= 1e-12 * a.abs().max(b.abs()) {
        a
    } else {
        (a - b) / (a.ln() - b.ln())
    }
}

fn ln_ratio(after: f64, before: f64) -> f64 {
    (after / before).ln()
}

fn pct(before: f64, after: f64) -> Option<f64> {
    fmt::change(before, after)
}

/// Decompose the change from `before` to `after` (per-model rows of each
/// period). Pure: no database access.
pub fn decompose(
    before: &[ModelTotals],
    after: &[ModelTotals],
    unit: &dyn Unit,
    periods: Periods,
) -> Change {
    let (b, a) = (side(before, unit), side(after, unit));
    // Reference split of context tokens by type, pooled over both periods, so
    // the model-mix index does not move when only the hit rate moves.
    let mut pool = [0.0; 4];
    for (i, p) in pool.iter_mut().enumerate() {
        *p = b.mix[i] + a.mix[i];
    }
    let mix_ctx = |s: &Side| -> f64 {
        if s.ctx <= 0.0 {
            return 0.0;
        }
        s.models
            .iter()
            .map(|(c, _, w)| c / s.ctx * ctx_price(w, &pool))
            .sum()
    };
    let mix_out = |s: &Side| -> f64 {
        if s.out <= 0.0 {
            return 0.0;
        }
        s.models.iter().map(|(_, o, w)| o / s.out * w.output).sum()
    };

    let per = |x: f64, n: f64| if n > 0.0 { x / n } else { 0.0 };
    let (c0, c1) = (per(b.ctx, b.requests), per(a.ctx, a.requests));
    let (o0, o1) = (per(b.out, b.requests), per(a.out, a.requests));
    let (m0, m1) = (mix_ctx(&b), mix_ctx(&a));
    let (p0, p1) = (mix_out(&b), mix_out(&a));
    let (h0, h1) = (
        if m0 > 0.0 {
            per(b.ctx_amount, b.ctx) / m0
        } else {
            0.0
        },
        if m1 > 0.0 {
            per(a.ctx_amount, a.ctx) / m1
        } else {
            0.0
        },
    );

    let mut contrib = [0.0f64; 5];
    // Context part: N × c × M × H.
    let (x0, x1) = (b.ctx_amount, a.ctx_amount);
    if x0 > 0.0 && x1 > 0.0 {
        let l = logmean(x1, x0);
        contrib[0] += l * ln_ratio(a.requests, b.requests);
        contrib[1] += l * ln_ratio(c1, c0);
        contrib[4] += l * ln_ratio(m1, m0);
        contrib[3] += l * ln_ratio(h1, h0);
    } else {
        // Usage started or stopped: nothing to split, it is all volume.
        contrib[0] += x1 - x0;
    }
    // Output part: N × o × P.
    let (y0, y1) = (b.out_amount, a.out_amount);
    if y0 > 0.0 && y1 > 0.0 {
        let l = logmean(y1, y0);
        contrib[0] += l * ln_ratio(a.requests, b.requests);
        contrib[2] += l * ln_ratio(o1, o0);
        contrib[4] += l * ln_ratio(p1, p0);
    } else if b.requests > 0.0 && a.requests > 0.0 {
        contrib[2] += y1 - y0;
    } else {
        contrib[0] += y1 - y0;
    }

    let total0 = x0 + y0;
    let total1 = x1 + y1;
    // Combined price index for display: context and output mix weighted by
    // the after-period's amounts.
    let mix_show = |m: f64, p: f64, s: &Side| {
        let w = s.ctx_amount + s.out_amount;
        if w > 0.0 {
            (m * s.ctx_amount + p * s.out_amount) / w
        } else {
            m.max(p)
        }
    };
    let factors = vec![
        Factor {
            id: FactorId::Requests,
            label: FactorId::Requests.label().into(),
            before: b.requests,
            after: a.requests,
            change: pct(b.requests, a.requests),
            contribution: contrib[0],
        },
        Factor {
            id: FactorId::ContextPerRequest,
            label: FactorId::ContextPerRequest.label().into(),
            before: c0,
            after: c1,
            change: pct(c0, c1),
            contribution: contrib[1],
        },
        Factor {
            id: FactorId::OutputPerRequest,
            label: FactorId::OutputPerRequest.label().into(),
            before: o0,
            after: o1,
            change: pct(o0, o1),
            contribution: contrib[2],
        },
        Factor {
            id: FactorId::CacheHitRate,
            label: FactorId::CacheHitRate.label().into(),
            before: b.hit,
            after: a.hit,
            change: (b.ctx > 0.0 && a.ctx > 0.0).then_some((a.hit - b.hit) * 100.0),
            contribution: contrib[3],
        },
        Factor {
            id: FactorId::ModelMix,
            label: FactorId::ModelMix.label().into(),
            before: mix_show(m0, p0, &b),
            after: mix_show(m1, p1, &a),
            change: pct(mix_show(m0, p0, &b), mix_show(m1, p1, &a)),
            contribution: contrib[4],
        },
    ];
    let per_request_change = pct(per(total0, b.requests), per(total1, a.requests));
    let mut c = Change {
        unit: unit.id().to_string(),
        periods,
        before_totals: sum_totals(before),
        after_totals: sum_totals(after),
        before: total0,
        after: total1,
        delta: total1 - total0,
        change: pct(total0, total1),
        per_request_change,
        factors,
        explanation: String::new(),
    };
    c.explanation = explain(&c, unit);
    c
}

/// A driver's change, as written in the sentence: "Requests −39%".
fn factor_phrase(f: &Factor) -> Option<String> {
    let ch = f.change?;
    let sign = if ch < 0.0 { "−" } else { "+" };
    Some(match f.id {
        FactorId::CacheHitRate => format!("cache hit rate {sign}{:.0} pts", ch.abs()),
        FactorId::Requests => format!("Requests {sign}{:.0}%", ch.abs()),
        _ => format!("{} {sign}{:.0}%", f.label.to_lowercase(), ch.abs()),
    })
}

/// What the main driver means, in words.
fn cause(f: &Factor) -> &'static str {
    let up = f.contribution > 0.0;
    match (f.id, up) {
        (FactorId::Requests, true) => "you worked more",
        (FactorId::Requests, false) => "you worked less",
        (FactorId::ContextPerRequest, true) => "each request carried more context",
        (FactorId::ContextPerRequest, false) => "each request carried less context",
        (FactorId::OutputPerRequest, true) => "the model wrote more per request",
        (FactorId::OutputPerRequest, false) => "the model wrote less per request",
        (FactorId::CacheHitRate, true) => "less of the prompt came from cache",
        (FactorId::CacheHitRate, false) => "more of the prompt came from cache",
        (FactorId::ModelMix, true) => "more of the work ran on pricier models",
        (FactorId::ModelMix, false) => "more of the work ran on cheaper models",
    }
}

/// Changes smaller than this (percent, or points for the hit rate) are left
/// out of the sentence.
const NOTICEABLE: f64 = 3.0;

fn explain(c: &Change, unit: &dyn Unit) -> String {
    if c.before <= 0.0 && c.after <= 0.0 {
        return "No usage in either period.".into();
    }
    if c.before <= 0.0 {
        return format!(
            "No usage in the earlier period; {} since.",
            unit.format(c.after)
        );
    }
    if c.after <= 0.0 {
        return "No usage in this period yet.".into();
    }
    // Drivers that moved, largest effect first. In tokens, cache hit rate and
    // model mix have no effect but are still worth naming when they moved a lot.
    let mut moved: Vec<&Factor> = c
        .factors
        .iter()
        .filter(|f| f.change.is_some_and(|x| x.abs() >= NOTICEABLE))
        .collect();
    moved.sort_by(|a, b| {
        b.contribution.abs().total_cmp(&a.contribution.abs()).then(
            b.change
                .unwrap_or(0.0)
                .abs()
                .total_cmp(&a.change.unwrap_or(0.0).abs()),
        )
    });
    let mut parts: Vec<String> = moved
        .iter()
        .take(3)
        .filter_map(|f| factor_phrase(f))
        .collect();
    if let Some(p) = parts.first_mut() {
        // Sentence case for the first driver.
        let mut ch = p.chars();
        if let Some(f) = ch.next() {
            *p = f.to_uppercase().collect::<String>() + ch.as_str();
        }
    }
    let total = c.change.unwrap_or(0.0);
    let head = if parts.is_empty() {
        format!("Usage {}", signed_pct(total))
    } else {
        parts.join(", ")
    };
    let main = c
        .factors
        .iter()
        .filter(|f| f.contribution.abs() > 0.0)
        .max_by(|a, b| a.contribution.abs().total_cmp(&b.contribution.abs()));
    let per = c.per_request_change.unwrap_or(0.0);
    let per_clause = if per.abs() < 5.0 {
        "each request cost about the same".to_string()
    } else if per > 0.0 {
        format!("each request cost {per:.0}% more")
    } else {
        format!("each request cost {:.0}% less", per.abs())
    };
    match main {
        Some(m) if total.abs() >= NOTICEABLE => {
            let why = cause(m);
            if m.id == FactorId::Requests {
                format!("{head}: {why}; {per_clause}.")
            } else {
                format!("{head}: {why}.")
            }
        }
        _ => format!("{head}: about the same as before; {per_clause}."),
    }
}

fn signed_pct(p: f64) -> String {
    if p < 0.0 {
        format!("−{:.0}%", p.abs())
    } else {
        format!("+{p:.0}%")
    }
}

/// Per-model totals of one period, through `query::breakdown`.
pub fn model_totals(conn: &Connection, base: &Filter, p: &Period) -> Result<Vec<ModelTotals>> {
    Ok(query::breakdown(conn, &p.filter(base), Dim::Model, 10_000)?
        .into_iter()
        .map(|g| ModelTotals {
            model: g.key,
            totals: g.totals,
        })
        .collect())
}

/// Compare two periods in `unit`, keeping `base`'s model, project, session,
/// subagent and weekday filters (its dates are replaced).
pub fn compare(
    conn: &Connection,
    base: &Filter,
    periods: Periods,
    unit: &dyn Unit,
) -> Result<Change> {
    let b = model_totals(conn, base, &periods.before)?;
    let a = model_totals(conn, base, &periods.after)?;
    Ok(decompose(&b, &a, unit, periods))
}

/// This week against last week to the same time of day, in tokens.
pub fn week_over_week(conn: &Connection, base: &Filter, now: &Zoned) -> Result<Change> {
    compare(conn, base, Periods::week_to_date(now)?, &Tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(requests: u64, input: u64, cw: u64, cr: u64, out: u64) -> Totals {
        Totals {
            requests,
            input,
            cache_write: cw,
            cache_write_5m: cw,
            cache_read: cr,
            output: out,
            ..Default::default()
        }
    }

    fn m(model: &str, totals: Totals) -> ModelTotals {
        ModelTotals {
            model: model.into(),
            totals,
        }
    }

    fn periods() -> Periods {
        Periods {
            before: Period {
                from: "2026-11-02".into(),
                to: "2026-11-08".into(),
                before_ts: None,
            },
            after: Period {
                from: "2026-11-09".into(),
                to: "2026-11-15".into(),
                before_ts: None,
            },
            label: "test".into(),
        }
    }

    fn sum(c: &Change) -> f64 {
        c.factors.iter().map(|f| f.contribution).sum()
    }

    #[test]
    fn contributions_add_up() {
        let b = [m("opus", t(100, 1000, 9000, 90000, 5000))];
        let a = [m("opus", t(61, 700, 6000, 58000, 3500))];
        let c = decompose(&b, &a, &Tokens, periods());
        assert!(
            (sum(&c) - c.delta).abs() < 1e-6,
            "{} vs {}",
            sum(&c),
            c.delta
        );
        assert_eq!(c.factors[3].contribution, 0.0);
        assert_eq!(c.factors[4].contribution, 0.0);
    }

    #[test]
    fn fewer_requests_same_cost_each() {
        // −39% requests, +4% context per request.
        let b = [m("opus", t(100, 0, 0, 100_000, 0))];
        let a = [m("opus", t(61, 0, 0, 63_440, 0))];
        let c = decompose(&b, &a, &Tokens, periods());
        assert_eq!(
            c.explanation,
            "Requests −39%, context per request +4%: you worked less; each request cost about the same."
        );
    }

    #[test]
    fn bigger_context_is_named() {
        let b = [m("opus", t(100, 0, 0, 100_000, 1_000))];
        let a = [m("opus", t(100, 0, 0, 180_000, 1_000))];
        let c = decompose(&b, &a, &Tokens, periods());
        assert!(c.explanation.starts_with("Context per request +80%"));
        assert!(c.explanation.contains("each request carried more context"));
        assert!((sum(&c) - c.delta).abs() < 1e-6);
    }

    struct Priced;
    impl Unit for Priced {
        fn id(&self) -> &str {
            "usd"
        }
        fn weights(&self, model: &str) -> Option<Weights> {
            let k = if model == "opus" { 5.0 } else { 1.0 };
            Some(Weights {
                input: k,
                output: 5.0 * k,
                cache_write_5m: 1.25 * k,
                cache_write_1h: 2.0 * k,
                cache_read: 0.1 * k,
            })
        }
        fn format(&self, amount: f64) -> String {
            format!("${amount:.2}")
        }
    }

    #[test]
    fn priced_unit_attributes_mix_and_cache() {
        // Same tokens, but work moved to the pricier model and the cache
        // hit rate fell.
        let b = [
            m("opus", t(50, 0, 1000, 49_000, 500)),
            m("sonnet", t(50, 0, 1000, 49_000, 500)),
        ];
        let a = [
            m("opus", t(80, 0, 8000, 72_000, 800)),
            m("sonnet", t(20, 0, 2000, 18_000, 200)),
        ];
        let c = decompose(&b, &a, &Priced, periods());
        assert!((sum(&c) - c.delta).abs() < 1e-6 * c.delta.abs().max(1.0));
        assert!(c.factors[4].contribution > 0.0, "mix");
        assert!(c.factors[3].contribution > 0.0, "cache");
        assert!(c.factors[0].contribution.abs() < 1e-9, "requests unchanged");
        assert_eq!(c.factors[3].change.map(|x| x.round()), Some(-8.0));
    }

    #[test]
    fn empty_periods() {
        let c = decompose(&[], &[], &Tokens, periods());
        assert_eq!(c.explanation, "No usage in either period.");
        let a = [m("opus", t(10, 0, 0, 1000, 10))];
        let c = decompose(&[], &a, &Tokens, periods());
        assert!((sum(&c) - c.delta).abs() < 1e-9);
        assert!(c.explanation.starts_with("No usage in the earlier period"));
    }

    #[test]
    fn periods_line_up() {
        let now: Zoned = "2026-11-16T15:00[UTC]".parse().unwrap();
        let p = Periods::week_to_date(&now).unwrap();
        assert_eq!(p.after.from, "2026-11-16");
        assert_eq!(p.before.from, "2026-11-09");
        assert_eq!(p.before.to, "2026-11-09");
        let p = Periods::trailing(&now, 7).unwrap();
        assert_eq!(p.after.from, "2026-11-10");
        assert_eq!(p.before.from, "2026-11-03");
        assert_eq!(p.before.to, "2026-11-09");
    }
}
