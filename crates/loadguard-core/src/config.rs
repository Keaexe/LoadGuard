//! User configuration, stored in `$XDG_CONFIG_HOME/loadguard/config.toml`.

use crate::Limit;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub backends: Backends,
    /// Saved limits, keyed by app id.
    pub apps: BTreeMap<String, Limit>,
}

/// On/off switch for each backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Backends {
    pub nftables: bool,
}

impl Default for Backends {
    fn default() -> Self {
        Self { nftables: true }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        let absolute = |v| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
        let base = env::var_os("XDG_CONFIG_HOME")
            .and_then(absolute)
            .or_else(|| {
                env::var_os("HOME")
                    .and_then(absolute)
                    .map(|h| h.join(".config"))
            })?;
        Some(base.join("loadguard/config.toml"))
    }

    /// A missing file means defaults; a malformed one is an error.
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes to a temporary file and renames, so a crash cannot truncate it.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let err = |e: io::Error| format!("{}: {e}", path.display());
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = path.with_extension(format!("toml.{}", std::process::id()));
        fs::write(&tmp, text).map_err(err)?;
        fs::rename(&tmp, path).map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_handles_missing_file() {
        let dir = env::temp_dir().join(format!("loadguard-test-{}", std::process::id()));
        let path = dir.join("sub/config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let mut c = Config::default();
        c.apps.insert(
            "org.x.Y".into(),
            Limit {
                down: "1.5M".parse().ok(),
                up: None,
            },
        );
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_invalid_content() {
        assert!(toml::from_str::<Config>("[apps.x]\ndown = \"0\"").is_err());
        assert!(toml::from_str::<Config>("[apps.x]\ndwon = \"5M\"").is_err());
    }
}
