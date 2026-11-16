//! Synthetic transcript builder. Every line is generated here with filler
//! text of a known size; nothing is copied from real sessions.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::path::Path;
use usage_core::session::message_hash;

/// Marker inside every filler string, so tests can check it never leaks.
pub const MARKER: &str = "SECRETTEXT";

/// `n` bytes of filler that start with the marker.
pub fn filler(n: usize) -> String {
    let mut s = String::with_capacity(n);
    s.push_str(&MARKER[..MARKER.len().min(n)]);
    while s.len() < n {
        s.push('z');
    }
    s
}

pub enum Block {
    Thinking,
    Text(usize),
    /// id, tool name, bytes of the `command` argument.
    Tool(&'static str, &'static str, usize),
}

pub struct Usage {
    pub ctx: u64,
    pub cw: u64,
    pub out: u64,
    pub think: u64,
}

pub fn usage(ctx: u64, cw: u64, out: u64, think: u64) -> Usage {
    Usage {
        ctx,
        cw,
        out,
        think,
    }
}

#[derive(Default)]
pub struct Transcript {
    lines: Vec<String>,
    secs: i64,
    sidechain: bool,
    /// Messages as the API would see them, for request snapshots.
    msgs: Vec<(String, Value)>,
    last_api: Option<(String, usize)>,
    pending_ref: Option<String>,
    shape: Option<String>,
    n: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// A subagent transcript: every line is a sidechain line.
    pub fn sidechain() -> Self {
        Transcript {
            sidechain: true,
            ..Self::default()
        }
    }

    pub fn ts(&self) -> String {
        let t = self.secs;
        let (d, h, m, s) = (16 + t / 86_400, t / 3600 % 24, t / 60 % 60, t % 60);
        format!("2026-11-{d:02}T{h:02}:{m:02}:{s:02}.000Z")
    }

    pub fn wait(&mut self, secs: i64) -> &mut Self {
        self.secs += secs;
        self
    }

    fn push(&mut self, mut v: Value) {
        self.secs += 1;
        let o = v.as_object_mut().unwrap();
        o.insert("timestamp".into(), json!(self.ts()));
        o.insert("sessionId".into(), json!("s1"));
        if self.sidechain {
            o.insert("isSidechain".into(), json!(true));
        }
        self.lines.push(v.to_string());
    }

    pub fn raw(&mut self, line: &str) -> &mut Self {
        self.lines.push(line.to_string());
        self
    }

    pub fn prompt(&mut self, bytes: usize) -> &mut Self {
        self.prompt_from(bytes, "human")
    }

    pub fn prompt_from(&mut self, bytes: usize, origin: &str) -> &mut Self {
        let c = json!(filler(bytes));
        self.msgs.push(("user".into(), c.clone()));
        self.push(
            json!({"type":"user","turnOrigin":origin,"permissionMode":"default",
            "message":{"role":"user","content":c}}),
        );
        self
    }

    pub fn image_prompt(&mut self, text: usize) -> &mut Self {
        let c = json!([{"type":"image","source":{"type":"base64","data":filler(5000)}},
            {"type":"text","text":filler(text)}]);
        self.msgs.push(("user".into(), c.clone()));
        self.push(
            json!({"type":"user","turnOrigin":"human","message":{"role":"user","content":c}}),
        );
        self
    }

    pub fn meta(&mut self, bytes: usize) -> &mut Self {
        let c = json!(filler(bytes));
        self.msgs.push(("user".into(), c.clone()));
        self.push(json!({"type":"user","isMeta":true,"message":{"role":"user","content":c}}));
        self
    }

    pub fn attachment(&mut self, kind: &str, bytes: usize) -> &mut Self {
        self.push(json!({"type":"attachment","attachment":{"type":kind},
            "rendered":[{"content":filler(bytes)}]}));
        self
    }

    pub fn tool_result(&mut self, id: &str, bytes: usize) -> &mut Self {
        let c = json!([{"type":"tool_result","tool_use_id":id,"content":filler(bytes)}]);
        self.msgs.push(("user".into(), c.clone()));
        self.push(json!({"type":"user","message":{"role":"user","content":c}}));
        self
    }

    pub fn denied(&mut self, id: &str) -> &mut Self {
        let c = json!([{"type":"tool_result","tool_use_id":id,"content":"no","is_error":true}]);
        self.msgs.push(("user".into(), c.clone()));
        self.push(json!({"type":"user","toolDenialKind":"user-rejected",
            "message":{"role":"user","content":c}}));
        self
    }

