//! The context ledger: for every request, which pieces were in context and
//! how many tokens each accounted for. Per request the pieces always sum to
//! the exact context size, so the ledger total equals the session's real
//! context tokens.
//!
//! Two engines:
//!
//! - **Exact** (request snapshots): `api-request` records list the messages
//!   of each request (base's first `keep` messages + `tail` hashes). The
//!   pieces in context are exactly those messages' pieces, plus the system
//!   prompt and tool definitions of the request's shape. Earlier thinking is
//!   kept or dropped per `params.context_management`. Sizes inside the
//!   request are estimates scaled to the exact total; the model's own output
//!   is exact.
//! - **Estimated** (older transcripts): the growth between two requests is
//!   split across what the transcript added in between; pieces stay until a
//!   compaction.
//!
//! The estimated engine runs on every request to keep its state; a request
//! uses the exact engine's attribution when its snapshot resolves.

use super::labels::*;
use super::read::{Kind, Piece, ThinkingPolicy, Trace};
use serde::Serialize;
use std::collections::BTreeMap;

pub const DEFAULT_BYTES_PER_TOKEN: f64 = 3.6;
pub const DEFAULT_IMAGE_TOKENS: f64 = 1_600.0;
/// Calibration needs at least this many clean steps per content type.
const MIN_SAMPLES: usize = 3;
/// A line fit needs more samples than a median.
const FIT_SAMPLES: usize = 8;
/// A request snapshot counts as exact when at most this share of its
/// messages could not be matched to transcript content.
const MAX_UNRESOLVED: f64 = 0.1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Bytes per token for prompts, injected text, summaries.
    pub prose_bytes_per_token: f64,
    /// Bytes per token for tool results and tool definitions.
    pub data_bytes_per_token: f64,
    pub image_tokens: f64,
    pub prose_samples: u32,
    pub data_samples: u32,
    pub image_samples: u32,
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration {
            prose_bytes_per_token: DEFAULT_BYTES_PER_TOKEN,
            data_bytes_per_token: DEFAULT_BYTES_PER_TOKEN,
            image_tokens: DEFAULT_IMAGE_TOKENS,
            prose_samples: 0,
            data_samples: 0,
            image_samples: 0,
        }
    }
}

impl Calibration {
    pub fn est(&self, p: &Piece) -> f64 {
        let img = p.images as f64 * self.image_tokens;
        let b = p.bytes as f64;
        let t = match p.kind {
            Kind::Output => return p.exact.unwrap_or(b / self.prose_bytes_per_token),
            Kind::Prose => b / self.prose_bytes_per_token,
            Kind::Data | Kind::Schema | Kind::Image => b / self.data_bytes_per_token,
        };
        if t + img > 0.0 {
            (t + img).max(1.0)
        } else {
            0.0
        }
    }
}

