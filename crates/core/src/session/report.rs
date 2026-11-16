//! Builds the [`SessionAnalysis`] from a trace and its ledger.

use super::labels::*;
use super::ledger::{self, Calibration, Ledger};
use super::read::{Kind, Trace};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Timeline points kept; longer sessions are bucketed.
const MAX_POINTS: usize = 600;
const TOP_PIECES: usize = 20;
const SUBAGENT_TOP: usize = 10;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    pub label: String,
    /// See [`super::labels::GROUPS`].
    pub group: String,
    /// Context tokens this piece accounted for, summed over all requests
    /// (for `outputSplit`: output tokens; for `cacheWrites`: tokens written).
    pub tokens: f64,
    pub share: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub ts: i64,
    pub context: u64,
    pub cache_write: u64,
    pub output: u64,
    /// Whether every request in this point was attributed from snapshots.
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub ts: i64,
    /// compaction, rebuild, subagent, error.
    pub kind: String,
    pub label: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStat {
    pub name: String,
    pub group: String,
    pub calls: u64,
    /// Tokens its results added to the context.
    pub result_tokens: f64,
    /// Context tokens its results accounted for over the session.
    pub carried_tokens: f64,
    pub largest_result: f64,
    pub errors: u64,
    /// Output tokens the model spent writing its arguments.
    pub call_output: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    /// Requests, all tokens and output of the subagent's own transcript.
    pub requests: u64,
    pub tokens: u64,
    pub output: u64,
    pub first_ts: Option<i64>,
    /// The `Agent` call in the parent that launched it.
    pub tool_use_id: Option<String>,
    /// Turn of the parent the call was made in.
    pub turn: Option<u32>,
    /// `exact` or `estimated`, for its own ledger.
    pub mode: String,
    /// Sum of its requests' context.
    pub context_tokens: f64,
    pub cache_write: u64,
    pub groups: Vec<Slice>,
    /// Its largest context categories.
    pub composition: Vec<Slice>,
    /// Size of the result it returned into the parent's context.
    pub result_tokens: f64,
    /// Parent context tokens that result accounted for.
    pub result_carried: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    /// info or warn.
    pub level: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub web_search: u64,
    pub usd: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    /// Claude Code's own estimate at API prices.
    pub usd: f64,
    pub by_model: Vec<(String, f64)>,
    pub models: Vec<ModelCost>,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub api_ms: u64,
    pub tool_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: String,
    pub tokens: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDefs {
    pub name: String,
    pub tokens: f64,
    /// Context tokens its definitions accounted for over the session.
    pub carried_tokens: f64,
    /// Calls to its tools in this session.
    pub calls: u64,
    pub tools: Vec<ToolDef>,
}

/// System prompt and tool definitions, from the request shape used by the
/// most requests. Sizes are estimates scaled to the ledger.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overhead {
    /// Distinct shapes seen in the session.
    pub shapes: u32,
    pub total: f64,
    pub system_tokens: f64,
    pub system_blocks: Vec<f64>,
    pub builtin_tokens: f64,
    pub builtin: Vec<ToolDef>,
    pub mcp_tokens: f64,
    pub servers: Vec<ServerDefs>,
    pub tools_loaded: u64,
    /// Requests before the first shape record that use it (back-filled).
    pub backfilled_requests: u64,
}

/// Model usage Claude Code counted that no transcript row explains: titles,
/// web search, compaction and other background calls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundModel {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub tokens: u64,
    pub web_search: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCount {
    pub name: String,
    pub calls: u64,
}

/// One prompt and everything it triggered. No prompt text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub index: u32,
    pub ts: i64,
    /// human, task_notification, peer, sdk, scheduled, … or unknown.
    pub origin: String,
    pub permission_mode: Option<String>,
    pub requests: u64,
    /// Index of its first request in the session.
    pub first_request: Option<u32>,
    /// Sum of its requests' context.
    pub context_tokens: u64,
    /// Tokens of new content that entered the context during the turn.
    pub context_added: f64,
    pub output: u64,
    pub thinking: u64,
    pub cache_write: u64,
    pub tools: Vec<ToolCount>,
    pub subagents: u64,
    pub skill: Option<String>,
    /// compacted, rebuild, error, denied.
    pub flags: Vec<String>,
    pub duration_ms: i64,
}

