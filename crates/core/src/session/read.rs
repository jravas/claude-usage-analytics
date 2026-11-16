//! One streaming pass over a transcript. Produces a [`Trace`]: the pieces
//! that entered the context (sizes only, never text), the requests with their
//! exact context sizes, request snapshots (`api-request`), request shapes,
//! turns, events and counters.
//!
//! Lines are read one at a time; nothing keeps message text. Assistant
//! content blocks of the response being streamed are held until the response
//! is complete, to hash the merged message, then dropped.

use super::canon::{self, Hash};
use super::labels::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Prompts, injected text, compaction summaries, system prompt.
    Prose,
    /// Tool results: code, JSON, command output.
    Data,
    /// Only images (counted, not sized).
    Image,
    /// Tool definitions (JSON schema).
    Schema,
    /// The model's own output, re-sent as input. Exact when `exact` is set.
    Output,
}

#[derive(Debug, Clone)]
pub struct Piece {
    pub label: String,
    pub group: &'static str,
    pub kind: Kind,
    /// Bytes of text or JSON, for the estimate.
    pub bytes: u64,
    pub images: u32,
    /// Exact token count (the model's own output).
    pub exact: Option<f64>,
    pub thinking: bool,
    /// Tool name, for tool results and tool-call arguments.
    pub tool: Option<String>,
    /// Tool use id or attachment type. Never text.
    pub source: Option<String>,
    pub ts: i64,
    pub turn: Option<u32>,
    pub from_attachment: bool,
}

