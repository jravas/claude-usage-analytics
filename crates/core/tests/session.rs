//! Session analyzer on synthetic transcripts with known piece sizes.
//!
//! Sizes are multiples of 36 bytes so that at the default 3.6 bytes per token
//! every estimate is a whole number: 360 bytes = 100 tokens.

mod common;

use common::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use usage_core::session::{analyze_files, SessionAnalysis};

fn run(dir: &Path, t: &Transcript) -> SessionAnalysis {
    let p = dir.join("proj/s1.jsonl");
    t.write(&p);
    analyze_files(&[p])
}

fn cat(d: &SessionAnalysis, label: &str) -> f64 {
    d.composition
        .iter()
        .filter(|s| s.label == label)
        .map(|s| s.tokens)
        .sum()
}

fn group(d: &SessionAnalysis, g: &str) -> f64 {
    d.groups
        .iter()
        .filter(|s| s.group == g)
        .map(|s| s.tokens)
        .sum()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

/// The ledger adds up to the sum of every request's context (within 0.5%),
/// and no message text leaks into the output.
fn check(d: &SessionAnalysis) {
    let total: f64 = d.composition.iter().map(|s| s.tokens).sum();
    let ctx = d.context_tokens as f64;
    assert!(
        (total - ctx).abs() <= 0.005 * ctx.max(1.0),
        "ledger {total} vs context {ctx}"
    );
    let json = serde_json::to_string(d).unwrap();
    assert!(
        !json.contains(MARKER),
        "message text leaked into the output"
    );
}

/// Shape with 1000-token system prompt, built-in tool, and one MCP tool.
fn base(t: &mut Transcript) {
    t.shape("sh1", 3600, &[("Bash", 3600), ("mcp__srv__do", 3600)]);
}

#[test]
fn every_category_is_attributed() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    base(&mut t);
    t.attachment("instructions", 720)
        .prompt(360)
        // 3000 overhead + 200 instructions + 100 prompt.
        .assistant(
            usage(3300, 3300, 100, 40),
            &[
                Block::Thinking,
                Block::Text(36),
                Block::Tool("t1", "Bash", 4),
            ],
        )
        .tool_result("t1", 3600)
        // + 100 output + 1000 result.
        .assistant(usage(4400, 1100, 50, 0), &[Block::Text(36)])
        .prompt(720)
        // + 50 output + 200 prompt.
        .assistant(usage(4650, 250, 30, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.requests, 3);
    assert_eq!(d.context_tokens, 3300 + 4400 + 4650);
    assert_eq!(d.mode, "estimated");
    assert!(d.has_snapshots);
    assert!(close(cat(&d, "System prompt"), 3000.0));
    assert!(close(cat(&d, "Built-in tool definitions"), 3000.0));
    assert!(close(cat(&d, "MCP tool definitions: srv"), 3000.0));
    assert!(close(cat(&d, "CLAUDE.md and instructions"), 600.0));
    assert!(close(cat(&d, "Your prompts"), 500.0));
    assert!(close(cat(&d, "Thinking"), 80.0));
    assert!(close(cat(&d, "Bash"), 2000.0));
    // r1's 60 non-thinking tokens split by bytes: text block 36 bytes, tool
    // input {"command":"SECR"} 18 bytes.
    let text1 = 60.0 * 36.0 / 54.0;
    let call1 = 60.0 * 18.0 / 54.0;
    assert!(close(cat(&d, "Text"), 2.0 * text1 + 50.0));
    assert!(close(cat(&d, "Tool calls"), 2.0 * call1));
    assert!(close(group(&d, "Overhead"), 9000.0));

    // Output split: every response, by kind and tool.
    let out = |l: &str| {
        d.output_split
            .iter()
            .find(|s| s.label == l)
            .map(|s| s.tokens)
            .unwrap_or(0.0)
    };
    assert!(close(out("Thinking"), 40.0));
    assert!(close(out("Text"), text1 + 50.0 + 30.0));
    assert!(close(out("Tool calls: Bash"), call1));
    assert_eq!(d.output, 180);

    // Overhead panel.
    let o = d.overhead.as_ref().unwrap();
    assert!(close(o.total, 3000.0));
    assert_eq!(o.tools_loaded, 2);
    assert_eq!(o.servers.len(), 1);
    assert_eq!(o.servers[0].name, "srv");
    assert_eq!(o.servers[0].calls, 0);
    assert!(close(o.servers[0].tokens, 1000.0));
    assert_eq!(o.builtin[0].name, "Bash");

    // Turns and pieces.
    assert_eq!(d.turns.len(), 2);
    assert_eq!(d.turns[0].origin, "human");
    assert_eq!(d.turns[0].requests, 2);
    assert_eq!(d.turns[0].tools[0].name, "Bash");
    assert_eq!(d.turns[1].requests, 1);
    assert_eq!(d.turns[0].context_tokens, 3300 + 4400);
    assert!(close(d.turns[1].context_added, 250.0));
    let top = &d.top_pieces[0];
    assert_eq!(top.label, "Bash");
    assert_eq!(top.tool.as_deref(), Some("Bash"));
    assert_eq!(top.source.as_deref(), Some("t1"));
    assert_eq!(top.requests, 2);
    assert!(close(top.tokens, 1000.0));
    assert!(d.top_pieces.iter().all(|p| p.group != "Overhead"));
    let bash = d.tools.iter().find(|x| x.name == "Bash").unwrap();
    assert_eq!(bash.calls, 1);
    assert!(close(bash.result_tokens, 1000.0));
    assert!(close(bash.carried_tokens, 2000.0));
}

#[test]
fn compaction_resets_the_context() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    base(&mut t);
    t.prompt(360)
        .assistant(usage(3100, 3100, 10, 0), &[Block::Tool("t1", "Read", 4)])
        .tool_result("t1", 3600)
        .assistant(usage(4110, 1010, 10, 0), &[Block::Text(36)])
        .compact(4110, 720)
        .prompt(360)
        // 3000 overhead + 200 summary + 100 prompt.
        .assistant(usage(3300, 3300, 10, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.compactions, 1);
    assert!(close(cat(&d, "Read"), 1000.0), "result left at compaction");
    assert!(close(cat(&d, "Compaction summary"), 200.0));
    assert!(close(cat(&d, "Your prompts"), 300.0));
    assert!(d
        .turns
        .iter()
        .any(|t| t.flags.iter().any(|f| f == "compacted")));
    assert!(d.events.iter().any(|e| e.kind == "compaction"));
}

#[test]
fn idle_cache_rebuild_is_its_own_category() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    // 20000-token system prompt.
    t.shape("big", 72_000, &[("Bash", 360)]);
    t.prompt(360)
        .assistant(usage(20_200, 20_200, 20, 0), &[Block::Text(36)])
        .wait(15 * 60)
        .prompt(360)
        .assistant(usage(20_320, 20_320, 20, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.rebuilds, 1);
    assert_eq!(d.rebuild_tokens, 20_320);
    let rb = d
        .cache_writes
        .iter()
        .find(|s| s.label == "Cache rebuild after idle")
        .unwrap();
    assert_eq!(rb.group, "Cache rebuild");
    assert!(close(rb.tokens, 20_320.0));
    let written: f64 = d.cache_writes.iter().map(|s| s.tokens).sum();
    assert!(close(written, 20_200.0 + 20_320.0));
    assert!(d.turns[1].flags.iter().any(|f| f == "rebuild"));
    assert!(d.events.iter().any(|e| e.kind == "rebuild"));
}

#[test]
fn subagents_roll_up_to_their_agent_call() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("proj/s1.jsonl");
    let mut t = Transcript::new();
    base(&mut t);
    t.prompt(360)
        .assistant(
            usage(3100, 3100, 20, 0),
            &[Block::Tool("toolu_A", "Agent", 8)],
        )
        .tool_result("toolu_A", 720)
        // + 20 output + 200 result.
        .assistant(usage(3320, 220, 20, 0), &[Block::Text(36)])
        .cost_state(json!({
            "claude-opus-5-5": {"inputTokens": 0, "outputTokens": 40 + 70,
                // Cache reads: 0 + 3100 in the parent, 0 + 1000 in the subagent.
                "cacheReadInputTokens": 3100 + 1000,
                "cacheCreationInputTokens": 3100 + 220 + 1000 + 500, "costUSD": 1.0},
            "claude-haiku-5-5": {"inputTokens": 5000, "outputTokens": 300,
                "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0,
                "webSearchRequests": 2, "costUSD": 0.01}
        }));
    t.write(&main);
    let mut s = Transcript::sidechain();
    s.prompt(360)
        .assistant(usage(1000, 1000, 30, 0), &[Block::Tool("sub1", "Grep", 4)])
        .tool_result("sub1", 1440)
        .assistant(usage(1500, 500, 40, 0), &[Block::Text(36)]);
    let sub = dir.path().join("proj/s1/subagents/agent-x1.jsonl");
    s.write(&sub);
    std::fs::write(
        dir.path().join("proj/s1/subagents/agent-x1.meta.json"),
        r#"{"agentType":"Explore","description":"Look around","toolUseId":"toolu_A"}"#,
    )
    .unwrap();

    let d = analyze_files(&[main]);
    check(&d);
    assert_eq!(d.subagents.len(), 1);
    let a = &d.subagents[0];
    assert_eq!(a.id, "x1");
    assert_eq!(a.agent_type.as_deref(), Some("Explore"));
    assert_eq!(a.tool_use_id.as_deref(), Some("toolu_A"));
    assert_eq!(a.turn, Some(0));
    assert_eq!(a.requests, 2);
    assert_eq!(a.output, 70);
    assert!(close(a.context_tokens, 2500.0));
    assert_eq!(a.tokens, 2500 + 70);
    assert!(close(a.result_tokens, 200.0));
    assert!(close(a.result_carried, 200.0));
    assert!(!a.groups.is_empty());
    assert!(close(cat(&d, "Subagent results"), 200.0));
    assert!(close(group(&d, "Subagents"), 200.0));
    assert_eq!(d.turns[0].subagents, 1);
    // Background: only what no row explains.
    assert_eq!(d.background.len(), 1);
    assert_eq!(d.background[0].model, "claude-haiku-5-5");
    assert_eq!(d.background[0].tokens, 5300);
    assert_eq!(d.background[0].web_search, 2);
}

