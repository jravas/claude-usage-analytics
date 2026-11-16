//! Claude Desktop's session index:
//! `~/Library/Application Support/Claude/claude-code-sessions/<account>/<session>/local_*.json`.
//!
//! One file per Desktop session. It names the Claude Code session it runs
//! (`cliSessionId`) and the ones it ran before a resume
//! (`priorCliSessionIds`), and carries the title Desktop shows, settings,
//! and the names of the connectors (remote MCP servers) it had, keyed by
//! UUID. Tool names of connectors in transcripts use that UUID, so
//! [`mcp_name`] turns them back into names.
//!
//! Stored in `desktop_sessions`, `session_links` (every CLI session id,
//! current or prior, to its canonical id: the Desktop session's current
//! one) and `mcp_names`. Desktop's title replaces the transcript's in
//! `sessions`.

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSession {
    /// Desktop's own id, `local_<uuid>`.
    pub id: String,
    pub cli_session_id: Option<String>,
    pub prior_cli_session_ids: Vec<String>,
    pub title: Option<String>,
    /// `auto`, `tool` or `user`.
    pub title_source: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub archived: bool,
    /// Unix ms.
    pub created_at: Option<i64>,
    pub last_activity_at: Option<i64>,
    pub completed_turns: Option<i64>,
    pub branch: Option<String>,
    pub source_branch: Option<String>,
    pub origin_cwd: Option<String>,
    /// Connectors: (uuid, name).
    pub mcp_servers: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    session_id: Option<String>,
    cli_session_id: Option<String>,
    prior_cli_session_ids: Option<Vec<String>>,
    title: Option<String>,
    title_source: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    is_archived: Option<bool>,
    created_at: Option<i64>,
    last_activity_at: Option<i64>,
    completed_turns: Option<i64>,
    branch: Option<String>,
    source_branch: Option<String>,
    origin_cwd: Option<String>,
    remote_mcp_servers_config: Option<Vec<RawServer>>,
}

#[derive(Deserialize)]
struct RawServer {
    uuid: Option<String>,
    name: Option<String>,
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

/// Parse one `local_*.json`. Unknown keys are ignored.
pub fn parse_session(bytes: &[u8], file_stem: &str) -> serde_json::Result<DesktopSession> {
    let r: Raw = serde_json::from_slice(bytes)?;
    Ok(DesktopSession {
        id: nonempty(r.session_id).unwrap_or_else(|| file_stem.to_string()),
        cli_session_id: nonempty(r.cli_session_id),
        prior_cli_session_ids: r
            .prior_cli_session_ids
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect(),
        title: nonempty(r.title),
        title_source: r.title_source,
        model: r.model,
        effort: r.effort,
        permission_mode: r.permission_mode,
        archived: r.is_archived.unwrap_or(false),
        created_at: r.created_at,
        last_activity_at: r.last_activity_at,
        completed_turns: r.completed_turns,
        branch: nonempty(r.branch),
        source_branch: nonempty(r.source_branch),
        origin_cwd: r.origin_cwd,
        mcp_servers: r
            .remote_mcp_servers_config
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| Some((s.uuid?, s.name?)))
            .collect(),
    })
}

pub fn parse_session_file(path: &Path) -> std::io::Result<DesktopSession> {
    let b = std::fs::read(path)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    parse_session(&b, &stem).map_err(std::io::Error::other)
}

/// Every `<root>/<account>/<session>/local_*.json`.
pub fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    for account in dirs(root) {
        for session in dirs(&account) {
            let Ok(rd) = std::fs::read_dir(&session) else {
                continue;
            };
            for e in rd.flatten() {
                let n = e.file_name();
                let n = n.to_string_lossy();
                if n.starts_with("local_") && n.ends_with(".json") {
                    out.push(e.path());
                }
            }
        }
    }
    out
}

