//! Global recommendations (PLAN 4.G): rules that turn usage data into "what
//! should I change", ranked by how many tokens each one concerns.
//!
//! Three stages, so the rules can be tested without transcripts:
//!
//! 1. [`gather`] reads the index (request rows through `query::*`, plus a
//!    little read-only SQL) and one [`SessionSummary`] per session. Summaries
//!    come from `session::analyze` through the single adapter
//!    [`SessionSummary::from_detail`], and are cached in the `insight_cache`
//!    table this module owns, keyed by the session's transcript files.
//! 2. [`evaluate`] runs every rule over the gathered [`Inputs`] and the
//!    [`Thresholds`]. Pure.
//! 3. [`report`] does both and adds the week-over-week change.
//!
//! Amounts are tokens. A session's figures from the analyzer cover the whole
//! session; when a session only partly falls in the range they are scaled by
//! the share of its tokens that do, so every amount is "in the range".

use crate::change::{self, Change, FactorId, Periods, Tokens};
use crate::query::{self, Dim, Filter, Totals};
use crate::session::{self, SessionDetail};
use crate::{fmt, Result};
use jiff::Zoned;
use rayon::prelude::*;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bump when [`SessionSummary`] or the adapter changes; cached rows with an
/// older version are recomputed.
pub const CACHE_VERSION: i64 = 1;

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Every tunable number of every rule.
///
/// Defaults were tuned on two weeks of one heavy user's data (about 170
/// sessions and 20k requests, mostly main thread) so that each rule
/// fires for a real, actionable pattern and stays quiet otherwise:
///
/// - `min_share` 0.5%: project- and tool-level insights below this were
///   noise (dozens of tiny projects); above it every hit was worth reading.
/// - `long_session_requests` 300 and `long_session_share` 40%: one in ten
///   sessions passes 300 main-thread requests, and on this data those
///   sessions (plus any that compacted) carried well over half the tokens.
///   Below 40% long sessions are normal background, not a pattern.
/// - `rebuild_min_count` 3 and `rebuild_min_tokens` 2M: one or two rebuilds
///   a fortnight are unavoidable (lunch, overnight); three or more that
///   rewrote 2M+ tokens are a habit.
/// - `overhead_tokens` 30k: the system prompt alone is about 9.5k, so 30k
///   means 20k+ of tool definitions on every request; the analyzer's own
///   session warning uses the same figure. `tools_loaded` 80 catches
///   sessions with many small tools. On this data every large project sat
///   at 48 to 61k, so `collapse_projects` (3) turns the per-project hits
///   into one global insight: the cause is a global MCP setup, not a
///   project.
/// - `tool_result_tokens` 15k (PLAN 4.G says 25k): Claude Code caps one
///   `Read` at 25k real tokens, which the analyzer's bytes/3.6 estimate puts
///   at about 19k, so 25k never fired (largest seen: Read 19k, browser MCPs
///   17 to 19k). 15k catches results near the cap: whole large files, full
///   page dumps.
/// - `tool_dominates_share` 25%: Bash sits at 15 to 22% of context in most
///   projects here, Read at 10 to 15%; one tool above a quarter of
///   everything is unusual.
/// - `instructions_tokens` 4k: average CLAUDE.md and nested instruction
///   files in context per request. Six small projects were under 2k; the
///   five above 4k were 6.5k to 48k (many nested CLAUDE.md files).
/// - `injected_share` 15% of context, not counting instructions (they have
///   their own rule, and were the largest injected type in most projects,
///   which made the two rules repeat each other). Typical projects are 5
///   to 10%.
/// - `thinking_context_share` 25% and `thinking_output_share` 50%: earlier
///   thinking was 27% of context in the heaviest session (PLAN 2.3) but 13%
///   overall; thinking is 36% of output overall and 42% at high effort and
///   above. Neither fires on this data, which is right: effort is mostly
///   medium.
/// - `subagent_share` 20%: subagents are 6% of tokens on this data; the
///   rule is meant for heavy delegators.
/// - `hook_errors_min` 5, `api_errors_min` 10: fewer is transient. This
///   data has 3,187 hook failures (one broken hook, not sent to the model),
///   so the rule shows as informational with amount 0.
/// - `max_per_rule` 3: per-project rules otherwise list a dozen projects.
/// - `change_pct` 20%: weekly totals routinely move ±15% with no change in
///   habits.
/// - `severity_high` 10%, `severity_medium` 3% of the range's tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Thresholds {
    pub min_share: f64,
    pub long_session_requests: u64,
    pub long_session_share: f64,
    pub rebuild_min_count: u64,
    pub rebuild_min_tokens: f64,
    pub overhead_tokens: f64,
    pub tools_loaded: u64,
    pub tool_result_tokens: f64,
    pub tool_dominates_share: f64,
    pub instructions_tokens: f64,
    pub injected_share: f64,
    pub thinking_context_share: f64,
    pub thinking_output_share: f64,
    pub subagent_share: f64,
    pub hook_errors_min: u64,
    pub api_errors_min: u64,
    pub change_pct: f64,
    /// Days compared by the week-over-week rule (this many vs the same
    /// number before).
    pub change_days: i64,
    pub severity_high: f64,
    pub severity_medium: f64,
    /// Most insights kept per rule, largest first.
    pub max_per_rule: usize,
    /// When a per-project rule fires for at least this many projects and
    /// the cause is a global setting, report it once, globally.
    pub collapse_projects: usize,
    /// Sessions analyzed per report, largest first; the rest count as not
    /// analyzed.
    pub max_sessions: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            min_share: 0.005,
            long_session_requests: 300,
            long_session_share: 0.40,
            rebuild_min_count: 3,
            rebuild_min_tokens: 2_000_000.0,
            overhead_tokens: 30_000.0,
            tools_loaded: 80,
            tool_result_tokens: 15_000.0,
            tool_dominates_share: 0.25,
            instructions_tokens: 4_000.0,
            injected_share: 0.15,
            thinking_context_share: 0.25,
            thinking_output_share: 0.50,
            subagent_share: 0.20,
            hook_errors_min: 5,
            api_errors_min: 10,
            change_pct: 20.0,
            change_days: 7,
            severity_high: 0.10,
            severity_medium: 0.03,
            max_per_rule: 3,
            collapse_projects: 3,
            max_sessions: 400,
        }
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleId {
    LongSessions,
    IdleRebuilds,
    UnusedMcpServers,
    HeavyToolDefinitions,
    OversizedToolResults,
    OneToolDominates,
    LargeInstructions,
    InjectedShare,
    ThinkingShare,
    SubagentCost,
    AutonomousSpend,
    ModelFallback,
    ErrorsAndHooks,
    WeekOverWeek,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleId::LongSessions => "long-sessions",
            RuleId::IdleRebuilds => "idle-rebuilds",
            RuleId::UnusedMcpServers => "unused-mcp-servers",
            RuleId::HeavyToolDefinitions => "heavy-tool-definitions",
            RuleId::OversizedToolResults => "oversized-tool-results",
            RuleId::OneToolDominates => "one-tool-dominates",
            RuleId::LargeInstructions => "large-instructions",
            RuleId::InjectedShare => "injected-share",
            RuleId::ThinkingShare => "thinking-share",
            RuleId::SubagentCost => "subagent-cost",
            RuleId::AutonomousSpend => "autonomous-spend",
            RuleId::ModelFallback => "model-fallback",
            RuleId::ErrorsAndHooks => "errors-and-hooks",
            RuleId::WeekOverWeek => "week-over-week",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            RuleId::LongSessions => "Long sessions dominate",
            RuleId::IdleRebuilds => "Idle cache rebuilds",
            RuleId::UnusedMcpServers => "Unused MCP servers",
            RuleId::HeavyToolDefinitions => "Heavy tool definitions",
            RuleId::OversizedToolResults => "Oversized tool results",
            RuleId::OneToolDominates => "One tool dominates",
            RuleId::LargeInstructions => "Large CLAUDE.md / instructions",
            RuleId::InjectedShare => "Injected content share high",
            RuleId::ThinkingShare => "Thinking share high",
            RuleId::SubagentCost => "Subagent cost",
            RuleId::AutonomousSpend => "Autonomous spend",
            RuleId::ModelFallback => "Model fallback",
            RuleId::ErrorsAndHooks => "Errors and hooks",
            RuleId::WeekOverWeek => "Week-over-week change",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Session,
    Project,
    Global,
}