#[test]
fn missing_fields_and_broken_lines_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    t.raw("not json at all")
        .raw(r#"{"type":"assistant"}"#)
        .raw(r#"{"type":"user"}"#)
        .raw(r#"{"type":"attachment","attachment":{"type":"date"}}"#)
        .raw(r#"{"type":"system"}"#)
        .raw(r#"{"type":"brand-new-record","x":1}"#)
        .raw(r#"{"type":"api-request","params":{}}"#)
        .raw(r#"{"type":"api-request-shape"}"#)
        .raw(r#"{"type":"cost-state"}"#)
        .raw(r#"{"type":"assistant","message":{"id":"m0","content":"odd"}}"#)
        .raw(r#"{"type":"assistant","message":{"id":"m9","usage":{"cache_read_input_tokens":500}}}"#)
        .raw(r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#)
        .raw(r#"{"type":"assistant","requestId":"r2","message":{"id":"m2","model":"<synthetic>","usage":{"output_tokens":0},"content":[{"type":"text","text":"x"}]}}"#)
        .prompt(360)
        .assistant(usage(800, 800, 10, 0), &[Block::Text(36)])
        .raw(r#"{"type":"assistant","message":{"id":"half"#);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.requests, 2);
    assert!(d.unreadable_lines >= 2);
    assert!(d.cost.is_some());
}

#[test]
fn old_transcripts_without_snapshots_are_estimated() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    t.prompt(360)
        .assistant(usage(5000, 5000, 10, 0), &[Block::Tool("t1", "Read", 4)])
        .tool_result("t1", 3600)
        .assistant(usage(6010, 1010, 10, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.mode, "estimated");
    assert_eq!(d.exact_requests, 0);
    assert!(!d.has_snapshots);
    assert!(d.overhead.is_none());
    // The first prompt is split out; the rest of the first request is the
    // unknown start of context.
    assert!(close(
        cat(&d, "Start of context (system prompt, tools, history)"),
        2.0 * 4900.0
    ));
    assert!(close(cat(&d, "Your prompts"), 200.0));
    assert!(close(cat(&d, "Read"), 1000.0));
}

#[test]
fn shape_logged_late_is_backfilled() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    t.prompt(360)
        .assistant(usage(3100, 3100, 10, 0), &[Block::Text(36)]);
    base(&mut t);
    t.prompt(360)
        .assistant(usage(3210, 110, 10, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert!(close(cat(&d, "System prompt"), 2000.0));
    assert_eq!(
        cat(&d, "Start of context (system prompt, tools, history)"),
        0.0
    );
    assert_eq!(d.overhead.as_ref().unwrap().backfilled_requests, 1);
}

/// Request snapshots give the exact message list; `context_management`
/// decides whether earlier thinking is in it.
fn exact_session(cm: serde_json::Value, ctx2: u64) -> SessionAnalysis {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    base(&mut t);
    t.prompt(360)
        .snapshot(cm.clone())
        // 3000 overhead + 100 prompt.
        .assistant(
            usage(3100, 3100, 100, 60),
            &[Block::Thinking, Block::Text(36)],
        )
        .prompt(360)
        .attachment("skill_listing", 720)
        .unlogged(700)
        .blob("system", 360)
        .snapshot(cm)
        .assistant(usage(ctx2, 300, 10, 0), &[Block::Text(36)]);
    let p = dir.path().join("proj/s1.jsonl");
    t.write(&p);
    analyze_files(&[p])
}

#[test]
fn snapshots_keep_earlier_thinking_when_told_to() {
    // + 100 output (60 thinking) + 100 prompt + 200 skills + 100 system.
    let d = exact_session(keep_all(), 3100 + 100 + 100 + 200 + 100);
    check(&d);
    assert_eq!(d.mode, "exact");
    assert_eq!(d.exact_requests, 2);
    assert_eq!(d.thinking_policy.get("all"), Some(&2));
    assert!(close(cat(&d, "Thinking"), 60.0));
    assert!(close(cat(&d, "Text"), 40.0));
    assert!(close(cat(&d, "Your prompts"), 300.0));
    // The unlogged message was the skill list attachment.
    assert!(close(cat(&d, "Skill list"), 200.0));
    assert!(close(cat(&d, "System messages"), 100.0));
    assert!(close(cat(&d, "System prompt"), 2000.0));
    assert!(d.timeline.iter().all(|p| p.exact));
}

#[test]
fn snapshots_drop_earlier_thinking_when_cleared() {
    // Thinking of the previous response is not re-sent: 60 tokens fewer.
    let d = exact_session(keep_turns(0), 3100 + 40 + 100 + 200 + 100);
    check(&d);
    assert_eq!(d.mode, "exact");
    assert_eq!(d.thinking_policy.get("last 0"), Some(&2));
    assert_eq!(cat(&d, "Thinking"), 0.0);
    assert!(close(cat(&d, "Text"), 40.0));
    assert!(close(cat(&d, "Skill list"), 200.0));
    // Generated thinking still counts as output.
    assert_eq!(d.thinking_output, 60);
}

#[test]
fn bytes_per_token_is_calibrated_from_clean_steps() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    t.prompt(360)
        .assistant(usage(2000, 2000, 0, 0), &[Block::Tool("t0", "Read", 4)]);
    let mut ctx = 2000u64;
    // Each step adds only a tool result; real growth is bytes / 2.0.
    for i in 0..12usize {
        let bytes = 2_000 + i * 1_000;
        let id: &'static str = Box::leak(format!("t{}", i + 1).into_boxed_str());
        let prev: &'static str = Box::leak(format!("t{i}").into_boxed_str());
        t.tool_result(prev, bytes);
        ctx += (bytes / 2) as u64;
        t.assistant(
            usage(ctx, (bytes / 2) as u64, 0, 0),
            &[Block::Tool(id, "Read", 4)],
        );
    }
    let d = run(dir.path(), &t);
    check(&d);
    assert!(d.calibration.data_samples >= 8);
    assert!(
        (d.calibration.data_bytes_per_token - 2.0).abs() < 0.01,
        "{:?}",
        d.calibration
    );
    // With the calibrated factor every result is sized exactly. Result i is
    // 1000 + 500 i tokens for 12 - i requests: the sixth carries the most.
    let top = &d.top_pieces[0];
    assert_eq!(top.source.as_deref(), Some("t5"));
    assert!(close(top.tokens, 3_500.0));
    assert_eq!(top.requests, 7);
}

#[test]
fn turns_record_origin_flags_and_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Transcript::new();
    base(&mut t);
    t.prompt(360)
        .assistant(usage(3100, 3100, 10, 0), &[Block::Tool("t1", "Edit", 4)])
        .denied("t1")
        .assistant(usage(3120, 20, 10, 0), &[Block::Text(36)])
        .wait(60)
        .prompt_from(360, "task_notification")
        .api_error()
        .assistant(usage(3240, 120, 10, 0), &[Block::Text(36)])
        .image_prompt(36);
    t.assistant(usage(5000, 1760, 10, 0), &[Block::Text(36)]);
    let d = run(dir.path(), &t);
    check(&d);
    assert_eq!(d.prompts, 3);
    assert_eq!(d.turns.len(), 3);
    assert_eq!(d.turns[0].origin, "human");
    assert!(d.turns[0].flags.iter().any(|f| f == "denied"));
    assert_eq!(d.turns[0].tools[0].name, "Edit");
    assert_eq!(d.turns[0].permission_mode.as_deref(), Some("default"));
    assert_eq!(d.turns[1].origin, "task_notification");
    assert!(d.turns[1].flags.iter().any(|f| f == "error"));
    assert_eq!(d.turns[1].requests, 1);
    assert!(cat(&d, "Images you pasted") > 0.0);
    let ctx: u64 = d.turns.iter().map(|t| t.context_tokens).sum();
    assert_eq!(ctx, d.context_tokens);
}

#[test]
fn missing_files_give_an_empty_analysis() {
    let d = analyze_files(&[PathBuf::from("/nonexistent/s.jsonl")]);
    assert_eq!(d.requests, 0);
    assert!(d.composition.is_empty());
    check(&d);
}