/// Bytes per token from (bytes, tokens) steps. Each step also carries a
/// small fixed cost (message framing, measured at a few hundred tokens), so
/// the slope of a least-squares line is the honest factor; with too few or
/// too uniform samples, the median ratio.
fn bytes_per_token(s: &[(f64, f64)]) -> Option<f64> {
    if s.len() < MIN_SAMPLES {
        return None;
    }
    let n = s.len() as f64;
    let mx = s.iter().map(|p| p.0).sum::<f64>() / n;
    let my = s.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = s.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = s.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if s.len() >= FIT_SAMPLES && sxx > 0.0 && sxy > 0.0 {
        let slope = sxy / sxx;
        let intercept = my - slope * mx;
        // A negative intercept beyond noise means the line does not fit.
        if intercept > -0.25 * my {
            return Some(1.0 / slope);
        }
    }
    let mut r: Vec<f64> = s.iter().map(|p| p.0 / p.1).collect();
    Some(median(&mut r))
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Bytes per token per content type, from steps where the exact growth is
/// known and only one kind of content was added (besides the previous
/// response, whose size is exact). With request snapshots the added content
/// is exactly the messages appended since the previous request; without
/// them it is what the transcript logged in between.
/// The shape a request was sent with: its snapshot's, else the latest seen.
pub fn shape_of(t: &Trace, r: &super::read::Req) -> Option<u32> {
    r.api
        .and_then(|a| t.apis[a as usize].shape.as_deref())
        .and_then(|h| t.shape_index(h))
        .or(r.shape)
}

pub fn calibrate(t: &Trace, lists: &[Option<Vec<u32>>]) -> Calibration {
    let (mut prose, mut data, mut image) = (Vec::new(), Vec::new(), Vec::new());
    let list = |r: &super::read::Req| r.api.and_then(|a| lists[a as usize].as_ref());
    for w in t.reqs.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.compact_before || shape_of(t, a) != shape_of(t, b) || b.ctx < a.ctx {
            continue;
        }
        if b.api
            .is_some_and(|i| t.apis[i as usize].thinking != ThinkingPolicy::All)
        {
            continue;
        }
        let added: Vec<u32> = match (list(a), list(b)) {
            (Some(la), Some(lb)) => {
                if lb.len() < la.len() || lb[..la.len()] != la[..] {
                    continue;
                }
                let new = &lb[la.len()..];
                if new.iter().any(|&h| t.hash_pieces[h as usize].is_empty()) {
                    continue;
                }
                new.iter()
                    .flat_map(|&h| t.hash_pieces[h as usize].iter().copied())
                    .collect()
            }
            (None, None) => b.added.clone(),
            _ => continue,
        };
        let mut rest = b.ctx as f64 - a.ctx as f64;
        let (mut bytes, mut images) = (0u64, 0u32);
        let mut kinds = (false, false, false);
        for &p in &added {
            let p = &t.pieces[p as usize];
            match (p.kind, p.exact) {
                (Kind::Output, Some(e)) => rest -= e,
                (Kind::Prose, _) if p.images == 0 => {
                    kinds.0 = true;
                    bytes += p.bytes
                }
                (Kind::Data, _) if p.images == 0 => {
                    kinds.1 = true;
                    bytes += p.bytes
                }
                (Kind::Image, _) if p.bytes < 200 => {
                    kinds.2 = true;
                    images += p.images
                }
                _ => kinds = (true, true, true),
            }
        }
        if rest < 200.0 {
            continue;
        }
        match kinds {
            (true, false, false) if bytes >= 1_000 => prose.push((bytes as f64, rest)),
            (false, true, false) if bytes >= 1_000 => data.push((bytes as f64, rest)),
            (false, false, true) if images > 0 => image.push(rest / images as f64),
            _ => {}
        }
    }
    let mut c = Calibration::default();
    if let Some(f) = bytes_per_token(&prose) {
        c.prose_bytes_per_token = f.clamp(1.5, 6.0);
    }
    if let Some(f) = bytes_per_token(&data) {
        c.data_bytes_per_token = f.clamp(1.5, 6.0);
    }
    if image.len() >= MIN_SAMPLES {
        c.image_tokens = median(&mut image).clamp(200.0, 6_000.0);
    }
    c.prose_samples = prose.len() as u32;
    c.data_samples = data.len() as u32;
    c.image_samples = image.len() as u32;
    c
}