/// Write one session into the tables. Called inside the refresh
/// transaction.
pub(crate) fn store(conn: &Connection, s: &DesktopSession) -> rusqlite::Result<()> {
    let servers: Vec<&str> = s.mcp_servers.iter().map(|(_, n)| n.as_str()).collect();
    conn.execute(
        "INSERT OR REPLACE INTO desktop_sessions(id, cli_session_id, title, title_source, model,
           effort, permission_mode, archived, created_at, last_activity_at, completed_turns,
           branch, source_branch, origin_cwd, mcp_servers)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            s.id,
            s.cli_session_id,
            s.title,
            s.title_source,
            s.model,
            s.effort,
            s.permission_mode,
            s.archived,
            s.created_at,
            s.last_activity_at,
            s.completed_turns,
            s.branch,
            s.source_branch,
            s.origin_cwd,
            serde_json::to_string(&servers).unwrap_or_else(|_| "[]".into()),
        ],
    )?;
    for (uuid, name) in &s.mcp_servers {
        conn.execute(
            "INSERT OR REPLACE INTO mcp_names(uuid, name) VALUES (?1, ?2)",
            params![uuid, name],
        )?;
    }
    let Some(cli) = &s.cli_session_id else {
        return Ok(());
    };
    // An id that is some Desktop session's current one stays its own
    // canonical id; this session's earlier ids point at its current one.
    conn.execute(
        "INSERT INTO session_links(cli_session_id, canonical, desktop_id) VALUES (?1, ?1, ?2)
         ON CONFLICT(cli_session_id) DO UPDATE SET canonical = excluded.canonical,
           desktop_id = excluded.desktop_id",
        params![cli, s.id],
    )?;
    for prior in s.prior_cli_session_ids.iter().filter(|p| *p != cli) {
        conn.execute(
            "INSERT INTO session_links(cli_session_id, canonical, desktop_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(cli_session_id) DO UPDATE SET canonical = excluded.canonical,
               desktop_id = excluded.desktop_id
             WHERE session_links.desktop_id = excluded.desktop_id
                OR session_links.canonical <> session_links.cli_session_id",
            params![prior, cli, s.id],
        )?;
    }
    if let Some(t) = &s.title {
        for id in std::iter::once(cli).chain(&s.prior_cli_session_ids) {
            conn.execute(
                "INSERT INTO sessions(id, title) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title",
                params![id, t],
            )?;
        }
    }
    Ok(())
}

/// The connector name for a UUID, if Claude Desktop recorded one.
pub fn mcp_name(conn: &Connection, uuid: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT name FROM mcp_names WHERE uuid = ?1", [uuid], |r| {
            r.get(0)
        })
        .optional()?)
}

