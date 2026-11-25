//! User preferences (limits live in the helper), stored in `$XDG_CONFIG_HOME/loadguard/config.toml`.

use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub backends: Backends,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_switch() {
        assert!(
            Config::load(Path::new("/nonexistent"))
                .unwrap()
                .backends
                .nftables
        );
        let c: Config = toml::from_str("[backends]\nnftables = false").unwrap();
        assert!(!c.backends.nftables);
        assert!(toml::from_str::<Config>("[backends]\ntypo = true").is_err());
    }
}
