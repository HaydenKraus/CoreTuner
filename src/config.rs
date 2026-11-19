//! Настройки приложения и корневой конфиг (config.json).
use crate::model::{AppBinding, Profile};
use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Lang {
    #[default]
    Ru,
    En,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub autostart: bool,
    pub minimize_to_tray_on_close: bool,
    pub always_show_tray: bool,
    pub start_minimized: bool,
    pub lang: Lang,
    pub theme: ThemeMode,
    /// Интервал опроса процессов, мс (500..=10000).
    pub poll_interval_ms: u64,
    /// Повторно применять настройки, если процесс их сбросил.
    pub reapply_enforce: bool,
    /// Уведомление в трее при автоприменении.
    pub notify_on_apply: bool,
    /// Автоприменение приостановлено.
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            autostart: false,
            minimize_to_tray_on_close: true,
            always_show_tray: true,
            start_minimized: false,
            lang: Lang::Ru,
            theme: ThemeMode::System,
            poll_interval_ms: 1500,
            reapply_enforce: false,
            notify_on_apply: true,
            paused: false,
        }
    }
}

impl Settings {
    pub fn clamp(&mut self) {
        self.poll_interval_ms = self.poll_interval_ms.clamp(500, 10_000);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub settings: Settings,
    pub profiles: Vec<Profile>,
    pub bindings: Vec<AppBinding>,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: CONFIG_VERSION, settings: Settings::default(), profiles: Vec::new(), bindings: Vec::new() }
    }
}

impl Config {
    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}
