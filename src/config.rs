//! Per-user configuration for the client.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Contents of `~/.config/corral/config.toml`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    /// SSH host (usually an alias in `~/.ssh/config`) that runs live on.
    pub host: Option<String>,

    /// Command that runs `corral` on the host. Defaults to `corral`.
    ///
    /// This is run by the host's shell, so it can set up the environment
    /// first.
    pub remote_command: Option<String>,
}

impl Config {
    /// Path to the config file: `$CORRAL_CONFIG`, or under the XDG config
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("CORRAL_CONFIG") {
            return Ok(path.into());
        }
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(
                std::env::var_os("HOME").context("HOME is not set")?,
            )
            .join(".config"),
        };
        Ok(base.join("corral").join("config.toml"))
    }

    /// Load the config. A missing file is an empty config.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid config {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(error) => Err(error)
                .with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Write the config, creating its directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }
}
