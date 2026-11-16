use std::path::PathBuf;

/// Bundle identifier of the app. The CLI uses the same data folder, so both
/// share one index.
pub const APP_ID: &str = "digital.prototyp.claude-usage-analytics";

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
}

pub fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

/// Same folder Tauri's `app_data_dir()` resolves to on macOS.
pub fn data_dir() -> PathBuf {
    home().join("Library/Application Support").join(APP_ID)
}

/// `$USAGE_INDEX_PATH`, or `index.sqlite` in the data folder. The override
/// lets a development build use a copy instead of the shared index.
pub fn index_path() -> PathBuf {
    std::env::var_os("USAGE_INDEX_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("index.sqlite"))
}

/// Claude Desktop's data folder.
pub fn desktop_dir() -> PathBuf {
    home().join("Library/Application Support/Claude")
}

/// Claude Desktop's Claude Code session index:
/// `<this>/<account>/<session>/local_*.json`.
pub fn desktop_sessions_dir() -> PathBuf {
    desktop_dir().join("claude-code-sessions")
}
