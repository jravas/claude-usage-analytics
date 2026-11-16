//! Menu bar item (plan 4.H.1).
//!
//! The title follows the `menuBar` setting. By default it compares today with
//! the user's own usual day up to the same time ("1.4× usual", see
//! `usual.rs`): on a subscription plan dollars mean nothing, and a percentage
//! against last week's same morning explodes on a small baseline. Context
//! size, weighted tokens, raw tokens, API-equivalent cost (for API users) and
//! icon only are the other choices. The menu explains the numbers in words:
//! today and this week against usual, the latest session's context and
//! compactions, cost and raw tokens, and a submenu to pick the title. It is
//! rebuilt after every `index-updated`.
//!
//! Events for the UI: `open-session` with the session id (a string), emitted
//! when the user picks the session row in the menu (the window is shown
//! first); `settings-changed` when the title mode is changed from the menu.
//!
//! Plan-limit usage is not in local data, so the menu says so instead of
//! estimating it.

use crate::alerts::{Alerts, Notice};
use crate::live::{self, ActiveSession};
use crate::settings::{self, MenuBar, Settings, SettingsState};
use crate::state::{with_index, AppState};
use crate::usual::{self, Report, Versus};
use jiff::Zoned;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_notification::NotificationExt;
use usage_core::{fmt, query, Filter, Overview};

const TRAY_ID: &str = "main";
pub const OPEN_SESSION: &str = "open-session";
pub const SETTINGS_CHANGED: &str = "settings-changed";
/// Menu ids of the title mode items: this prefix and `MenuBar::id`.
const MODE_PREFIX: &str = "menu-bar:";

/// Session id behind the menu's session row.
#[derive(Default)]
pub struct TrayState(Mutex<Option<String>>);

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "loading", "Reading transcripts…", false, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(
                app,
                "open",
                "Open Claude Usage Analytics",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
        ],
    )?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon())
        .icon_as_template(true)
        .tooltip("Claude Usage Analytics")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu)
        .build(app)?;
    Ok(())
}