    /// A response logged one content block per line, like Claude Code does.
    pub fn assistant(&mut self, u: Usage, blocks: &[Block]) -> &mut Self {
        self.n += 1;
        let id = format!("msg_{}", self.n);
        let req = format!("req_{}", self.n);
        let us = json!({"input_tokens":0,"output_tokens":u.out,
            "cache_creation_input_tokens":u.cw,"cache_read_input_tokens":u.ctx - u.cw,
            "output_tokens_details":{"thinking_tokens":u.think}});
        let mut content = Vec::new();
        for b in blocks {
            content.push(match b {
                Block::Thinking => json!({"type":"thinking","thinking":"","signature":"sig"}),
                Block::Text(n) => json!({"type":"text","text":filler(*n)}),
                Block::Tool(id, name, n) => {
                    json!({"type":"tool_use","id":id,"name":name,"input":{"command":filler(*n)}})
                }
            });
        }
        if content.is_empty() {
            content.push(json!({"type":"text","text":""}));
        }
        let rref = self.pending_ref.take();
        for b in &content {
            let mut line = json!({"type":"assistant","requestId":req,
                "message":{"id":id,"model":"claude-opus-5-5","role":"assistant",
                    "content":[b],"usage":us}});
            if let Some(r) = &rref {
                line["requestRef"] = json!(r);
            }
            self.push(line);
        }
        self.msgs.push(("assistant".into(), Value::Array(content)));
        self
    }

    pub fn compact(&mut self, pre: u64, summary_bytes: usize) -> &mut Self {
        self.push(json!({"type":"system","subtype":"compact_boundary",
            "compactMetadata":{"trigger":"auto","preTokens":pre,"postTokens":1000}}));
        self.msgs.clear();
        self.last_api = None;
        let c = json!(filler(summary_bytes));
        self.msgs.push(("user".into(), c.clone()));
        self.push(json!({"type":"user","isCompactSummary":true,
            "message":{"role":"user","content":c}}));
        self
    }

    pub fn api_error(&mut self) -> &mut Self {
        self.push(json!({"type":"system","subtype":"api_error","retryAttempt":1}));
        self
    }

    /// A request shape: system prompt of `system` bytes and tools with
    /// definitions of the given JSON sizes.
    pub fn shape(&mut self, hash: &str, system: usize, tools: &[(&str, usize)]) -> &mut Self {
        let tools: Vec<Value> = tools
            .iter()
            .map(|(name, bytes)| {
                // {"description":"D","name":"N"} is 28 bytes plus D and N.
                json!({"name":name,"description":"d".repeat(bytes - 28 - name.len())})
            })
            .collect();
        self.shape = Some(hash.to_string());
        self.push(json!({"type":"api-request-shape","shapeHash":hash,
            "shape":{"system":[{"type":"text","text":filler(system)}],"tools":tools}}));
        self
    }

    /// A message the API saw that the transcript did not log as such, stored
    /// as a blob (as Claude Code does for host-injected messages).
    pub fn blob(&mut self, role: &str, bytes: usize) -> &mut Self {
        let m = json!({"role":role,"content":filler(bytes)});
        let h = message_hash(role, &m["content"]);
        self.msgs.push((role.into(), m["content"].clone()));
        self.push(json!({"type":"api-request-blob","hash":h,"message":m}));
        self
    }

    /// A message the API saw that the transcript has nowhere (no blob).
    pub fn unlogged(&mut self, bytes: usize) -> &mut Self {
        self.msgs
            .push(("user".into(), json!(format!("unlogged {}", filler(bytes)))));
        self
    }

    /// Records a request snapshot of the current message list; the next
    /// assistant response refers to it.
    pub fn snapshot(&mut self, context_management: Value) -> &mut Self {
        let id = format!("api-{}", self.lines.len());
        let hashes: Vec<String> = self.msgs.iter().map(|(r, c)| message_hash(r, c)).collect();
        let (base, keep) = match &self.last_api {
            Some((b, k)) => (Some(b.clone()), *k),
            None => (None, 0),
        };
        let mut v = json!({"type":"api-request","id":id,"querySource":"sdk",
            "params":{"model":"claude-opus-5-5","context_management":context_management},
            "keep":keep,"tail":hashes[keep..]});
        if let Some(b) = base {
            v["base"] = json!(b);
        }
        if let Some(s) = &self.shape {
            v["shapeHash"] = json!(s);
        }
        self.push(v);
        self.last_api = Some((id.clone(), hashes.len()));
        self.pending_ref = Some(id);
        self
    }

    pub fn cost_state(&mut self, model_usage: Value) -> &mut Self {
        self.push(json!({"type":"cost-state","totalCostUSD":1.5,"modelUsage":model_usage}));
        self
    }

    pub fn write(&self, path: &Path) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        let mut s = self.lines.join("\n");
        s.push('\n');
        std::fs::write(path, s).unwrap();
    }
}

pub fn keep_all() -> Value {
    json!({"edits":[{"type":"clear_thinking_20251015","keep":"all"}]})
}

pub fn keep_turns(n: u64) -> Value {
    json!({"edits":[{"type":"clear_thinking_20251015","keep":{"type":"thinking_turns","value":n}}]})
}
