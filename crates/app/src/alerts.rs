//! Opt-in notifications (plan 4.H.3), checked after every index refresh.
//!
//! - Context: a session's latest main-thread request is above the threshold
//!   share of the context window.
//! - Cache rewrite: a request wrote a large amount to the cache after the
//!   session was idle longer than the cache lifetime (5 minutes, or 1 hour
//!   when the write used the 1 hour cache).
//! - Daily spike: today's total is above N × the median active day of the
//!   previous 4 weeks (needs at least 7 such days).
//!
//! Only requests indexed while the app runs are considered, so starting the
//! app never replays old history. Each kind is shown at most once per session
//! per cooldown; the daily spike at most once per day.

use crate::live::{self, NewRequest};
use crate::settings::Settings;
use jiff::{Span, Zoned};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;
use usage_core::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

#[derive(Default)]
pub struct Alerts(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    /// Requests with a higher rowid have not been checked yet.
    after_rowid: Option<i64>,
    /// (kind, session or day) → last shown, Unix ms.
    shown: HashMap<(&'static str, String), i64>,
    /// Median for the day it was computed for.
    median: Option<(String, Option<u64>)>,
}

const MIN: i64 = 60_000;

impl Alerts {
    /// Notices to show for what was indexed since the last call.
    pub fn check(
        &self,
        conn: &Connection,
        s: &Settings,
        today_total: u64,
        now: &Zoned,
    ) -> rusqlite::Result<Vec<Notice>> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(after) = g.after_rowid else {
            g.after_rowid = Some(live::max_rowid(conn)?);
            return Ok(vec![]);
        };
        let rows = live::new_requests(conn, after)?;
        if let Some(last) = rows.last() {
            g.after_rowid = Some(last.rowid);
        } else {
            g.after_rowid = Some(live::max_rowid(conn)?.max(after));
        }
        if !s.notifications_enabled {
            return Ok(vec![]);
        }
        let now_ms = now.timestamp().as_millisecond();
        let cooldown = s.notify_cooldown_minutes as i64 * MIN;
        let mut out = vec![];

        for (kind, session, text) in decide(&rows, s) {
            let key = (kind, session.clone());
            if g.shown.get(&key).is_some_and(|t| now_ms - t < cooldown) {
                continue;
            }
            g.shown.insert(key, now_ms);
            let title = live::session_title(conn, &session);
            out.push(text(&title));
        }

        if s.notify_daily_spike && today_total > 0 {
            let day = now.date().to_string();
            if g.median.as_ref().map(|m| &m.0) != Some(&day) {
                let start = now.start_of_day().map_err(other)?;
                let to = start.timestamp().as_millisecond();
                let from = start
                    .checked_sub(Span::new().days(28))
                    .map_err(other)?
                    .timestamp()
                    .as_millisecond();
                let off = now.offset().seconds() as i64;
                g.median = Some((day.clone(), live::median_daily_total(conn, from, to, off)?));
            }
            let median = g.median.as_ref().and_then(|m| m.1).unwrap_or(0);
            let key = ("spike", day);
            if median > 0
                && today_total as f64 > s.daily_spike_multiple * median as f64
                && !g.shown.contains_key(&key)
            {
                g.shown.insert(key, now_ms);
                out.push(Notice {
                    title: format!(
                        "Today's usage is {:.1}× a usual day",
                        today_total as f64 / median as f64
                    ),
                    body: format!(
                        "{} tokens so far, against a median of {} over the last 4 weeks. \
                         Open the app to see which sessions are spending it.",
                        fmt::tokens(today_total),
                        fmt::tokens(median)
                    ),
                });
            }
        }
        g.shown.retain(|_, t| now_ms - *t < 2 * 86_400_000);
        Ok(out)
    }
}

