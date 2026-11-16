# Claude Usage Analytics: product and implementation plan

Status: plan only. Nothing in this document is implemented unless section 1 says so.
Audience: the agent (or person) who will implement it. Read sections 1 to 3 before touching code.

## 0. Goal and principles

**Goal:** a user opens the app and, within a minute, can answer:

1. How much am I using, and is it more than usual?
2. What is spending it: which sessions, projects, tools, MCP servers, subagents, habits?
3. What should I change to spend less?

Today the app answers 1 well, 2 for one session at a time, and 3 only as a few per-session warnings.

**Principles (apply to every item below):**

- **Explain causes, not counters.** Every number should lead to "because of X". A total with no drill-down is a dead end.
- **One honest unit.** Raw token totals are 98% cache reads, which are the cheapest tokens. Showing them as the headline misleads. See section 3.1.
- **Exact where possible, labelled where estimated.** Context sizes per request are exact. Splits inside one step are estimates. The UI must say which is which.
- **Private by construction.** Read-only, offline, no message text stored or displayed. Sizes, counts, names and timestamps only. Any feature that needs text (for example showing a prompt) is opt-in and reads the transcript on demand without persisting it.
- **Actionable wording.** Each insight names the thing, its cost, and one concrete action.
- **Keep the stack.** Rust core + Tauri 2 + plain JS, no Node toolchain, same as `claude-storage-cleaner`.

## 1. What exists today

Repository: `~/Projects/claude-usage-analytics` (not committed yet, no remote).

| Part | Files | What it does |
| --- | --- | --- |
| Parser | `crates/core/src/parse.rs` | Reads `~/.claude/projects/**/*.jsonl` incrementally. One row per API response, keyed `message.id:requestId` (each response is logged once per content block, 54% of assistant lines are repeats). One project per file: first `cwd`, or `relocatedCwd`. Worktrees fold into their repo. |
| Index | `crates/core/src/index.rs` | SQLite (`rusqlite` bundled) at `~/Library/Application Support/digital.prototyp.claude-usage-analytics/index.sqlite`. Tables `files`, `requests`, `sessions(id,title)`. Schema version 2; a version bump drops and rebuilds. Full index of 3 GB / 1,285 files takes about 2 s, refresh about 60 ms. |
| Queries | `crates/core/src/query.rs` | `totals`, `daily`, `breakdown` by model, project, session, effort, agent, hour, weekday, entrypoint; `overview` (this week vs last week to the same time of day). |
| Session analyzer | `crates/core/src/session.rs` | On demand, re-reads one session's transcript. Context attribution (section 3.2), timeline, events (compaction, cache rebuild, subagent, API error), tools, subagents, warnings, Claude Code's `cost-state`. About 0.3 s for a 154 MB transcript. |
| CLI | `crates/core/src/bin/usage.rs` | `usage daily / summary / by <dim> / models / session <id>`, `--json`. |
| App | `crates/app` | Tauri commands `refresh`, `overview`, `daily`, `breakdown`, `models`, `session`. One `Mutex<Option<Index>>`. |
| UI | `ui/index.html`, `app.js`, `session.js`, `styles.css` | Overview, six breakdown tables, session detail page. Hand-written SVG charts. |
| Tests | `crates/core` | 12 tests: parser, dedupe, incremental read, queries, labels. No tests for the session analyzer's attribution. |

**Known defects and debts to fix first (Phase 0):**

1. Nothing is committed. Make the initial commit before any other work.
2. A schema bump drops all rows, including history of transcripts that were since deleted. Needs real migrations (section 4.A.1).
3. `session::analyze` is recomputed on every render, including the 60 s auto-refresh and window focus. Cache it (4.A.4).
4. Refresh holds the index mutex; every query waits. Use a separate read connection (WAL already on).
5. The session analyzer assumes earlier thinking stays in context. It should read this from `api-request.params.context_management` per request, not assume it.
6. Before the first `api-request-shape` record in a session, overhead is unknown and labelled "Start of context". When a later shape exists, back-fill from it.
7. Session attribution ignores subagent transcripts (they are listed, not attributed) and background model calls (Haiku for titles/web search show only in `cost-state.modelUsage`).
8. `model LIKE '%x%'` does not escape `%` and `_`.
9. `day`/`hour` are frozen in the system time zone at index time. Store `ts` only and derive in SQL with `localtime`, or re-derive on time zone change.
10. UI state leaks: `state.allTools` persists across sessions; filters are hidden on the session page with no date context.
11. There is no in-repo way to preview the UI without Tauri. During development a mock harness was used from a scratch folder: `window.mockInvoke` plus JSON produced by `usage … --json`. Add it to the repo as `ui/dev/` with a script that regenerates the mock from the local index (4.L.3).
12. Cask, signing, and release workflow are copied but untested for this app.