/// Where the UI should go to show the evidence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// overview, sessions, session, project, tools, cache, injected,
    /// overhead, effort, subagents, problems.
    pub view: String,
    /// The range and subject to open it with.
    pub filter: Filter,
    /// What to highlight inside the view: a tool name, a category label.
    pub focus: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Insight {
    pub rule: RuleId,
    pub title: String,
    /// Stable id for dismissing: rule id plus subject.
    pub key: String,
    pub severity: Severity,
    pub scope: Scope,
    /// Project path, session id or tool name, by scope.
    pub subject: Option<String>,
    pub subject_label: Option<String>,
    /// Tokens this insight concerns, in the range.
    pub amount: f64,
    /// `amount` as a share of all tokens in the range.
    pub share: f64,
    pub unit: String,
    pub finding: String,
    pub action: String,
    pub evidence: Evidence,
}

/// A rule that did not run, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub rule: RuleId,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub from: Option<String>,
    pub to: Option<String>,
    pub unit: String,
    pub totals: Totals,
    /// Sessions with any request in the range.
    pub sessions: usize,
    /// Sessions with a summary (cached or analyzed now).
    pub analyzed: usize,
    /// Of those, analyzed during this call.
    pub analyzed_now: usize,
    pub insights: Vec<Insight>,
    pub skipped: Vec<Skipped>,
    pub change: Option<Change>,
    pub thresholds: Thresholds,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SliceSum {
    pub label: String,
    pub group: String,
    pub tokens: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolSum {
    pub name: String,
    /// Tools or MCP.
    pub group: String,
    pub calls: u64,
    pub result_tokens: f64,
    pub carried_tokens: f64,
    pub largest_result: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubagentSum {
    pub agent_type: Option<String>,
    pub requests: u64,
    pub tokens: u64,
}

/// What the rules need from one session's analysis. This is the cached shape,
/// independent of `session::SessionDetail`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionSummary {
    /// Main-thread requests.
    pub requests: u64,
    pub prompts: u64,
    pub peak_context: u64,
    pub avg_context: f64,
    pub output: u64,
    pub compactions: u64,
    pub rebuilds: u64,
    pub rebuild_tokens: u64,
    pub hook_errors: u64,
    pub api_errors: u64,
    /// System prompt + tool definitions per request, when snapshots exist.
    pub overhead_tokens: Option<f64>,
    pub tools_loaded: Option<u64>,
    /// Context tokens over the whole session (sum of `slices`).
    pub context_tokens: f64,
    /// What filled the context, per category.
    pub slices: Vec<SliceSum>,
    pub tools: Vec<ToolSum>,
    pub subagents: Vec<SubagentSum>,
}

/// Category labels the rules look for, as `session.rs` names them.
mod labels {
    pub const INSTRUCTIONS: &str = "CLAUDE.md and instructions";
    pub const THINKING: &str = "Thinking";
    pub const HOOK_ERRORS: &str = "Hook errors";
    pub const OVERHEAD: &str = "System prompt and tool definitions";
    pub const GROUP_INJECTED: &str = "Injected";
}

impl SessionSummary {
    /// The adapter: the only place that reads `session::analyze` output.
    pub fn from_detail(d: &SessionDetail) -> SessionSummary {
        let slices: Vec<SliceSum> = d
            .composition
            .iter()
            .map(|s| SliceSum {
                label: s.label.clone(),
                group: s.group.clone(),
                tokens: s.tokens,
            })
            .collect();
        SessionSummary {
            requests: d.requests,
            prompts: d.prompts,
            peak_context: d.peak_context,
            avg_context: d.avg_context,
            output: d.output,
            compactions: d.compactions,
            rebuilds: d.rebuilds,
            rebuild_tokens: d.rebuild_tokens,
            hook_errors: d.hook_errors,
            api_errors: d.api_errors,
            overhead_tokens: d.overhead_tokens,
            tools_loaded: d.tools_loaded,
            context_tokens: slices.iter().map(|s| s.tokens).sum(),
            slices,
            tools: d
                .tools
                .iter()
                .map(|t| ToolSum {
                    name: t.name.clone(),
                    group: t.group.clone(),
                    calls: t.calls,
                    result_tokens: t.result_tokens,
                    carried_tokens: t.carried_tokens,
                    largest_result: t.largest_result,
                })
                .collect(),
            subagents: d
                .subagents
                .iter()
                .map(|s| SubagentSum {
                    agent_type: s.agent_type.clone(),
                    requests: s.requests,
                    tokens: s.tokens,
                })
                .collect(),
        }
    }

    fn label_tokens(&self, label: &str) -> f64 {
        self.slices
            .iter()
            .filter(|s| s.label == label)
            .map(|s| s.tokens)
            .sum()
    }

    fn group_tokens(&self, group: &str) -> f64 {
        self.slices
            .iter()
            .filter(|s| s.group == group)
            .map(|s| s.tokens)
            .sum()
    }
}

/// Analyze one session and summarize it for the rules.
pub fn summarize(conn: &Connection, session: &str) -> Result<SessionSummary> {
    Ok(SessionSummary::from_detail(&session::analyze(
        conn, session,
    )?))
}

/// One session as the rules see it.
#[derive(Debug, Clone, Default)]
pub struct SessionFacts {
    pub id: String,
    /// Title, or the id's first 8 characters.
    pub label: String,
    pub project: String,
    /// All tokens in the range, subagents included.
    pub tokens: u64,
    /// Subagent tokens in the range.
    pub subagent_tokens: u64,
    /// Share of the whole session's tokens that fall in the range (0..1).
    pub scale: f64,
    pub summary: Option<SessionSummary>,
}

impl SessionFacts {
    /// A whole-session figure, scaled to the range.
    fn scaled(&self, x: f64) -> f64 {
        x * self.scale
    }
}

#[derive(Debug, Clone, Default)]
pub struct EffortRow {
    pub effort: String,
    pub requests: u64,
    pub output: u64,
    pub thinking: u64,
}

/// Everything the rules read.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub filter: Filter,
    pub totals: Totals,
    pub sessions: Vec<SessionFacts>,
    pub effort: Vec<EffortRow>,
    pub change: Option<Change>,
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

type Rule = fn(&Inputs, &Thresholds) -> Vec<Insight>;

/// Rules that run, in table order.
const RULES: &[(RuleId, Rule)] = &[
    (RuleId::LongSessions, long_sessions),
    (RuleId::IdleRebuilds, idle_rebuilds),
    (RuleId::HeavyToolDefinitions, heavy_tool_definitions),
    (RuleId::OversizedToolResults, oversized_tool_results),
    (RuleId::OneToolDominates, one_tool_dominates),
    (RuleId::LargeInstructions, large_instructions),
    (RuleId::InjectedShare, injected_share),
    (RuleId::ThinkingShare, thinking_share),
    (RuleId::SubagentCost, subagent_cost),
    (RuleId::ErrorsAndHooks, errors_and_hooks),
    (RuleId::WeekOverWeek, week_over_week),
];

/// Rules that need data the index does not have yet.
fn skipped_rules() -> Vec<Skipped> {
    let s = |rule: RuleId, reason: &str| Skipped {
        rule,
        title: rule.title().into(),
        reason: reason.into(),
    };
    vec![
        // TODO(4.G): needs per-server tool-definition sizes (`overhead`
        // table, PLAN 4.A.2) and per-session "server called" flags, so the
        // definition cost of servers never called in >= 90% of the sessions
        // that loaded them can be measured.
        s(
            RuleId::UnusedMcpServers,
            "needs per-server tool-definition sizes, not indexed yet",
        ),
        // TODO(4.G): needs `turnOrigin` per turn (`turns` table, PLAN 4.A.2)
        // to sum the cost of task-notification, peer, scheduled and SDK turns.
        s(
            RuleId::AutonomousSpend,
            "needs who started each turn, not indexed yet",
        ),
        // TODO(4.G): needs `requestedModel` per request row; the index stores
        // only the model that answered.
        s(
            RuleId::ModelFallback,
            "needs the requested model per request, not indexed yet",
        ),
    ]
}

fn severity(share: f64, cfg: &Thresholds) -> Severity {
    if share >= cfg.severity_high {
        Severity::High
    } else if share >= cfg.severity_medium {
        Severity::Medium
    } else {
        Severity::Low
    }
}

fn pct(x: f64) -> String {
    let p = x * 100.0;
    if p > 0.0 && p < 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

fn tok(x: f64) -> String {
    fmt::tokens(x.max(0.0).round() as u64)
}

fn base_name(p: &str) -> String {
    p.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

struct Draft {
    rule: RuleId,
    scope: Scope,
    subject: Option<String>,
    subject_label: Option<String>,
    amount: f64,
    severity: Option<Severity>,
    finding: String,
    action: String,
    view: &'static str,
    filter: Filter,
    focus: Option<String>,
}

fn build(inp: &Inputs, cfg: &Thresholds, d: Draft) -> Insight {
    let total = inp.totals.total.max(1) as f64;
    let share = d.amount / total;
    Insight {
        rule: d.rule,
        title: d.rule.title().into(),
        key: match &d.subject {
            Some(s) => format!("{}:{s}", d.rule.as_str()),
            None => d.rule.as_str().into(),
        },
        severity: d.severity.unwrap_or_else(|| severity(share, cfg)),
        scope: d.scope,
        subject: d.subject,
        subject_label: d.subject_label,
        amount: d.amount,
        share,
        unit: "tokens".into(),
        finding: d.finding,
        action: d.action,
        evidence: Evidence {
            view: d.view.into(),
            filter: d.filter,
            focus: d.focus,
        },
    }
}

fn project_filter(inp: &Inputs, project: &str) -> Filter {
    Filter {
        project: Some(project.to_string()),
        ..inp.filter.clone()
    }
}

fn session_filter(inp: &Inputs, id: &str) -> Filter {
    Filter {
        session: Some(id.to_string()),
        ..inp.filter.clone()
    }
}

/// Analyzed sessions grouped by project, in first-seen order.
fn by_project(inp: &Inputs) -> Vec<(&str, Vec<&SessionFacts>)> {
    let mut order: Vec<&str> = Vec::new();
    let mut m: HashMap<&str, Vec<&SessionFacts>> = HashMap::new();
    for s in inp.sessions.iter().filter(|s| s.summary.is_some()) {
        let e = m.entry(s.project.as_str()).or_default();
        if e.is_empty() {
            order.push(s.project.as_str());
        }
        e.push(s);
    }
    order
        .into_iter()
        .map(|p| (p, m.remove(p).unwrap_or_default()))
        .collect()
}

fn summary(s: &SessionFacts) -> &SessionSummary {
    s.summary.as_ref().expect("analyzed")
}

/// Sessions that ran past a compaction or a request count use most tokens.
fn long_sessions(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let n = inp.sessions.len() as u64;
    let long: Vec<&SessionFacts> = inp
        .sessions
        .iter()
        .filter(|s| {
            s.summary
                .as_ref()
                .is_some_and(|x| x.compactions > 0 || x.requests >= cfg.long_session_requests)
        })
        .collect();
    let amount: f64 = long.iter().map(|s| s.tokens as f64).sum();
    let total = inp.totals.total.max(1) as f64;
    if long.is_empty() || amount / total < cfg.long_session_share {
        return vec![];
    }
    let compacted = long.iter().filter(|s| summary(s).compactions > 0).count();
    let top = long.iter().max_by_key(|s| s.tokens).unwrap();
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::LongSessions,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount,
            severity: None,
            finding: format!(
                "{} of {} sessions ran past {} requests or a compaction ({} compacted) and used {} of all tokens ({}); the largest, \"{}\", alone used {}.",
                long.len(),
                n,
                cfg.long_session_requests,
                compacted,
                pct(amount / total),
                tok(amount),
                top.label,
                tok(top.tokens as f64)
            ),
            action: "Start a new session for a new task; use /clear or /compact at a natural break."
                .into(),
            view: "sessions",
            filter: inp.filter.clone(),
            focus: Some("long".into()),
        },
    )]
}