fn other(e: jiff::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

type Text = Box<dyn Fn(&str) -> Notice>;

/// Which per-session notices the new rows call for, before rate limiting.
fn decide(rows: &[NewRequest], s: &Settings) -> Vec<(&'static str, String, Text)> {
    let mut out: Vec<(&'static str, String, Text)> = vec![];
    if s.notify_cache_rewrite {
        for r in rows {
            let ttl = if r.cache_write_1h > 0 {
                60 * MIN
            } else {
                5 * MIN
            };
            let Some(prev) = r.prev_ts else { continue };
            let idle = r.ts - prev;
            if r.cache_write >= s.cache_rewrite_min_tokens && idle > ttl {
                let (cw, mins) = (r.cache_write, idle / MIN);
                out.push((
                    "cache",
                    r.session.clone(),
                    Box::new(move |t| Notice {
                        title: format!("Cache rewritten in “{t}”"),
                        body: format!(
                            "The session resumed after {} idle, past the cache lifetime, so this \
                             request wrote {} tokens to the cache again. Run /compact or start a \
                             new session before long breaks.",
                            idle_text(mins),
                            fmt::tokens(cw)
                        ),
                    }),
                ));
            }
        }
    }
    if s.notify_context {
        let limit = s.context_threshold_pct / 100.0 * s.context_window_tokens as f64;
        let mut latest: HashMap<&str, &NewRequest> = HashMap::new();
        for r in rows {
            latest.insert(&r.session, r);
        }
        let mut v: Vec<_> = latest.into_values().collect();
        v.sort_by_key(|r| r.rowid);
        for r in v {
            if r.context as f64 >= limit {
                let pct = r.context as f64 / s.context_window_tokens as f64 * 100.0;
                let ctx = r.context;
                out.push((
                    "context",
                    r.session.clone(),
                    Box::new(move |t| Notice {
                        title: format!("Context at {pct:.0}% in “{t}”"),
                        body: format!(
                            "The latest request sent {} tokens, and every following request \
                             re-reads all of it. Run /compact, or start a new session for \
                             unrelated work.",
                            fmt::tokens(ctx)
                        ),
                    }),
                ));
            }
        }
    }
    out
}

fn idle_text(mins: i64) -> String {
    if mins >= 120 {
        format!("{} h", mins / 60)
    } else {
        format!("{mins} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::tests::{add, db};

    fn on() -> Settings {
        Settings {
            notifications_enabled: true,
            ..Settings::default()
        }
    }

    fn at(ms: i64) -> Zoned {
        jiff::Timestamp::from_millisecond(ms)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC)
    }

    #[test]
    fn first_check_only_sets_the_mark() {
        let c = db();
        add(&c, "a", 1_000, false, 0, 900_000);
        let a = Alerts::default();
        assert!(a.check(&c, &on(), 0, &at(2_000)).unwrap().is_empty());
        assert!(a.check(&c, &on(), 0, &at(2_000)).unwrap().is_empty());
    }

    #[test]
    fn context_and_cache_rules_with_cooldown() {
        let c = db();
        add(&c, "a", 1_000, false, 0, 10);
        let a = Alerts::default();
        a.check(&c, &on(), 0, &at(1_000)).unwrap();

        // 10 minutes idle, then a 750k cache write: both rules fire.
        add(&c, "a", 601_000, false, 750_000, 0);
        let n = a.check(&c, &on(), 0, &at(601_000)).unwrap();
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(n[0].title.starts_with("Cache rewritten"));
        assert!(n[0].body.contains("10 min"));
        assert!(n[1].title.starts_with("Context at 75%"));

        // Same session again within the cooldown: nothing.
        add(&c, "a", 1_300_000, false, 760_000, 0);
        assert!(a.check(&c, &on(), 0, &at(1_300_000)).unwrap().is_empty());
    }

    #[test]
    fn off_by_default() {
        let c = db();
        let a = Alerts::default();
        a.check(&c, &Settings::default(), 0, &at(0)).unwrap();
        add(&c, "a", 1_000, false, 0, 990_000);
        assert!(a
            .check(&c, &Settings::default(), 0, &at(1_000))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn daily_spike_once_per_day() {
        let c = db();
        let day = 86_400_000;
        for d in 0..10 {
            add(&c, "a", d * day + 1, false, 0, 1_000);
        }
        let a = Alerts::default();
        let now = at(10 * day + 3_600_000);
        a.check(&c, &on(), 0, &now).unwrap();
        let n = a.check(&c, &on(), 5_000, &now).unwrap();
        assert_eq!(n.len(), 1);
        assert!(n[0].title.contains("4.9×"), "{}", n[0].title);
        assert!(a.check(&c, &on(), 6_000, &now).unwrap().is_empty());
        assert!(a
            .check(&c, &on(), 1_500, &at(11 * day + 1))
            .unwrap()
            .is_empty());
    }
}
