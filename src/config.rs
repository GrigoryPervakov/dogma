//! Config — server URL and a couple of UI toggles. Loaded from a TOML file
//! under `~/.dogma/config.toml`. Logs land at `~/.dogma/dogma.log`. Both paths
//! mirror Nerve's `~/.nerve/` convention so they're easy to find.
//!
//! v1 is intentionally minimal. No secrets here. No token persistence.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server: String,
    #[serde(default = "default_true")]
    pub sidebar_open: bool,
    #[serde(default)]
    pub theme: String,
}

fn default_server() -> String {
    "http://127.0.0.1:8900".into()
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: default_server(),
            sidebar_open: true,
            theme: "default".into(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(cfg)
    }

    pub fn override_server(mut self, server: Option<String>) -> Self {
        if let Some(s) = server {
            self.server = s;
        }
        self
    }
}

/// Root data directory: `~/.dogma/` (or `./.dogma/` if HOME is unset).
pub fn dogma_dir() -> PathBuf {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dogma");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    dogma_dir().join("config.toml")
}

pub fn log_path() -> PathBuf {
    if let Ok(p) = std::env::var("DOGMA_LOG") {
        return PathBuf::from(p);
    }
    dogma_dir().join("dogma.log")
}

pub fn smoke_log_path() -> PathBuf {
    dogma_dir().join("smoke.log")
}

pub fn smoke_failures_dir() -> PathBuf {
    let dir = dogma_dir().join("smoke-failures");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
