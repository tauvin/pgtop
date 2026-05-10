//! Конфиг и резолвинг параметров запуска.
//!
//! Layered loading (lowest → highest priority):
//! 1. Hardcoded defaults (`DEFAULT_DSN`, все флаги off).
//! 2. Profile из TOML-файла (`~/.config/pgtop/config.toml`).
//! 3. `DATABASE_URL` env (legacy/CI-friendly).
//! 4. CLI-флаги (`--dsn`, `--read-only`, `--allow-actions`).
//!
//! `figment` используется для file-loading с хорошими ошибками (file:line). Для
//! env+CLI — ручная логика, потому что nested HashMap из figment-Env требует
//! awkward `PGTOP_PROFILES__LOCAL__DSN` синтаксиса; проще переопределять
//! resolved-поля сразу.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use figment::{
    Figment,
    providers::{Format, Toml},
};
use serde::Deserialize;

use crate::theme::Theme;

/// Default DSN если ни профиль, ни env, ни CLI ничего не задали.
pub const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

/// Top-level конфиг — то, что лежит в `~/.config/pgtop/config.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Имя профиля для использования если CLI не передал явный.
    /// `None` → используем DSN из env / CLI / `DEFAULT_DSN`.
    pub default_profile: Option<String>,

    /// `[profiles.<name>]`-секции из TOML.
    /// `#[serde(default)]` → пустой HashMap при отсутствии секции.
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,

    /// Phase 7: UI-настройки — пока только тема.
    #[serde(default)]
    pub ui: UiConfig,

    /// Phase 7: интервалы опроса по collector'ам, в секундах.
    #[serde(default)]
    pub intervals: IntervalsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UiConfig {
    /// Имя темы: `"dark"` (default) или `"light"`. Unknown name → fallback
    /// на `dark` без ошибки.
    pub theme: Option<String>,
}

/// Интервалы опроса в **секундах**. None в любом поле = использовать
/// hardcoded дефолт. Полная overrideability на per-collector basis.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IntervalsConfig {
    pub activity: Option<u64>,
    pub locks: Option<u64>,
    pub top_queries: Option<u64>,
    pub replication: Option<u64>,
    pub stats: Option<u64>,
}

/// Resolved intervals — `Duration` готовые к передаче collector'ам.
/// Defaults: activity/locks/stats = 1s, replication = 5s, top_queries = 10s.
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    pub activity: Duration,
    pub locks: Duration,
    pub top_queries: Duration,
    pub replication: Duration,
    pub stats: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            activity: Duration::from_secs(1),
            locks: Duration::from_secs(1),
            top_queries: Duration::from_secs(10),
            replication: Duration::from_secs(5),
            stats: Duration::from_secs(1),
        }
    }
}

impl Intervals {
    /// Применить config-overrides поверх дефолтов. Каждое `Some(n)` →
    /// `Duration::from_secs(n)`; `None` → дефолт остаётся.
    fn from_config(cfg: &IntervalsConfig) -> Self {
        let d = Self::default();
        Self {
            activity: cfg.activity.map(Duration::from_secs).unwrap_or(d.activity),
            locks: cfg.locks.map(Duration::from_secs).unwrap_or(d.locks),
            top_queries: cfg
                .top_queries
                .map(Duration::from_secs)
                .unwrap_or(d.top_queries),
            replication: cfg
                .replication
                .map(Duration::from_secs)
                .unwrap_or(d.replication),
            stats: cfg.stats.map(Duration::from_secs).unwrap_or(d.stats),
        }
    }
}

/// Один именованный профиль подключения.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Profile {
    pub dsn: Option<String>,
    /// Если true — forces `actions_allowed = false` независимо от
    /// `--allow-actions`. Безопасный «seal» для prod-профилей.
    #[serde(default)]
    pub read_only: bool,
}

/// Финальный набор runtime-параметров после применения всех layer'ов.
/// Это то, что main передаёт дальше в App / db::connect / executor.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub dsn: String,
    pub actions_allowed: bool,
    pub read_only: bool,
    /// Имя выбранного профиля (если был) — для отображения в title-bar.
    pub profile_name: Option<String>,
    /// Resolved-тема (из `config.ui.theme`).
    pub theme: Theme,
    /// Resolved-интервалы (из `config.intervals` поверх дефолтов).
    pub intervals: Intervals,
}

/// Путь к конфигу: `$XDG_CONFIG_HOME/pgtop/config.toml` (через `dirs`).
/// Fallback на cwd если HOME нет — для CI-сценариев.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
        .join("config.toml")
}

/// Загрузить конфиг через figment. Если файла нет — возвращаем дефолт-конфиг
/// (без профилей). figment даёт хорошие ошибки парсинга (с line:column).
pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    Figment::new()
        .merge(Toml::file(&path))
        .extract()
        .map_err(|e| eyre!("failed to load config from {}: {e}", path.display()))
}

impl Resolved {
    /// Свести все layer'ы в финальные значения.
    ///
    /// - **DSN**: CLI `--dsn` > env `DATABASE_URL` > profile.dsn > `DEFAULT_DSN`.
    /// - **read_only**: CLI `--read-only` ∨ profile.read_only.
    /// - **actions_allowed**: CLI `--allow-actions` ∧ ¬read_only.
    ///
    /// Read-only — «sticky off» для actions: если хоть один источник установил
    /// read_only, actions выключены даже при явном `--allow-actions`. Это анти-fool
    /// для случая «прокинул prod-профиль и забыл, что флаг включён».
    pub fn from_layers(
        config: &Config,
        cli_profile: Option<&str>,
        cli_dsn: Option<&str>,
        cli_allow_actions: bool,
        cli_read_only: bool,
    ) -> Result<Self> {
        // 1. Выбор профиля: CLI > config.default_profile > None.
        let profile_name = cli_profile
            .map(str::to_string)
            .or_else(|| config.default_profile.clone());

        // 2. Получить Profile (или default если имя не задано).
        // Если имя задано но не найдено — фейлим явно с ошибкой.
        let profile = match &profile_name {
            Some(name) => config.profiles.get(name).cloned().ok_or_else(|| {
                let available: Vec<&str> = config.profiles.keys().map(String::as_str).collect();
                eyre!(
                    "profile '{name}' not found in config; available: [{}]",
                    available.join(", ")
                )
            })?,
            None => Profile::default(),
        };

        // 3. DSN-resolution в порядке убывания приоритета.
        let dsn = cli_dsn
            .map(str::to_string)
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .or(profile.dsn)
            .unwrap_or_else(|| DEFAULT_DSN.to_string());

        // 4. read_only: OR двух источников; никогда не «снимается» CLI'ем.
        let read_only = cli_read_only || profile.read_only;

        // 5. actions_allowed: CLI-флаг, но read_only гасит.
        let actions_allowed = cli_allow_actions && !read_only;

        // 6. Theme — name-based; unknown → dark.
        let theme = config
            .ui
            .theme
            .as_deref()
            .map(Theme::from_name)
            .unwrap_or_default();

        // 7. Intervals — config overlay поверх дефолтов.
        let intervals = Intervals::from_config(&config.intervals);

        Ok(Self {
            dsn,
            actions_allowed,
            read_only,
            profile_name,
            theme,
            intervals,
        })
    }
}