/// One piece of context, by how much context it accounted for. Names and
/// times only, never content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PieceStat {
    pub label: String,
    pub group: String,
    pub tool: Option<String>,
    /// Attachment type or tool use id.
    pub source: Option<String>,
    pub ts: i64,
    pub turn: Option<u32>,
    /// Size when it entered the context.
    pub tokens: f64,
    /// Requests it was in context for.
    pub requests: u32,
    pub carried_tokens: f64,
    /// Size is exact (the model's own output), not estimated.
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnalysis {
    pub id: String,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    /// Main thread only.
    pub requests: u64,
    pub prompts: u64,
    pub peak_context: u64,
    pub avg_context: f64,
    pub output: u64,
    /// Context tokens of the model's earlier thinking.
    pub thinking: u64,
    /// Thinking tokens generated.
    pub thinking_output: u64,
    pub compactions: u64,
    pub rebuilds: u64,
    pub rebuild_tokens: u64,
    pub hook_errors: u64,
    pub api_errors: u64,
    /// System prompt + tool definitions per request, estimated.
    pub overhead_tokens: Option<f64>,
    pub tools_loaded: Option<u64>,
    /// Whether request shapes were recorded (newer Claude Code versions).
    pub has_snapshots: bool,
    /// `exact` when request snapshots attributed (nearly) every request,
    /// otherwise `estimated`.
    pub mode: String,
    pub exact_requests: u64,
    /// How earlier thinking was handled, by number of requests.
    pub thinking_policy: BTreeMap<String, u64>,
    /// Sum of every request's context; the ledger adds up to this.
    pub context_tokens: u64,
    pub composition: Vec<Slice>,
    pub groups: Vec<Slice>,
    pub overhead: Option<Overhead>,
    /// Output tokens by thinking, text, and tool-call arguments per tool.
    pub output_split: Vec<Slice>,
    /// Cache writes by what was written; rebuilds after idle on their own.
    pub cache_writes: Vec<Slice>,
    pub calibration: Calibration,
    pub timeline: Vec<Point>,
    pub events: Vec<Event>,
    pub tools: Vec<ToolStat>,
    pub subagents: Vec<Subagent>,
    pub background: Vec<BackgroundModel>,
    pub turns: Vec<Turn>,
    pub top_pieces: Vec<PieceStat>,
    pub warnings: Vec<Warning>,
    pub cost: Option<Cost>,
    /// Lines that were not valid JSON (usually a line still being written).
    pub unreadable_lines: u64,
}

