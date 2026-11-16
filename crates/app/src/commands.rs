use crate::state::{with_index, AppState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use usage_core::query::{self, DayRow};
use usage_core::{paths, Dim, Filter, Group, Overview, RefreshStats};

#[tauri::command]
pub fn version() -> String {
    usage_core::version().to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read new transcript lines into the index. Emits `refresh-progress` with
/// `{ done, total }`, at most every 100 ms, then `index-updated` (see
/// `watcher.rs`) with reason `"manual"`.
#[tauri::command]
pub async fn refresh(app: AppHandle, st: State<'_, AppState>) -> Result<RefreshStats, String> {
    let slot = st.index.clone();
    let app2 = app.clone();
    let stats = tauri::async_runtime::spawn_blocking(move || {
        let last = AtomicU64::new(0);
        with_index(&slot, |idx| {
            idx.refresh(&paths::projects_dir(), &|done, total| {
                let t = now_ms();
                if done == total || t.saturating_sub(last.load(Ordering::Relaxed)) > 100 {
                    last.store(t, Ordering::Relaxed);
                    let _ = app.emit(
                        "refresh-progress",
                        serde_json::json!({ "done": done, "total": total }),
                    );
                }
            })
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    // Tell other listeners (menu bar, notifications) off the command's path.
    let published = stats.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::watcher::publish(&app2, published, "manual")
    });
    Ok(stats)
}

#[tauri::command]
pub async fn overview(st: State<'_, AppState>, filter: Filter) -> Result<Overview, String> {
    with_index(&st.index, |i| {
        query::overview(&i.conn, &filter, &jiff::Zoned::now())
    })
}

#[tauri::command]
pub async fn daily(st: State<'_, AppState>, filter: Filter) -> Result<Vec<DayRow>, String> {
    with_index(&st.index, |i| query::daily(&i.conn, &filter))
}

#[tauri::command]
pub async fn breakdown(
    st: State<'_, AppState>,
    filter: Filter,
    dim: Dim,
    limit: Option<usize>,
) -> Result<Vec<Group>, String> {
    with_index(&st.index, |i| {
        query::breakdown(&i.conn, &filter, dim, limit.unwrap_or(200))
    })
}

#[tauri::command]
pub async fn session(
    st: State<'_, AppState>,
    id: String,
) -> Result<usage_core::session::SessionDetail, String> {
    with_index(&st.index, |i| usage_core::session::analyze(&i.conn, &id))
}

#[tauri::command]
pub async fn models(st: State<'_, AppState>) -> Result<Vec<String>, String> {
    with_index(&st.index, |i| query::models(&i.conn))
}

// ---------------------------------------------------------------------------
// Session detail, cached: analysing a large transcript takes a moment, and
// the page re-renders on every index update.
// ---------------------------------------------------------------------------

type SessionCache = std::collections::HashMap<String, (String, usage_core::session::SessionDetail)>;

fn session_cache() -> &'static std::sync::Mutex<SessionCache> {
    static C: std::sync::OnceLock<std::sync::Mutex<SessionCache>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Changes whenever the session gets a new request.
fn session_stamp(conn: &rusqlite::Connection, id: &str) -> usage_core::Result<String> {
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(ts), 0) FROM requests WHERE session = ?1",
        [id],
        |r| Ok(format!("{}:{}", r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )?)
}

#[tauri::command]
pub async fn session_cached(
    st: State<'_, AppState>,
    id: String,
) -> Result<usage_core::session::SessionDetail, String> {
    let slot = st.index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        with_index(&slot, |i| {
            let stamp = session_stamp(&i.conn, &id)?;
            if let Some((s, d)) = session_cache()
                .lock()
                .ok()
                .and_then(|c| c.get(&id).cloned())
            {
                if s == stamp {
                    return Ok(d);
                }
            }
            let d = usage_core::session::analyze(&i.conn, &id)?;
            if let Ok(mut c) = session_cache().lock() {
                if c.len() > 32 {
                    c.clear();
                }
                c.insert(id.clone(), (stamp, d.clone()));
            }
            Ok(d)
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Insights, week-over-week change, prices
// ---------------------------------------------------------------------------

/// Ranked insights for `filter`. Session summaries are cached in the index,
/// so this runs on the writer connection.
#[tauri::command]
pub async fn insights(
    st: State<'_, AppState>,
    filter: Filter,
) -> Result<usage_core::insights::Report, String> {
    let slot = st.index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::state::with_writer(&slot, |i| {
            usage_core::insights::report(
                &i.conn,
                &filter,
                &jiff::Zoned::now(),
                &usage_core::insights::Thresholds::default(),
            )
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// This week against last week to the same time, explained.
#[tauri::command]
pub async fn change(
    st: State<'_, AppState>,
    filter: Filter,
) -> Result<usage_core::change::Change, String> {
    with_index(&st.index, |i| {
        usage_core::change::week_over_week(&i.conn, &filter, &jiff::Zoned::now())
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceInfo {
    source: String,
    retrieved: String,
    overridden: bool,
}

#[tauri::command]
pub fn price_info() -> Result<PriceInfo, String> {
    let t = usage_core::cost::PriceTable::load().map_err(|e| e.to_string())?;
    Ok(PriceInfo {
        source: t.source.clone(),
        retrieved: t.retrieved.clone(),
        overridden: usage_core::cost::override_path().exists(),
    })
}

// ---------------------------------------------------------------------------
// Sharing: export a summary, compare with other people's
// ---------------------------------------------------------------------------

fn own_summary(
    conn: &rusqlite::Connection,
    name: Option<String>,
) -> usage_core::Result<usage_core::share::Summary> {
    let (from, to) = usage_core::share::default_range(usage_core::query::today(), 4);
    usage_core::share::export_summary(
        conn,
        &usage_core::share::ExportOptions {
            from,
            to,
            name,
            ..Default::default()
        },
    )
}

/// The summary that "Export" would write, for the preview.
#[tauri::command]
pub async fn summary_preview(
    st: State<'_, AppState>,
    name: Option<String>,
) -> Result<String, String> {
    with_index(&st.index, |i| {
        Ok(usage_core::share::to_json(&own_summary(&i.conn, name)?))
    })
}

/// Ask where to save, then write the summary. Returns the path, or None
/// when the user cancelled.
#[tauri::command]
pub async fn export_summary(
    app: AppHandle,
    st: State<'_, AppState>,
    name: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let json = with_index(&st.index, |i| {
        Ok(usage_core::share::to_json(&own_summary(&i.conn, name)?))
    })?;
    let file = format!("claude-usage-{}.json", usage_core::query::today());
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Usage summary", &["json"])
            .set_file_name(&file)
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(p) = picked else { return Ok(None) };
    let p = p.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&p, json).map_err(|e| e.to_string())?;
    Ok(Some(p.to_string_lossy().into_owned()))
}

/// Ask for other people's summary files and line them up with yours, week by
/// week. Returns None when the user cancelled.
#[tauri::command]
pub async fn compare(
    app: AppHandle,
    st: State<'_, AppState>,
    family: Option<String>,
    paths: Option<Vec<String>>,
) -> Result<Option<(Vec<String>, usage_core::share::Comparison)>, String> {
    use tauri_plugin_dialog::DialogExt;
    let paths: Vec<std::path::PathBuf> = match paths {
        Some(p) if !p.is_empty() => p.into_iter().map(Into::into).collect(),
        _ => {
            let picked = tauri::async_runtime::spawn_blocking(move || {
                app.dialog()
                    .file()
                    .add_filter("Usage summary", &["json"])
                    .blocking_pick_files()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(files) = picked else { return Ok(None) };
            files
                .into_iter()
                .filter_map(|f| f.into_path().ok())
                .collect()
        }
    };
    let others = usage_core::share::load_summaries(&paths).map_err(|e| e.to_string())?;
    let mine = with_index(&st.index, |i| own_summary(&i.conn, Some("You".into())))?;
    let mut people = vec![usage_core::share::Loaded {
        label: "You".into(),
        path: Default::default(),
        summary: mine,
    }];
    people.extend(others);
    let weeks = usage_core::share::default_weeks(&people, 5);
    let cmp = usage_core::share::compare(
        &people,
        &weeks,
        &usage_core::share::CompareOptions { family },
    )
    .map_err(|e| e.to_string())?;
    Ok(Some((
        paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        cmp,
    )))
}
