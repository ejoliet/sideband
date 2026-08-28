//! Configuration: defaults, then `~/.config/sideband/config.toml`, then env.

use std::path::PathBuf;

use anyhow::Context;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

use crate::error::SidebandError;

/// Resolved configuration. See the Configuration Reference in README.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// `xoxb-` token, required by `send` and `bot`.
    pub slack_bot_token: Option<String>,
    /// `xapp-` token, required by `bot` for Socket Mode.
    pub slack_app_token: Option<String>,
    /// Default channel for `send` when `--channel` is absent.
    pub channel: Option<String>,
    /// How long the ticket stays valid and the sender keeps seeding.
    pub ttl_hours: u32,
    /// Stop seeding after N completed downloads. 0 means unlimited.
    pub max_downloads: u32,
    /// Self-hosted relay. Empty means the n0 public relays.
    pub relay_url: Option<String>,
    /// Ed25519-signed team license, verified offline. Unused until M3.
    pub license_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            slack_bot_token: None,
            slack_app_token: None,
            channel: None,
            ttl_hours: 24,
            max_downloads: 0,
            relay_url: None,
            license_key: None,
        }
    }
}

impl Config {
    /// Loads defaults, then the config file, then `SIDEBAND_*` and `SLACK_*` env vars.
    pub fn load() -> anyhow::Result<Self> {
        Figment::from(Serialized::defaults(Self::default()))
            .merge(Toml::file(config_path()))
            .merge(Env::prefixed("SIDEBAND_"))
            .merge(Env::raw().only(&["SLACK_BOT_TOKEN", "SLACK_APP_TOKEN"]))
            .extract()
            .context("reading sideband configuration")
    }

    /// Bot token or a `Config` error naming the variable to set.
    pub fn bot_token(&self) -> Result<&str, SidebandError> {
        self.slack_bot_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| SidebandError::Config("SLACK_BOT_TOKEN is not set".into()))
    }

    /// App-level token or a `Config` error naming the variable to set.
    pub fn app_token(&self) -> Result<&str, SidebandError> {
        self.slack_app_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| SidebandError::Config("SLACK_APP_TOKEN is not set".into()))
    }
}

/// `$XDG_CONFIG_HOME/sideband/config.toml`, falling back to `~/.config`.
pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
        .join("sideband")
        .join("config.toml")
}

/// Blob store root. Persistent so that an interrupted `get` resumes on re-run.
pub fn store_dir() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache")
        .join("sideband")
        .join("store")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(fallback),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}