fn slices(m: BTreeMap<(String, &'static str), f64>) -> Vec<Slice> {
    let total: f64 = m.values().filter(|v| **v > 0.0).sum::<f64>().max(1.0);
    let mut v: Vec<Slice> = m
        .into_iter()
        .filter(|(_, t)| *t > 0.0)
        .map(|((l, g), t)| Slice {
            label: l,
            group: g.to_string(),
            tokens: t,
            share: t / total,
        })
        .collect();
    v.sort_by(|a, b| b.tokens.total_cmp(&a.tokens));
    v
}

fn group_slices(c: &[Slice]) -> Vec<Slice> {
    let mut m: BTreeMap<(String, &'static str), f64> = BTreeMap::new();
    for s in c {
        let g = GROUPS
            .iter()
            .find(|g| **g == s.group)
            .copied()
            .unwrap_or("Other");
        *m.entry((g.to_string(), g)).or_default() += s.tokens;
    }
    slices(m)
}

#[derive(Default, Clone, Copy)]
struct Rows {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

pub(super) fn analyze_paths(paths: &[PathBuf], main: bool) -> SessionAnalysis {
    analyze_inner(paths, main).0
}

fn analyze_inner(paths: &[PathBuf], main: bool) -> (SessionAnalysis, BTreeMap<String, Rows>) {
    let mut t = Trace::default();
    for p in paths {
        // A missing or unreadable file contributes nothing.
        let _ = t.read(p, !main);
    }
    let lists = ledger::reconstruct(&t);
    let cal = ledger::calibrate(&t, &lists);
    let l = ledger::run(&mut t, &cal, &lists);
    let mut rows: BTreeMap<String, Rows> = BTreeMap::new();
    for r in &t.reqs {
        let e = rows.entry(r.model.clone()).or_default();
        e.input += r.input;
        e.output += r.out;
        e.cache_read += r.cache_read;
        e.cache_write += r.cw;
    }
    let mut subagents = Vec::new();
    if main {
        subagents = find_subagents(paths, &t, &l, &mut rows);
    }
    let own = rows.clone();
    (build(t, l, cal, subagents, rows, paths), own)
}

fn find_subagents(
    main: &[PathBuf],
    t: &Trace,
    l: &Ledger,
    rows: &mut BTreeMap<String, Rows>,
) -> Vec<Subagent> {
    // Result pieces of each Agent call in the parent.
    let mut results: HashMap<&str, (f64, f64)> = HashMap::new();
    for (i, p) in t.pieces.iter().enumerate() {
        if p.group == "Subagents" && p.kind != Kind::Output {
            if let Some(src) = p.source.as_deref() {
                let e = results.entry(src).or_default();
                e.0 += l.entry[i];
                e.1 += l.carried[i];
            }
        }
    }
    let mut out = Vec::new();
    for m in main {
        let dir = m.with_extension("").join("subagents");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for p in files {
            let stem = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let meta: Option<Value> =
                std::fs::read_to_string(dir.join(format!("{stem}.meta.json")))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok());
            let s = |k: &str| {
                meta.as_ref()
                    .and_then(|m| m.get(k))
                    .and_then(Value::as_str)
                    .map(String::from)
            };
            let tool_use_id = s("toolUseId");
            let launch = tool_use_id.as_deref().and_then(|id| t.tool_uses.get(id));
            let (a, sub_rows) = analyze_inner(std::slice::from_ref(&p), false);
            let mut tokens = 0u64;
            let mut cw = 0u64;
            for (m, r) in sub_rows {
                tokens += r.input + r.output + r.cache_read + r.cache_write;
                cw += r.cache_write;
                let e = rows.entry(m).or_default();
                e.input += r.input;
                e.output += r.output;
                e.cache_read += r.cache_read;
                e.cache_write += r.cache_write;
            }
            let (result_tokens, result_carried) = tool_use_id
                .as_deref()
                .and_then(|id| results.get(id).copied())
                .unwrap_or((0.0, 0.0));
            out.push(Subagent {
                id: stem.trim_start_matches("agent-").to_string(),
                agent_type: s("agentType"),
                description: s("description"),
                requests: a.requests,
                tokens,
                output: a.output,
                first_ts: launch.map(|l| l.1).or(a.first_ts),
                turn: launch.and_then(|l| l.2),
                tool_use_id,
                mode: a.mode.clone(),
                context_tokens: a.context_tokens as f64,
                cache_write: cw,
                groups: a.groups.clone(),
                composition: a.composition.iter().take(SUBAGENT_TOP).cloned().collect(),
                result_tokens,
                result_carried,
            });
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.tokens));
    out
}

fn build(
    t: Trace,
    l: Ledger,
    cal: Calibration,
    subagents: Vec<Subagent>,
    rows: BTreeMap<String, Rows>,
    paths: &[PathBuf],
) -> SessionAnalysis {
    let id = paths
        .first()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Composition.
    let mut by_cat: BTreeMap<(String, &'static str), f64> = BTreeMap::new();
    for (i, p) in t.pieces.iter().enumerate() {
        if l.carried[i] > 0.0 {
            *by_cat.entry((p.label.clone(), p.group)).or_default() += l.carried[i];
        }
    }
    let composition = slices(by_cat);
    let groups = group_slices(&composition);
    let context_tokens: u64 = t.reqs.iter().map(|r| r.ctx).sum();

    // Output split: every response, whether or not it stayed in context.
    let mut out_split: BTreeMap<(String, &'static str), f64> = BTreeMap::new();
    let mut call_output: HashMap<String, f64> = HashMap::new();
    for p in &t.pieces {
        if p.kind != Kind::Output {
            continue;
        }
        let Some(e) = p.exact else { continue };
        let label = match (&p.tool, p.label.as_str()) {
            (Some(tool), _) => {
                *call_output.entry(tool_label(tool).0).or_default() += e;
                format!("{TOOL_CALLS}: {tool}")
            }
            (None, l) => l.to_string(),
        };
        *out_split.entry((label, "Model output")).or_default() += e;
    }
    let output_split = slices(out_split);

    // Tools.
    let mut tool_res: HashMap<(String, &'static str), (f64, f64, f64)> = HashMap::new();
    for (i, p) in t.pieces.iter().enumerate() {
        if p.kind == Kind::Output || p.tool.is_none() {
            continue;
        }
        let size = if l.first[i].is_some() {
            l.entry[i]
        } else {
            cal.est(p)
        };
        let e = tool_res.entry((p.label.clone(), p.group)).or_default();
        e.0 += size;
        e.1 += l.carried[i];
        e.2 = e.2.max(size);
    }
    let mut tools: Vec<ToolStat> = t
        .tools
        .iter()
        .map(|(name, a)| {
            let (res, carried, largest) = tool_res
                .get(&(name.clone(), a.group))
                .copied()
                .unwrap_or_default();
            ToolStat {
                name: name.clone(),
                group: a.group.to_string(),
                calls: a.calls,
                result_tokens: res,
                carried_tokens: carried,
                largest_result: largest,
                errors: a.errors,
                call_output: call_output.get(name).copied().unwrap_or(0.0),
            }
        })
        .collect();
    tools.sort_by(|a, b| {
        b.carried_tokens
            .total_cmp(&a.carried_tokens)
            .then(b.calls.cmp(&a.calls))
            .then(a.name.cmp(&b.name))
    });

    // Overhead, from the shape used by the most requests.
    let overhead = overhead(&t, &l);

    // Events.
    let mut events: Vec<Event> = t
        .events
        .iter()
        .map(|e| Event {
            ts: e.ts,
            kind: e.kind.into(),
            label: e.label.clone(),
            tokens: e.tokens,
        })
        .collect();
    let (mut rebuilds, mut rebuild_tokens) = (0u64, 0u64);
    for (i, r) in t.reqs.iter().enumerate() {
        if l.rebuild[i] {
            rebuilds += 1;
            rebuild_tokens += r.cw;
            events.push(Event {
                ts: r.ts,
                kind: "rebuild".into(),
                label: format!("Cache rebuilt after {} idle", human_gap(l.gap_ms[i])),
                tokens: r.cw,
            });
        }
    }
    for a in &subagents {
        if let Some(ts) = a.first_ts {
            events.push(Event {
                ts,
                kind: "subagent".into(),
                label: format!(
                    "Subagent: {}",
                    a.description
                        .clone()
                        .or(a.agent_type.clone())
                        .unwrap_or_else(|| a.id.clone())
                ),
                tokens: a.tokens,
            });
        }
    }
    events.sort_by_key(|e| e.ts);

    // Turns.
    let mut turns: Vec<Turn> = t
        .turns
        .iter()
        .enumerate()
        .map(|(i, a)| Turn {
            index: i as u32,
            ts: a.ts,
            origin: a.origin.clone(),
            permission_mode: a.permission_mode.clone(),
            requests: 0,
            first_request: None,
            context_tokens: 0,
            context_added: 0.0,
            output: 0,
            thinking: 0,
            cache_write: 0,
            tools: a
                .tools
                .iter()
                .map(|(n, c)| ToolCount {
                    name: n.clone(),
                    calls: *c,
                })
                .collect(),
            subagents: a.subagents,
            skill: a.skill.clone(),
            flags: Vec::new(),
            duration_ms: 0,
        })
        .collect();
    for (i, r) in t.reqs.iter().enumerate() {
        let Some(tn) = turns.get_mut(r.turn as usize) else {
            continue;
        };
        tn.requests += 1;
        tn.first_request.get_or_insert(i as u32);
        tn.context_tokens += r.ctx;
        tn.context_added += l.added[i];
        tn.output += r.out;
        tn.thinking += r.think;
        tn.cache_write += r.cw;
        tn.duration_ms = tn.duration_ms.max(r.ts - tn.ts);
        if l.rebuild[i] && !tn.flags.iter().any(|f| f == "rebuild") {
            tn.flags.push("rebuild".into());
        }
    }
    for (tn, a) in turns.iter_mut().zip(&t.turns) {
        if a.compacted {
            tn.flags.push("compacted".into());
        }
        if a.errors > 0 {
            tn.flags.push("error".into());
        }
        if a.denials > 0 {
            tn.flags.push("denied".into());
        }
        if tn.ts == 0 {
            tn.ts = tn.first_request.map(|r| t.reqs[r as usize].ts).unwrap_or(0);
        }
    }

    // Top pieces.
    let mut order: Vec<usize> = (0..t.pieces.len())
        .filter(|&i| {
            let p = &t.pieces[i];
            // Overhead has its own panel; it would crowd out everything else.
            l.carried[i] > 0.0 && p.label != UNATTRIBUTED && p.group != "Overhead"
        })
        .collect();
    order.sort_by(|&a, &b| l.carried[b].total_cmp(&l.carried[a]));
    let top_pieces = order
        .into_iter()
        .take(TOP_PIECES)
        .map(|i| {
            let p = &t.pieces[i];
            PieceStat {
                label: p.label.clone(),
                group: p.group.to_string(),
                tool: p.tool.clone(),
                source: p.source.clone(),
                ts: if p.ts > 0 {
                    p.ts
                } else {
                    l.first[i].map(|r| t.reqs[r as usize].ts).unwrap_or(0)
                },
                turn: p.turn,
                tokens: l.entry[i],
                requests: l.requests[i],
                carried_tokens: l.carried[i],
                exact: p.exact.is_some(),
            }
        })
        .collect();

    // Background model usage: what Claude Code counted that no row explains.
    let background: Vec<BackgroundModel> = t
        .cost
        .as_ref()
        .map(|c| {
            c.models
                .iter()
                .filter_map(|(m, u)| {
                    let r = rows.get(m).copied().unwrap_or_default();
                    let b = BackgroundModel {
                        model: m.clone(),
                        input: u.input.saturating_sub(r.input),
                        output: u.output.saturating_sub(r.output),
                        cache_read: u.cache_read.saturating_sub(r.cache_read),
                        cache_write: u.cache_write.saturating_sub(r.cache_write),
                        tokens: 0,
                        web_search: u.web_search,
                    };
                    let tokens = b.input + b.output + b.cache_read + b.cache_write;
                    (tokens > 0).then_some(BackgroundModel { tokens, ..b })
                })
                .collect()
        })
        .unwrap_or_default();

    let cost = t.cost.as_ref().map(|c| Cost {
        usd: c.usd,
        by_model: c.models.iter().map(|(m, u)| (m.clone(), u.usd)).collect(),
        models: c
            .models
            .iter()
            .map(|(m, u)| ModelCost {
                model: m.clone(),
                input: u.input,
                output: u.output,
                cache_read: u.cache_read,
                cache_write: u.cache_write,
                web_search: u.web_search,
                usd: u.usd,
            })
            .collect(),
        lines_added: c.lines_added,
        lines_removed: c.lines_removed,
        api_ms: c.api_ms,
        tool_ms: c.tool_ms,
    });

    let requests = t.reqs.len() as u64;
    let exact_requests = l.exact_req.iter().filter(|x| **x).count() as u64;
    let peak_context = t.reqs.iter().map(|r| r.ctx).max().unwrap_or(0);
    let avg_context = if requests > 0 {
        context_tokens as f64 / requests as f64
    } else {
        0.0
    };
    let thinking = composition
        .iter()
        .filter(|s| s.label == THINKING)
        .map(|s| s.tokens)
        .sum::<f64>();
    let compactions = events.iter().filter(|e| e.kind == "compaction").count() as u64;
    let mut d = SessionAnalysis {
        id,
        first_ts: t.reqs.first().map(|r| r.ts),
        last_ts: t.reqs.last().map(|r| r.ts),
        requests,
        prompts: t.prompts,
        peak_context,
        avg_context,
        output: t.reqs.iter().map(|r| r.out).sum(),
        thinking: thinking as u64,
        thinking_output: t.reqs.iter().map(|r| r.think).sum(),
        compactions,
        rebuilds,
        rebuild_tokens,
        hook_errors: t.hook_errors,
        api_errors: t.api_errors,
        overhead_tokens: overhead.as_ref().map(|o| o.total),
        tools_loaded: overhead.as_ref().map(|o| o.tools_loaded),
        has_snapshots: !t.shapes.is_empty(),
        mode: if requests > 0 && exact_requests as f64 >= 0.95 * requests as f64 {
            "exact".into()
        } else {
            "estimated".into()
        },
        exact_requests,
        thinking_policy: l.policies.clone(),
        context_tokens,
        composition,
        groups,
        overhead,
        output_split,
        cache_writes: slices(l.cache_writes.clone()),
        calibration: cal,
        timeline: timeline(&t, &l),
        events,
        tools,
        subagents,
        background,
        turns,
        top_pieces,
        warnings: Vec::new(),
        cost,
        unreadable_lines: t.unreadable,
    };
    d.warnings = warnings(&d);
    d
}

fn overhead(t: &Trace, l: &Ledger) -> Option<Overhead> {
    if t.shapes.is_empty() {
        return None;
    }
    // Requests per shape: the snapshot's shape when recorded, else the
    // latest seen, else the first (back-fill).
    let first = t.reqs.iter().find_map(|r| r.shape).unwrap_or(0);
    let mut uses = vec![0u64; t.shapes.len()];
    let mut backfilled = 0u64;
    for r in &t.reqs {
        let s = r
            .api
            .and_then(|a| t.apis[a as usize].shape.as_deref())
            .and_then(|h| t.shape_index(h))
            .or(r.shape);
        if s.is_none() {
            backfilled += 1;
        }
        uses[s.unwrap_or(first) as usize] += 1;
    }
    let si = (0..t.shapes.len()).max_by_key(|&i| (uses[i], i))?;
    let sh = &t.shapes[si];
    // Average size per request of each component, as the ledger counted it.
    let avg = |p: u32| {
        let i = p as usize;
        if l.requests[i] > 0 {
            l.carried[i] / l.requests[i] as f64
        } else {
            0.0
        }
    };
    let per_byte = |p: u32| {
        let b = t.pieces[p as usize].bytes;
        if b > 0 {
            avg(p) / b as f64
        } else {
            0.0
        }
    };
    let sys_piece = sh.pieces[0];
    let builtin_piece = sh.pieces[1];
    let system_tokens = avg(sys_piece);
    let system_blocks = sh
        .system
        .iter()
        .map(|b| *b as f64 * per_byte(sys_piece))
        .collect();
    let mut builtin: Vec<ToolDef> = Vec::new();
    let mut servers: BTreeMap<String, ServerDefs> = BTreeMap::new();
    let server_piece: HashMap<&str, u32> = sh.pieces[2..]
        .iter()
        .filter_map(|&p| t.pieces[p as usize].source.as_deref().map(|s| (s, p)))
        .collect();
    for (name, b) in &sh.tools {
        match mcp_server(name) {
            None => builtin.push(ToolDef {
                name: name.clone(),
                tokens: *b as f64 * per_byte(builtin_piece),
            }),
            Some(srv) => {
                let p = server_piece.get(srv.as_str()).copied();
                let e = servers.entry(srv.clone()).or_insert_with(|| ServerDefs {
                    name: srv.clone(),
                    tokens: p.map(avg).unwrap_or(0.0),
                    carried_tokens: p.map(|p| l.carried[p as usize]).unwrap_or(0.0),
                    calls: t.tools.get(&srv).map(|a| a.calls).unwrap_or(0),
                    tools: Vec::new(),
                });
                e.tools.push(ToolDef {
                    name: name.clone(),
                    tokens: *b as f64 * p.map(per_byte).unwrap_or(0.0),
                });
            }
        }
    }
    builtin.sort_by(|a, b| b.tokens.total_cmp(&a.tokens));
    let mut servers: Vec<ServerDefs> = servers.into_values().collect();
    for s in &mut servers {
        s.tools.sort_by(|a, b| b.tokens.total_cmp(&a.tokens));
    }
    servers.sort_by(|a, b| b.tokens.total_cmp(&a.tokens));
    let builtin_tokens = avg(builtin_piece);
    let mcp_tokens: f64 = servers.iter().map(|s| s.tokens).sum();
    Some(Overhead {
        shapes: t.shapes.len() as u32,
        total: system_tokens + builtin_tokens + mcp_tokens,
        system_tokens,
        system_blocks,
        builtin_tokens,
        builtin,
        mcp_tokens,
        servers,
        tools_loaded: sh.tools.len() as u64,
        backfilled_requests: backfilled,
    })
}

fn human_gap(ms: i64) -> String {
    let m = ms / 60_000;
    if m >= 24 * 60 {
        format!("{}d", m / (24 * 60))
    } else if m >= 60 {
        format!("{}h", m / 60)
    } else {
        format!("{m}m")
    }
}

fn timeline(t: &Trace, l: &Ledger) -> Vec<Point> {
    let n = t.reqs.len();
    let per = n.div_ceil(MAX_POINTS).max(1);
    (0..n)
        .step_by(per)
        .map(|s| {
            let e = (s + per).min(n);
            let c = &t.reqs[s..e];
            let top = c.iter().max_by_key(|r| r.ctx).unwrap();
            Point {
                ts: top.ts,
                context: top.ctx,
                cache_write: c.iter().map(|r| r.cw).sum(),
                output: c.iter().map(|r| r.out).sum(),
                exact: l.exact_req[s..e].iter().all(|x| *x),
            }
        })
        .collect()
}

fn ktok(t: f64) -> String {
    crate::fmt::tokens(t.max(0.0) as u64)
}

fn warnings(d: &SessionAnalysis) -> Vec<Warning> {
    let mut w = Vec::new();
    let share = |label: &str| {
        d.composition
            .iter()
            .filter(|s| s.label == label)
            .map(|s| s.share)
            .sum::<f64>()
    };
    let tokens = |label: &str| {
        d.composition
            .iter()
            .filter(|s| s.label == label)
            .map(|s| s.tokens)
            .sum::<f64>()
    };
    if d.compactions > 0 {
        w.push(Warning {
            level: "warn".into(),
            title: format!(
                "Context filled up {} time{}",
                d.compactions,
                if d.compactions == 1 { "" } else { "s" }
            ),
            detail: format!(
                "Requests averaged {} of context and peaked at {}. Every request re-sends all of it, so late requests cost many times the early ones. Starting a fresh session for a new task keeps the context small.",
                ktok(d.avg_context),
                ktok(d.peak_context as f64)
            ),
        });
    }
    if d.hook_errors >= 5 {
        let t = tokens("Hook errors");
        w.push(if t > 0.0 {
            Warning {
                level: "warn".into(),
                title: format!("A hook failed {} times", d.hook_errors),
                detail: format!(
                    "Its error output was added to the conversation and stayed there: {} context tokens. Fix or remove the failing hook.",
                    ktok(t)
                ),
            }
        } else {
            Warning {
                level: "info".into(),
                title: format!("A hook failed {} times", d.hook_errors),
                detail: "The errors were not sent to the model, so they cost no tokens, but the hook is not doing its job. Check your hooks in settings.json.".into(),
            }
        });
    }
    if d.rebuilds >= 2 {
        w.push(Warning {
            level: "warn".into(),
            title: format!("Prompt cache rebuilt {} times", d.rebuilds),
            detail: format!(
                "After a pause longer than the cache lifetime, the whole prompt is written to the cache again: {} tokens here. Resuming a large session after a break is not free.",
                ktok(d.rebuild_tokens as f64)
            ),
        });
    }
    let injected: f64 = d
        .groups
        .iter()
        .filter(|g| g.group == "Injected")
        .map(|g| g.share)
        .sum();
    if injected > 0.15 {
        let top = d
            .composition
            .iter()
            .find(|s| s.group == "Injected")
            .map(|s| {
                format!(
                    " The largest is {} at {:.0}%.",
                    s.label.to_lowercase(),
                    s.share * 100.0
                )
            })
            .unwrap_or_default();
        w.push(Warning {
            level: "warn".into(),
            title: format!("{:.0}% of the context was injected by Claude Code", injected * 100.0),
            detail: format!("Reminders, tool lists, hook output and file snapshots added automatically, not by you or the model.{top}"),
        });
    }
    if let Some(o) = &d.overhead {
        if o.total > 30_000.0 {
            w.push(Warning {
                level: "info".into(),
                title: format!(
                    "{} tokens of system prompt and tool definitions on every request",
                    ktok(o.total)
                ),
                detail: format!(
                    "{} tools were loaded. Disconnecting MCP servers this project doesn't use makes every request smaller.",
                    o.tools_loaded
                ),
            });
        }
        let unused: Vec<&ServerDefs> = o
            .servers
            .iter()
            .filter(|s| s.calls == 0 && s.tokens > 2_000.0)
            .collect();
        if !unused.is_empty() {
            let carried: f64 = unused.iter().map(|s| s.carried_tokens).sum();
            let names: Vec<&str> = unused.iter().take(3).map(|s| s.name.as_str()).collect();
            w.push(Warning {
                level: "info".into(),
                title: format!(
                    "{} MCP server{} loaded but never called",
                    unused.len(),
                    if unused.len() == 1 { " was" } else { "s were" }
                ),
                detail: format!(
                    "Their tool definitions ({}) rode along on every request: {} context tokens. Disable them for this project if you don't need them.",
                    names.join(", "),
                    ktok(carried)
                ),
            });
        }
    }
    if let Some(t) = d
        .tools
        .iter()
        .max_by(|a, b| a.largest_result.total_cmp(&b.largest_result))
    {
        if t.largest_result > 25_000.0 {
            w.push(Warning {
                level: "info".into(),
                title: format!("One {} result was about {} tokens", t.name, ktok(t.largest_result)),
                detail: "Large tool results stay in context for every later request. Reading a slice of a file or filtering command output keeps them small.".into(),
            });
        }
    }
    let th = share(THINKING);
    if th > 0.25 {
        w.push(Warning {
            level: "info".into(),
            title: format!("{:.0}% of the context was the model's earlier thinking", th * 100.0),
            detail: "Thinking from earlier turns is kept in the conversation. A lower effort level produces less of it.".into(),
        });
    }
    let sub: u64 = d.subagents.iter().map(|s| s.tokens).sum();
    let main = d.context_tokens + d.output;
    if sub > 0 && sub as f64 > 0.3 * (main + sub) as f64 {
        w.push(Warning {
            level: "info".into(),
            title: format!(
                "Subagents used {:.0}% of this session's tokens",
                sub as f64 / (main + sub) as f64 * 100.0
            ),
            detail: format!("{} subagents ran.", d.subagents.len()),
        });
    }
    w
}
