//! Atomic configuration file loading and persistence helpers.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::schema::{ConfigError, ConfigV1};

/// Returns the default directory where `zerovdd` stores persistent configuration.
pub fn default_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("PROGRAMDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("zerovdd")
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(xdg).join("zerovdd");
        }
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(home).join(".config").join("zerovdd");
        }
        PathBuf::from("/etc/zerovdd")
    }
}

/// Returns the default path to `config.toml`.
pub fn default_config_path() -> PathBuf {
    default_config_dir().join("config.toml")
}

/// Loads and strictly validates a [`ConfigV1`] file from `path`.
pub fn load_from_path(path: &Path) -> Result<ConfigV1, ConfigError> {
    let raw = fs::read_to_string(path)?;
    ConfigV1::from_toml_str(&raw)
}

/// Loads [`ConfigV1`] from `path`, or returns [`ConfigV1::default()`] if the file does not exist.
///
/// Corrupt TOML, unknown fields, or semantic validation errors are always returned as `Err`
/// rather than silently falling back to defaults.
pub fn load_or_default(path: &Path) -> Result<ConfigV1, ConfigError> {
    match fs::read_to_string(path) {
        Ok(raw) => ConfigV1::from_toml_str(&raw),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(ConfigV1::default()),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Validates, serializes, syncs, and atomically persists `config` to `path`.
///
/// Writes to a temporary file inside the destination parent directory, calls `sync_all`,
/// and atomically replaces `path` so an interrupted write never corrupts a prior valid file.
pub fn save_atomic(path: &Path, config: &ConfigV1) -> Result<(), ConfigError> {
    save_atomic_with_pre_commit(path, config, || Ok(()))
}

/// Internal atomic save helper allowing deterministic fault injection before rename.
#[doc(hidden)]
pub fn save_atomic_with_pre_commit<F>(
    path: &Path,
    config: &ConfigV1,
    pre_commit: F,
) -> Result<(), ConfigError>
where
    F: FnOnce() -> std::io::Result<()>,
{
    let serialized = config.to_toml_string()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| ConfigError::InvalidPath(path.display().to_string()))?;

    fs::create_dir_all(parent)?;

    // WHY: Creating the temporary file inside the exact destination directory guarantees
    // the subsequent persist() rename stays on the same volume/filesystem and is atomic.
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(serialized.as_bytes())?;
    temp.flush()?;
    temp.as_file().sync_all()?;

    pre_commit()?;

    temp.persist(path)
        .map_err(|err| ConfigError::Io(err.error))?;
    Ok(())
}
