//! Claude Code token usage, read from the transcripts Claude Code writes under
//! `~/.claude/projects`, kept in a local SQLite index and queried by day,
//! model, project, session and more.
//!
//! Only metadata is read: token counts, model, effort, timestamps, working
//! directory and session titles. Message text is never stored.

pub mod change;
pub mod cost;
pub mod desktop;
pub mod fmt;
pub mod index;
pub mod insights;
pub mod parse;
pub mod paths;
pub mod query;
pub mod session;
pub mod share;

pub use index::{Index, RefreshStats};
pub use query::{Dim, Filter, Group, Overview, Totals};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
