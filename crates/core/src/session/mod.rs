//! One session in depth: what filled its context, how the context grew, what
//! the model produced, and what to look at.
//!
//! Every request re-sends the whole conversation, so a session's tokens are
//! mostly context. The context size of each request is exact (from `usage`).
//! The ledger ([`ledger`]) says which pieces made up each request's context:
//! exactly from request snapshots (`api-request`) when Claude Code recorded
//! them, otherwise by splitting the growth between two requests across what
//! the transcript added in between. Per request the pieces sum to the real
//! context, so the totals always match.
//!
//! [`analyze_files`] is a pure function over transcript files; [`analyze`]
//! adds what only the index knows (title, project, totals).
//!
//! Text is read only to measure its size. Nothing but sizes, counts, tool
//! names and timestamps leaves this module.

pub mod canon;
pub mod labels;
pub mod ledger;
mod read;
mod report;

pub use ledger::Calibration;
pub use report::*;

use crate::query::{self, Filter, Totals};
use crate::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Hex hash of a message as Claude Code records it in `api-request.tail`.
pub fn message_hash(role: &str, content: &serde_json::Value) -> String {
    canon::to_hex(&canon::hash(role, content))
}

/// Analysis plus what only the index knows. Serializes flat, so the JSON keeps
/// every field the app used before.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub title: Option<String>,
    pub project: Option<String>,
    /// All requests, subagents included, from the index.
    pub totals: Totals,
    #[serde(flatten)]
    pub analysis: SessionAnalysis,
}

impl std::ops::Deref for SessionDetail {
    type Target = SessionAnalysis;
    fn deref(&self) -> &SessionAnalysis {
        &self.analysis
    }
}

/// Analyzes the session's main transcript files (subagent transcripts are
/// found next to them). Unreadable files are skipped.
pub fn analyze_files(main: &[PathBuf]) -> SessionAnalysis {
    report::analyze_paths(main, true)
}

/// Analyzes one transcript on its own, as a subagent would be.
pub fn analyze_subagent_file(path: &Path) -> SessionAnalysis {
    report::analyze_paths(&[path.to_path_buf()], false)
}

pub fn analyze(conn: &Connection, session: &str) -> Result<SessionDetail> {
    if let Ok(names) = crate::desktop::mcp_names(conn) {
        labels::set_connector_names(names);
    }
    let files: Vec<(String, i64)> = {
        let mut st = conn.prepare(
            "SELECT file, MAX(subagent) FROM requests WHERE session = ?1 GROUP BY file ORDER BY MIN(ts)",
        )?;
        let rows = st.query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let main: Vec<PathBuf> = files
        .iter()
        .filter(|(_, s)| *s == 0)
        .map(|(f, _)| PathBuf::from(f))
        .collect();
    let title: Option<String> = conn
        .query_row("SELECT title FROM sessions WHERE id = ?1", [session], |r| {
            r.get(0)
        })
        .ok();
    let project: Option<String> = conn
        .query_row(
            "SELECT project FROM requests WHERE session = ?1 GROUP BY project ORDER BY COUNT(*) DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .ok();
    let totals = query::totals(
        conn,
        &Filter {
            session: Some(session.to_string()),
            ..Default::default()
        },
    )?;
    let mut analysis = analyze_files(&main);
    analysis.id = session.to_string();
    Ok(SessionDetail {
        title,
        project,
        totals,
        analysis,
    })
}