fn on_menu(app: &AppHandle, ev: MenuEvent) {
    let id = ev.id().as_ref();
    if let Some(m) = id.strip_prefix(MODE_PREFIX).and_then(MenuBar::from_id) {
        set_mode(app, m);
        return;
    }
    match id {
        "open" => show_main(app),
        "session" => {
            show_main(app);
            let id = app
                .state::<TrayState>()
                .0
                .lock()
                .ok()
                .and_then(|g| g.clone());
            if let Some(id) = id {
                let _ = app.emit(OPEN_SESSION, id);
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

struct Snapshot {
    overview: Overview,
    usual: Report,
    active: Option<ActiveSession>,
    notices: Vec<Notice>,
}

/// A session counts as active in the title for this long after its latest
/// request.
const ACTIVE_MS: i64 = 30 * 60_000;

/// Re-read today's numbers, rebuild the menu and send due notifications.
pub fn update(app: &AppHandle) {
    let settings = app.state::<SettingsState>().get();
    let alerts = app.state::<Alerts>();
    let now = Zoned::now();
    let snap = with_index(&app.state::<AppState>().index, |i| {
        let overview = query::overview(&i.conn, &Filter::default(), &now)?;
        let usual = usual::report(&i.conn, &now)?;
        let day_start = now
            .start_of_day()
            .map(|z| z.timestamp().as_millisecond())
            .unwrap_or(0);
        let active = live::active_session(&i.conn, day_start)?;
        let notices = alerts.check(&i.conn, &settings, overview.today_totals.total, &now)?;
        Ok(Snapshot {
            overview,
            usual,
            active,
            notices,
        })
    });
    let snap = match snap {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[tray] {e}");
            return;
        }
    };
    for n in &snap.notices {
        eprintln!("[notify] {}", n.title);
        if let Err(e) = app
            .notification()
            .builder()
            .title(&n.title)
            .body(&n.body)
            .show()
        {
            eprintln!("[notify] failed: {e}");
        }
    }
    if let Ok(mut g) = app.state::<TrayState>().0.lock() {
        *g = snap.active.as_ref().map(|a| a.id.clone());
    }
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let t = title(settings.menu_bar, &snap, now.timestamp().as_millisecond());
    let lines = menu_lines(&snap, &settings, &now);
    if cfg!(debug_assertions) {
        eprintln!("[tray] title: {:?}", t.as_deref().unwrap_or(""));
        for l in lines.iter().filter(|l| l.id != "-") {
            eprintln!("[tray] menu: {}", l.text);
        }
    }
    let _ = tray.set_title(t);
    match build_menu(app, &lines, settings.menu_bar) {
        Ok(m) => {
            let _ = tray.set_menu(Some(m));
        }
        Err(e) => eprintln!("[tray] menu: {e}"),
    }
}

/// A title mode picked from the menu: save it and redraw.
fn set_mode(app: &AppHandle, m: MenuBar) {
    let st = app.state::<SettingsState>();
    let mut s = st.get();
    if s.menu_bar == m {
        return;
    }
    s.menu_bar = m;
    if let Err(e) = settings::save(app, &s) {
        eprintln!("[settings] {e}");
    }
    if let Ok(mut g) = st.0.lock() {
        *g = s;
    }
    let _ = app.emit(SETTINGS_CHANGED, ());
    update(app);
}

/// Text next to the menu bar icon, or None for the icon alone.
fn title(mode: MenuBar, s: &Snapshot, now_ms: i64) -> Option<String> {
    let o = &s.overview;
    let weighted = || tokens_f(o.today_totals.weighted);
    let t = match mode {
        MenuBar::Usual => match s.usual.day {
            Versus::Ratio { ratio, .. } => format!("{} usual", usual::multiple(ratio)),
            // Too little history, or a usual day has barely started by now:
            // no multiple against a tiny baseline, just today's amount.
            _ if o.today_totals.weighted > 0.0 => weighted(),
            _ => String::new(),
        },
        MenuBar::Context => match &s.active {
            Some(a) if now_ms - a.last_ts <= ACTIVE_MS => {
                format!("{} ctx", fmt::tokens(a.context))
            }
            _ => String::new(),
        },
        MenuBar::Weighted => weighted(),
        MenuBar::Tokens => fmt::tokens(o.today_totals.total),
        MenuBar::Cost => usage_core::cost::format_usd(o.today_totals.cost_usd),
        MenuBar::Icon => String::new(),
    };
    (!t.is_empty()).then_some(t)
}

fn tokens_f(v: f64) -> String {
    fmt::tokens(v.max(0.0).round() as u64)
}

/// One menu row. "-" is a separator and "session" the clickable session row;
/// every other row is text only.
struct Line {
    id: &'static str,
    text: String,
}

/// Headline and detail rows for a period against its usual.
fn versus_lines(
    name: &str,
    usual_name: &str,
    by: &str,
    amount: f64,
    v: Versus,
    periods: &str,
) -> (String, String) {
    let amt = tokens_f(amount);
    match v {
        Versus::Ratio {
            ratio,
            usual_so_far,
            usual_full,
        } => (
            format!(
                "{name}: {} a usual {usual_name} by {by}",
                usual::multiple(ratio)
            ),
            format!(
                "{amt} weighted tokens so far; a usual {usual_name} has {} by now, {} in all",
                tokens_f(usual_so_far),
                tokens_f(usual_full)
            ),
        ),
        Versus::Early { usual_full } => (
            format!("{name}: {amt} weighted tokens so far"),
            format!(
                "A usual {usual_name} has barely started by {by} ({} in all)",
                tokens_f(usual_full)
            ),
        ),
        Versus::TooLittleHistory { have, need } => (
            format!("{name}: {amt} weighted tokens so far"),
            format!(
                "Not enough history for a usual {usual_name} yet \
                 ({have} of {need} {periods} with use in the last 4 weeks)"
            ),
        ),
    }
}

fn menu_lines(s: &Snapshot, settings: &Settings, now: &Zoned) -> Vec<Line> {
    let o = &s.overview;
    let u = &s.usual;
    let line = |id, text: String| Line { id, text };
    let now_ms = now.timestamp().as_millisecond();
    let mut v = vec![];

    let (kind, kinds) = if u.weekend {
        ("weekend day", "weekend days")
    } else {
        ("weekday", "weekdays")
    };
    let (a, b) = versus_lines("Today", kind, &o.as_of, u.today, u.day, kinds);
    v.push(line("today", a));
    v.push(line("today-detail", b));
    let by = format!("{} {}", now.strftime("%a"), o.as_of);
    let (a, b) = versus_lines("This week", "week", &by, u.week, u.week_vs, "weeks");
    v.push(line("week", a));
    v.push(line("week-detail", b));
    v.push(line(
        "weighted-help",
        "Weighted tokens count each token by its price, so cheap cache reads count little".into(),
    ));
    v.push(line("-", String::new()));

    match &s.active {
        Some(a) => {
            let head = if now_ms - a.last_ts <= ACTIVE_MS {
                "Active session"
            } else {
                "Latest session"
            };
            v.push(line(
                "session",
                format!("{head}: {}", truncate(&a.title, 48)),
            ));
            let window = settings.context_window_tokens.max(1);
            let mut d = format!(
                "Context {} ({:.0}% of a {} window) · {} requests",
                fmt::tokens(a.context),
                a.context as f64 / window as f64 * 100.0,
                fmt::tokens(window),
                a.requests
            );
            match a.compactions_today {
                0 => d += " · not compacted today",
                n => d += &format!(" · compacted {n}× today"),
            }
            d += &format!(" · {}", ago(now_ms - a.last_ts));
            v.push(line("session-detail", d));
        }
        None => v.push(line("no-session", "No sessions yet".into())),
    }
    v.push(line("-", String::new()));

    let t = &o.today_totals;
    v.push(line(
        "cost",
        format!(
            "API-equivalent cost: {} today · {} this week (not what a plan bills)",
            usage_core::cost::format_usd(t.cost_usd),
            usage_core::cost::format_usd(o.this_week.cost_usd)
        ),
    ));
    let reads = if t.total > 0 {
        t.cache_read as f64 / t.total as f64 * 100.0
    } else {
        0.0
    };
    v.push(line(
        "tokens",
        format!(
            "Raw tokens: {} today, {reads:.0}% of them cache reads",
            fmt::tokens(t.total)
        ),
    ));
    v.push(line("-", String::new()));
    v.push(line(
        "limit",
        "Plan-limit usage is not in local data, so it is not shown here".into(),
    ));
    v
}

fn build_menu(app: &AppHandle, lines: &[Line], mode: MenuBar) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    for l in lines {
        if l.id == "-" {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        } else {
            let clickable = l.id == "session";
            menu.append(&MenuItem::with_id(
                app,
                l.id,
                &l.text,
                clickable,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let modes = Submenu::with_id(app, "menu-bar", "Menu bar shows", true)?;
    for m in MenuBar::ALL {
        modes.append(&CheckMenuItem::with_id(
            app,
            format!("{MODE_PREFIX}{}", m.id()),
            m.label(),
            true,
            m == mode,
            None::<&str>,
        )?)?;
    }
    menu.append(&modes)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open Claude Usage Analytics",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

fn ago(ms: i64) -> String {
    let m = ms.max(0) / 60_000;
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        60..=2879 => format!("{} h ago", m / 60),
        _ => format!("{} days ago", m / 1440),
    }
}

/// A 36 × 36 template image (18 pt at 2×): three rising bars.
fn icon() -> Image<'static> {
    const N: usize = 36;
    let mut px = vec![0u8; N * N * 4];
    let bars = [(6, 13, 22), (15, 22, 14), (24, 31, 6)];
    for (x0, x1, top) in bars {
        for y in top..31 {
            for x in x0..x1 {
                let i = (y * N + x) * 4;
                px[i + 3] = 255;
            }
        }
    }
    Image::new_owned(px, N as u32, N as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use usage_core::query::Totals;

    fn snap(day: Versus, weighted: f64) -> Snapshot {
        let o = Overview {
            today: "2026-11-16".into(),
            as_of: "14:05".into(),
            today_totals: Totals {
                weighted,
                total: 12_000_000,
                cost_usd: 68.53,
                ..Default::default()
            },
            same_day_last_week: Totals::default(),
            this_week: Totals::default(),
            last_week_to_date: Totals::default(),
            last_week: Totals::default(),
            week_start: "2026-11-16".into(),
        };
        Snapshot {
            overview: o,
            usual: Report {
                today: weighted,
                day,
                week: weighted,
                week_vs: Versus::TooLittleHistory { have: 1, need: 3 },
                weekend: false,
            },
            active: Some(ActiveSession {
                id: "s".into(),
                title: "Fix the tray".into(),
                project: "/p".into(),
                last_ts: 0,
                context: 412_000,
                requests: 120,
                compactions_today: 1,
            }),
            notices: vec![],
        }
    }

    #[test]
    fn titles() {
        let ratio = Versus::Ratio {
            ratio: 1.42,
            usual_so_far: 1e6,
            usual_full: 3e6,
        };
        let s = snap(ratio, 1_420_000.0);
        assert_eq!(title(MenuBar::Usual, &s, 0).as_deref(), Some("1.4× usual"));
        assert_eq!(title(MenuBar::Weighted, &s, 0).as_deref(), Some("1.4M"));
        assert_eq!(title(MenuBar::Tokens, &s, 0).as_deref(), Some("12.0M"));
        assert_eq!(title(MenuBar::Cost, &s, 0).as_deref(), Some("$68.53"));
        assert_eq!(title(MenuBar::Icon, &s, 0), None);
        assert_eq!(
            title(MenuBar::Context, &s, 60_000).as_deref(),
            Some("412.0k ctx")
        );
        // An old session is not "active".
        assert_eq!(title(MenuBar::Context, &s, ACTIVE_MS + 1), None);

        // No multiple without enough history: today's amount instead.
        let s = snap(Versus::TooLittleHistory { have: 2, need: 5 }, 800_000.0);
        assert_eq!(title(MenuBar::Usual, &s, 0).as_deref(), Some("800.0k"));
        let s = snap(Versus::Early { usual_full: 3e6 }, 0.0);
        assert_eq!(title(MenuBar::Usual, &s, 0), None);
    }

    #[test]
    fn menu_words() {
        let (a, b) = versus_lines(
            "Today",
            "weekday",
            "14:05",
            1_420_000.0,
            Versus::Ratio {
                ratio: 1.42,
                usual_so_far: 1e6,
                usual_full: 3e6,
            },
            "weekdays",
        );
        assert_eq!(a, "Today: 1.4× a usual weekday by 14:05");
        assert_eq!(
            b,
            "1.4M weighted tokens so far; a usual weekday has 1.0M by now, 3.0M in all"
        );
        let (_, b) = versus_lines(
            "This week",
            "week",
            "Mon 09:00",
            10.0,
            Versus::TooLittleHistory { have: 1, need: 3 },
            "weeks",
        );
        assert!(b.contains("1 of 3 weeks"), "{b}");
        let s = snap(Versus::Early { usual_full: 3e6 }, 0.0);
        let now = jiff::Timestamp::from_millisecond(60_000)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC);
        let lines = menu_lines(&s, &Settings::default(), &now);
        let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(text.contains(&"Active session: Fix the tray"), "{text:?}");
        assert!(
            text.iter()
                .any(|t| t.starts_with("Context 412.0k (41% of a 1.0M window)")
                    && t.contains("compacted 1× today")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.starts_with("Plan-limit")));
    }

    #[test]
    fn labels() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(3 * 3_600_000), "3 h ago");
    }
}