/// The prompt cache expired during a pause and the whole context was written
/// again.
fn idle_rebuilds(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut count = 0u64;
    let mut amount = 0.0;
    let mut worst: Option<(&SessionFacts, f64, u64)> = None;
    for s in &inp.sessions {
        let Some(x) = &s.summary else { continue };
        if x.rebuilds == 0 {
            continue;
        }
        let a = s.scaled(x.rebuild_tokens as f64);
        // Count only rebuilds that fall in the range, roughly.
        count += (x.rebuilds as f64 * s.scale).round().max(1.0) as u64;
        amount += a;
        if worst.is_none_or(|w| a > w.1) {
            worst = Some((s, a, x.rebuilds));
        }
    }
    if count < cfg.rebuild_min_count || amount < cfg.rebuild_min_tokens {
        return vec![];
    }
    let (ws, wa, wn) = worst.unwrap();
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::IdleRebuilds,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount,
            severity: None,
            finding: format!(
                "The prompt cache expired during a pause and was rebuilt about {} times, rewriting {} of context; the most in \"{}\" ({}, {}).",
                count,
                tok(amount),
                ws.label,
                plural(wn, "rebuild", "rebuilds"),
                tok(wa)
            ),
            action: "Finish or compact before long breaks; a resumed large session rewrites its whole context.".into(),
            view: "cache",
            filter: inp.filter.clone(),
            focus: Some("rebuilds".into()),
        },
    )]
}

/// System prompt and tool definitions take a large slice of every request.
fn heavy_tool_definitions(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut out = Vec::new();
    // (project, average overhead, most tools, amount) for the global summary.
    let mut hits: Vec<(String, f64, u64, f64)> = Vec::new();
    for (project, ss) in by_project(inp) {
        let with: Vec<&&SessionFacts> = ss
            .iter()
            .filter(|s| summary(s).overhead_tokens.is_some())
            .collect();
        let reqs: f64 = with.iter().map(|s| summary(s).requests as f64).sum();
        if reqs <= 0.0 {
            continue;
        }
        let avg = with
            .iter()
            .map(|s| summary(s).overhead_tokens.unwrap_or(0.0) * summary(s).requests as f64)
            .sum::<f64>()
            / reqs;
        let tools = with
            .iter()
            .filter_map(|s| summary(s).tools_loaded)
            .max()
            .unwrap_or(0);
        if avg < cfg.overhead_tokens && tools < cfg.tools_loaded {
            continue;
        }
        let amount: f64 = with
            .iter()
            .map(|s| s.scaled(summary(s).label_tokens(labels::OVERHEAD)))
            .sum();
        let ctx: f64 = with
            .iter()
            .map(|s| summary(s).avg_context * summary(s).requests as f64)
            .sum::<f64>()
            / reqs;
        let name = base_name(project);
        let i = build(
            inp,
            cfg,
            Draft {
                rule: RuleId::HeavyToolDefinitions,
                scope: Scope::Project,
                subject: Some(project.into()),
                subject_label: Some(name.clone()),
                amount,
                severity: None,
                finding: format!(
                    "In {name}, every request carries about {} of system prompt and tool definitions (up to {tools} tools), {} of an average request; {} over the range.",
                    tok(avg),
                    pct(if ctx > 0.0 { avg / ctx } else { 0.0 }),
                    tok(amount)
                ),
                action: "Trim connected MCP servers for this project; prefer deferred tool loading."
                    .into(),
                view: "overhead",
                filter: project_filter(inp, project),
                focus: None,
            },
        );
        if i.share >= cfg.min_share {
            hits.push((name, avg, tools, amount));
            out.push(i);
        }
    }
    if cfg.collapse_projects > 0 && out.len() >= cfg.collapse_projects {
        hits.sort_by(|a, b| b.3.total_cmp(&a.3));
        let amount: f64 = hits.iter().map(|h| h.3).sum();
        let lo = hits.iter().map(|h| h.1).fold(f64::INFINITY, f64::min);
        let hi = hits.iter().map(|h| h.1).fold(0.0, f64::max);
        let tools = hits.iter().map(|h| h.2).max().unwrap_or(0);
        let names: Vec<&str> = hits.iter().take(3).map(|h| h.0.as_str()).collect();
        return vec![build(
            inp,
            cfg,
            Draft {
                rule: RuleId::HeavyToolDefinitions,
                scope: Scope::Global,
                subject: None,
                subject_label: None,
                amount,
                severity: None,
                finding: format!(
                    "In {} projects every request carries {} to {} of system prompt and tool definitions (up to {tools} tools); {} over the range, most in {}.",
                    hits.len(),
                    tok(lo),
                    tok(hi),
                    tok(amount),
                    names.join(", ")
                ),
                action: "Disconnect MCP servers you rarely use, or enable them only in the projects that need them; prefer deferred tool loading.".into(),
                view: "overhead",
                filter: inp.filter.clone(),
                focus: None,
            },
        )];
    }
    out
}