## 2. Data reference: what the transcripts contain

Everything below was verified on real transcripts (Claude Code 2.1.3xx, Claude Desktop entrypoint). Field coverage varies with Claude Code version: always code defensively and record "unknown" rather than guessing.

### 2.1 Files

- `~/.claude/projects/<encoded cwd>/<sessionId>.jsonl`: main thread.
- `…/<sessionId>/subagents/agent-<id>.jsonl`: one per subagent, plus `agent-<id>.meta.json` with `agentType`, `description`, `toolUseId` (links to the `Agent` tool call in the parent), `spawnDepth`, `finished`.
- `~/Library/Application Support/Claude/claude-code-sessions/<account>/<session>/local_*.json`: Desktop's session index. Keys include `cliSessionId`, `title`, `titleSource`, `model`, `effort`, `permissionMode`, `isArchived`, `createdAt`, `lastActivityAt`, `completedTurns`, `enabledMcpTools`, `remoteMcpServersConfig` (maps connector UUIDs to names), `branch`, `sourceBranch`, `originCwd`.
- `~/.claude/policy-limits.json`: org restrictions only, no usage or plan-limit numbers. Plan-limit percentages are not available locally.

### 2.2 Record types in a transcript (by `type`)

| Type | Useful fields | Use |
| --- | --- | --- |
| `assistant` | `message.{id,model,usage,stop_reason,content[]}`, `requestId`, `requestedModel`, `effort`, `perTurnEffort`, `advisorModel`, `isSidechain`, `timestamp`, `cwd`, `gitBranch`, `entrypoint`, `version`, `thinkingDurationMs`, `attributionMcpServer`, `attributionMcpTool`, `attributionSkill`, `attributionPlugin` | The request rows. `usage` has `input_tokens`, `output_tokens`, `output_tokens_details.thinking_tokens`, `cache_creation_input_tokens`, `cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`, `cache_read_input_tokens`, `server_tool_use.{web_search,web_fetch}_requests`, `service_tier`, `speed`, `inference_geo`. `content[]` blocks: `thinking` (text usually empty), `text`, `tool_use{id,name,input}`. |
| `user` | `message.content` (string, or blocks `text` / `image` / `tool_result{tool_use_id,content}`), `isMeta`, `isCompactSummary`, `promptSource`, `origin.kind`, `turnOrigin`, `turnPosition{promptIndex,turnIndex}`, `permissionMode`, `permissionDecision`, `toolDenialKind`, `toolUseResult`, `sourceToolUseID`, `imagePasteIds`, `mcpMeta`, `promptId` | Prompts, tool results, who started the turn. `turnOrigin` values seen: `human`, `task_notification`, `peer`, `sdk`, `host_synthetic`, `scheduled`. `toolDenialKind`: `permission-rule`, `automode-blocked`, `automode-unavailable`, `user-rejected`. |
| `attachment` | `attachment.type`, `rendered` (null when not sent to the model) | Injected context. Types seen: `environment`, `session_context`, `instructions`, `skill_listing`, `agent_listing_delta`, `deferred_tools_delta`, `mcp_instructions_delta`, `edited_text_file`, `file`, `compact_file_reference`, `queued_command`, `hook_additional_context`, `date`, `model`, `auto_mode`, `plan_mode`, `silent_turn_reminder`; and never rendered: `hook_non_blocking_error`, `deferred_tools_record`, `prompt_snapshot`, `credential_org`, `command_permissions`, `thinking_drop`. |
| `system` | `subtype`: `compact_boundary` with `compactMetadata{trigger,preTokens,postTokens,cumulativeDroppedTokens,durationMs,preCompactDiscoveredTools}`; `api_error` with `retryAttempt`, `retryInMs`; `stop_hook_summary` with `hookCount`, `hookErrors`, `preventedContinuation`; `informational` | Events. |
| `api-request-shape` | `shapeHash`, `shape.system[]`, `shape.tools[]` | The exact system prompt blocks and every tool definition sent. Measured in one session: system about 9.5k tokens; 75 tools about 52k tokens, of which built-in 29.5k (the `Artifact` tool alone 10k), `Claude_Browser` 6.2k, `claude-in-chrome` 4.7k. |
| `api-request` | `id`, `timestamp`, `querySource` (`sdk`, `agent:builtin:Explore`, `agent:custom:<name>`, …), `shapeHash`, `params{model,max_tokens,thinking,context_management,output_config.effort}`, `base`, `keep`, `tail[]` (blob hashes), `cacheControl[]` | Reconstructs the exact message list of every request: messages = base request's first `keep` messages + `tail`. Present in 271 of 600 sessions. |
| `api-request-blob` | `hash`, `message{role,content}` | The messages referenced by `tail`. |
| `cost-state` | `totalCostUSD`, `modelUsage{<model>:{inputTokens,outputTokens,thinkingTokens,cacheReadInputTokens,cacheCreationInputTokens,cacheCreationEphemeral5m/1hInputTokens,webSearchRequests,costUSD}}`, `totalAPIDuration`, `totalToolDuration`, `totalDuration`, `totalLinesAdded`, `totalLinesRemoved` | Claude Code's own running estimate. Present in 379 of 600 sessions. Includes background models that have no `assistant` rows. |
| `custom-title`, `agent-name` | `customTitle`, `agentName` | Session title. |
| `relocated` | `relocatedCwd` | Session moved folders. |
| `tools-used` | `tools[]` | Tools used so far. |
| `queue-operation` | `operation` (`enqueue`/`dequeue`/`remove`), `reason` (`absorbed_mid_turn`, …) | Messages queued while the model was working. |
| `mode`, `file-history-snapshot`, `file-history-delta`, `last-prompt`, `frame-link`, others | | Not needed, or skip for size. |

