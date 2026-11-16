//! Transcript parsing.
//!
//! `~/.claude/projects/<encoded cwd>/<session>.jsonl` holds one JSON record
//! per line; subagents write their own files under
//! `<session>/subagents/agent-*.jsonl`. An assistant response is written as
//! one line per content block, and every one of those lines repeats the same
//! `usage`, so rows are keyed on `message.id` + `requestId` and counted once.

use jiff::tz::TimeZone;
use memchr::memmem;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// One API response with its token usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Row {
    pub key: String,
    pub session: String,
    pub ts: i64,
    /// Local calendar day, `YYYY-MM-DD`.
    pub day: String,
    pub hour: u8,
    /// 0 = Monday … 6 = Sunday.
    pub dow: u8,
    pub model: String,
    pub effort: Option<String>,
    pub speed: Option<String>,
    pub entrypoint: Option<String>,
    pub project: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub subagent: bool,
    pub input: u64,
    pub output: u64,
    pub thinking: u64,
    pub cw5m: u64,
    pub cw1h: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub web_search: u64,
    pub web_fetch: u64,
    /// Model Claude Code asked for; differs from `model` on fallback or
    /// routing.
    pub requested_model: Option<String>,
    pub stop_reason: Option<String>,
    /// Skill the response ran under, such as `anthropic-skills:pdf`.
    pub attribution_skill: Option<String>,
    pub attribution_plugin: Option<String>,
    /// MCP server whose tool the response called.
    pub attribution_mcp_server: Option<String>,
    pub thinking_ms: Option<u64>,
    pub inference_geo: Option<String>,
}

impl Row {
    /// Fill fields this row lacks from another line of the same response.
    /// A response is logged once per content block and some fields only
    /// appear on one of them (thinking time on the thinking block, MCP
    /// attribution on the tool call).
    pub fn fill_from(&mut self, o: &Row) {
        fn f<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if a.is_none() {
                a.clone_from(b);
            }
        }
        f(&mut self.requested_model, &o.requested_model);
        f(&mut self.stop_reason, &o.stop_reason);
        f(&mut self.attribution_skill, &o.attribution_skill);
        f(&mut self.attribution_plugin, &o.attribution_plugin);
        f(&mut self.attribution_mcp_server, &o.attribution_mcp_server);
        f(&mut self.thinking_ms, &o.thinking_ms);
        f(&mut self.inference_geo, &o.inference_geo);
    }
}

/// Local calendar day (`YYYY-MM-DD`), hour, and weekday (0 = Monday) of an
/// instant in `tz`.
pub fn local_parts(ts: jiff::Timestamp, tz: &TimeZone) -> (String, u8, u8) {
    let z = ts.to_zoned(tz.clone());
    (
        z.date().to_string(),
        z.hour() as u8,
        z.weekday().to_monday_zero_offset() as u8,
    )
}

#[derive(Debug, Default)]
pub struct FileParse {
    pub rows: Vec<Row>,
    /// (session id, title), last one wins.
    pub titles: Vec<(String, String)>,
    /// Byte offset just past the last complete line read.
    pub offset: u64,
    /// The file's project: where its session started, or where it was
    /// relocated to. Every row of a file carries it.
    pub project: Option<String>,
    /// The session was moved to another folder in this read; rows already
    /// in the index for this file need the new project.
    pub relocated: bool,
}

/// The string value of `"key":"…"` in a raw JSON line, without parsing it.
fn raw_str<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a str> {
    let i = memmem::find(line, key)? + key.len();
    let end = memchr::memchr(b'"', &line[i..])?;
    std::str::from_utf8(&line[i..i + end]).ok()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    session_id: Option<String>,
    entrypoint: Option<String>,
    effort: Option<String>,
    request_id: Option<String>,
    message: Option<Msg>,
    custom_title: Option<String>,
    requested_model: Option<String>,
    thinking_duration_ms: Option<u64>,
    attribution_skill: Option<String>,
    attribution_plugin: Option<String>,
    attribution_mcp_server: Option<String>,
}

#[derive(Deserialize)]
struct Msg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens_details: Option<OutDetails>,
    cache_creation: Option<CacheCreation>,
    server_tool_use: Option<ServerTools>,
    speed: Option<String>,
    inference_geo: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServerTools {
    web_search_requests: Option<u64>,
    web_fetch_requests: Option<u64>,
}

/// The repository a working directory belongs to: Claude Desktop worktrees
/// (`<repo>/.claude/worktrees/<name>`) fold into their repo.
pub fn project_of(cwd: &str) -> String {
    if let Some(i) = cwd.find("/.claude/worktrees/") {
        return cwd[..i].to_string();
    }
    if cwd.contains("/Library/Application Support/Claude/") {
        return "Claude scratch".to_string();
    }
    cwd.to_string()
}

