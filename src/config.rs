//! Configuration loading and runtime parameter resolution.
//!
//! Layered priority (lowest → highest): hardcoded defaults, profile from
//! `~/.config/pgtop/config.toml`, `DATABASE_URL` env, CLI flags.

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

/// Default DSN used when nothing else specifies one.
pub const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

/// Top-level config loaded from `~/.config/pgtop/config.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Profile to use when the CLI does not pick one. `None` falls back to
    /// env / CLI / `DEFAULT_DSN`.
    pub default_profile: Option<String>,

    /// `[profiles.<name>]` sections from TOML.
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,

    #[serde(default)]
    pub ui: UiConfig,

    /// Poll intervals per collector.
    #[serde(default)]
    pub intervals: IntervalsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UiConfig {
    /// Theme name: `"dark"` (default) or `"light"`. Unknown name falls back
    /// to dark.
    pub theme: Option<String>,
}

/// Poll intervals in **seconds**. `None` keeps the hardcoded default.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IntervalsConfig {
    pub activity: Option<u64>,
    pub locks: Option<u64>,
    pub top_queries: Option<u64>,
    pub replication: Option<u64>,
    pub stats: Option<u64>,
    pub databases: Option<u64>,
    pub tables: Option<u64>,
}

/// Resolved intervals as `Duration`s. Defaults: activity/locks/stats = 1s,
/// replication = 5s, databases = 5s, top_queries / tables = 10s.
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    pub activity: Duration,
    pub locks: Duration,
    pub top_queries: Duration,
    pub replication: Duration,
    pub stats: Duration,
    pub databases: Duration,
    pub tables: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            activity: Duration::from_secs(1),
            locks: Duration::from_secs(1),
            top_queries: Duration::from_secs(10),
            replication: Duration::from_secs(5),
            stats: Duration::from_secs(1),
            databases: Duration::from_secs(5),
            tables: Duration::from_secs(10),
        }
    }
}

impl Intervals {
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
            databases: cfg
                .databases
                .map(Duration::from_secs)
                .unwrap_or(d.databases),
            tables: cfg.tables.map(Duration::from_secs).unwrap_or(d.tables),
        }
    }
}

/// One named connection profile.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Profile {
    pub dsn: Option<String>,
    /// When true, forces `actions_allowed = false` regardless of
    /// `--allow-actions` — a safe seal for prod profiles.
    #[serde(default)]
    pub read_only: bool,
}

/// Final runtime parameters after applying every layer.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub dsn: String,
    pub actions_allowed: bool,
    pub read_only: bool,
    /// Selected profile name, if any — shown in the title bar.
    pub profile_name: Option<String>,
    pub theme: Theme,
    pub intervals: Intervals,
}

/// Path to the config file (`$XDG_CONFIG_HOME/pgtop/config.toml`).
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
        .join("config.toml")
}

/// Load the config from disk. Returns the default config if the file is
/// missing.
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
    /// Combine all layers into final values.
    ///
    /// - DSN: CLI `--dsn` > env `DATABASE_URL` > `profile.dsn` > `DEFAULT_DSN`.
    /// - `read_only`: CLI `--read-only` ∨ `profile.read_only`.
    /// - `actions_allowed`: CLI `--allow-actions` ∧ ¬read_only.
    pub fn from_layers(
        config: &Config,
        cli_profile: Option<&str>,
        cli_dsn: Option<&str>,
        cli_allow_actions: bool,
        cli_read_only: bool,
    ) -> Result<Self> {
        let profile_name = cli_profile
            .map(str::to_string)
            .or_else(|| config.default_profile.clone());

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

        let dsn = cli_dsn
            .map(str::to_string)
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .or(profile.dsn)
            .unwrap_or_else(|| DEFAULT_DSN.to_string());

        let read_only = cli_read_only || profile.read_only;

        let actions_allowed = cli_allow_actions && !read_only;

        let theme = config
            .ui
            .theme
            .as_deref()
            .map(Theme::from_name)
            .unwrap_or_default();

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