fn result_action(tool: &str, group: &str) -> String {
    match tool {
        "Read" => "Read file ranges instead of whole files; avoid reading generated or minified files.".into(),
        "Bash" => "Filter command output (tail, grep, quiet flags); avoid dumping logs.".into(),
        "Grep" | "Glob" => "Narrow search patterns and limit the number of results.".into(),
        "Agent" | "Task" => "Ask subagents for short summaries instead of full reports.".into(),
        _ if group == "MCP" => format!("Ask {tool} for narrower results: specific fields, fewer rows, text instead of screenshots."),
        _ => "Read file ranges, filter command output, avoid dumping logs.".into(),
    }
}

/// Single tool results so large that they weigh on every later request.
fn oversized_tool_results(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    #[derive(Default)]
    struct Acc {
        group: String,
        sessions: u64,
        largest: f64,
        amount: f64,
    }
    let mut by_tool: HashMap<String, Acc> = HashMap::new();
    for s in &inp.sessions {
        let Some(x) = &s.summary else { continue };
        for t in &x.tools {
            if t.largest_result < cfg.tool_result_tokens || t.result_tokens <= 0.0 {
                continue;
            }
            let a = by_tool.entry(t.name.clone()).or_default();
            a.group = t.group.clone();
            a.sessions += 1;
            a.largest = a.largest.max(t.largest_result);
            // The carried cost of this tool's results, in proportion to how
            // much of them was the one large result.
            a.amount += s.scaled(t.carried_tokens * (t.largest_result / t.result_tokens).min(1.0));
        }
    }
    let mut out = Vec::new();
    for (tool, a) in by_tool {
        let i = build(
            inp,
            cfg,
            Draft {
                rule: RuleId::OversizedToolResults,
                scope: Scope::Global,
                subject: Some(tool.clone()),
                subject_label: Some(tool.clone()),
                amount: a.amount,
                severity: None,
                finding: format!(
                    "{tool} returned a result over {} in {} (largest {}); those results stayed in context for about {}.",
                    tok(cfg.tool_result_tokens),
                    plural(a.sessions, "session", "sessions"),
                    tok(a.largest),
                    tok(a.amount)
                ),
                action: result_action(&tool, &a.group),
                view: "tools",
                filter: inp.filter.clone(),
                focus: Some(tool),
            },
        );
        if i.share >= cfg.min_share {
            out.push(i);
        }
    }
    out
}

fn dominant_action(tool: &str, group: &str) -> String {
    let l = tool.to_lowercase();
    if group == "MCP"
        && ["browser", "chrome", "playwright", "puppeteer"]
            .iter()
            .any(|k| l.contains(k))
    {
        return format!("Screenshots and page dumps from {tool} are large: read page text or query specific elements, and take fewer screenshots.");
    }
    match tool {
        "Read" => "Repeated full-file reads: read line ranges, and avoid re-reading files already in context.".into(),
        "Bash" => "Verbose command output: run tests and builds in quiet mode and filter output before it reaches the model.".into(),
        "Grep" | "Glob" => "Broad searches: narrow patterns and cap the number of results.".into(),
        "Agent" | "Task" => "Long subagent reports: ask agents to return short summaries.".into(),
        _ => format!("Use {tool} for narrower requests, or less often."),
    }
}

/// One tool or MCP server fills a large part of a project's context.
fn one_tool_dominates(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut out = Vec::new();
    for (project, ss) in by_project(inp) {
        let ctx: f64 = ss.iter().map(|s| s.scaled(summary(s).context_tokens)).sum();
        if ctx <= 0.0 {
            continue;
        }
        let mut tools: HashMap<(&str, &str), f64> = HashMap::new();
        for s in &ss {
            for t in &summary(s).tools {
                *tools.entry((&t.name, &t.group)).or_default() += s.scaled(t.carried_tokens);
            }
        }
        let Some(((tool, group), amount)) = tools
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
        else {
            continue;
        };
        if amount / ctx < cfg.tool_dominates_share {
            continue;
        }
        let name = base_name(project);
        let what = if group == "MCP" { "MCP server" } else { "tool" };
        let i = build(
            inp,
            cfg,
            Draft {
                rule: RuleId::OneToolDominates,
                scope: Scope::Project,
                subject: Some(format!("{project}#{tool}")),
                subject_label: Some(format!("{name}: {tool}")),
                amount,
                severity: None,
                finding: format!(
                    "In {name}, results from the {what} {tool} were {} of the context ({}).",
                    pct(amount / ctx),
                    tok(amount)
                ),
                action: dominant_action(tool, group),
                view: "tools",
                filter: project_filter(inp, project),
                focus: Some(tool.to_string()),
            },
        );
        if i.share >= cfg.min_share {
            out.push(i);
        }
    }
    out
}

/// A large CLAUDE.md or other instructions ride on every request.
fn large_instructions(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut out = Vec::new();
    for (project, ss) in by_project(inp) {
        let with: Vec<&&SessionFacts> = ss
            .iter()
            .filter(|s| summary(s).label_tokens(labels::INSTRUCTIONS) > 0.0)
            .collect();
        let reqs: f64 = with.iter().map(|s| summary(s).requests as f64).sum();
        if reqs <= 0.0 {
            continue;
        }
        let carried: f64 = with
            .iter()
            .map(|s| summary(s).label_tokens(labels::INSTRUCTIONS))
            .sum();
        // Average size in context per request: a lower bound of the file's
        // size, since it enters a little after the session starts.
        let size = carried / reqs;
        if size < cfg.instructions_tokens {
            continue;
        }
        let amount: f64 = with
            .iter()
            .map(|s| s.scaled(summary(s).label_tokens(labels::INSTRUCTIONS)))
            .sum();
        let name = base_name(project);
        let i = build(
            inp,
            cfg,
            Draft {
                rule: RuleId::LargeInstructions,
                scope: Scope::Project,
                subject: Some(project.into()),
                subject_label: Some(name.clone()),
                amount,
                severity: None,
                finding: format!(
                    "In {name}, CLAUDE.md and other instruction files averaged {} of every request's context; {} over the range.",
                    tok(size),
                    tok(amount)
                ),
                action: "Shorten or split the file; move rarely needed parts into skills.".into(),
                view: "injected",
                filter: project_filter(inp, project),
                focus: Some(labels::INSTRUCTIONS.into()),
            },
        );
        if i.share >= cfg.min_share {
            out.push(i);
        }
    }
    out
}

/// Content Claude Code adds by itself is a large share of a project's context.
fn injected_share(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut out = Vec::new();
    for (project, ss) in by_project(inp) {
        let ctx: f64 = ss.iter().map(|s| s.scaled(summary(s).context_tokens)).sum();
        if ctx <= 0.0 {
            continue;
        }
        // Instructions have their own rule.
        let amount: f64 = ss
            .iter()
            .map(|s| {
                let x = summary(s);
                s.scaled(
                    x.group_tokens(labels::GROUP_INJECTED) - x.label_tokens(labels::INSTRUCTIONS),
                )
            })
            .sum();
        if amount / ctx < cfg.injected_share {
            continue;
        }
        let mut types: HashMap<&str, f64> = HashMap::new();
        for s in &ss {
            for sl in summary(s)
                .slices
                .iter()
                .filter(|x| x.group == labels::GROUP_INJECTED && x.label != labels::INSTRUCTIONS)
            {
                *types.entry(&sl.label).or_default() += s.scaled(sl.tokens);
            }
        }
        let top = types
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(a.0)));
        let name = base_name(project);
        let (top_label, top_amount) = top.unwrap_or(("injected content", 0.0));
        let i = build(
            inp,
            cfg,
            Draft {
                rule: RuleId::InjectedShare,
                scope: Scope::Project,
                subject: Some(project.into()),
                subject_label: Some(name.clone()),
                amount,
                severity: None,
                finding: format!(
                    "In {name}, {} of the context was added by Claude Code itself, not counting CLAUDE.md ({}); the largest part is {} at {}.",
                    pct(amount / ctx),
                    tok(amount),
                    top_label.to_lowercase(),
                    tok(top_amount)
                ),
                action: injected_action(top_label),
                view: "injected",
                filter: project_filter(inp, project),
                focus: Some(top_label.to_string()),
            },
        );
        if i.share >= cfg.min_share {
            out.push(i);
        }
    }
    out
}

