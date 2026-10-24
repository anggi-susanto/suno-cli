//! Configuration with 3-tier precedence: compiled defaults < TOML file
//! (`config path` shows where) < `SUNO_*` env vars. Command-line flags beat
//! all three at the call sites that consume these values.

use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format as _, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

use crate::errors::CliError;

/// A directory-path env override, ignored when unset or blank.
fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppConfig {
    /// Default --model for generate/describe/extend/cover (clap name,
    /// e.g. "v6" — not the chirp-* API key).
    pub default_model: String,
    /// Initial poll backoff for --wait (doubles up to 15s).
    pub poll_interval_secs: u64,
    /// Total --wait timeout before giving up on a generation.
    pub poll_timeout_secs: u64,
    /// Default directory for `download` when -o is not given.
    pub output_dir: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_model: "v6".into(),
            poll_interval_secs: 5,
            poll_timeout_secs: 600,
            output_dir: ".".into(),
        }
    }
}

/// Config directory (holds `config.toml` and `auth.json`). `SUNO_CONFIG_DIR`
/// overrides it — required for hermetic tests on Windows, where the
/// `directories` crate reads the Known Folder API and ignores HOME/XDG, and
/// handy for agents that want to redirect state.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_dir("SUNO_CONFIG_DIR") {
        return dir;
    }
    directories::ProjectDirs::from("com", "suno-cli", "suno-cli")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~/.config/suno-cli"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// State directory (duplicate-guard locks, solver Chrome profile).
/// `SUNO_DATA_DIR` overrides it (see [`config_dir`]).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env_dir("SUNO_DATA_DIR") {
        return dir;
    }
    directories::ProjectDirs::from("com", "suno-cli", "suno-cli")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub const CONFIG_KEYS: &[&str] = &[
    "default_model",
    "poll_interval_secs",
    "poll_timeout_secs",
    "output_dir",
];

impl AppConfig {
    /// Env keys are flat (`SUNO_POLL_INTERVAL_SECS` → `poll_interval_secs`),
    /// so no `.split("_")` — splitting would shred multi-word field names
    /// into nonexistent nested tables.
    pub fn load() -> Result<Self, CliError> {
        let _timing = crate::timings::span("config.load");
        Figment::from(Serialized::defaults(AppConfig::default()))
            .merge(Toml::file(config_path()))
            .merge(Env::prefixed("SUNO_"))
            .extract()
            .map_err(|e| CliError::Config(format!("config: {e}")))
    }

    /// Validate and persist one key to the TOML file, preserving any keys
    /// already there (including ones this version doesn't know about).
    pub fn set_value(key: &str, value: &str) -> Result<PathBuf, CliError> {
        let parsed = match key {
            "poll_interval_secs" | "poll_timeout_secs" => {
                let n: u64 = value.parse().map_err(|_| {
                    CliError::InvalidInput(format!("{key} must be a positive integer, got {value}"))
                })?;
                if n == 0 {
                    return Err(CliError::InvalidInput(format!(
                        "{key} must be greater than zero, got {value}"
                    )));
                }
                // TOML integers are i64; a checked conversion keeps a u64 past
                // i64::MAX from silently persisting as a negative value.
                let n = i64::try_from(n).map_err(|_| {
                    CliError::InvalidInput(format!("{key} is too large, got {value}"))
                })?;
                toml::Value::Integer(n)
            }
            "default_model" => {
                <crate::cli::ModelVersion as clap::ValueEnum>::from_str(value, true).map_err(
                    |_| {
                        CliError::InvalidInput(format!(
                            "unknown model '{value}' — see `suno generate --help` for valid --model values"
                        ))
                    },
                )?;
                toml::Value::String(value.into())
            }
            "output_dir" => toml::Value::String(value.into()),
            other => {
                return Err(CliError::InvalidInput(format!(
                    "unknown config key '{other}' — valid keys: {}",
                    CONFIG_KEYS.join(", ")
                )));
            }
        };

        let path = config_path();
        let mut table: toml::Table = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)
                .map_err(|e| CliError::Config(format!("cannot parse {}: {e}", path.display())))?
        } else {
            toml::Table::new()
        };
        table.insert(key.into(), parsed);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(&table)
            .map_err(|e| CliError::Config(format!("config serialize: {e}")))?;
        atomic_write(&path, &serialized)?;
        Ok(path)
    }

    /// Semantic validation of the merged effective config, run by
    /// `config check`. Parsing (via `load`) already caught type errors; this
    /// catches values that parse but can't work: a zero poll interval/timeout
    /// or a `default_model` that no longer maps to a `--model` value (e.g. a
    /// stale `SUNO_DEFAULT_MODEL`).
    pub fn validate(&self) -> Result<(), CliError> {
        if self.poll_interval_secs == 0 {
            return Err(CliError::Config(
                "poll_interval_secs must be greater than zero".into(),
            ));
        }
        if self.poll_timeout_secs == 0 {
            return Err(CliError::Config(
                "poll_timeout_secs must be greater than zero".into(),
            ));
        }
        <crate::cli::ModelVersion as clap::ValueEnum>::from_str(&self.default_model, true)
            .map_err(|_| {
                CliError::Config(format!(
                    "unknown default_model '{}' — see `suno generate --help` for valid values",
                    self.default_model
                ))
            })?;
        Ok(())
    }
}