/// Every known connector UUID with its name.
pub fn mcp_names(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut st = conn.prepare("SELECT uuid, name FROM mcp_names")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Replace a leading connector UUID in an MCP server or tool name
/// (`mcp__<uuid>__tool`, or a bare `<uuid>`) with the connector's name.
pub fn resolve_mcp_name(names: &HashMap<String, String>, s: &str) -> String {
    let (prefix, rest) = s.strip_prefix("mcp__").map_or(("", s), |r| ("mcp__", r));
    let (head, tail) = rest.split_once("__").map_or((rest, ""), |(h, t)| (h, t));
    match names.get(head) {
        Some(n) if tail.is_empty() => format!("{prefix}{n}"),
        Some(n) => format!("{prefix}{n}__{tail}"),
        None => s.to_string(),
    }
}

/// The canonical session id for a CLI session id: the id of the Desktop
/// session's current Claude Code session after resumes.
pub fn canonical_session(conn: &Connection, cli_session_id: &str) -> Result<String> {
    Ok(conn
        .query_row(
            "SELECT canonical FROM session_links WHERE cli_session_id = ?1",
            [cli_session_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| cli_session_id.to_string()))
}

/// Every CLI session id that is part of the same session as `id`, current
/// one first.
pub fn session_ids(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let c = canonical_session(conn, id)?;
    let mut st = conn.prepare(
        "SELECT cli_session_id FROM session_links WHERE canonical = ?1 AND cli_session_id <> ?1",
    )?;
    let mut out = vec![c.clone()];
    for r in st.query_map([&c], |r| r.get::<_, String>(0))? {
        out.push(r?);
    }
    Ok(out)
}

/// What Claude Desktop recorded for a CLI session id (current or prior).
pub fn session_info(conn: &Connection, cli_session_id: &str) -> Result<Option<DesktopSession>> {
    let row = conn
        .query_row(
            "SELECT d.id, d.cli_session_id, d.title, d.title_source, d.model, d.effort,
               d.permission_mode, d.archived, d.created_at, d.last_activity_at, d.completed_turns,
               d.branch, d.source_branch, d.origin_cwd
             FROM session_links l JOIN desktop_sessions d ON d.id = l.desktop_id
             WHERE l.cli_session_id = ?1",
            [cli_session_id],
            |r| {
                Ok(DesktopSession {
                    id: r.get(0)?,
                    cli_session_id: r.get(1)?,
                    title: r.get(2)?,
                    title_source: r.get(3)?,
                    model: r.get(4)?,
                    effort: r.get(5)?,
                    permission_mode: r.get(6)?,
                    archived: r.get(7)?,
                    created_at: r.get(8)?,
                    last_activity_at: r.get(9)?,
                    completed_turns: r.get(10)?,
                    branch: r.get(11)?,
                    source_branch: r.get(12)?,
                    origin_cwd: r.get(13)?,
                    ..Default::default()
                })
            },
        )
        .optional()?;
    let Some(mut d) = row else { return Ok(None) };
    let mut st = conn.prepare(
        "SELECT cli_session_id FROM session_links WHERE desktop_id = ?1 AND cli_session_id <> canonical",
    )?;
    d.prior_cli_session_ids = st
        .query_map([&d.id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let servers: String = conn.query_row(
        "SELECT mcp_servers FROM desktop_sessions WHERE id = ?1",
        [&d.id],
        |r| r.get(0),
    )?;
    let names: Vec<String> = serde_json::from_str(&servers).unwrap_or_default();
    let mut by_name = conn.prepare("SELECT uuid FROM mcp_names WHERE name = ?1 LIMIT 1")?;
    for n in names {
        let uuid: Option<String> = by_name.query_row([&n], |r| r.get(0)).optional()?;
        d.mcp_servers.push((uuid.unwrap_or_default(), n));
    }
    Ok(Some(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Index;

    fn write(root: &Path, account: &str, id: &str, json: serde_json::Value) {
        let d = root.join(account).join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{id}.json")), json.to_string()).unwrap();
    }

    fn fixture(root: &Path) {
        write(
            root,
            "acct",
            "local_a",
            serde_json::json!({
                "sessionId": "local_a", "cliSessionId": "cli-2", "priorCliSessionIds": ["cli-1"],
                "title": "Fix the parser", "titleSource": "auto", "model": "claude-opus-5-5",
                "effort": "high", "permissionMode": "default", "isArchived": true,
                "createdAt": 1_789_000_000_000i64, "lastActivityAt": 1_789_000_100_000i64,
                "completedTurns": 4, "branch": "fix/parser", "originCwd": "/r/app",
                "remoteMcpServersConfig": [
                    {"uuid": "f9fb062d-0000", "name": "Notion", "tools": [{"name": "search", "inputSchema": {}}]}
                ],
                "enabledMcpTools": {"f9fb062d-0000:search": true},
                "somethingNew": {"nested": [1, 2, 3]}
            }),
        );
        write(
            root,
            "acct",
            "local_b",
            serde_json::json!({"sessionId": "local_b", "createdAt": 1, "lastActivityAt": 2,
                "model": "m", "isArchived": false, "permissionMode": "default",
                "remoteMcpServersConfig": []}),
        );
        std::fs::write(root.join("acct/local_b/scheduled-tasks.json"), "{}").unwrap();
    }

    #[test]
    fn reads_sessions_links_titles_and_connectors() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut idx = Index::open_in_memory().unwrap();
        // A transcript title exists before Desktop's is read.
        idx.conn
            .execute(
                "INSERT INTO sessions VALUES ('cli-2', 'From transcript')",
                [],
            )
            .unwrap();
        assert_eq!(idx.refresh_desktop(dir.path()).unwrap(), 2);
        assert_eq!(idx.refresh_desktop(dir.path()).unwrap(), 0, "unchanged");

        let title: String = idx
            .conn
            .query_row("SELECT title FROM sessions WHERE id = 'cli-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Fix the parser");
        let title: String = idx
            .conn
            .query_row("SELECT title FROM sessions WHERE id = 'cli-2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Fix the parser");

        assert_eq!(canonical_session(&idx.conn, "cli-1").unwrap(), "cli-2");
        assert_eq!(canonical_session(&idx.conn, "other").unwrap(), "other");
        assert_eq!(
            session_ids(&idx.conn, "cli-1").unwrap(),
            vec!["cli-2".to_string(), "cli-1".to_string()]
        );

        assert_eq!(
            mcp_name(&idx.conn, "f9fb062d-0000").unwrap().as_deref(),
            Some("Notion")
        );
        assert_eq!(mcp_name(&idx.conn, "nope").unwrap(), None);
        let names = mcp_names(&idx.conn).unwrap();
        assert_eq!(
            resolve_mcp_name(&names, "mcp__f9fb062d-0000__search"),
            "mcp__Notion__search"
        );
        assert_eq!(resolve_mcp_name(&names, "f9fb062d-0000"), "Notion");
        assert_eq!(resolve_mcp_name(&names, "Bash"), "Bash");

        let d = session_info(&idx.conn, "cli-1").unwrap().unwrap();
        assert_eq!(d.id, "local_a");
        assert_eq!(d.cli_session_id.as_deref(), Some("cli-2"));
        assert_eq!(d.prior_cli_session_ids, vec!["cli-1".to_string()]);
        assert!(d.archived);
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(d.branch.as_deref(), Some("fix/parser"));
        assert_eq!(
            d.mcp_servers,
            vec![("f9fb062d-0000".to_string(), "Notion".to_string())]
        );
        assert!(session_info(&idx.conn, "nope").unwrap().is_none());
    }

    #[test]
    fn transcript_title_does_not_replace_desktop_title() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let proj = dir.path().join("projects/-r-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("cli-2.jsonl"),
            "{\"type\":\"custom-title\",\"customTitle\":\"Transcript title\",\"sessionId\":\"cli-2\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"Only here\",\"sessionId\":\"cli-9\"}\n",
        )
        .unwrap();
        let mut idx = Index::open_in_memory().unwrap();
        idx.set_desktop_root(Some(dir.path().to_path_buf()));
        idx.refresh_desktop(dir.path()).unwrap();
        let s = idx
            .refresh(&dir.path().join("projects"), &|_, _| {})
            .unwrap();
        assert_eq!(s.files_read, 1);
        let t = |id: &str| -> String {
            idx.conn
                .query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!(t("cli-2"), "Fix the parser");
        assert_eq!(t("cli-9"), "Only here");
    }
}