#[derive(Debug, Default)]
pub struct Ledger {
    /// Context tokens each piece accounted for, summed over all requests.
    pub carried: Vec<f64>,
    /// Size when it entered the context.
    pub entry: Vec<f64>,
    pub first: Vec<Option<u32>>,
    pub requests: Vec<u32>,
    pub exact_req: Vec<bool>,
    pub added: Vec<f64>,
    pub rebuild: Vec<bool>,
    pub gap_ms: Vec<i64>,
    /// Cache writes by (label, group).
    pub cache_writes: BTreeMap<(String, &'static str), f64>,
    pub policies: BTreeMap<String, u64>,
}

#[derive(Default)]
struct Live {
    size: Vec<f64>,
    on: Vec<bool>,
    members: Vec<u32>,
}

impl Live {
    fn grow(&mut self, n: usize) {
        if self.size.len() < n {
            self.size.resize(n, 0.0);
            self.on.resize(n, false);
        }
    }
    fn clear(&mut self) {
        for &m in &self.members {
            self.size[m as usize] = 0.0;
            self.on[m as usize] = false;
        }
        self.members.clear();
    }
    fn add(&mut self, p: u32, t: f64) {
        self.grow(p as usize + 1);
        if !self.on[p as usize] {
            self.on[p as usize] = true;
            self.members.push(p);
        }
        self.size[p as usize] += t;
    }
    fn remove(&mut self, p: u32) -> f64 {
        if (p as usize) >= self.on.len() || !self.on[p as usize] {
            return 0.0;
        }
        self.on[p as usize] = false;
        self.members.retain(|&m| m != p);
        std::mem::take(&mut self.size[p as usize])
    }
    fn total(&self) -> f64 {
        self.members.iter().map(|&m| self.size[m as usize]).sum()
    }
    fn scale(&mut self, k: f64) {
        for &m in &self.members {
            self.size[m as usize] *= k;
        }
    }
}

fn new_piece(t: &mut Trace, label: &str, group: &'static str, kind: Kind) -> u32 {
    t.pieces.push(Piece {
        label: label.into(),
        group,
        kind,
        bytes: 0,
        images: 0,
        exact: None,
        thinking: false,
        tool: None,
        source: None,
        ts: 0,
        turn: None,
        from_attachment: false,
    });
    (t.pieces.len() - 1) as u32
}

/// Message hashes of every snapshot, resolved through `base`/`keep`.
pub fn reconstruct(t: &Trace) -> Vec<Option<Vec<u32>>> {
    let mut out: Vec<Option<Vec<u32>>> = Vec::with_capacity(t.apis.len());
    for a in &t.apis {
        let list = match &a.base {
            None => Some(a.tail.clone()),
            Some(b) => match t.api_ids.get(b) {
                // A base always precedes the request that refers to it.
                Some(&bi) if (bi as usize) < out.len() => out[bi as usize].as_ref().map(|base| {
                    let keep = a.keep.min(base.len());
                    let mut v = Vec::with_capacity(keep + a.tail.len());
                    v.extend_from_slice(&base[..keep]);
                    v.extend_from_slice(&a.tail);
                    v
                }),
                _ => None,
            },
        };
        out.push(list);
    }
    out
}

pub fn run(t: &mut Trace, cal: &Calibration, lists: &[Option<Vec<u32>>]) -> Ledger {
    let first_shape = t.reqs.iter().find_map(|r| r.shape).or_else(|| {
        // A shape logged after the last request still describes the session.
        (!t.shapes.is_empty()).then_some(0)
    });
    let unattributed = new_piece(t, UNATTRIBUTED, "Other", Kind::Prose);
    let start = new_piece(t, START_OF_CONTEXT, "Overhead", Kind::Prose);
    let n = t.reqs.len();
    let mut l = Ledger {
        exact_req: vec![false; n],
        added: vec![0.0; n],
        rebuild: vec![false; n],
        gap_ms: vec![0; n],
        ..Default::default()
    };
    let mut est_live = Live::default();
    let mut est_shape: Option<u32> = None;
    let mut used_attachment = vec![false; t.pieces.len()];
    let mut placeholder = vec![false; t.hash_pieces.len()];
    let mut prev_ctx: Option<u64> = None;
    let mut prev_ts: Option<i64> = None;
    let mut last_seen: Vec<Option<u32>> = Vec::new();

    for ri in 0..n {
        let r = t.reqs[ri].clone();
        let shape = r.shape.or(first_shape);
        let ctx = r.ctx as f64;

        // Estimated engine.
        est_live.grow(t.pieces.len());
        let mut rest;
        let mut start_fill = false;
        if prev_ctx.is_none() || r.compact_before {
            est_live.clear();
            match shape {
                Some(s) => {
                    for &p in &t.shapes[s as usize].pieces {
                        est_live.add(p, cal.est(&t.pieces[p as usize]));
                    }
                    let o = est_live.total();
                    if o > ctx && o > 0.0 {
                        est_live.scale(ctx / o);
                    }
                    rest = ctx - est_live.total();
                }
                None => {
                    // No shape at all (older Claude Code): whatever the
                    // added pieces do not explain is the start of context.
                    start_fill = true;
                    rest = ctx;
                }
            }
            est_shape = shape;
        } else {
            rest = ctx - prev_ctx.unwrap_or(0) as f64;
            if shape != est_shape {
                if let Some(old) = est_shape {
                    for &p in &t.shapes[old as usize].pieces {
                        rest += est_live.remove(p);
                    }
                }
                if let Some(s) = shape {
                    for &p in &t.shapes[s as usize].pieces {
                        let e = cal.est(&t.pieces[p as usize]);
                        est_live.add(p, e);
                        rest -= e;
                    }
                }
                est_shape = shape;
            }
        }
        let mut rough = 0.0;
        for &p in &r.added {
            let pc = &t.pieces[p as usize];
            if let Some(e) = pc.exact {
                est_live.add(p, e);
                rest -= e;
            } else {
                rough += cal.est(pc);
            }
        }
        if start_fill {
            let k = if rough > rest.max(0.0) && rough > 0.0 {
                rest.max(0.0) / rough
            } else {
                1.0
            };
            for &p in &r.added {
                let pc = &t.pieces[p as usize];
                if pc.exact.is_none() {
                    est_live.add(p, cal.est(pc) * k);
                }
            }
            let left = rest - rough * k;
            if left > 0.0 {
                est_live.add(start, left);
            }
        } else if rough > 0.0 {
            let k = if rest > 0.0 { rest / rough } else { 1.0 };
            for &p in &r.added {
                let pc = &t.pieces[p as usize];
                if pc.exact.is_none() {
                    est_live.add(p, cal.est(pc) * k);
                }
            }
        } else if rest > 0.0 {
            est_live.add(unattributed, rest);
        }
        let sum = est_live.total();
        if sum > 0.0 && (sum - ctx).abs() > 1e-6 {
            est_live.scale(ctx / sum);
        } else if sum <= 0.0 && ctx > 0.0 {
            est_live.add(unattributed, ctx);
        }

        // Exact engine.
        let mut exact: Option<Vec<(u32, f64)>> = None;
        if let Some(list) = r.api.and_then(|a| lists[a as usize].as_ref()) {
            let api = &t.apis[r.api.unwrap() as usize];
            let policy = api.thinking;
            *l.policies
                .entry(match policy {
                    ThinkingPolicy::All => "all".to_string(),
                    ThinkingPolicy::Last(k) => format!("last {k}"),
                })
                .or_default() += 1;
            let shape = api
                .shape
                .as_deref()
                .and_then(|h| t.shape_index(h))
                .or(shape);
            // Attachments that entered at this request and are not part of
            // any matched message: Claude Code merged them into a message we
            // could not match by hash.
            let mut fresh: Vec<u32> = r
                .added
                .iter()
                .copied()
                .filter(|&p| t.pieces[p as usize].from_attachment && !used_attachment[p as usize])
                .collect();
            let mut unresolved = 0usize;
            let mut occ: Vec<u32> = Vec::with_capacity(list.len() * 2);
            for &h in list {
                let h = h as usize;
                if t.hash_pieces[h].is_empty() {
                    if !fresh.is_empty() {
                        for &p in &fresh {
                            used_attachment[p as usize] = true;
                        }
                        t.hash_pieces[h] = std::mem::take(&mut fresh);
                    } else {
                        t.hash_pieces[h] = vec![unattributed];
                        placeholder[h] = true;
                    }
                }
                if placeholder[h] {
                    unresolved += 1;
                }
                occ.extend_from_slice(&t.hash_pieces[h]);
            }
            if (unresolved as f64) <= MAX_UNRESOLVED * list.len().max(1) as f64 {
                // Thinking kept per the request's context management.
                let mut drop_thinking = std::collections::HashSet::new();
                if let ThinkingPolicy::Last(k) = policy {
                    let mut seen = 0usize;
                    for &h in list.iter().rev() {
                        let ps = &t.hash_pieces[h as usize];
                        let think: Vec<u32> = ps
                            .iter()
                            .copied()
                            .filter(|&p| t.pieces[p as usize].thinking)
                            .collect();
                        if !think.is_empty() {
                            seen += 1;
                            if seen > k {
                                drop_thinking.extend(think);
                            }
                        }
                    }
                }
                let mut items: Vec<(u32, f64, bool)> = Vec::with_capacity(occ.len() + 8);
                if let Some(s) = shape {
                    for &p in &t.shapes[s as usize].pieces {
                        items.push((p, cal.est(&t.pieces[p as usize]), false));
                    }
                }
                for p in occ {
                    if p == unattributed || drop_thinking.contains(&p) {
                        continue;
                    }
                    let pc = &t.pieces[p as usize];
                    match pc.exact {
                        Some(e) => items.push((p, e, true)),
                        None => items.push((p, cal.est(pc), false)),
                    }
                }
                let fixed: f64 = items.iter().filter(|i| i.2).map(|i| i.1).sum();
                let est: f64 = items.iter().filter(|i| !i.2).map(|i| i.1).sum();
                let mut out: Vec<(u32, f64)> = Vec::with_capacity(items.len() + 1);
                if fixed <= ctx {
                    let k = if est > 0.0 { (ctx - fixed) / est } else { 0.0 };
                    for (p, s, ex) in items {
                        out.push((p, if ex { s } else { s * k }));
                    }
                    if est <= 0.0 && ctx > fixed {
                        out.push((unattributed, ctx - fixed));
                    }
                } else {
                    let k = ctx / (fixed + est);
                    for (p, s, _) in items {
                        out.push((p, s * k));
                    }
                }
                exact = Some(out);
            }
        }

        // Accumulate from the chosen engine.
        let chosen: Vec<(u32, f64)> = match exact {
            Some(v) => {
                l.exact_req[ri] = true;
                v
            }
            None => est_live
                .members
                .iter()
                .map(|&m| (m, est_live.size[m as usize]))
                .collect(),
        };
        let np = t.pieces.len();
        if l.carried.len() < np {
            l.carried.resize(np, 0.0);
            l.entry.resize(np, 0.0);
            l.first.resize(np, None);
            l.requests.resize(np, 0);
            last_seen.resize(np, None);
        }
        let mut added = 0.0;
        let mut added_by: BTreeMap<(String, &'static str), f64> = BTreeMap::new();
        let ri32 = ri as u32;
        for &(p, s) in &chosen {
            let pi = p as usize;
            l.carried[pi] += s;
            if last_seen[pi] != Some(ri32) {
                l.requests[pi] += 1;
            }
            let new = match last_seen[pi] {
                None => true,
                Some(x) => x + 1 < ri32,
            };
            if l.first[pi].is_none() {
                l.first[pi] = Some(ri32);
                l.entry[pi] = s;
            }
            if new || (r.compact_before && p != unattributed) {
                added += s;
                let pc = &t.pieces[pi];
                *added_by.entry((pc.label.clone(), pc.group)).or_default() += s;
            }
            last_seen[pi] = Some(ri32);
        }
        l.added[ri] = added;

        // Cache writes: a rebuild after idle rewrites everything; otherwise
        // the write is the new content, and any excess re-wrote what was
        // already there.
        let gap = prev_ts.map(|p| r.ts - p).unwrap_or(0);
        l.gap_ms[ri] = gap;
        let rebuild = prev_ctx.is_some_and(|p| p > 0)
            && !r.compact_before
            && r.cw > 20_000
            && r.cw as f64 > 0.5 * ctx
            && gap > 5 * 60_000;
        l.rebuild[ri] = rebuild;
        let cw = r.cw as f64;
        if cw > 0.0 {
            if rebuild {
                *l.cache_writes
                    .entry((CACHE_REBUILD.into(), "Cache rebuild"))
                    .or_default() += cw;
            } else {
                let fresh = cw.min(added);
                if added > 0.0 {
                    for (k, v) in added_by {
                        *l.cache_writes.entry(k).or_default() += fresh * v / added;
                    }
                }
                let excess = cw - fresh;
                let total: f64 = chosen.iter().map(|c| c.1).sum();
                if excess > 0.0 && total > 0.0 {
                    for &(p, s) in &chosen {
                        let pc = &t.pieces[p as usize];
                        *l.cache_writes
                            .entry((pc.label.clone(), pc.group))
                            .or_default() += excess * s / total;
                    }
                }
            }
        }
        prev_ctx = Some(r.ctx);
        prev_ts = Some(r.ts);
    }
    let np = t.pieces.len();
    l.carried.resize(np, 0.0);
    l.entry.resize(np, 0.0);
    l.first.resize(np, None);
    l.requests.resize(np, 0);
    l
}