pub enum Parsed {
    Row(Box<Row>),
    Title(String, String),
}

/// Parse one transcript line. Lines without usage or a title return None.
pub fn parse_line(line: &[u8], subagent: bool, tz: &TimeZone) -> Option<Parsed> {
    let has_usage = memmem::find(line, b"\"usage\"").is_some();
    let has_title = !has_usage && memmem::find(line, b"\"customTitle\"").is_some();
    if !has_usage && !has_title {
        return None;
    }
    let l: Line = serde_json::from_slice(line).ok()?;
    if has_title {
        return match (l.kind.as_deref(), l.session_id, l.custom_title) {
            (Some("custom-title"), Some(s), Some(t)) if !t.is_empty() => Some(Parsed::Title(s, t)),
            _ => None,
        };
    }
    if l.kind.as_deref() != Some("assistant") {
        return None;
    }
    let msg = l.message?;
    let model = msg.model?;
    // "<synthetic>" marks locally generated error turns that never hit the API.
    if model.starts_with('<') {
        return None;
    }
    let u = msg.usage?;
    let ts: jiff::Timestamp = l.timestamp?.parse().ok()?;
    let (day, hour, dow) = local_parts(ts, tz);
    let cwd = l.cwd.unwrap_or_default();
    let cc = u.cache_creation.unwrap_or_default();
    let st = u.server_tool_use.unwrap_or_default();
    let key = format!(
        "{}:{}",
        msg.id.unwrap_or_default(),
        l.request_id.unwrap_or_default()
    );
    Some(Parsed::Row(Box::new(Row {
        key,
        session: l.session_id.unwrap_or_default(),
        ts: ts.as_millisecond(),
        day,
        hour,
        dow,
        model,
        effort: l.effort,
        speed: u.speed,
        entrypoint: l.entrypoint,
        project: project_of(&cwd),
        cwd,
        branch: l.git_branch,
        subagent,
        input: u.input_tokens.unwrap_or(0),
        output: u.output_tokens.unwrap_or(0),
        thinking: u
            .output_tokens_details
            .and_then(|d| d.thinking_tokens)
            .unwrap_or(0),
        cw5m: cc.ephemeral_5m_input_tokens.unwrap_or(0),
        cw1h: cc.ephemeral_1h_input_tokens.unwrap_or(0),
        cache_write: u.cache_creation_input_tokens.unwrap_or(0),
        cache_read: u.cache_read_input_tokens.unwrap_or(0),
        web_search: st.web_search_requests.unwrap_or(0),
        web_fetch: st.web_fetch_requests.unwrap_or(0),
        requested_model: l.requested_model.filter(|s| !s.is_empty()),
        stop_reason: msg.stop_reason,
        attribution_skill: l.attribution_skill.filter(|s| !s.is_empty()),
        attribution_plugin: l.attribution_plugin.filter(|s| !s.is_empty()),
        attribution_mcp_server: l.attribution_mcp_server.filter(|s| !s.is_empty()),
        thinking_ms: l.thinking_duration_ms,
        inference_geo: u.inference_geo.filter(|s| !s.is_empty()),
    })))
}

pub fn is_subagent_file(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "subagents")
}