fn injected_action(label: &str) -> String {
    match label {
        "Deferred tool lists" | "MCP server instructions" => {
            "Disconnect MCP servers this project does not use; each one adds its tool list and instructions.".into()
        }
        "Skill list" | "Agent list" => {
            "Disable plugins and skills this project does not use; their lists are re-sent in every session.".into()
        }
        "Hook output" | "Hook errors" => "Make hooks print less, or only on failure.".into(),
        "Edited file snapshots" => {
            "Files changed outside the session are re-sent in full; avoid editing large files in parallel with a session.".into()
        }
        "Attached files" => "Attach smaller files, or reference a line range.".into(),
        _ => format!(
            "Reduce {}: fewer connected servers, skills and hooks that add context on every turn.",
            label.to_lowercase()
        ),
    }
}

fn high_effort(e: &str) -> bool {
    matches!(e, "high" | "xhigh" | "max")
}

/// The model's earlier thinking fills the context, or most output is
/// thinking.
fn thinking_share(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let ctx: f64 = inp
        .sessions
        .iter()
        .filter_map(|s| s.summary.as_ref().map(|x| s.scaled(x.context_tokens)))
        .sum();
    let thinking_ctx: f64 = inp
        .sessions
        .iter()
        .filter_map(|s| {
            s.summary
                .as_ref()
                .map(|x| s.scaled(x.label_tokens(labels::THINKING)))
        })
        .sum();
    let (out, think): (u64, u64) = inp
        .effort
        .iter()
        .fold((0, 0), |a, e| (a.0 + e.output, a.1 + e.thinking));
    let ctx_share = if ctx > 0.0 { thinking_ctx / ctx } else { 0.0 };
    let out_share = if out > 0 {
        think as f64 / out as f64
    } else {
        0.0
    };
    if ctx_share < cfg.thinking_context_share && out_share < cfg.thinking_output_share {
        return vec![];
    }
    let reqs: u64 = inp.effort.iter().map(|e| e.requests).sum();
    let high: u64 = inp
        .effort
        .iter()
        .filter(|e| high_effort(&e.effort))
        .map(|e| e.requests)
        .sum();
    let (ho, ht) = inp
        .effort
        .iter()
        .filter(|e| high_effort(&e.effort))
        .fold((0u64, 0u64), |a, e| (a.0 + e.output, a.1 + e.thinking));
    let effort_note = if reqs > 0 && high > 0 && ho > 0 {
        format!(
            " {} of requests ran at high effort or above, where {} of output was thinking.",
            pct(high as f64 / reqs as f64),
            pct(ht as f64 / ho as f64)
        )
    } else {
        String::new()
    };
    let amount = thinking_ctx + think as f64;
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::ThinkingShare,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount,
            severity: None,
            finding: format!(
                "Earlier thinking was {} of the context ({}) and {} of output was thinking.{effort_note}",
                pct(ctx_share),
                tok(thinking_ctx),
                pct(out_share)
            ),
            action: "Lower effort for routine work; keep high effort for hard problems.".into(),
            view: "effort",
            filter: inp.filter.clone(),
            focus: Some(labels::THINKING.into()),
        },
    )]
}

/// Subagents use a large share of tokens.
fn subagent_cost(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let amount: f64 = inp.sessions.iter().map(|s| s.subagent_tokens as f64).sum();
    let total = inp.totals.total.max(1) as f64;
    if amount / total < cfg.subagent_share {
        return vec![];
    }
    let mut types: HashMap<&str, (u64, f64)> = HashMap::new();
    for s in &inp.sessions {
        let Some(x) = &s.summary else { continue };
        for a in &x.subagents {
            let e = types
                .entry(a.agent_type.as_deref().unwrap_or("unknown"))
                .or_default();
            e.0 += 1;
            e.1 += s.scaled(a.tokens as f64);
        }
    }
    let mut types: Vec<_> = types.into_iter().collect();
    types.sort_by(|a, b| b.1 .1.total_cmp(&a.1 .1).then(a.0.cmp(b.0)));
    let named = types
        .iter()
        .take(2)
        .map(|(t, (n, a))| format!("{t} ({}, {})", plural(*n, "run", "runs"), tok(*a)))
        .collect::<Vec<_>>()
        .join(" and ");
    let top = inp
        .sessions
        .iter()
        .max_by_key(|s| s.subagent_tokens)
        .filter(|s| s.subagent_tokens > 0);
    let top_note = top
        .map(|s| {
            format!(
                " The most was in \"{}\" ({}).",
                s.label,
                tok(s.subagent_tokens as f64)
            )
        })
        .unwrap_or_default();
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::SubagentCost,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount,
            severity: None,
            finding: format!(
                "Subagents used {} of all tokens ({}){}.{top_note}",
                pct(amount / total),
                tok(amount),
                if named.is_empty() {
                    String::new()
                } else {
                    format!(", mostly {named}")
                }
            ),
            action: "Review which agent types run; narrow their briefs and ask for short results."
                .into(),
            view: "subagents",
            filter: inp.filter.clone(),
            focus: None,
        },
    )]
}

/// Failing hooks and API errors.
fn errors_and_hooks(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let (mut hooks, mut api, mut amount) = (0u64, 0u64, 0.0);
    let mut worst: Option<&SessionFacts> = None;
    for s in &inp.sessions {
        let Some(x) = &s.summary else { continue };
        hooks += x.hook_errors;
        api += x.api_errors;
        amount += s.scaled(x.label_tokens(labels::HOOK_ERRORS));
        if worst.is_none_or(|w| {
            let wx = summary(w);
            x.hook_errors + x.api_errors > wx.hook_errors + wx.api_errors
        }) {
            worst = Some(s);
        }
    }
    if hooks < cfg.hook_errors_min && api < cfg.api_errors_min {
        return vec![];
    }
    let mut parts = Vec::new();
    if hooks > 0 {
        parts.push(format!("hooks failed {} times", hooks));
    }
    if api > 0 {
        parts.push(plural(api, "API error", "API errors"));
    }
    let cost = if amount >= 1.0 {
        format!("; hook error output added {} of context", tok(amount))
    } else {
        "; the errors were not sent to the model".to_string()
    };
    let w = worst.unwrap();
    let action = if hooks >= cfg.hook_errors_min {
        "Fix or remove the failing hook (check hooks in settings.json)."
    } else {
        "Frequent API errors cause retries; check connectivity, rate limits and the model status page."
    };
    // TODO(4.G): tokens on retried requests and tool denials by kind need
    // the `events` table (PLAN 4.A.2).
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::ErrorsAndHooks,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount,
            severity: if amount / inp.totals.total.max(1) as f64 >= cfg.severity_medium {
                None
            } else {
                Some(Severity::Info)
            },
            finding: format!(
                "{}{cost}. The most were in \"{}\".",
                {
                    let s = parts.join(" and ");
                    let mut c = s.chars();
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default()
                },
                w.label
            ),
            action: action.into(),
            view: "session",
            filter: session_filter(inp, &w.id),
            focus: Some("errors".into()),
        },
    )]
}

/// Usage moved a lot since the period before; say why.
fn week_over_week(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let Some(c) = &inp.change else { return vec![] };
    let Some(p) = c.change else { return vec![] };
    if p.abs() < cfg.change_pct {
        return vec![];
    }
    let main = c
        .factors
        .iter()
        .max_by(|a, b| a.contribution.abs().total_cmp(&b.contribution.abs()))
        .map(|f| f.id);
    let action = match (main, p > 0.0) {
        (_, false) => "Nothing to fix; usage went down.",
        (Some(FactorId::Requests), true) => {
            "Nothing to fix if you worked more; otherwise look for loops or scheduled tasks."
        }
        (Some(FactorId::ContextPerRequest), true) => {
            "Requests got bigger: see long sessions and the largest tool results."
        }
        (Some(FactorId::OutputPerRequest), true) => {
            "The model wrote more per request: check effort level and large file writes."
        }
        (Some(FactorId::CacheHitRate), true) => {
            "Less was served from cache: look for idle rebuilds and frequent new sessions."
        }
        (Some(FactorId::ModelMix), true) => {
            "More work ran on pricier models: use a smaller model for routine tasks."
        }
        (None, true) => "Compare the two periods on the Overview.",
    };
    vec![build(
        inp,
        cfg,
        Draft {
            rule: RuleId::WeekOverWeek,
            scope: Scope::Global,
            subject: None,
            subject_label: None,
            amount: c.delta.abs(),
            severity: Some(if p > 0.0 {
                if p >= 2.0 * cfg.change_pct {
                    Severity::Medium
                } else {
                    Severity::Low
                }
            } else {
                Severity::Info
            }),
            finding: format!(
                "Tokens {} ({}, {} → {}). {}",
                if p < 0.0 {
                    format!("−{:.0}%", p.abs())
                } else {
                    format!("+{p:.0}%")
                },
                c.periods.label,
                tok(c.before),
                tok(c.after),
                c.explanation
            ),
            action: action.into(),
            view: "overview",
            filter: inp.filter.clone(),
            focus: Some("change".into()),
        },
    )]
}