/// Write via a same-directory temp file plus atomic rename so a crash or a
/// concurrent reader never observes a half-written config. The temp name is
/// process-scoped to avoid two writers colliding on the same intermediate.
fn atomic_write(path: &Path, contents: &str) -> Result<(), CliError> {
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("config.toml");
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents)?;
    // rename replaces the destination atomically on both POSIX and Windows.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // figment::Jail's closure returns figment's own large Error type;
    // nothing we can shrink on our side.
    #[allow(clippy::result_large_err)]
    fn env_overrides_defaults_without_splitting() {
        // Flat env keys must map onto multi-word field names; a `.split("_")`
        // provider would break every key in this config.
        figment::Jail::expect_with(|jail| {
            jail.set_env("SUNO_POLL_INTERVAL_SECS", "99");
            jail.set_env("SUNO_DEFAULT_MODEL", "v4.5");
            let cfg = AppConfig::load().expect("load");
            assert_eq!(cfg.poll_interval_secs, 99);
            assert_eq!(cfg.default_model, "v4.5");
            // Untouched keys keep their defaults.
            assert_eq!(cfg.poll_timeout_secs, 600);
            Ok(())
        });
    }

    #[test]
    fn set_value_rejects_unknown_keys_and_bad_values() {
        assert!(matches!(
            AppConfig::set_value("nope", "1"),
            Err(CliError::InvalidInput(_))
        ));
        assert!(matches!(
            AppConfig::set_value("poll_interval_secs", "fast"),
            Err(CliError::InvalidInput(_))
        ));
        assert!(matches!(
            AppConfig::set_value("default_model", "v99"),
            Err(CliError::InvalidInput(_))
        ));
        // Zero and past-i64::MAX must not persist (the latter used to wrap to a
        // negative TOML integer under an unchecked `as i64`).
        assert!(matches!(
            AppConfig::set_value("poll_timeout_secs", "0"),
            Err(CliError::InvalidInput(_))
        ));
        assert!(matches!(
            AppConfig::set_value("poll_interval_secs", "18446744073709551615"),
            Err(CliError::InvalidInput(_))
        ));
    }

    #[test]
    fn validate_rejects_unusable_effective_config() {
        assert!(AppConfig::default().validate().is_ok());

        let bad_model = AppConfig {
            default_model: "v99".into(),
            ..AppConfig::default()
        };
        assert!(matches!(bad_model.validate(), Err(CliError::Config(_))));

        let zero_interval = AppConfig {
            poll_interval_secs: 0,
            ..AppConfig::default()
        };
        assert!(matches!(zero_interval.validate(), Err(CliError::Config(_))));

        let zero_timeout = AppConfig {
            poll_timeout_secs: 0,
            ..AppConfig::default()
        };
        assert!(matches!(zero_timeout.validate(), Err(CliError::Config(_))));
    }

    #[test]
    fn atomic_write_replaces_without_leaving_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        atomic_write(&path, "a = 1\n").unwrap();
        atomic_write(&path, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        // No stray temp files left behind in the target directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }
}
