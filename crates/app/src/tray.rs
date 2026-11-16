//! Menu bar item (plan 4.H.1). Its title shows today's tokens and the change
//! against the same weekday last week up to the same time of day. The menu
//! shows today, this week, the most recently active session and actions. It
//! is rebuilt after every `index-updated`.
//!
//! Event for the UI: `open-session` with the session id (a string), emitted
//! when the user picks the session row in the menu. The window is shown first.
//!
//! Plan-limit usage is not in local data, so the menu says so instead of
//! estimating it.

use crate::alerts::{Alerts, Notice};
use crate::live::{self, ActiveSession};
use crate::settings::SettingsState;
use crate::state::{with_index, AppState};
use jiff::Zoned;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_notification::NotificationExt;
use usage_core::{fmt, query, Filter, Overview};

const TRAY_ID: &str = "main";
pub const OPEN_SESSION: &str = "open-session";

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
    match ev.id().as_ref() {
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
    active: Option<ActiveSession>,
    notices: Vec<Notice>,
}

/// Re-read today's numbers, rebuild the menu and send due notifications.
pub fn update(app: &AppHandle) {
    let settings = app.state::<SettingsState>().get();
    let alerts = app.state::<Alerts>();
    let now = Zoned::now();
    let snap = with_index(&app.state::<AppState>().index, |i| {
        let overview = query::overview(&i.conn, &Filter::default(), &now)?;
        let day_start = now
            .start_of_day()
            .map(|z| z.timestamp().as_millisecond())
            .unwrap_or(0);
        let active = live::active_session(&i.conn, day_start)?;
        let notices = alerts.check(&i.conn, &settings, overview.today_totals.total, &now)?;
        Ok(Snapshot {
            overview,
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
    let _ = tray.set_title(Some(title(&snap.overview)));
    match build_menu(app, &snap, &now) {
        Ok(m) => {
            let _ = tray.set_menu(Some(m));
        }
        Err(e) => eprintln!("[tray] menu: {e}"),
    }
}

/// Today's API-equivalent cost and its change against last week at the same
/// time. Cost, not tokens: tokens are mostly cheap cache reads.
fn title(o: &Overview) -> String {
    let today = o.today_totals.cost_usd;
    let usd = usage_core::cost::format_usd(today);
    match fmt::change(o.same_day_last_week.cost_usd, today) {
        Some(c) => format!("{usd} {}", arrow(c)),
        None => usd,
    }
}

fn arrow(c: f64) -> String {
    if c.abs() < 0.5 {
        "±0%".into()
    } else if c > 0.0 {
        format!("↑{c:.0}%")
    } else {
        format!("↓{:.0}%", -c)
    }
}

fn signed(c: Option<f64>) -> String {
    match c {
        Some(c) if c.abs() >= 0.5 => format!("{:+.0}%", c),
        Some(_) => "±0%".into(),
        None => "no data".into(),
    }
}

fn build_menu(app: &AppHandle, s: &Snapshot, now: &Zoned) -> tauri::Result<Menu<Wry>> {
    let o = &s.overview;
    let weekday = now.strftime("%a").to_string();
    let today = format!(
        "Today: {} · {} tokens · {} vs last {weekday} to {}",
        usage_core::cost::format_usd(o.today_totals.cost_usd),
        fmt::tokens(o.today_totals.total),
        signed(fmt::change(
            o.same_day_last_week.cost_usd,
            o.today_totals.cost_usd
        )),
        o.as_of
    );
    let week = format!(
        "This week: {} · {} tokens · {} vs last week to date",
        usage_core::cost::format_usd(o.this_week.cost_usd),
        fmt::tokens(o.this_week.total),
        signed(fmt::change(
            o.last_week_to_date.cost_usd,
            o.this_week.cost_usd
        ))
    );
    let info = |id: &str, text: &str| MenuItem::with_id(app, id, text, false, None::<&str>);
    let menu = Menu::new(app)?;
    menu.append(&info("today", &today)?)?;
    menu.append(&info("week", &week)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    match &s.active {
        Some(a) => {
            let label = format!("Latest session: {}", truncate(&a.title, 48));
            menu.append(&MenuItem::with_id(
                app,
                "session",
                label,
                true,
                None::<&str>,
            )?)?;
            let mut detail = format!(
                "Context {} · {} requests",
                fmt::tokens(a.context),
                a.requests
            );
            if a.compactions_today > 0 {
                let n = a.compactions_today;
                detail += &format!(" · {n} compaction{} today", if n == 1 { "" } else { "s" });
            }
            detail += &format!(" · {}", ago(now.timestamp().as_millisecond() - a.last_ts));
            menu.append(&info("session-detail", &detail)?)?;
        }
        None => menu.append(&info("session", "No sessions yet")?)?,
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&info("limit", "Plan-limit usage is not recorded locally")?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
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

    #[test]
    fn labels() {
        assert_eq!(arrow(12.4), "↑12%");
        assert_eq!(arrow(-7.6), "↓8%");
        assert_eq!(arrow(0.2), "±0%");
        assert_eq!(signed(None), "no data");
        assert_eq!(signed(Some(-3.0)), "-3%");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(3 * 3_600_000), "3 h ago");
    }
}
