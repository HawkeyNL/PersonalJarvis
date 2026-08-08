//! Typed, environment-driven configuration for Jarvis services.
//!
//! Values are loaded from an optional `jarvis.toml` and then overridden by
//! `JARVIS_`-prefixed environment variables. Secrets (e.g. `database_url`) are
//! redacted from the `Debug` output so they never leak into logs.

// `figment::Error` is a large third-party error type that we surface directly
// from `load()` for ergonomics; boxing it would break `?` in anyhow callers.
#![allow(clippy::result_large_err)]

use std::fmt;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

/// Top-level application configuration.
#[derive(Clone, Deserialize)]
pub struct AppConfig {
    /// `host:port` the HTTP server binds to.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// PostgreSQL connection string. Secret — redacted in `Debug`.
    pub database_url: String,

    /// Emit logs as JSON (recommended for production).
    #[serde(default)]
    pub log_json: bool,

    /// Deployment environment name (e.g. `development`, `production`).
    #[serde(default = "default_environment")]
    pub environment: String,
}

fn default_bind_addr() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_environment() -> String {
    "development".to_string()
}

impl AppConfig {
    /// Load configuration from `jarvis.toml` (optional) and `JARVIS_` env vars.
    ///
    /// Environment variables take precedence over the file.
    pub fn load() -> Result<Self, figment::Error> {
        Figment::new()
            .merge(Toml::file("jarvis.toml"))
            .merge(Env::prefixed("JARVIS_"))
            .extract()
    }
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("log_json", &self.log_json)
            .field("environment", &self.environment)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_database_url() {
        let cfg = AppConfig {
            bind_addr: "0.0.0.0:8080".to_string(),
            database_url: "postgres://user:supersecret@localhost/jarvis".to_string(),
            log_json: false,
            environment: "test".to_string(),
        };

        let rendered = format!("{cfg:?}");

        assert!(
            !rendered.contains("supersecret"),
            "database_url secret leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn env_overrides_are_applied() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("JARVIS_DATABASE_URL", "postgres://localhost/x");
            jail.set_env("JARVIS_BIND_ADDR", "127.0.0.1:9999");
            jail.set_env("JARVIS_LOG_JSON", "true");

            let cfg = AppConfig::load()?;
            assert_eq!(cfg.bind_addr, "127.0.0.1:9999");
            assert_eq!(cfg.database_url, "postgres://localhost/x");
            assert!(cfg.log_json);
            assert_eq!(cfg.environment, "development"); // default
            Ok(())
        });
    }
}