impl Piece {
    fn new(label: impl Into<String>, group: &'static str, kind: Kind, ts: i64) -> Self {
        Piece {
            label: label.into(),
            group,
            kind,
            bytes: 0,
            images: 0,
            exact: None,
            thinking: false,
            tool: None,
            source: None,
            ts,
            turn: None,
            from_attachment: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Req {
    pub ts: i64,
    pub ctx: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cw: u64,
    pub out: u64,
    pub think: u64,
    pub model: String,
    /// Pieces that entered the transcript since the previous request.
    pub added: Vec<u32>,
    pub compact_before: bool,
    /// Latest shape seen before this request (`None` before the first).
    pub shape: Option<u32>,
    /// The request snapshot, when Claude Code recorded one.
    pub api: Option<u32>,
    pub turn: u32,
}

#[derive(Debug, Clone)]
pub struct Shape {
    pub hash: String,
    /// Bytes per system prompt block.
    pub system: Vec<u64>,
    /// (tool name, bytes of its definition).
    pub tools: Vec<(String, u64)>,
    /// System prompt, built-in tools, then one piece per MCP server.
    pub pieces: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingPolicy {
    /// Every earlier thinking block is re-sent.
    All,
    /// Only the thinking of the last N assistant messages is re-sent.
    Last(usize),
}

#[derive(Debug, Clone)]
pub struct Api {
    pub base: Option<String>,
    pub keep: usize,
    pub tail: Vec<u32>,
    pub shape: Option<String>,
    pub thinking: ThinkingPolicy,
}

#[derive(Debug, Clone, Default)]
pub struct TurnAcc {
    pub ts: i64,
    pub origin: String,
    pub permission_mode: Option<String>,
    pub tools: BTreeMap<String, u64>,
    pub compacted: bool,
    pub errors: u64,
    pub denials: u64,
    pub subagents: u64,
    pub skill: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolAcc {
    pub group: &'static str,
    pub calls: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ModelUse {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub web_search: u64,
    pub usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct CostState {
    pub usd: f64,
    pub models: Vec<(String, ModelUse)>,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub api_ms: u64,
    pub tool_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RawEvent {
    pub ts: i64,
    pub kind: &'static str,
    pub label: String,
    pub tokens: u64,
}

struct OpenMsg {
    id: String,
    blocks: Vec<Value>,
    out: u64,
    think: u64,
    ts: i64,
    turn: Option<u32>,
    real: bool,
}

#[derive(Default)]
pub struct Trace {
    pub pieces: Vec<Piece>,
    pub reqs: Vec<Req>,
    pub shapes: Vec<Shape>,
    shape_ids: HashMap<String, u32>,
    pub apis: Vec<Api>,
    pub api_ids: HashMap<String, u32>,
    hash_ids: HashMap<Hash, u32>,
    /// Pieces of each message hash; empty when not resolved.
    pub hash_pieces: Vec<Vec<u32>>,
    pub turns: Vec<TurnAcc>,
    pub events: Vec<RawEvent>,
    pub tools: HashMap<String, ToolAcc>,
    /// tool_use id → (tool name, launch ts, turn).
    pub tool_uses: HashMap<String, (String, i64, Option<u32>)>,
    /// tool_use id → result piece.
    pub tool_results: HashMap<String, u32>,
    /// tool_use id → pieces of the assistant message that made the call.
    tool_use_msg: HashMap<String, Vec<u32>>,
    /// Byte length → transcript text pieces of that length (prompts,
    /// reminders, summaries, attachments), to recognize them inside merged
    /// messages that were stored as blobs.
    text_by_len: HashMap<u64, u32>,
    pub prompts: u64,
    pub hook_errors: u64,
    pub api_errors: u64,
    pub cost: Option<CostState>,
    pub unreadable: u64,
    pending: Vec<u32>,
    compact_pending: bool,
    cur_shape: Option<u32>,
    cur_turn: Option<u32>,
    open: Option<OpenMsg>,
    seen: HashSet<String>,
}

fn ts_ms(v: &Value) -> Option<i64> {
    v.get("timestamp")?
        .as_str()?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Bytes and image count of a content value: text by length, images counted,
/// anything else by its JSON length.
pub fn measure(v: &Value) -> (u64, u32) {
    match v {
        Value::String(s) => (s.len() as u64, 0),
        Value::Array(a) => a
            .iter()
            .map(measure)
            .fold((0, 0), |x, y| (x.0 + y.0, x.1 + y.1)),
        Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("image") => (0, 1),
        Value::Object(o) => {
            if let Some(t) = o.get("text").and_then(Value::as_str) {
                (t.len() as u64, 0)
            } else if let Some(c) = o.get("content") {
                measure(c)
            } else {
                (v.to_string().len() as u64, 0)
            }
        }
        Value::Null => (0, 0),
        other => (other.to_string().len() as u64, 0),
    }
}

fn lines(path: &Path, mut f: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut buf = Vec::with_capacity(256 * 1024);
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        f(&buf);
    }
}

/// Texts shorter than this are not matched by length.
const MIN_MATCH_LEN: u64 = 64;

/// Lines that are large and carry nothing this module needs.
fn skip(line: &[u8]) -> bool {
    line.starts_with(br#"{"type":"file-history"#)
}

impl Trace {
    /// Reads one transcript file. `sidechain` keeps lines marked
    /// `isSidechain` (subagent transcripts are all sidechain).
    pub fn read(&mut self, path: &Path, sidechain: bool) -> std::io::Result<()> {
        lines(path, |line| {
            if skip(line) {
                return;
            }
            let Ok(d) = serde_json::from_slice::<Value>(line) else {
                if line.iter().any(|b| !b.is_ascii_whitespace()) {
                    self.unreadable += 1;
                }
                return;
            };
            if !sidechain && d.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return;
            }
            self.record(&d);
        })?;
        self.finish_open();
        Ok(())
    }

    fn add(&mut self, mut p: Piece) -> u32 {
        p.turn = self.cur_turn;
        self.pieces.push(p);
        (self.pieces.len() - 1) as u32
    }

    /// Remembers a text piece by its length (only long texts: short ones
    /// collide).
    fn remember_text(&mut self, len: u64, piece: u32) {
        if len >= MIN_MATCH_LEN {
            self.text_by_len.insert(len, piece);
        }
    }

    fn known_text(&self, len: u64) -> Option<u32> {
        (len >= MIN_MATCH_LEN)
            .then(|| self.text_by_len.get(&len).copied())
            .flatten()
    }

    fn intern(&mut self, h: Hash) -> u32 {
        let n = self.hash_pieces.len() as u32;
        *self.hash_ids.entry(h).or_insert_with(|| {
            self.hash_pieces.push(Vec::new());
            n
        })
    }

    fn bind(&mut self, h: Hash, pieces: &[u32]) {
        let i = self.intern(h) as usize;
        if self.hash_pieces[i].is_empty() {
            self.hash_pieces[i] = pieces.to_vec();
        }
    }

    fn is_bound(&self, h: &Hash) -> bool {
        self.hash_ids
            .get(h)
            .is_some_and(|i| !self.hash_pieces[*i as usize].is_empty())
    }

    fn turn_mut(&mut self) -> Option<&mut TurnAcc> {
        self.cur_turn.map(|t| &mut self.turns[t as usize])
    }

    fn record(&mut self, d: &Value) {
        match s(d, "type").unwrap_or("") {
            "api-request-shape" => self.shape(d),
            "api-request" => {
                self.finish_open();
                self.api_request(d);
            }
            "api-request-blob" => {
                self.finish_open();
                self.blob(d);
            }
            "cost-state" => self.cost_state(d),
            "system" => self.system(d),
            "attachment" => self.attachment(d),
            "user" => self.user(d),
            "assistant" => self.assistant(d),
            _ => {}
        }
    }

    fn shape(&mut self, d: &Value) {
        let Some(sh) = d.get("shape") else { return };
        let hash = s(d, "shapeHash").unwrap_or("").to_string();
        if let Some(&i) = self.shape_ids.get(&hash) {
            self.cur_shape = Some(i);
            return;
        }
        let ts = ts_ms(d).unwrap_or(0);
        let system: Vec<u64> = match sh.get("system") {
            Some(Value::Array(a)) => a.iter().map(|b| measure(b).0).collect(),
            Some(v) => vec![measure(v).0],
            None => Vec::new(),
        };
        let tools: Vec<(String, u64)> = sh
            .get("tools")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|t| {
                        (
                            s(t, "name").unwrap_or("?").to_string(),
                            t.to_string().len() as u64,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut pieces = Vec::new();
        let mut sys = Piece::new(SYSTEM_PROMPT, "Overhead", Kind::Prose, ts);
        sys.bytes = system.iter().sum();
        pieces.push(sys);
        let mut builtin = Piece::new(BUILTIN_TOOLS, "Overhead", Kind::Schema, ts);
        let mut servers: BTreeMap<String, u64> = BTreeMap::new();
        for (name, b) in &tools {
            match mcp_server(name) {
                Some(srv) => *servers.entry(srv).or_default() += b,
                None => builtin.bytes += b,
            }
        }
        pieces.push(builtin);
        for (srv, b) in servers {
            let mut p = Piece::new(mcp_definitions(&srv), "Overhead", Kind::Schema, ts);
            p.bytes = b;
            p.source = Some(srv);
            pieces.push(p);
        }
        let ids = pieces
            .into_iter()
            .map(|mut p| {
                p.turn = None;
                self.pieces.push(p);
                (self.pieces.len() - 1) as u32
            })
            .collect();
        let i = self.shapes.len() as u32;
        self.shapes.push(Shape {
            hash: hash.clone(),
            system,
            tools,
            pieces: ids,
        });
        self.shape_ids.insert(hash, i);
        self.cur_shape = Some(i);
    }

    pub fn shape_index(&self, hash: &str) -> Option<u32> {
        self.shape_ids.get(hash).copied()
    }

    fn api_request(&mut self, d: &Value) {
        let Some(id) = s(d, "id") else { return };
        let tail = d
            .get("tail")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|h| {
                        let h = h.as_str().and_then(canon::parse_hex).unwrap_or([0; 32]);
                        self.intern(h)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let thinking = d
            .get("params")
            .and_then(|p| p.get("context_management"))
            .and_then(|c| c.get("edits"))
            .and_then(Value::as_array)
            .and_then(|edits| {
                edits
                    .iter()
                    .find(|e| s(e, "type").is_some_and(|t| t.starts_with("clear_thinking")))
            })
            .map(|e| match e.get("keep") {
                Some(Value::String(k)) if k == "all" => ThinkingPolicy::All,
                Some(Value::Object(o)) => ThinkingPolicy::Last(
                    o.get("value").and_then(Value::as_u64).unwrap_or(1) as usize,
                ),
                Some(Value::Number(n)) => ThinkingPolicy::Last(n.as_u64().unwrap_or(1) as usize),
                // The API's default for clear_thinking keeps the last turn.
                _ => ThinkingPolicy::Last(1),
            })
            // No edit: current models keep earlier thinking.
            .unwrap_or(ThinkingPolicy::All);
        let api = Api {
            base: s(d, "base").map(String::from),
            keep: u(d, "keep") as usize,
            tail,
            shape: s(d, "shapeHash").map(String::from),
            thinking,
        };
        self.api_ids.insert(id.to_string(), self.apis.len() as u32);
        self.apis.push(api);
    }

    fn blob(&mut self, d: &Value) {
        let Some(h) = s(d, "hash").and_then(canon::parse_hex) else {
            return;
        };
        if self.is_bound(&h) {
            return;
        }
        let Some(m) = d.get("message") else { return };
        let ts = ts_ms(d).unwrap_or(0);
        let role = s(m, "role").unwrap_or("");
        let content = m.get("content").unwrap_or(&Value::Null);
        let mut ids: Vec<u32> = Vec::new();
        match role {
            "assistant" => {
                // The same response as a transcript message logged in a
                // slightly different form: find it by its tool calls.
                let known = content.as_array().and_then(|a| {
                    a.iter().find_map(|b| {
                        b.get("id")
                            .and_then(Value::as_str)
                            .and_then(|id| self.tool_use_msg.get(id).cloned())
                    })
                });
                if let Some(k) = known {
                    ids = k;
                } else if let Some(a) = content.as_array() {
                    for b in a {
                        match s(b, "type") {
                            Some("text") => {
                                let mut p = Piece::new(TEXT, "Model output", Kind::Prose, ts);
                                p.bytes = measure(b).0;
                                ids.push(self.add(p));
                            }
                            Some("tool_use") => {
                                let name = s(b, "name").unwrap_or("?").to_string();
                                let mut p = Piece::new(TOOL_CALLS, "Model output", Kind::Data, ts);
                                p.bytes =
                                    b.get("input").map(|i| i.to_string().len()).unwrap_or(0) as u64;
                                p.tool = Some(name);
                                p.source = s(b, "id").map(String::from);
                                ids.push(self.add(p));
                            }
                            _ => {}
                        }
                    }
                } else {
                    let mut p = Piece::new(TEXT, "Model output", Kind::Prose, ts);
                    p.bytes = measure(content).0;
                    ids.push(self.add(p));
                }
            }
            "user" => {
                let has_result = content
                    .as_array()
                    .is_some_and(|a| a.iter().any(|b| s(b, "type") == Some("tool_result")));
                match content {
                    Value::Array(a) => {
                        for b in a {
                            match s(b, "type") {
                                Some("tool_result") => {
                                    let id = s(b, "tool_use_id").unwrap_or("");
                                    if let Some(&p) = self.tool_results.get(id) {
                                        ids.push(p);
                                    } else {
                                        ids.push(self.tool_result_piece(b, ts));
                                    }
                                }
                                Some("image") => {
                                    let mut p = Piece::new(IMAGES, "You", Kind::Image, ts);
                                    p.images = 1;
                                    ids.push(self.add(p));
                                }
                                _ => {
                                    let len = measure(b).0;
                                    if let Some(p) = self.known_text(len) {
                                        ids.push(p);
                                        continue;
                                    }
                                    let (l, g) = if has_result {
                                        (REMINDERS, "Injected")
                                    } else {
                                        (PROMPTS, "You")
                                    };
                                    let mut p = Piece::new(l, g, Kind::Prose, ts);
                                    p.bytes = len;
                                    ids.push(self.add(p));
                                }
                            }
                        }
                    }
                    other => {
                        let len = measure(other).0;
                        if let Some(p) = self.known_text(len) {
                            ids.push(p);
                        } else {
                            let mut p = Piece::new(PROMPTS, "You", Kind::Prose, ts);
                            p.bytes = len;
                            ids.push(self.add(p));
                        }
                    }
                }
            }
            _ => {
                let mut p = Piece::new(SYSTEM_MESSAGES, "Injected", Kind::Prose, ts);
                p.bytes = measure(content).0;
                ids.push(self.add(p));
            }
        }
        if !ids.is_empty() {
            self.bind(h, &ids);
        }
    }

    fn cost_state(&mut self, d: &Value) {
        let mut models: Vec<(String, ModelUse)> = d
            .get("modelUsage")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            ModelUse {
                                input: u(v, "inputTokens"),
                                output: u(v, "outputTokens"),
                                cache_read: u(v, "cacheReadInputTokens"),
                                cache_write: u(v, "cacheCreationInputTokens"),
                                web_search: u(v, "webSearchRequests"),
                                usd: v.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        models.sort_by(|a, b| b.1.usd.total_cmp(&a.1.usd));
        self.cost = Some(CostState {
            usd: d.get("totalCostUSD").and_then(Value::as_f64).unwrap_or(0.0),
            models,
            lines_added: u(d, "totalLinesAdded"),
            lines_removed: u(d, "totalLinesRemoved"),
            api_ms: u(d, "totalAPIDuration"),
            tool_ms: u(d, "totalToolDuration"),
        });
    }

    fn system(&mut self, d: &Value) {
        let ts = ts_ms(d).unwrap_or(0);
        match s(d, "subtype") {
            Some("compact_boundary") => {
                // Everything logged before the boundary is in the summary,
                // not in the next request's context.
                self.finish_open();
                self.pending.clear();
                self.compact_pending = true;
                if let Some(t) = self.turn_mut() {
                    t.compacted = true;
                }
                let m = d.get("compactMetadata");
                let pre = m.map(|m| u(m, "preTokens")).unwrap_or(0);
                let post = m.map(|m| u(m, "postTokens")).unwrap_or(0);
                let trig = m.and_then(|m| s(m, "trigger")).unwrap_or("auto");
                self.events.push(RawEvent {
                    ts,
                    kind: "compaction",
                    label: format!(
                        "{} compaction: {}k → {}k",
                        if trig == "manual" { "Manual" } else { "Auto" },
                        pre / 1000,
                        post / 1000
                    ),
                    tokens: pre,
                });
            }
            Some("api_error") => {
                self.api_errors += 1;
                if let Some(t) = self.turn_mut() {
                    t.errors += 1;
                }
                self.events.push(RawEvent {
                    ts,
                    kind: "error",
                    label: "API error".into(),
                    tokens: 0,
                });
            }
            _ => {}
        }
    }

    fn attachment(&mut self, d: &Value) {
        let k = d
            .get("attachment")
            .and_then(|a| a.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("other");
        if k == "hook_non_blocking_error" {
            self.hook_errors += 1;
        }
        match d.get("rendered") {
            None | Some(Value::Null) => {}
            Some(r) => {
                let (bytes, images) = measure(r);
                if bytes == 0 && images == 0 {
                    return;
                }
                let mut p = Piece::new(
                    attachment_label(k),
                    "Injected",
                    if bytes == 0 { Kind::Image } else { Kind::Prose },
                    ts_ms(d).unwrap_or(0),
                );
                p.bytes = bytes;
                p.images = images;
                p.source = Some(k.to_string());
                p.from_attachment = true;
                let id = self.add(p);
                if r.as_array().is_some_and(|a| a.len() == 1) || r.is_string() {
                    self.remember_text(bytes, id);
                }
                self.pending.push(id);
            }
        }
    }

    fn tool_result_piece(&mut self, b: &Value, ts: i64) -> u32 {
        let id = s(b, "tool_use_id").unwrap_or("");
        let name = self
            .tool_uses
            .get(id)
            .map(|t| t.0.clone())
            .unwrap_or_else(|| "Unknown tool".into());
        let (label, group) = tool_label(&name);
        let (bytes, images) = b.get("content").map(measure).unwrap_or((0, 0));
        let mut p = Piece::new(
            label,
            group,
            if bytes == 0 && images > 0 {
                Kind::Image
            } else {
                Kind::Data
            },
            ts,
        );
        p.bytes = bytes;
        p.images = images;
        p.tool = Some(name);
        p.source = Some(id.to_string());
        self.add(p)
    }

    fn start_turn(&mut self, d: &Value, ts: i64) {
        let origin = s(d, "turnOrigin")
            .map(String::from)
            .or_else(|| {
                d.get("origin")
                    .and_then(|o| s(o, "kind"))
                    .map(|k| k.replace('-', "_"))
            })
            .unwrap_or_else(|| "unknown".into());
        self.turns.push(TurnAcc {
            ts,
            origin,
            permission_mode: s(d, "permissionMode").map(String::from),
            ..Default::default()
        });
        self.cur_turn = Some((self.turns.len() - 1) as u32);
    }

    fn user(&mut self, d: &Value) {
        let Some(c) = d.get("message").and_then(|m| m.get("content")) else {
            return;
        };
        let ts = ts_ms(d).unwrap_or(0);
        if d.get("toolDenialKind").is_some_and(|v| !v.is_null()) {
            if let Some(t) = self.turn_mut() {
                t.denials += 1;
            }
        }
        let mut ids = Vec::new();
        if d.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
            let mut p = Piece::new(COMPACTION_SUMMARY, "Compaction", Kind::Prose, ts);
            p.bytes = measure(c).0;
            let id = self.add(p);
            self.remember_text(self.pieces[id as usize].bytes, id);
            ids.push(id);
        } else {
            let meta = d.get("isMeta").and_then(Value::as_bool) == Some(true);
            let prompt = match c {
                Value::String(_) => true,
                Value::Array(a) => a
                    .iter()
                    .any(|b| matches!(s(b, "type"), Some("text") | Some("image"))),
                _ => false,
            };
            if prompt && !meta {
                self.prompts += 1;
                self.start_turn(d, ts);
            }
            let (pl, pg) = if meta {
                (REMINDERS, "Injected")
            } else {
                (PROMPTS, "You")
            };
            match c {
                Value::String(t) => {
                    let mut p = Piece::new(pl, pg, Kind::Prose, ts);
                    p.bytes = t.len() as u64;
                    let id = self.add(p);
                    self.remember_text(t.len() as u64, id);
                    ids.push(id);
                }
                Value::Array(blocks) => {
                    for b in blocks {
                        match s(b, "type") {
                            Some("tool_result") => {
                                let p = self.tool_result_piece(b, ts);
                                let id = s(b, "tool_use_id").unwrap_or("").to_string();
                                let piece = &self.pieces[p as usize];
                                let (label, group) = (piece.label.clone(), piece.group);
                                let acc = self.tools.entry(label).or_default();
                                acc.group = group;
                                if b.get("is_error").and_then(Value::as_bool) == Some(true) {
                                    acc.errors += 1;
                                }
                                self.tool_results.insert(id, p);
                                ids.push(p);
                            }
                            Some("image") => {
                                let mut p = Piece::new(IMAGES, "You", Kind::Image, ts);
                                p.images = 1;
                                ids.push(self.add(p));
                            }
                            Some("text") => {
                                let mut p = Piece::new(pl, pg, Kind::Prose, ts);
                                p.bytes = measure(b).0;
                                let id = self.add(p);
                                self.remember_text(self.pieces[id as usize].bytes, id);
                                ids.push(id);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if ids.is_empty() {
            return;
        }
        self.pending.extend_from_slice(&ids);
        self.bind(canon::hash("user", c), &ids);
        if let Value::String(t) = c {
            let blocks = serde_json::json!([{"type": "text", "text": t}]);
            self.bind(canon::hash("user", &blocks), &ids);
        }
    }

    fn assistant(&mut self, d: &Value) {
        let Some(m) = d.get("message") else { return };
        let ts = ts_ms(d).unwrap_or(0);
        let mid = s(m, "id").unwrap_or("").to_string();
        if self.open.as_ref().is_some_and(|o| o.id != mid) {
            self.finish_open();
        }
        let model = s(m, "model").unwrap_or("");
        let us = m.get("usage");
        let real = us.is_some() && !model.starts_with('<');
        if self.open.is_none() {
            let (out, think) = us
                .map(|us| {
                    let out = u(us, "output_tokens");
                    let think = us
                        .get("output_tokens_details")
                        .map(|x| u(x, "thinking_tokens"))
                        .unwrap_or(0)
                        .min(out);
                    (out, think)
                })
                .unwrap_or((0, 0));
            self.open = Some(OpenMsg {
                id: mid.clone(),
                blocks: Vec::new(),
                out,
                think,
                ts,
                turn: self.cur_turn,
                real,
            });
        }
        if let Some(blocks) = m.get("content").and_then(Value::as_array) {
            for b in blocks {
                if s(b, "type") == Some("tool_use") {
                    let id = s(b, "id").unwrap_or("").to_string();
                    let name = s(b, "name").unwrap_or("?").to_string();
                    let (label, group) = tool_label(&name);
                    let acc = self.tools.entry(label).or_default();
                    acc.group = group;
                    acc.calls += 1;
                    let turn = self.cur_turn;
                    if let Some(t) = self.turn_mut() {
                        *t.tools.entry(name.clone()).or_default() += 1;
                        if is_agent_tool(&name) {
                            t.subagents += 1;
                        }
                    }
                    self.tool_uses.insert(id, (name, ts, turn));
                }
                if let Some(o) = self.open.as_mut() {
                    o.blocks.push(b.clone());
                }
            }
        }
        if let Some(sk) = s(d, "attributionSkill") {
            if let Some(t) = self.turn_mut() {
                t.skill.get_or_insert_with(|| sk.to_string());
            }
        }
        let Some(us) = us.filter(|_| real) else {
            return;
        };
        let key = format!("{}:{}", mid, s(d, "requestId").unwrap_or(""));
        if !self.seen.insert(key) {
            return;
        }
        if self.cur_turn.is_none() {
            // Requests before any prompt (resumed or automated sessions).
            self.start_turn(&Value::Null, ts);
            self.turns.last_mut().unwrap().origin = "unknown".into();
            if let Some(o) = self.open.as_mut() {
                o.turn = self.cur_turn;
            }
        }
        let cw = u(us, "cache_creation_input_tokens");
        let cr = u(us, "cache_read_input_tokens");
        let input = u(us, "input_tokens");
        let out = u(us, "output_tokens");
        let think = us
            .get("output_tokens_details")
            .map(|x| u(x, "thinking_tokens"))
            .unwrap_or(0)
            .min(out);
        let api = s(d, "requestRef").and_then(|r| self.api_ids.get(r).copied());
        self.reqs.push(Req {
            ts,
            ctx: input + cr + cw,
            input,
            cache_read: cr,
            cw,
            out,
            think,
            model: model.to_string(),
            added: std::mem::take(&mut self.pending),
            compact_before: std::mem::take(&mut self.compact_pending),
            shape: self.cur_shape,
            api,
            turn: self.cur_turn.unwrap_or(0),
        });
    }

    /// The streamed response is complete: its output joins the context of the
    /// next request.
    fn finish_open(&mut self) {
        let Some(o) = self.open.take() else { return };
        let mut text_bytes = 0u64;
        let mut calls: Vec<(String, String, u64)> = Vec::new();
        for b in &o.blocks {
            match s(b, "type") {
                Some("text") => text_bytes += measure(b).0,
                Some("tool_use") => calls.push((
                    s(b, "name").unwrap_or("?").to_string(),
                    s(b, "id").unwrap_or("").to_string(),
                    b.get("input").map(|i| i.to_string().len()).unwrap_or(0) as u64,
                )),
                _ => {}
            }
        }
        let saved = self.cur_turn;
        self.cur_turn = o.turn;
        let mut ids = Vec::new();
        if o.real {
            if o.think > 0 {
                let mut p = Piece::new(THINKING, "Model output", Kind::Output, o.ts);
                p.exact = Some(o.think as f64);
                p.thinking = true;
                ids.push(self.add(p));
            }
            let rest = (o.out - o.think) as f64;
            let call_bytes: u64 = calls.iter().map(|c| c.2).sum();
            let total = (text_bytes + call_bytes) as f64;
            if rest > 0.0 && (text_bytes > 0 || total == 0.0) {
                let share = if total > 0.0 {
                    text_bytes as f64 / total
                } else {
                    1.0
                };
                let mut p = Piece::new(TEXT, "Model output", Kind::Output, o.ts);
                p.exact = Some(rest * share);
                p.bytes = text_bytes;
                ids.push(self.add(p));
            }
            for (name, id, b) in &calls {
                let mut p = Piece::new(TOOL_CALLS, "Model output", Kind::Output, o.ts);
                p.exact = Some(if total > 0.0 {
                    rest * *b as f64 / total
                } else {
                    0.0
                });
                p.bytes = *b;
                p.tool = Some(name.clone());
                p.source = Some(id.clone());
                ids.push(self.add(p));
            }
        } else if text_bytes + calls.iter().map(|c| c.2).sum::<u64>() > 0 {
            // A synthetic message (error placeholder): sized by bytes.
            let mut p = Piece::new(TEXT, "Model output", Kind::Prose, o.ts);
            p.bytes = text_bytes + calls.iter().map(|c| c.2).sum::<u64>();
            ids.push(self.add(p));
        }
        self.cur_turn = saved;
        if ids.is_empty() {
            return;
        }
        for (_, id, _) in &calls {
            self.tool_use_msg.insert(id.clone(), ids.clone());
        }
        self.pending.extend_from_slice(&ids);
        let content = Value::Array(o.blocks);
        self.bind(canon::hash("assistant", &content), &ids);
    }
}