### 2.3 Typical patterns (use as sanity checks)

- About 98% of all tokens are cache reads; most usage is on the main thread, subagents a few percent.
- Long sessions dominate: a session that reaches the context limit averages hundreds of thousands of tokens of context per request and compacts several times.
- Idle pauses longer than the cache lifetime make the next request rewrite the whole context to the cache.
- `requestedModel` is missing on older records; compare it with `message.model` only when present.
- Most turns are started by a person; the rest come from task notifications, peer messages, scheduled triggers and the SDK.

## 3. Two concepts the whole app should be built on

### 3.1 Units: stop leading with raw tokens

Problem: "737M tokens" sounds alarming and tells the user nothing, because a cache-read token costs a small fraction of an output token. Users cannot see what actually drains their limit or money.

Plan:

1. Introduce **cost weight** as the default unit everywhere a total is shown. Three switchable units, one global toggle in the toolbar, remembered:
   - **Cost ($)**: API-equivalent dollars. Label it "at API prices", never "your bill".
   - **Weighted tokens**: tokens weighted by relative price (input = 1). Useful for subscription users who do not think in dollars.
   - **Raw tokens**: what the app shows today.
2. Price table: a versioned JSON file in the repo (`crates/core/prices.json`) keyed by model family, with input, output, cache-write 5m, cache-write 1h, cache-read, web search per request, and fast-mode and long-context multipliers if they apply. **Do not write prices from memory.** Take them from Anthropic's pricing page at implementation time, record the date in the file, and let the user edit them in Settings. Unknown model: show tokens, flag "no price".
3. Calibrate and verify against `cost-state`: for sessions that have it, recompute cost from the indexed tokens and compare with `totalCostUSD`. Show nothing if the two disagree by more than 10%; log it. This is also the regression test for the price table.
4. Per-request cost is stored at index time (so breakdowns can sum it), and recomputed when the price table changes.
5. Every chart that stacks token types must be stackable by cost, so the user sees, for example, that output is 0.3% of tokens but a much larger share of cost.

Acceptance: for the 379 sessions with `cost-state`, the app's computed cost is within 10% for at least 95% of them; the Overview headline is in the selected unit; switching unit changes every number on screen consistently.

### 3.2 Attribution: the context ledger

This exists for one session (see `session.rs`). It must become the engine for the whole app.

Model: every request's context is a set of **pieces**. A piece has a category (label + group), a size, the request at which it entered, and the request at which it left (compaction, or session end). Its **carried cost** is its size multiplied by the number of requests it was in context for (weighted by cache-read or cache-write price once 3.1 exists).

