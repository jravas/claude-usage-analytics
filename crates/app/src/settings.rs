//! App settings, kept as `settings.json` in the app data folder
//! (`~/Library/Application Support/digital.prototyp.claude-usage-analytics/`).
//!
//! Commands for the UI:
//! - `get_settings() -> Settings`
//! - `set_settings({ settings: Settings }) -> Settings` validates, saves,
//!   applies launch-at-login and returns what was stored.
//!
//! Every field has a default, so a missing or older file still loads. All
//! notifications are off until `notificationsEnabled` is turned on.
//! `menuBar` is one of `usual` (default), `context`, `weighted`, `tokens`,
//! `cost`, `icon`; the menu bar menu can change it too.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Master switch for every notification below. Off by default.
    pub notifications_enabled: bool,

    /// Latest request's context above `context_threshold_pct` of
    /// `context_window_tokens`.
    pub notify_context: bool,
    pub context_threshold_pct: f64,
    pub context_window_tokens: u64,

    /// A request that wrote at least `cache_rewrite_min_tokens` to the cache
    /// after the session sat idle longer than the cache lifetime.
    pub notify_cache_rewrite: bool,
    pub cache_rewrite_min_tokens: u64,

    /// Today's total above `daily_spike_multiple` × the median day of the
    /// last 4 weeks.
    pub notify_daily_spike: bool,
    pub daily_spike_multiple: f64,

    /// The same kind of notification is shown at most once per session in
    /// this many minutes (daily spike: once per day).
    pub notify_cooldown_minutes: u64,

    /// Start the app (in the menu bar, window hidden) at login.
    pub launch_at_login: bool,

    /// What the menu bar item shows next to its icon. An unknown value (from
    /// a newer version) reads as the default rather than resetting the file.
    #[serde(deserialize_with = "lenient_menu_bar")]
    pub menu_bar: MenuBar,
}

/// What the menu bar title shows. Relative to the user's own usual day by
/// default: on a subscription plan dollars mean nothing, and plan-limit usage
/// is not in local data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuBar {
    /// Today as a multiple of a usual day up to this time, e.g. "1.4× usual".
    #[default]
    Usual,
    /// Context size of the most recently active session.
    Context,
    /// Today's price-weighted tokens.
    Weighted,
    /// Today's raw tokens (mostly cache reads).
    Tokens,
    /// Today's API-equivalent cost, for API users.
    Cost,
    /// No text, icon only.
    Icon,
}

fn lenient_menu_bar<'de, D: serde::Deserializer<'de>>(d: D) -> Result<MenuBar, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_str().and_then(MenuBar::from_id).unwrap_or_default())
}

impl MenuBar {
    pub const ALL: [MenuBar; 6] = [
        MenuBar::Usual,
        MenuBar::Context,
        MenuBar::Weighted,
        MenuBar::Tokens,
        MenuBar::Cost,
        MenuBar::Icon,
    ];

    pub fn id(self) -> &'static str {
        match self {
            MenuBar::Usual => "usual",
            MenuBar::Context => "context",
            MenuBar::Weighted => "weighted",
            MenuBar::Tokens => "tokens",
            MenuBar::Cost => "cost",
            MenuBar::Icon => "icon",
        }
    }

    pub fn from_id(s: &str) -> Option<MenuBar> {
        MenuBar::ALL.into_iter().find(|m| m.id() == s)
    }

    /// Label in the menu and on the Settings page.
    pub fn label(self) -> &'static str {
        match self {
            MenuBar::Usual => "Today compared with a usual day",
            MenuBar::Context => "Active session's context size",
            MenuBar::Weighted => "Today's weighted tokens",
            MenuBar::Tokens => "Today's raw tokens",
            MenuBar::Cost => "Today's API-equivalent cost",
            MenuBar::Icon => "Icon only",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            notifications_enabled: false,
            notify_context: true,
            context_threshold_pct: 70.0,
            context_window_tokens: 1_000_000,
            notify_cache_rewrite: true,
            cache_rewrite_min_tokens: 100_000,
            notify_daily_spike: true,
            daily_spike_multiple: 2.0,
            notify_cooldown_minutes: 60,
            launch_at_login: false,
            menu_bar: MenuBar::Usual,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<(), String> {
        if !(1.0..=100.0).contains(&self.context_threshold_pct) {
            return Err("Context threshold must be between 1 and 100%.".into());
        }
        if self.context_window_tokens < 1_000 {
            return Err("Context window must be at least 1,000 tokens.".into());
        }
        if self.daily_spike_multiple < 1.0 {
            return Err("Daily multiple must be at least 1.".into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct SettingsState(pub Mutex<Settings>);

impl SettingsState {
    pub fn get(&self) -> Settings {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

/// Read the settings file; defaults when it is missing or unreadable.
pub fn load(app: &AppHandle) -> Settings {
    path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = path(app).ok_or("no app data folder")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// Make the login item match `want`. Errors are returned, not fatal.
pub fn apply_autostart(app: &AppHandle, want: bool) -> Result<(), String> {
    let al = app.autolaunch();
    let is = al.is_enabled().unwrap_or(false);
    match (want, is) {
        (true, false) => al.enable().map_err(|e| e.to_string()),
        (false, true) => al.disable().map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

#[tauri::command]
pub fn get_settings(app: AppHandle, st: State<'_, SettingsState>) -> Settings {
    let mut s = st.get();
    // The login item can be removed in System Settings; report what is real.
    if let Ok(on) = app.autolaunch().is_enabled() {
        s.launch_at_login = on;
    }
    s
}

#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    st: State<'_, SettingsState>,
    settings: Settings,
) -> Result<Settings, String> {
    settings.validate()?;
    apply_autostart(&app, settings.launch_at_login)?;
    save(&app, &settings)?;
    let menu_bar_changed = st.get().menu_bar != settings.menu_bar;
    if let Ok(mut g) = st.0.lock() {
        *g = settings.clone();
    }
    if menu_bar_changed {
        crate::tray::update(&app);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_missing_fields() {
        let s: Settings = serde_json::from_str(r#"{"notificationsEnabled":true}"#).unwrap();
        assert!(s.notifications_enabled);
        assert_eq!(s.context_threshold_pct, 70.0);
        assert!(!Settings::default().notifications_enabled);
        assert!(Settings::default().validate().is_ok());
        assert_eq!(s.menu_bar, MenuBar::Usual);
    }

    #[test]
    fn menu_bar_values() {
        let s: Settings = serde_json::from_str(r#"{"menuBar":"cost"}"#).unwrap();
        assert_eq!(s.menu_bar, MenuBar::Cost);
        let s: Settings = serde_json::from_str(r#"{"menuBar":"gauge"}"#).unwrap();
        assert_eq!(s.menu_bar, MenuBar::Usual);
        assert_eq!(
            serde_json::to_value(MenuBar::Context).unwrap(),
            serde_json::json!("context")
        );
        for m in MenuBar::ALL {
            assert_eq!(MenuBar::from_id(m.id()), Some(m));
        }
    }
}
