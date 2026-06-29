//! Config — server URL and a couple of UI toggles. Loaded from a TOML file
//! under `~/.dogma/config.toml`. Logs land at `~/.dogma/dogma.log`. Both paths
//! mirror Nerve's `~/.nerve/` convention so they're easy to find.
//!
//! A per-instance `password` may be stored here for unattended login; it's
//! plaintext, so keep the file `chmod 600`. Tokens are never persisted.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server: String,
    /// Multiple instances to drive at once. Takes precedence over `server`
    /// when non-empty; a repeated `--server` flag overrides both.
    #[serde(default)]
    pub servers: Vec<ServerCfg>,
    #[serde(default = "default_true")]
    pub sidebar_open: bool,
    #[serde(default)]
    pub theme: String,
}

/// One Nerve instance: a URL plus an optional friendly label and password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCfg {
    #[serde(default)]
    pub name: Option<String>,
    pub url: String,
    /// Optional plaintext password — when set, dogma logs in without
    /// prompting. Stored in `~/.dogma/config.toml`; keep that file `chmod 600`.
    #[serde(default)]
    pub password: Option<String>,
}

impl ServerCfg {
    /// Parse a `--server` flag value: `name=url`, or a bare `url`.
    pub fn parse(s: &str) -> Self {
        match s.split_once('=') {
            Some((name, url)) if !name.is_empty() && url.contains("://") => Self {
                name: Some(name.to_string()),
                url: url.to_string(),
                password: None,
            },
            _ => Self {
                name: None,
                url: s.to_string(),
                password: None,
            },
        }
    }

    /// Display label: the explicit name, else the `host:port` of the url.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| host_label(&self.url).to_string())
    }
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
            servers: Vec::new(),
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

    /// The instances to connect to. CLI `--server` flags (each `[name=]url`)
    /// win if any are present; otherwise the `[[servers]]` array; otherwise the
    /// single `server`. Never empty.
    pub fn resolve_servers(&self, cli_servers: &[String]) -> Vec<ServerCfg> {
        if !cli_servers.is_empty() {
            return cli_servers.iter().map(|s| ServerCfg::parse(s)).collect();
        }
        if !self.servers.is_empty() {
            return self.servers.clone();
        }
        vec![ServerCfg {
            name: None,
            url: self.server.clone(),
            password: None,
        }]
    }
}

/// The `host:port` of a server URL — strips the scheme and any path
/// (`http://127.0.0.1:8900/` → `127.0.0.1:8900`). Used for labels and the
/// terminal title.
pub fn host_label(server: &str) -> &str {
    server
        .strip_prefix("https://")
        .or_else(|| server.strip_prefix("http://"))
        .unwrap_or(server)
        .split('/')
        .next()
        .unwrap_or(server)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_label_strips_scheme_and_path() {
        assert_eq!(host_label("http://127.0.0.1:8900"), "127.0.0.1:8900");
        assert_eq!(host_label("https://my-dev-vm:8900/"), "my-dev-vm:8900");
        assert_eq!(host_label("nerve.example.com:443"), "nerve.example.com:443");
    }

    #[test]
    fn server_cfg_parses_named_and_bare() {
        let named = ServerCfg::parse("vm=http://my-dev-vm:8900");
        assert_eq!(named.name.as_deref(), Some("vm"));
        assert_eq!(named.url, "http://my-dev-vm:8900");
        assert_eq!(named.label(), "vm");

        let bare = ServerCfg::parse("http://127.0.0.1:8900");
        assert_eq!(bare.name, None);
        assert_eq!(bare.label(), "127.0.0.1:8900");
    }

    #[test]
    fn resolve_prefers_cli_then_servers_then_single() {
        let cfg = Config {
            server: "http://single:8900".into(),
            servers: vec![ServerCfg {
                name: Some("a".into()),
                url: "http://a:8900".into(),
                password: None,
            }],
            ..Config::default()
        };
        // CLI flags win.
        let cli = cfg.resolve_servers(&["b=http://b:8900".to_string()]);
        assert_eq!(cli.len(), 1);
        assert_eq!(cli[0].label(), "b");
        // Else the [[servers]] array.
        assert_eq!(cfg.resolve_servers(&[])[0].label(), "a");
        // Else the single server.
        let single = Config::default().resolve_servers(&[]);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].url, "http://127.0.0.1:8900");
    }
}