Current method: context size per request is exact; growth between consecutive requests is split across what was added in between (previous response's output is exact; tool results, injected text, prompts are estimated by bytes / 3.6, images 1,600 tokens flat); after each step all pieces are rescaled so they sum to the real context.

Improvements, in order of value:

1. **Persist the ledger** (section 4.A.2) at piece granularity, not only category sums, so the app can show "the 10 most expensive things in this session" and aggregate across sessions.
2. **Use `api-request` + `api-request-blob` when present** to get the exact message list per request instead of inferring it from transcript order. This removes ordering errors, handles context edits (`context_management`), and tells exactly when a piece left the context. Fall back to the current method for older sessions. Mark each session `exact` or `estimated`.
3. **Split the fixed overhead** using `api-request-shape`: system prompt blocks, then tool definitions grouped by built-in vs each MCP server, with per-tool sizes. Carried cost of a tool definition = its size times every request. This directly answers "which MCP server costs me tokens even when I do not use it".
4. **Attribute output too.** Output tokens are the expensive ones. Split each response's output into thinking, text, and tool-call arguments (by block sizes; thinking is exact from `thinking_tokens`), and tag tool-call output with the tool. Large `Write`/`Edit` inputs are real output cost.
5. **Attribute subagents.** Run the same ledger over each subagent transcript; roll the subagent's total up to the `Agent` call that launched it (`meta.json.toolUseId`), and add the result it returned to the parent's context as a piece of group "Subagents".
6. **Background models.** Take per-model totals from `cost-state.modelUsage` and subtract what the rows explain; show the remainder as "Background (titles, web search, compaction)".
7. **Better size estimates.** Replace the flat 3.6 bytes per token with a per-content-type factor calibrated from steps where only one kind of thing was added (the exact growth is known). Calibrate image size from steps that added only images.
8. **Cache accounting per piece.** A piece costs cache-write price once (or again after each rebuild) and cache-read price on every later request. Rebuilds after idle re-charge everything in context at write price: attribute that to an explicit "Cache rebuild after idle" category so it is visible as waste.

Acceptance: ledger total equals the sum of per-request context for every session (within 0.5%); a unit test builds a synthetic transcript with known piece sizes and checks every category; exact and estimated modes agree within 15% per group on sessions that have both.

## 4. Workstreams

Each item: **What**, **Why** (what it makes clear to the user), **How**, **Done when**.

### 4.A Data layer

**A.1 Migrations instead of rebuild.** Why: history must survive upgrades, and the cleaner app deletes transcripts. How: `user_version` steps with `ALTER TABLE`/backfill; a rebuild is allowed only for derived tables. Keep raw request rows forever. Done when: upgrading from v2 keeps every row; test with a v2 fixture database.

**A.2 New tables.**

- `turns(session, turn_index, prompt_ts, origin, permission_mode, requests, output, context_added, cost, duration_ms, tool_calls, compacted)`: one row per user prompt (group requests by the prompt that started them, using `turnPosition`/`promptId` when present, else by non-meta user messages).
- `tool_calls(session, request_key, ts, tool, server, group, input_tokens_est, result_tokens_est, carried_cost, is_error, denied_kind, skill, plugin, subagent_id)`.
- `pieces(session, id, category, group, tokens, exact, entered_request, left_request, carried_tokens, carried_cost, source_ref)`: the ledger. `source_ref` is a tool call id or attachment type, never text.
- `events(session, ts, kind, tokens, detail)`: compaction, rebuild, api_error, retry, model fallback, hook failure, denial.
- `session_summary(session, …)`: everything the session list and insights need without re-reading the transcript: title, project, first/last ts, requests, prompts, peak and average context, compactions, rebuilds, output, cost, cost-state USD, group shares, flags, analyzer version, mode (`exact`/`estimated`).
- `overhead(shape_hash, component, kind, tokens)`: system blocks and per-tool definition sizes; `session_shapes(session, shape_hash, first_request, last_request)`.
- `desktop_sessions(cli_session_id, title, model, effort, archived, created_at, last_activity_at, mcp_servers)` and `mcp_names(uuid, name)` from Desktop's index.

Why: every screen below reads from these; none should parse transcripts at render time.

**A.3 Two-stage indexing.** Stage 1 (existing, fast): request rows, so totals appear in seconds. Stage 2 (background, rayon, resumable): per-session analysis filling A.2, newest sessions first, with progress in the UI ("Analyzing 120 of 600 sessions"). Re-analyze a session only when its files changed or the analyzer version changed. Done when: cold start shows the Overview in under 5 s on 3 GB and finishes stage 2 without blocking the UI.

**A.4 Caching and concurrency.** Separate read-only connection for queries; writer connection for refresh. Session detail served from `session_summary` + `pieces`; re-analysis only when stale.

**A.5 File watching.** `notify` on `~/.claude/projects` with debounce, replacing the 60 s timer; the timer stays as a fallback. Why: live numbers while a session runs (needed for 4.H).

**A.6 Desktop session index.** Read `local_*.json` for titles (better than transcript titles, and present for sessions with no `custom-title`), archived state, model, effort, and connector names. Map `prior_cli_session_ids` so a resumed session is one session.

**A.7 Retention and export of the index.** Settings: "Keep usage history after transcripts are deleted" (default on), "Delete history older than…", "Export index as JSON/CSV".

### 4.B Attribution engine

Implement section 3.2 items 1 to 8. Additional requirements:

- Analyzer is a pure function `analyze(transcript files) -> SessionAnalysis`, with no database access, so it can be unit-tested with fixture transcripts. Persistence is a separate step.
- Fixture transcripts live in `crates/core/tests/fixtures/` and are **synthetic**: generated by a small builder in the tests, never copied from real sessions.
- Keep the analyzer streaming: never load a whole transcript in memory (they reach 150 MB+). `api-request-blob` lines are large; index them by hash with offsets and read sizes only.
- Category taxonomy is a single enum in core, shared with the UI through a generated JSON, so labels, colors and help text never drift. Groups: Model output (thinking / text / tool-call arguments), Tools (per tool), MCP (per server, then per tool), Subagents, Injected (per attachment type), You (prompts / images / attached files), Compaction summaries, Overhead (system prompt / built-in tool definitions / MCP tool definitions per server), Cache rebuild, Background, Unattributed.

### 4.C Cost model

Section 3.1. Plus:

- Settings page: price table editor, unit preference, "my plan" (free text label only; plan limits are not readable locally, say so).
- Where `cost-state` exists, show "Claude Code's estimate" next to the app's own number on the session page; explain any gap (background models).

### 4.D Overview page

Why: this is the page that must answer "what is spending my tokens" without a click.

1. **Headline:** this week vs last week to the same time of day (exists), in the selected unit, with the per-request metrics moved under a "Why did it change?" line generated from the data: for example "Requests −39%, context per request +4%: you worked less, each request cost the same". This is the direct answer to "is Opus eating more tokens than last week".
2. **Change decomposition:** a small waterfall: last week's cost, then deltas attributed to number of requests, context size per request, output per request, cache hit rate, model mix, to this week's cost. All computable from request rows.
3. **Where it went** (new, the core of the page): one stacked bar for the selected range, grouped like the session page (model output, tools, MCP, subagents, injected, you, overhead, cache rebuilds), from `pieces` aggregated across sessions. Click a segment to open that breakdown.
4. **Top spenders:** three short lists side by side: top sessions, top projects, top tools/MCP servers, each with share and a sparkline. Each row links to its detail.
5. **Daily chart:** stacked by group or by token type or by model (toggle), in the selected unit. Keep the metric switch (cost, requests, output, context per request). Add a 4-week average line and mark days with compactions.
6. **Insights strip:** the top three insights from 4.G, each with the amount it cost in the range.
7. **Data coverage note:** "N sessions analyzed exactly, M estimated, K with no detail (older Claude Code)". Honest about limits; links to the explainer (4.I).

### 4.E Breakdowns

Keep the existing six. Changes to all: unit toggle, sort by any column, click-through to a filtered Overview ("everything, but only this project"), sparkline per row, CSV export, and a global search box.

New breakdowns:

1. **Tools and MCP servers** (global): calls, results added, largest result, carried cost, share, average result size, error/denial rate. Second level: per tool inside a server. Third level: the sessions where it cost the most. Includes **definition overhead** per server: "cost you X even in sessions that never called it" (from `overhead` × requests, split by whether the server was called in the session).
2. **Fixed overhead:** system prompt and tool definitions over time; number of tools loaded per session; per-server definition size; what share of each request is overhead (matters most in short sessions).
3. **Injected content:** by attachment type across sessions: instructions (CLAUDE.md), skill lists, deferred tool lists, MCP server instructions, file snapshots, hook output, reminders. Per type: size per injection, how often, carried cost. For `instructions`: flag large CLAUDE.md files by project.
4. **Subagents:** by agent type (`Explore`, `general-purpose`, custom names): runs, requests, cost, average cost per run, result size returned to the parent. Answers "are my subagents worth it".
5. **Skills and plugins:** from `attributionSkill` / `attributionPlugin`: cost of turns that ran under a skill.
6. **Who started the turn:** human vs task notification vs peer message vs scheduled vs SDK (`turnOrigin`). Why: autonomous and scheduled work spends tokens while the user is away; they should see how much.
7. **Session length:** cost by session size bucket (requests, peak context). Shows the long-session effect numerically: "sessions over 500 requests are 8% of your sessions and 61% of your cost".
8. **Cache:** hit rate over time, 5-minute vs 1-hour writes, rebuild events and what they cost, by project and by time of day (rebuilds cluster after lunch and overnight).
9. **Effort and model:** cost per request at each effort level, thinking share of output, `requestedModel` vs actual model (fallback rate), fast mode (`speed`).
10. **Time:** hour × weekday heatmap, and a calendar heatmap for the last 6 months.
11. **Branches / PRs:** by `gitBranch` within a project (cost of a feature). Worktree sessions make this meaningful.
12. **Problems:** API errors and retries, tool denials by kind, hook failures, sessions stopped by a hook: counts and the tokens spent on retried requests.

### 4.F Session detail v2

Exists: header cards, warnings, composition bar and top 14 list, context timeline with events, tools table, subagents table.

Add:

1. **Turn list** (the most important addition): one row per prompt: time, origin, requests it triggered, context added, output, cost, tools used (icons), flags (compacted, rebuild, error). Sort by cost. Why: users think in prompts, not requests: "that one question cost $9 because it triggered 40 requests and read 6 files". No prompt text by default; an opt-in "Show prompt text" setting reads it from the transcript on demand, truncated, never stored.
2. **Turn detail** (expand a row): the requests in it, each with context size, growth, what was added (pieces), output split.
3. **Most expensive pieces:** top 20 individual items by carried cost: "Read result, 19k tokens, in context for 380 requests". With `source_ref` only (tool name, time), no content.
4. **Stacked timeline:** the context chart stacked by group, so the user sees what the context is made of as it grows, not only its height. Brush to zoom; hover shows the request; click selects the turn.
5. **"If you had…" estimates** (clearly labelled as estimates): cost if the session had been split at each compaction; cost of each cache rebuild; cost of tool definitions never used in this session; cost of the largest tool result had it been capped at 5k tokens.
6. **Overhead panel:** system prompt and tool definitions for this session, per server, with "used / never used in this session".
7. **Subagent drill-down:** open a subagent as its own mini session page (its ledger), with a link back.
8. **Compare:** pick two sessions (or a session vs the user's median) side by side on the group shares and per-request metrics.
9. **Session identity:** Desktop title, model and effort settings, branch, PR link if recorded, archived state, link to open the transcript folder in Finder (`opener` plugin), and a button to copy `claude --resume <id>`.
10. **Exactness badge:** "Exact" (request snapshots) or "Estimated" with a tooltip explaining the difference.

### 4.G Insights engine (global recommendations)

Why: turns the data into "what should I change". Today only per-session warnings exist.

How: a rules module in core. Each rule: id, severity, the scope it applies to (session, project, global), the measured cost in the selected range, a one-sentence finding, a one-sentence action, and a link to the evidence view. Rules are data-driven and unit-tested. Ranked by cost. Dismissible per rule (stored in app data), with "show dismissed".

Initial rules:

| Rule | Evidence | Action text |
| --- | --- | --- |
| Long sessions dominate | share of cost from sessions past the first compaction, or above N requests | Start a new session for a new task; use `/clear` or `/compact` at a natural break. |
| Idle cache rebuilds | rebuild events and tokens rewritten | Finish or compact before long breaks; a resumed large session rewrites its whole context. |
| Unused MCP servers | definition overhead of servers never called in ≥ 90% of sessions that loaded them | Disable the server for this project. |
| Heavy tool definitions | tools loaded > N or overhead above X% of average request | Trim connected servers; prefer deferred tool loading. |
| Oversized tool results | results above 25k tokens, with carried cost | Read file ranges, filter command output, avoid dumping logs. |
| One tool dominates | a tool or server above X% of context in a project | Named per case: screenshots from browser MCP, repeated full-file reads, verbose test output. |
| Large CLAUDE.md / instructions | `instructions` attachment size × requests, by project | Shorten or split the file; move rarely needed parts into skills. |
| Injected content share high | injected group above X% | Names the largest type. |
| Thinking share high | thinking above X% of output or context at high effort | Lower effort for routine work. |
| Subagent cost vs return | subagents above X% of a session's cost | Review which agent types run; narrow their briefs. |
| Autonomous spend | cost of non-human turn origins | Review scheduled tasks and loops. |
| Model fallback | `requestedModel` ≠ model rate | Informational. |
| Errors and retries | tokens on retried requests, hook failures, denials | Fix the failing hook; adjust permissions. |
| Week-over-week change | decomposition from 4.D.2 | States the cause in one sentence. |

Thresholds live in one config struct with defaults; tune them against real data and record the chosen values and why.

A dedicated **Insights** page lists all active insights with cost, trend, and evidence link. The Overview shows the top three.

### 4.H Live view and menu bar

1. **Menu bar item** (Tauri tray): today's cost in the selected unit, change vs the same time last week, and the current session's context size as a small gauge. Click opens a popover with today, this week, the active session (context, requests, last compaction), and "Open app".
2. **Active session panel** in the app: detects sessions written to in the last 2 minutes; shows live context size against the limit, growth per turn, a forecast ("about 12 turns to compaction at this rate"), and what the last turn added.
3. **Notifications** (opt-in, off by default): context above a threshold (for example 70% of the window), a session resumed after the cache lifetime ("this turn rewrote 600k tokens"), daily cost above N× the user's median, a tool result above X tokens. Each notification states the cost and the suggested action. Use the Tauri notification plugin; rate-limit per session.
4. **Launch at login** option for the menu bar item.

Note: plan-limit percentage cannot be shown: it is not in local data. Say so in the popover rather than approximating it.

### 4.I Explanations inside the app

Why: the data is only clear if the user understands three ideas: every request re-sends the conversation; cached tokens are cheap but not free and expire; fixed overhead rides on every request.

1. **"How usage works" page:** three short illustrated sections with the user's own numbers filled in ("your average request carried 293k tokens, of which 61k was system prompt and tool definitions"). One diagram each: context growing per request; cache write then reads then expiry; overhead as a constant band.
2. **Tooltips with definitions** on every metric name (context per request, carried, cache hit rate, weighted tokens). One glossary source in core, reused by the UI and the CLI.
3. **First-run screen:** what is read, what is never stored, where the index lives, and the three limits (this Mac only, Claude Code only, no plan-limit data).
4. **Empty and partial states:** no transcripts found; indexing in progress; session predates request snapshots.
5. **Wording review:** plain language throughout. No internal category names in the UI (for example show "Tool lists added by Claude Code", not `deferred_tools_delta`).

### 4.J Sharing and team comparison

Why: the question that started this project was a colleague asking whether everyone's usage went up.

1. **Export a summary file** (JSON): per-day totals by model, per-request metrics, group shares, insight ids with costs, app and price-table version. No paths, titles, branches, prompts or tool arguments. Project names are hashed unless the user opts in. A preview shows exactly what the file contains before saving.
2. **Import and compare:** load one or more summary files; a Compare page overlays people on the weekly cost, context per request, and group shares. Stored locally only.
3. **Copy as text/Markdown/PNG:** any card, chart or table, for pasting into Slack.
4. **CSV/JSON export** of any table.
5. Out of scope here, note only: org-wide numbers exist in the claude.ai admin console for Team/Enterprise; this app does not replace them.

### 4.K CLI

Keep parity with the app for scripting and for use inside Claude Code itself:

- `usage summary --why` (the decomposition sentence), `usage top [sessions|projects|tools|mcp] --days N`, `usage insights`, `usage session <id> [--turns] [--pieces]`, `usage overhead`, `usage export --summary out.json`, `usage watch` (live line for the active session).
- `--unit cost|weighted|tokens`, `--json` everywhere.
- A statusline snippet in the README: context size and cost of the current session, from `usage watch --once --session $ID`.

### 4.L Quality, performance, release

1. **Tests:** analyzer unit tests on synthetic transcripts (each category, compaction, rebuild, subagent, missing fields, old-version transcript); price-table regression against `cost-state`; migration test; query tests for every new breakdown; a golden JSON for each Tauri command.
2. **Performance budgets:** cold index stage 1 under 5 s per 3 GB; stage 2 under 60 s per 600 sessions in the background; any page render under 150 ms from cached tables; memory under 300 MB while analyzing. Add a `usage bench` command that prints these.
3. **UI dev harness:** `ui/dev/mock.js` generated by `scripts/make-mock.sh` from the local index via the CLI, plus `ui/dev/index.html`, served with any static server. Document it in CONTRIBUTING. Mock data must not be committed (it contains session titles): add to `.gitignore` and ship a small synthetic mock instead.
4. **Charts:** the hand-written SVG is fine at this size; extract `barChart`, `stackedBar`, `stackedArea`, `heatmap`, `waterfall`, `sparkline` into `ui/charts.js` with shared axis, tooltip and theme code. Check light and dark themes, and color-blind-safe group colors (do not rely on hue alone: add patterns or direct labels).
5. **Accessibility:** keyboard navigation for tables and nav, focus rings, `aria` labels on charts with a text summary, respects reduced motion.
6. **Robustness:** unknown record types and fields are ignored, never fatal; analyzer version recorded per session; a "Diagnostics" page listing unparsed line counts by type and Claude Code versions seen, so format drift is visible.
7. **Privacy checks:** a test that greps the database after indexing fixtures and fails if any fixture message text appears; no network permission in the Tauri capabilities; CSP stays `'self'`.
8. **Release:** initial commit, GitHub repo, CI green, tag v0.1.0, DMGs for both architectures, Homebrew cask in `jravas/tap`, README with screenshots (use synthetic data), CHANGELOG. Signing is optional as in the cleaner.
9. **Relationship to Claude Storage Cleaner:** separate app (decided). Share nothing at build time; link each app from the other's README. Retaining history after the cleaner removes transcripts is a selling point: state it.

## 5. Phases

Each phase ends in a working, releasable app.

| Phase | Contents | Done when |
| --- | --- | --- |
| **0. Foundations** | Commit and repo; defects 2 to 11 from section 1; migrations (A.1); read/write connections (A.4); UI dev harness (L.3); analyzer as a pure function with synthetic-fixture tests (B) | CI green; analyzer has tests; no behavior change for the user except speed. |
| **1. Honest units** | Cost model and unit toggle (3.1, 4.C); verify against `cost-state` | Cost within 10% on ≥ 95% of sessions with `cost-state`; every screen respects the toggle. |
| **2. Ledger everywhere** | Tables A.2; two-stage indexing A.3; ledger persisted (3.2 items 1, 3, 4, 8); Desktop index A.6 | "Where it went" bar on the Overview; global Tools and MCP, Overhead, Injected breakdowns (4.E.1 to 3). |
| **3. Turns and drill-down** | Turn list and detail, top pieces, stacked timeline, overhead panel (4.F.1 to 6, 10); click-through and search (4.E) | A user can go Overview → project → session → turn → piece without a dead end. |
| **4. Insights** | Rules engine and Insights page (4.G); week-over-week decomposition (4.D.1, 4.D.2) | Each rule has a test; top three shown on the Overview with costs. |
| **5. Exactness and subagents** | `api-request` reconstruction (3.2 item 2); subagent ledger (item 5); background models (item 6); calibrated estimates (item 7) | Exact vs estimated agree within 15% per group; subagent drill-down works. |
| **6. Live** | File watcher, menu bar, active session panel, notifications (4.A.5, 4.H) | Menu bar updates within 2 s of a response; notifications off by default. |
| **7. Explain and share** | Explainer page, glossary, first run (4.I); export, import, compare (4.J); remaining breakdowns (4.E.4 to 12); CLI parity (4.K) | Two people can compare weekly summaries without exchanging any private text. |
| **8. Release** | 4.L items 4 to 9 | v0.1.0 published; cask installs. |

Phases 1 and 2 give the largest gain in clarity. If time is short, ship 0 to 4.

## 6. Open questions and risks

1. **Thinking in context.** The measured sessions send `context_management: clear_thinking … keep: all`, so earlier thinking is re-sent. If other configurations drop thinking, the ledger must follow the per-request setting. Verify with `api-request` reconstruction before stating "27% of your context is earlier thinking" as fact in the UI.
2. **Format drift.** The transcript format is not a public contract and changes between Claude Code versions. Mitigation: defensive parsing, the Diagnostics page, analyzer versioning, and a compatibility table in the README.
3. **Cost accuracy.** Prices change and plans differ. Always label as API-equivalent; never present as a bill; keep the table editable and dated.
4. **Plan limits.** Not available locally. Do not approximate them. If Anthropic exposes them later, add a provider behind the same unit toggle.
5. **Privacy of derived data.** Session titles, project paths and branch names are sensitive. They stay local; exports exclude them by default; screenshots in docs use synthetic data.
6. **Estimation honesty.** Per-piece numbers in estimated mode can be off by 10 to 20%. Show ranges or the "Estimated" badge rather than false precision; round to two significant figures.
7. **Index size.** `pieces` can be large (tens of rows per request). Store pieces above a size threshold individually and roll the rest into per-category rows per turn. Measure before optimizing: budget 500 MB for a 3 GB transcript corpus, then decide.
8. **Other entrypoints.** Only Claude Desktop sessions were examined. CLI, IDE and SDK sessions may lack some fields (`turnOrigin`, Desktop index). Test with at least one CLI session before Phase 2 is called done.
9. **Other platforms.** macOS only for now; paths are hard-coded for it. Keep them behind `paths.rs` so Windows/Linux remain possible.
