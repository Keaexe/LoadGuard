//! User preferences (limits live in the helper), stored in `$XDG_CONFIG_HOME/loadguard/config.toml`.

use crate::Unit;
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub backends: Backends,
    /// Unit shown in the GUI.
    pub unit: Unit,
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

/// Stores the unit, leaving every other setting as it is.
pub fn save_unit(path: &Path, unit: Unit) -> Result<(), String> {
    let err = |e: io::Error| format!("{}: {e}", path.display());
    let mut table: toml::Table = match fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(err(e)),
    };
    table.insert("unit".into(), unit.label().into());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(err)?;
    }
    let tmp = path.with_extension(format!("toml.{}", std::process::id()));
    fs::write(&tmp, table.to_string()).map_err(err)?;
    fs::rename(&tmp, path).map_err(err)
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

    #[test]
    fn saving_the_unit_keeps_other_settings() {
        let dir = env::temp_dir().join(format!("lg-config-{}", std::process::id()));
        let path = dir.join("sub/config.toml");
        save_unit(&path, Unit::MegaBits).unwrap();
        fs::write(&path, "future = 1\n[backends]\nnftables = false\n").unwrap();
        save_unit(&path, Unit::MegaBits).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!((c.unit, c.backends.nftables), (Unit::MegaBits, false));
        assert!(fs::read_to_string(&path).unwrap().contains("future"));
        fs::remove_dir_all(dir).unwrap();
    }
}
