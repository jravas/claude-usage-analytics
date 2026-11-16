//! Category labels and groups. One place, so the CLI, the app and the tests
//! use the same words.

/// Groups, in display order. `Cache rebuild` and `Background` never appear in
/// the context composition (they are not context); they label cache writes
/// and background model usage.
pub const GROUPS: &[&str] = &[
    "Model output",
    "Tools",
    "MCP",
    "Subagents",
    "Injected",
    "You",
    "Compaction",
    "Overhead",
    "Cache rebuild",
    "Background",
    "Other",
];

pub const THINKING: &str = "Thinking";
pub const TEXT: &str = "Text";
pub const TOOL_CALLS: &str = "Tool calls";
pub const SYSTEM_PROMPT: &str = "System prompt";
pub const BUILTIN_TOOLS: &str = "Built-in tool definitions";
pub const START_OF_CONTEXT: &str = "Start of context (system prompt, tools, history)";
pub const UNATTRIBUTED: &str = "Unattributed";
pub const SUBAGENT_RESULTS: &str = "Subagent results";
pub const COMPACTION_SUMMARY: &str = "Compaction summary";
pub const PROMPTS: &str = "Your prompts";
pub const IMAGES: &str = "Images you pasted";
pub const REMINDERS: &str = "Reminders";
pub const SYSTEM_MESSAGES: &str = "System messages";
pub const CACHE_REBUILD: &str = "Cache rebuild after idle";

pub fn mcp_definitions(server: &str) -> String {
    format!("MCP tool definitions: {server}")
}

fn uuid_like(s: &str) -> bool {
    s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Connector UUID → name, from Claude Desktop's session index. Set before an
/// analysis by [`set_connector_names`]; empty in tests.
static CONNECTOR_NAMES: std::sync::RwLock<Option<std::collections::HashMap<String, String>>> =
    std::sync::RwLock::new(None);

pub fn set_connector_names(names: std::collections::HashMap<String, String>) {
    if let Ok(mut g) = CONNECTOR_NAMES.write() {
        *g = Some(names);
    }
}

/// MCP server of a tool name, or None for a built-in tool. Connectors named by
/// UUID take their name from Claude Desktop when known, else the prefix of
/// their tool names ("notion-search" → "notion (connector)").
pub fn mcp_server(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let mut parts = rest.splitn(2, "__");
    let server = parts.next().unwrap_or(rest);
    let tool = parts.next().unwrap_or("");
    if uuid_like(server) {
        if let Some(n) = CONNECTOR_NAMES
            .read()
            .ok()
            .and_then(|g| g.as_ref().and_then(|m| m.get(server).cloned()))
        {
            return Some(n);
        }
    }
    Some(if uuid_like(server) {
        let p = tool.split(['-', '_']).next().unwrap_or("connector");
        format!("{p} (connector)")
    } else {
        server.to_string()
    })
}

pub fn is_agent_tool(name: &str) -> bool {
    name == "Agent" || name == "Task"
}

/// (label, group) for a tool's results in context.
pub fn tool_label(name: &str) -> (String, &'static str) {
    if is_agent_tool(name) {
        return (SUBAGENT_RESULTS.into(), "Subagents");
    }
    match mcp_server(name) {
        Some(s) => (s, "MCP"),
        None => (name.to_string(), "Tools"),
    }
}

pub fn attachment_label(kind: &str) -> String {
    match kind {
        "hook_non_blocking_error" => "Hook errors".into(),
        "hook_additional_context" | "hook_success" => "Hook output".into(),
        "deferred_tools_delta" | "deferred_tools_record" => "Deferred tool lists".into(),
        "skill_listing" => "Skill list".into(),
        "agent_listing_delta" => "Agent list".into(),
        "mcp_instructions_delta" => "MCP server instructions".into(),
        "environment" | "session_context" => "Environment info".into(),
        "edited_text_file" => "Edited file snapshots".into(),
        "file" | "compact_file_reference" => "Attached files".into(),
        "instructions" => "CLAUDE.md and instructions".into(),
        "plan_file_reference" | "plan_mode" | "plan_mode_exit" => "Plan mode".into(),
        "date" | "model" | "auto_mode" | "auto_mode_exit" => "Mode and date notes".into(),
        "queued_command" => "Queued messages".into(),
        other => {
            let s = other.replace('_', " ");
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => "Other".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(tool_label("Bash"), ("Bash".into(), "Tools"));
        assert_eq!(tool_label("Agent"), (SUBAGENT_RESULTS.into(), "Subagents"));
        assert_eq!(
            tool_label("mcp__claude-in-chrome__navigate"),
            ("claude-in-chrome".into(), "MCP")
        );
        assert_eq!(
            tool_label("mcp__0b6e1c2a-7d4f-4e9a-9c3b-5a8f2d1e6b7c__notion-search"),
            ("notion (connector)".into(), "MCP")
        );
        assert_eq!(mcp_server("Read"), None);
        assert_eq!(attachment_label("hook_non_blocking_error"), "Hook errors");
        assert_eq!(attachment_label("weird_new_thing"), "Weird new thing");
    }
}