/// Read complete lines from `offset` to the end of the file. A trailing line
/// without a newline is still being written; it is left for the next read.
/// `project` is the file's project from an earlier read, if any.
pub fn parse_file(
    path: &Path,
    offset: u64,
    project: Option<String>,
    tz: &TimeZone,
) -> std::io::Result<FileParse> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let subagent = is_subagent_file(path);
    let mut out = FileParse {
        offset,
        project,
        ..Default::default()
    };
    let mut buf = Vec::with_capacity(64 * 1024);
    // Response key -> position in `out.rows`.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        out.offset += n as u64;
        if let Some(c) = raw_str(&buf, b"\"relocatedCwd\":\"") {
            out.project = Some(project_of(c));
            out.relocated = true;
        } else if out.project.is_none() {
            if let Some(c) = raw_str(&buf, b"\"cwd\":\"") {
                out.project = Some(project_of(c));
            }
        }
        match parse_line(&buf, subagent, tz) {
            Some(Parsed::Row(mut row)) => match seen.get(&row.key) {
                Some(&i) => out.rows[i].fill_from(&row),
                None => {
                    if let Some(p) = &out.project {
                        row.project.clone_from(p);
                    }
                    seen.insert(row.key.clone(), out.rows.len());
                    out.rows.push(*row);
                }
            },
            Some(Parsed::Title(s, t)) => out.titles.push((s, t)),
            None => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Zagreb").unwrap()
    }

    const A: &str = r#"{"type":"assistant","timestamp":"2026-11-16T09:57:34.100Z","cwd":"/r/app/.claude/worktrees/x-1","gitBranch":"b","sessionId":"s1","entrypoint":"claude-desktop","effort":"medium","requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":2,"output_tokens":828,"cache_creation_input_tokens":5850,"cache_read_input_tokens":97962,"output_tokens_details":{"thinking_tokens":129},"cache_creation":{"ephemeral_1h_input_tokens":5850,"ephemeral_5m_input_tokens":0},"server_tool_use":{"web_search_requests":1,"web_fetch_requests":null},"speed":"standard","fallback_credit":null}}}"#;

    #[test]
    fn parses_assistant_usage() {
        let Some(Parsed::Row(r)) = parse_line(A.as_bytes(), false, &tz()) else {
            panic!("no row")
        };
        assert_eq!(r.key, "msg_1:req_1");
        assert_eq!(r.day, "2026-11-16");
        assert_eq!(r.hour, 10); // 09:57Z is 10:57 in Zagreb
        assert_eq!(r.dow, 0);
        assert_eq!(r.project, "/r/app");
        assert_eq!(r.output, 828);
        assert_eq!(r.thinking, 129);
        assert_eq!(r.cw1h, 5850);
        assert_eq!(r.cache_read, 97962);
        assert_eq!(r.web_search, 1);
        assert_eq!(r.web_fetch, 0);
        assert_eq!(r.effort.as_deref(), Some("medium"));
        assert_eq!(r.requested_model, None);
        assert_eq!(r.stop_reason, None);
    }

    #[test]
    fn parses_new_fields_and_merges_block_lines() {
        let a = A
            .replace(
                r#""requestId""#,
                r#""requestedModel":"claude-opus-5-5[1m]","thinkingDurationMs":2018,"attributionSkill":"anthropic-skills:pdf","attributionPlugin":"anthropic-skills","requestId""#,
            )
            .replace(r#""usage":{"#, r#""stop_reason":"tool_use","usage":{"inference_geo":"global","#);
        let Some(Parsed::Row(r)) = parse_line(a.as_bytes(), false, &tz()) else {
            panic!("no row")
        };
        assert_eq!(r.requested_model.as_deref(), Some("claude-opus-5-5[1m]"));
        assert_eq!(r.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(r.thinking_ms, Some(2018));
        assert_eq!(r.attribution_skill.as_deref(), Some("anthropic-skills:pdf"));
        assert_eq!(r.attribution_plugin.as_deref(), Some("anthropic-skills"));
        assert_eq!(r.inference_geo.as_deref(), Some("global"));

        // Thinking time on the first block, MCP attribution on the second.
        let first = A.replace(r#""requestId""#, r#""thinkingDurationMs":5,"requestId""#);
        let second = A.replace(
            r#""requestId""#,
            r#""attributionMcpServer":"notion","requestId""#,
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        std::fs::write(&p, format!("{first}\n{second}\n")).unwrap();
        let fp = parse_file(&p, 0, None, &tz()).unwrap();
        assert_eq!(fp.rows.len(), 1);
        assert_eq!(fp.rows[0].thinking_ms, Some(5));
        assert_eq!(fp.rows[0].attribution_mcp_server.as_deref(), Some("notion"));
    }

    #[test]
    fn skips_synthetic_and_other_lines() {
        let syn = A.replace("claude-opus-5-5", "<synthetic>");
        assert!(parse_line(syn.as_bytes(), false, &tz()).is_none());
        assert!(parse_line(
            br#"{"type":"user","message":{"content":"x"}}"#,
            false,
            &tz()
        )
        .is_none());
    }

    #[test]
    fn parses_titles() {
        let t = br#"{"type":"custom-title","customTitle":"Opus usage","sessionId":"s1"}"#;
        assert!(
            matches!(parse_line(t, false, &tz()), Some(Parsed::Title(s, t)) if s == "s1" && t == "Opus usage")
        );
    }

    #[test]
    fn project_folding() {
        assert_eq!(project_of("/a/b"), "/a/b");
        assert_eq!(project_of("/a/b/.claude/worktrees/c/sub"), "/a/b");
        assert_eq!(
            project_of("/Users/x/Library/Application Support/Claude/scratch-workspaces/1"),
            "Claude scratch"
        );
    }
}
