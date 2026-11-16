//! Keeps the index current while the app runs (plan 4.A.5).
//!
//! Watches `~/.claude/projects` recursively with `notify`. Changes to
//! `.jsonl` files are debounced (refresh after 1 s without changes, or at
//! most 5 s after the first one) and then run the incremental refresh. A slow
//! timer (every 2 minutes) refreshes too, in case events are missed or the
//! folder does not exist yet.
//!
//! Event for the UI:
//!
//! `index-updated`, emitted to all windows after a refresh that read at least
//! one file (and after every manual `refresh` command). Payload, camelCase:
//!
//! ```json
//! { "filesSeen": 1285, "filesRead": 2, "newRequests": 14, "errors": 0,
//!   "ms": 61, "reason": "watch" }
//! ```
//!
//! `reason` is `"startup"`, `"watch"`, `"timer"` or `"manual"`. The UI's own
//! `refresh` call already returns the stats, so it can ignore `"manual"`.
//!
//! After emitting, the menu bar item is updated and notifications are checked.

use crate::state::{with_index, AppState};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use usage_core::{paths, RefreshStats};

pub const INDEX_UPDATED: &str = "index-updated";

const QUIET: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(5);
const FALLBACK: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexUpdated {
    #[serde(flatten)]
    pub stats: RefreshStats,
    pub reason: &'static str,
}

/// Start the watcher thread. It does a first refresh right away.
pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("index-watcher".into())
        .spawn(move || run(app))
        .expect("spawn watcher thread");
}

fn run(app: AppHandle) {
    let dir = paths::projects_dir();
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("[watch] cannot create watcher: {e}; using the timer only");
            None
        }
    };
    let mut watching = false;
    let try_watch = |w: &mut Option<notify::RecommendedWatcher>| -> bool {
        let Some(w) = w.as_mut() else { return false };
        match w.watch(&dir, RecursiveMode::Recursive) {
            Ok(()) => {
                eprintln!("[watch] watching {}", dir.display());
                true
            }
            Err(e) => {
                eprintln!("[watch] cannot watch {}: {e}", dir.display());
                false
            }
        }
    };
    watching |= try_watch(&mut watcher);

    refresh_and_publish(&app, "startup");

    let mut next_timer = Instant::now() + FALLBACK;
    let mut first: Option<Instant> = None;
    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let wait = match first {
            Some(f) => (last + QUIET).min(f + MAX_WAIT).min(next_timer),
            None => next_timer,
        }
        .saturating_duration_since(now);
        match rx.recv_timeout(wait) {
            Ok(Ok(ev)) if relevant(&ev) => {
                let t = Instant::now();
                first.get_or_insert(t);
                last = t;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("[watch] error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The watcher is gone; keep the timer going.
                std::thread::sleep(next_timer.saturating_duration_since(Instant::now()));
            }
        }
        let now = Instant::now();
        if let Some(f) = first {
            if now >= last + QUIET || now >= f + MAX_WAIT {
                first = None;
                refresh_and_publish(&app, "watch");
                next_timer = Instant::now() + FALLBACK;
            }
        }
        if now >= next_timer {
            if !watching {
                watching |= try_watch(&mut watcher);
            }
            refresh_and_publish(&app, "timer");
            next_timer = Instant::now() + FALLBACK;
        }
    }
}

fn relevant(ev: &notify::Event) -> bool {
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    ) && ev.paths.iter().any(|p| is_transcript(p))
}

fn is_transcript(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "jsonl")
}

fn refresh_and_publish(app: &AppHandle, reason: &'static str) {
    let st = app.state::<AppState>();
    match with_index(&st.index, |idx| {
        idx.refresh(&paths::projects_dir(), &|_, _| {})
    }) {
        Ok(stats) => {
            if stats.files_read > 0 || reason == "startup" {
                eprintln!(
                    "[watch] {reason}: {} files read, {} new requests, {} ms",
                    stats.files_read, stats.new_requests, stats.ms
                );
                publish(app, stats, reason);
            }
        }
        Err(e) => eprintln!("[watch] refresh failed: {e}"),
    }
}

/// Emit `index-updated`, then update the menu bar and check notifications.
pub fn publish(app: &AppHandle, stats: RefreshStats, reason: &'static str) {
    let _ = app.emit(INDEX_UPDATED, IndexUpdated { stats, reason });
    crate::tray::update(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind};

    #[test]
    fn only_transcripts_count() {
        let ev = |k, p: &str| notify::Event::new(k).add_path(p.into());
        assert!(relevant(&ev(
            EventKind::Modify(ModifyKind::Any),
            "/p/a/s.jsonl"
        )));
        assert!(relevant(&ev(
            EventKind::Create(CreateKind::File),
            "/p/a/s/subagents/agent-1.jsonl"
        )));
        assert!(!relevant(&ev(
            EventKind::Modify(ModifyKind::Any),
            "/p/a/s/subagents/agent-1.meta.json"
        )));
        assert!(!relevant(&ev(
            EventKind::Access(notify::event::AccessKind::Any),
            "/p/a/s.jsonl"
        )));
    }

    #[test]
    fn payload_shape() {
        let v = serde_json::to_value(IndexUpdated {
            stats: RefreshStats::default(),
            reason: "watch",
        })
        .unwrap();
        for k in [
            "filesSeen",
            "filesRead",
            "newRequests",
            "errors",
            "ms",
            "reason",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
    }
}
