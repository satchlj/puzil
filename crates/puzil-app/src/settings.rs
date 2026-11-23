//! Puzil's own settings: app-wide defaults, which each server can override. Settings that
//! Zulip owns (notifications, privacy, profile) live on the server and are edited through
//! its API instead (see `ui::settings_view`).
//!
//! Stored in `<data dir>/settings.json`: `{ "app": {...}, "servers": { "<realm>": {...} } }`.
//! Every field is optional; the effective value is the server's, else the app's, else the
//! built-in default.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::theme::Palette;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    System,
    Reduced,
    Full,
}

/// One layer of settings. `None` means "not set here".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub palette: Option<String>,
    pub mode: Option<Mode>,
    pub clock_24h: Option<bool>,
    pub motion: Option<Motion>,
    pub dms_shown: Option<usize>,
    pub active_days: Option<u32>,
    pub show_muted_channels: Option<bool>,
    pub auto_read: Option<bool>,
    pub group_minutes: Option<u32>,
    /// The welcome (theme choice) has been shown. App layer only.
    pub onboarded: Option<bool>,
    /// The server that was showing last (its realm URL). App layer only.
    pub last_server: Option<String>,
}

/// Every setting resolved for one server.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub palette: Palette,
    pub mode: Mode,
    pub clock_24h: bool,
    pub motion: Motion,
    pub dms_shown: usize,
    pub active_days: u32,
    pub show_muted_channels: bool,
    pub auto_read: bool,
    pub group_minutes: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    app: Prefs,
    servers: BTreeMap<String, Prefs>,
}

pub struct Settings {
    path: PathBuf,
    file: File,
}

/// The app's one `Settings`, reachable from anywhere.
pub struct SettingsHandle(pub Entity<Settings>);
impl Global for SettingsHandle {}

/// Which layer a change goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    App,
    Server(String),
}

static CLOCK_24H: AtomicBool = AtomicBool::new(false);

/// Whether times show as 24-hour (read by `ui::fmt`).
pub fn clock_24h() -> bool {
    CLOCK_24H.load(Ordering::Relaxed)
}

pub fn init(cx: &mut App) {
    let path = crate::accounts::data_dir().join("settings.json");
    let file: File = std::fs::read_to_string(&path).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    // Environment overrides from before Settings existed still win, for testing.
    let entity = cx.new(|_| Settings { path, file });
    cx.set_global(SettingsHandle(entity));
}

pub fn handle(cx: &App) -> Entity<Settings> {
    cx.global::<SettingsHandle>().0.clone()
}

/// The effective settings for `realm` (or the app defaults with `None`).
pub fn effective(cx: &App, realm: Option<&str>) -> Effective {
    handle(cx).read(cx).resolve(realm)
}

impl Settings {
    pub fn layer(&self, scope: &Scope) -> Prefs {
        match scope {
            Scope::App => self.file.app.clone(),
            Scope::Server(r) => self.file.servers.get(r).cloned().unwrap_or_default(),
        }
    }

    pub fn resolve(&self, realm: Option<&str>) -> Effective {
        let s = realm.and_then(|r| self.file.servers.get(r)).cloned().unwrap_or_default();
        let a = &self.file.app;
        macro_rules! pick {
            ($f:ident, $d:expr) => {
                s.$f.clone().or_else(|| a.$f.clone()).unwrap_or($d)
            };
        }
        let env_palette = std::env::var("PUZIL_PALETTE").ok();
        let palette = s
            .palette
            .clone()
            .or_else(|| a.palette.clone())
            .or(env_palette)
            .and_then(|p| Palette::from_name(&p))
            .unwrap_or(Palette::Neutral);
        let env_mode = match std::env::var("PUZIL_THEME").as_deref() {
            Ok("dark") => Some(Mode::Dark),
            Ok("light") => Some(Mode::Light),
            _ => None,
        };
        Effective {
            palette,
            mode: s.mode.or(a.mode).or(env_mode).unwrap_or(Mode::System),
            clock_24h: pick!(clock_24h, false),
            motion: pick!(motion, Motion::System),
            dms_shown: pick!(dms_shown, 3),
            active_days: pick!(active_days, 7),
            show_muted_channels: pick!(show_muted_channels, true),
            auto_read: pick!(auto_read, std::env::var_os("PUZIL_NO_AUTO_READ").is_none()),
            group_minutes: pick!(group_minutes, 7),
        }
    }

    /// Changes one layer and saves.
    pub fn update(&mut self, scope: &Scope, f: impl FnOnce(&mut Prefs), cx: &mut Context<Self>) {
        let layer = match scope {
            Scope::App => &mut self.file.app,
            Scope::Server(r) => self.file.servers.entry(r.clone()).or_default(),
        };
        f(layer);
        if let Scope::Server(r) = scope
            && self.file.servers.get(r).is_some_and(|p| *p == Prefs::default())
        {
            self.file.servers.remove(r);
        }
        match serde_json::to_string_pretty(&self.file) {
            Ok(j) => {
                if let Err(e) = std::fs::write(&self.path, j) {
                    tracing::warn!("saving settings: {e}");
                }
            }
            Err(e) => tracing::warn!("saving settings: {e}"),
        }
        cx.notify();
    }
}

/// Applies what isn't read on demand: the clock and the motion preference.
pub fn apply_globals(e: &Effective, cx: &mut App) {
    CLOCK_24H.store(e.clock_24h, Ordering::Relaxed);
    match e.motion {
        Motion::Reduced => cx.set_reduce_motion(true),
        Motion::Full => cx.set_reduce_motion(false),
        Motion::System => {}
    }
}