/// Run every rule. Actionable insights first, then informational ones; each
/// group ranked by amount.
pub fn evaluate(inp: &Inputs, cfg: &Thresholds) -> Vec<Insight> {
    let mut out: Vec<Insight> = RULES
        .iter()
        .flat_map(|(_, r)| {
            let mut v = r(inp, cfg);
            v.sort_by(|a, b| b.amount.total_cmp(&a.amount));
            v.truncate(cfg.max_per_rule.max(1));
            v
        })
        .collect();
    out.sort_by(|a, b| {
        (a.severity == Severity::Info)
            .cmp(&(b.severity == Severity::Info))
            .then(b.amount.total_cmp(&a.amount))
            .then(a.key.cmp(&b.key))
    });
    out
}

/// Every rule with whether it runs on current data.
pub fn rules() -> Vec<(RuleId, bool)> {
    let mut v: Vec<(RuleId, bool)> = RULES.iter().map(|(id, _)| (*id, true)).collect();
    v.extend(skipped_rules().into_iter().map(|s| (s.rule, false)));
    v
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

const CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS insight_cache(
  session TEXT PRIMARY KEY, stamp TEXT NOT NULL, version INTEGER NOT NULL, summary TEXT NOT NULL)";

/// Create the cache table if missing. Safe to call on every report.
pub fn ensure_cache(conn: &Connection) -> Result<()> {
    conn.execute_batch(CACHE_SCHEMA)?;
    Ok(())
}

/// Drop every cached summary.
pub fn clear_cache(conn: &Connection) -> Result<()> {
    ensure_cache(conn)?;
    conn.execute("DELETE FROM insight_cache", [])?;
    Ok(())
}

/// A session's file stamps: count, newest mtime and total size of every
/// transcript file (main and subagents) with rows for it.
fn stamps(conn: &Connection, from: &str, to: &str) -> Result<HashMap<String, String>> {
    let mut st = conn.prepare(
        "SELECT r.session, COUNT(*), COALESCE(MAX(f.mtime), 0), COALESCE(SUM(f.size), 0)
         FROM (SELECT DISTINCT session, file FROM requests
               WHERE session IN (SELECT DISTINCT session FROM requests WHERE day >= ?1 AND day <= ?2)) r
         LEFT JOIN files f ON f.path = r.file
         GROUP BY r.session",
    )?;
    let rows = st.query_map(params![from, to], |r| {
        Ok((
            r.get::<_, String>(0)?,
            format!(
                "{}:{}:{}",
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?
            ),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn cached(conn: &Connection) -> Result<HashMap<String, (String, String)>> {
    let mut st =
        conn.prepare("SELECT session, stamp, summary FROM insight_cache WHERE version = ?1")?;
    let rows = st.query_map([CACHE_VERSION], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Analyze `ids`, in parallel when the index is a file.
fn analyze_many(conn: &Connection, ids: &[String]) -> Vec<(String, Option<SessionSummary>)> {
    let path = conn.path().filter(|p| !p.is_empty()).map(String::from);
    match path {
        Some(p) if ids.len() > 1 => ids
            .par_iter()
            .map_init(
                || {
                    Connection::open_with_flags(
                        &p,
                        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                    )
                    .ok()
                },
                |c, id| {
                    let s = c.as_ref().and_then(|c| summarize(c, id).ok());
                    (id.clone(), s)
                },
            )
            .collect(),
        _ => ids
            .iter()
            .map(|id| (id.clone(), summarize(conn, id).ok()))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Gathering
// ---------------------------------------------------------------------------

/// Read everything the rules need for `f`'s range, analyzing sessions whose
/// cached summary is missing or stale. Returns the inputs and how many
/// sessions were analyzed now.
pub fn gather(conn: &Connection, f: &Filter, cfg: &Thresholds) -> Result<(Inputs, usize)> {
    ensure_cache(conn)?;
    let totals = query::totals(conn, f)?;
    let groups = query::breakdown(conn, f, Dim::Session, 1_000_000)?;
    let subs: HashMap<String, u64> = query::breakdown(
        conn,
        &Filter {
            subagent: Some(true),
            ..f.clone()
        },
        Dim::Session,
        1_000_000,
    )?
    .into_iter()
    .map(|g| (g.key, g.totals.total))
    .collect();
    let effort = query::breakdown(conn, f, Dim::Effort, 100)?
        .into_iter()
        .map(|g| EffortRow {
            effort: g.key,
            requests: g.totals.requests,
            output: g.totals.output,
            thinking: g.totals.thinking,
        })
        .collect();

    let from = f.from.clone().unwrap_or_else(|| "0000-00-00".into());
    let to = f.to.clone().unwrap_or_else(|| "9999-99-99".into());
    // Whole-session totals and main project, for sessions active in the range.
    let mut whole: HashMap<String, (u64, String)> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT session, project, COUNT(*), SUM(input + output + cache_write + cache_read)
             FROM requests
             WHERE session IN (SELECT DISTINCT session FROM requests WHERE day >= ?1 AND day <= ?2)
             GROUP BY session, project",
        )?;
        let mut best: HashMap<String, i64> = HashMap::new();
        let rows = st.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (s, p, n, t) = row?;
            let e = whole.entry(s.clone()).or_insert((0, String::new()));
            e.0 += t.max(0) as u64;
            if best.get(&s).is_none_or(|b| n > *b) {
                best.insert(s, n);
                e.1 = p;
            }
        }
    }

    let stamps = stamps(conn, &from, &to)?;
    let cache = cached(conn)?;
    let mut summaries: HashMap<String, SessionSummary> = HashMap::new();
    let mut stale = Vec::new();
    // `groups` is ordered by tokens, largest first.
    for g in groups.iter().take(cfg.max_sessions) {
        let stamp = stamps.get(&g.key).cloned().unwrap_or_default();
        match cache.get(&g.key) {
            Some((s, json)) if *s == stamp => {
                if let Ok(x) = serde_json::from_str(json) {
                    summaries.insert(g.key.clone(), x);
                    continue;
                }
                stale.push(g.key.clone());
            }
            _ => stale.push(g.key.clone()),
        }
    }
    let fresh = analyze_many(conn, &stale);
    let analyzed_now = fresh.iter().filter(|x| x.1.is_some()).count();
    {
        let mut ins = conn.prepare(
            "INSERT INTO insight_cache(session, stamp, version, summary) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session) DO UPDATE SET stamp = excluded.stamp,
               version = excluded.version, summary = excluded.summary",
        )?;
        conn.execute_batch("BEGIN")?;
        for (id, s) in fresh {
            let Some(s) = s else { continue };
            let json = serde_json::to_string(&s).unwrap_or_default();
            let stamp = stamps.get(&id).cloned().unwrap_or_default();
            ins.execute(params![id, stamp, CACHE_VERSION, json])?;
            summaries.insert(id, s);
        }
        conn.execute_batch("COMMIT")?;
    }

    let sessions = groups
        .into_iter()
        .map(|g| {
            let (all, project) = whole.get(&g.key).cloned().unwrap_or_default();
            SessionFacts {
                label: g.label,
                project,
                tokens: g.totals.total,
                subagent_tokens: subs.get(&g.key).copied().unwrap_or(0),
                scale: if all > 0 {
                    (g.totals.total as f64 / all as f64).min(1.0)
                } else {
                    1.0
                },
                summary: summaries.remove(&g.key),
                id: g.key,
            }
        })
        .collect();
    Ok((
        Inputs {
            filter: f.clone(),
            totals,
            sessions,
            effort,
            change: None,
        },
        analyzed_now,
    ))
}

/// Insights for `f`'s range, with the week-over-week change ending `now`.
pub fn report(conn: &Connection, f: &Filter, now: &Zoned, cfg: &Thresholds) -> Result<Report> {
    let (mut inp, analyzed_now) = gather(conn, f, cfg)?;
    let periods = Periods::trailing(now, cfg.change_days)?;
    inp.change = Some(change::compare(conn, f, periods, &Tokens)?);
    let insights = evaluate(&inp, cfg);
    Ok(Report {
        from: f.from.clone(),
        to: f.to.clone(),
        unit: "tokens".into(),
        totals: inp.totals.clone(),
        sessions: inp.sessions.len(),
        analyzed: inp.sessions.iter().filter(|s| s.summary.is_some()).count(),
        analyzed_now,
        insights,
        skipped: skipped_rules(),
        change: inp.change,
        thresholds: cfg.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{decompose, ModelTotals, Period};

    const M: f64 = 1_000_000.0;

    fn totals(total: u64) -> Totals {
        Totals {
            total,
            ..Default::default()
        }
    }

    fn sum(slices: &[(&str, &str, f64)]) -> SessionSummary {
        let slices: Vec<SliceSum> = slices
            .iter()
            .map(|(l, g, t)| SliceSum {
                label: (*l).into(),
                group: (*g).into(),
                tokens: *t,
            })
            .collect();
        SessionSummary {
            requests: 100,
            context_tokens: slices.iter().map(|s| s.tokens).sum(),
            slices,
            ..Default::default()
        }
    }

    fn sess(id: &str, project: &str, tokens: f64, s: SessionSummary) -> SessionFacts {
        SessionFacts {
            id: id.into(),
            label: format!("session {id}"),
            project: project.into(),
            tokens: tokens as u64,
            scale: 1.0,
            summary: Some(s),
            ..Default::default()
        }
    }

    fn inputs(total: f64, sessions: Vec<SessionFacts>) -> Inputs {
        Inputs {
            totals: totals(total as u64),
            sessions,
            ..Default::default()
        }
    }

    fn run(rule: Rule, inp: &Inputs) -> Vec<Insight> {
        rule(inp, &Thresholds::default())
    }

    #[test]
    fn long_sessions_fire_on_share() {
        let mut long = sum(&[("Bash", "Tools", 50.0 * M)]);
        long.requests = 500;
        let short = sum(&[("Bash", "Tools", 10.0 * M)]);
        let inp = inputs(
            100.0 * M,
            vec![
                sess("a", "/p", 60.0 * M, long.clone()),
                sess("b", "/p", 40.0 * M, short.clone()),
            ],
        );
        let r = run(long_sessions, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].amount, 60.0 * M);
        assert_eq!(r[0].severity, Severity::High);
        assert!(r[0].finding.starts_with("1 of 2 sessions"));
        let inp = inputs(
            100.0 * M,
            vec![
                sess("a", "/p", 30.0 * M, long),
                sess("b", "/p", 70.0 * M, short),
            ],
        );
        assert!(run(long_sessions, &inp).is_empty());
    }

    #[test]
    fn idle_rebuilds_need_count_and_tokens() {
        let mut s = sum(&[]);
        s.rebuilds = 4;
        s.rebuild_tokens = 3_000_000;
        let inp = inputs(100.0 * M, vec![sess("a", "/p", 50.0 * M, s.clone())]);
        let r = run(idle_rebuilds, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].amount, 3.0 * M);
        assert_eq!(r[0].evidence.view, "cache");
        s.rebuilds = 2;
        let inp = inputs(100.0 * M, vec![sess("a", "/p", 50.0 * M, s)]);
        assert!(run(idle_rebuilds, &inp).is_empty());
    }

    #[test]
    fn heavy_tool_definitions_per_project() {
        let mut s = sum(&[
            (labels::OVERHEAD, "Overhead", 5.0 * M),
            ("Bash", "Tools", 5.0 * M),
        ]);
        s.overhead_tokens = Some(50_000.0);
        s.tools_loaded = Some(70);
        s.avg_context = 100_000.0;
        let mut light = s.clone();
        light.overhead_tokens = Some(12_000.0);
        let inp = inputs(
            20.0 * M,
            vec![
                sess("a", "/r/heavy", 10.0 * M, s),
                sess("b", "/r/light", 10.0 * M, light),
            ],
        );
        let r = run(heavy_tool_definitions, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].subject.as_deref(), Some("/r/heavy"));
        assert_eq!(r[0].scope, Scope::Project);
        assert_eq!(r[0].evidence.filter.project.as_deref(), Some("/r/heavy"));
        assert!(r[0].finding.contains("50.0k"));
        assert!(r[0].finding.contains("50%"));
    }

    #[test]
    fn oversized_results_by_tool() {
        let mut s = sum(&[]);
        s.tools = vec![
            ToolSum {
                name: "Read".into(),
                group: "Tools".into(),
                calls: 10,
                result_tokens: 60_000.0,
                carried_tokens: 6.0 * M,
                largest_result: 30_000.0,
            },
            ToolSum {
                name: "Grep".into(),
                group: "Tools".into(),
                calls: 10,
                result_tokens: 20_000.0,
                carried_tokens: 2.0 * M,
                largest_result: 5_000.0,
            },
        ];
        let inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        let r = run(oversized_tool_results, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].subject.as_deref(), Some("Read"));
        assert_eq!(r[0].amount, 3.0 * M);
        assert!(r[0].action.starts_with("Read file ranges"));
    }

    #[test]
    fn one_tool_dominates_names_the_case() {
        let mut s = sum(&[
            ("claude-in-chrome", "MCP", 4.0 * M),
            ("Bash", "Tools", 1.0 * M),
            ("Text and tool calls", "Model output", 5.0 * M),
        ]);
        s.tools = vec![
            ToolSum {
                name: "claude-in-chrome".into(),
                group: "MCP".into(),
                carried_tokens: 4.0 * M,
                ..Default::default()
            },
            ToolSum {
                name: "Bash".into(),
                group: "Tools".into(),
                carried_tokens: 1.0 * M,
                ..Default::default()
            },
        ];
        let inp = inputs(10.0 * M, vec![sess("a", "/r/web", 10.0 * M, s)]);
        let r = run(one_tool_dominates, &inp);
        assert_eq!(r.len(), 1);
        assert!(r[0].finding.contains("40%"));
        assert!(r[0].action.contains("Screenshots"));
        assert_eq!(r[0].evidence.focus.as_deref(), Some("claude-in-chrome"));
    }

    #[test]
    fn large_instructions_by_average_size() {
        let s = sum(&[
            (labels::INSTRUCTIONS, "Injected", 600_000.0),
            ("Bash", "Tools", 9.4 * M),
        ]);
        let small = sum(&[
            (labels::INSTRUCTIONS, "Injected", 100_000.0),
            ("Bash", "Tools", 9.9 * M),
        ]);
        let inp = inputs(
            20.0 * M,
            vec![
                sess("a", "/r/big", 10.0 * M, s),
                sess("b", "/r/small", 10.0 * M, small),
            ],
        );
        let r = run(large_instructions, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].subject.as_deref(), Some("/r/big"));
        assert!(r[0].finding.contains("averaged 6.0k of every request"));
    }

    #[test]
    fn injected_share_names_largest_type() {
        let s = sum(&[
            ("Deferred tool lists", "Injected", 1.5 * M),
            ("Reminders", "Injected", 0.5 * M),
            ("Bash", "Tools", 8.0 * M),
        ]);
        let inp = inputs(10.0 * M, vec![sess("a", "/r/x", 10.0 * M, s)]);
        let r = run(injected_share, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].amount, 2.0 * M);
        assert!(r[0].finding.contains("20%"));
        assert!(r[0].finding.contains("deferred tool lists"));
        let s = sum(&[
            ("Reminders", "Injected", 0.5 * M),
            ("Bash", "Tools", 9.5 * M),
        ]);
        let inp = inputs(10.0 * M, vec![sess("a", "/r/x", 10.0 * M, s)]);
        assert!(run(injected_share, &inp).is_empty());
        // Instructions are left to their own rule.
        let s = sum(&[
            (labels::INSTRUCTIONS, "Injected", 3.0 * M),
            ("Reminders", "Injected", 0.5 * M),
            ("Bash", "Tools", 6.5 * M),
        ]);
        let inp = inputs(10.0 * M, vec![sess("a", "/r/x", 10.0 * M, s)]);
        assert!(run(injected_share, &inp).is_empty());
    }

    #[test]
    fn heavy_definitions_collapse_to_global() {
        let mut s = sum(&[
            (labels::OVERHEAD, "Overhead", 5.0 * M),
            ("Bash", "Tools", 5.0 * M),
        ]);
        s.overhead_tokens = Some(50_000.0);
        s.avg_context = 100_000.0;
        let ss = (0..4)
            .map(|i| sess(&format!("s{i}"), &format!("/r/p{i}"), 10.0 * M, s.clone()))
            .collect();
        let r = run(heavy_tool_definitions, &inputs(40.0 * M, ss));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].scope, Scope::Global);
        assert_eq!(r[0].amount, 20.0 * M);
        assert!(r[0].finding.starts_with("In 4 projects"));
    }

    #[test]
    fn evaluate_caps_each_rule() {
        let ss = (0..5)
            .map(|i| {
                let s = sum(&[
                    ("Skill list", "Injected", (3 + i) as f64 * M),
                    ("Bash", "Tools", 7.0 * M),
                ]);
                sess(&format!("s{i}"), &format!("/r/p{i}"), 10.0 * M, s)
            })
            .collect();
        let r = evaluate(&inputs(50.0 * M, ss), &Thresholds::default());
        let inj: Vec<&Insight> = r
            .iter()
            .filter(|i| i.rule == RuleId::InjectedShare)
            .collect();
        assert_eq!(inj.len(), 3);
        assert_eq!(inj[0].subject.as_deref(), Some("/r/p4"));
        assert!(inj[0].action.starts_with("Disable plugins and skills"));
    }

    #[test]
    fn thinking_share_from_context_or_output() {
        let s = sum(&[
            (labels::THINKING, "Model output", 3.0 * M),
            ("Bash", "Tools", 7.0 * M),
        ]);
        let mut inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        inp.effort = vec![
            EffortRow {
                effort: "xhigh".into(),
                requests: 40,
                output: 1000,
                thinking: 600,
            },
            EffortRow {
                effort: "medium".into(),
                requests: 60,
                output: 1000,
                thinking: 100,
            },
        ];
        let r = run(thinking_share, &inp);
        assert_eq!(r.len(), 1);
        assert!(r[0].finding.contains("30% of the context"));
        assert!(r[0].finding.contains("40% of requests"));
        assert!(r[0].finding.contains("60% of output"));
        let s = sum(&[
            (labels::THINKING, "Model output", 1.0 * M),
            ("Bash", "Tools", 9.0 * M),
        ]);
        let mut inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        inp.effort = vec![EffortRow {
            effort: "medium".into(),
            requests: 1,
            output: 100,
            thinking: 10,
        }];
        assert!(run(thinking_share, &inp).is_empty());
    }

    #[test]
    fn subagent_cost_names_types() {
        let mut s = sum(&[]);
        s.subagents = vec![
            SubagentSum {
                agent_type: Some("Explore".into()),
                requests: 10,
                tokens: 2_000_000,
            },
            SubagentSum {
                agent_type: Some("Explore".into()),
                requests: 10,
                tokens: 1_000_000,
            },
        ];
        let mut a = sess("a", "/p", 10.0 * M, s);
        a.subagent_tokens = 3_000_000;
        let inp = inputs(10.0 * M, vec![a]);
        let r = run(subagent_cost, &inp);
        assert_eq!(r.len(), 1);
        assert!(r[0].finding.contains("30%"));
        assert!(r[0].finding.contains("Explore (2 runs"));
        let mut b = sess("b", "/p", 10.0 * M, sum(&[]));
        b.subagent_tokens = 1_000_000;
        assert!(run(subagent_cost, &inputs(10.0 * M, vec![b])).is_empty());
    }

    #[test]
    fn errors_and_hooks_info_when_free() {
        let mut s = sum(&[]);
        s.hook_errors = 12;
        s.api_errors = 2;
        let inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        let r = run(errors_and_hooks, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].severity, Severity::Info);
        assert!(r[0]
            .finding
            .starts_with("Hooks failed 12 times and 2 API errors"));
        assert_eq!(r[0].evidence.filter.session.as_deref(), Some("a"));
        let mut s = sum(&[(labels::HOOK_ERRORS, "Injected", 1.0 * M)]);
        s.hook_errors = 6;
        let inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        assert_eq!(run(errors_and_hooks, &inp)[0].severity, Severity::High);
        let mut s = sum(&[]);
        s.hook_errors = 2;
        let inp = inputs(10.0 * M, vec![sess("a", "/p", 10.0 * M, s)]);
        assert!(run(errors_and_hooks, &inp).is_empty());
    }

    fn change_of(before: u64, after: u64) -> Change {
        let t = |n: u64| Totals {
            requests: n,
            cache_read: n * 1000,
            ..Default::default()
        };
        let p = Period {
            from: "a".into(),
            to: "b".into(),
            before_ts: None,
        };
        decompose(
            &[ModelTotals {
                model: "m".into(),
                totals: t(before),
            }],
            &[ModelTotals {
                model: "m".into(),
                totals: t(after),
            }],
            &Tokens,
            Periods {
                before: p.clone(),
                after: p,
                label: "last 7 days vs the 7 before".into(),
            },
        )
    }

    #[test]
    fn week_over_week_states_cause() {
        let mut inp = inputs(10.0 * M, vec![]);
        inp.change = Some(change_of(100, 130));
        let r = run(week_over_week, &inp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].severity, Severity::Low);
        assert!(r[0].finding.contains("Tokens +30%"));
        assert!(r[0].finding.contains("you worked more"));
        inp.change = Some(change_of(100, 50));
        assert_eq!(run(week_over_week, &inp)[0].severity, Severity::Info);
        inp.change = Some(change_of(100, 110));
        assert!(run(week_over_week, &inp).is_empty());
    }

    #[test]
    fn scaling_and_ranking() {
        // Half of the session is in the range: whole-session amounts halve.
        let mut s = sum(&[]);
        s.rebuilds = 8;
        s.rebuild_tokens = 8_000_000;
        let mut a = sess("a", "/p", 10.0 * M, s);
        a.scale = 0.5;
        let mut long = sum(&[]);
        long.requests = 1000;
        let b = sess("b", "/p", 90.0 * M, long);
        let mut inp = inputs(100.0 * M, vec![a, b]);
        inp.change = Some(change_of(100, 50));
        let r = evaluate(&inp, &Thresholds::default());
        let ids: Vec<RuleId> = r.iter().map(|i| i.rule).collect();
        assert_eq!(
            ids,
            vec![
                RuleId::LongSessions,
                RuleId::IdleRebuilds,
                RuleId::WeekOverWeek
            ]
        );
        assert_eq!(r[1].amount, 4.0 * M);
    }

    #[test]
    fn stubbed_rules_are_listed() {
        let all = rules();
        assert!(all.contains(&(RuleId::UnusedMcpServers, false)));
        assert!(all.contains(&(RuleId::AutonomousSpend, false)));
        assert!(all.contains(&(RuleId::ModelFallback, false)));
        assert_eq!(all.len(), 14);
    }

    #[test]
    fn cache_round_trip() {
        let idx = crate::Index::open_in_memory().unwrap();
        let c = &idx.conn;
        ensure_cache(c).unwrap();
        ensure_cache(c).unwrap();
        let s = sum(&[("Bash", "Tools", 5.0)]);
        c.execute(
            "INSERT INTO insight_cache VALUES ('x', '1:2:3', ?1, ?2)",
            params![CACHE_VERSION, serde_json::to_string(&s).unwrap()],
        )
        .unwrap();
        let got = cached(c).unwrap();
        let back: SessionSummary = serde_json::from_str(&got["x"].1).unwrap();
        assert_eq!(back, s);
        clear_cache(c).unwrap();
        assert!(cached(c).unwrap().is_empty());
    }

    #[test]
    fn report_on_empty_index() {
        let idx = crate::Index::open_in_memory().unwrap();
        let now: Zoned = "2026-11-16T12:00[UTC]".parse().unwrap();
        let r = report(&idx.conn, &Filter::default(), &now, &Thresholds::default()).unwrap();
        assert_eq!(r.sessions, 0);
        assert!(r.insights.is_empty());
        assert_eq!(r.skipped.len(), 3);
    }
}
