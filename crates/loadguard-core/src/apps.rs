//! Finds running apps from the cgroups systemd creates for them.

use crate::cgroup::{CgroupPath, app_slice};
use std::{collections::BTreeMap, fs, io, path::Path};

pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Users control the tree below their app slice, and the root helper walks it.
const MAX_DEPTH: usize = 16;
const MAX_CGROUPS_PER_APP: usize = 64;

/// One app, possibly spread over several cgroups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub cgroups: Vec<CgroupPath>,
    pub processes: usize,
    /// Follows the desktop `app-*` naming, as opposed to a background service.
    pub desktop: bool,
}

/// Lists apps with at least one process. `root` is normally [`CGROUP_ROOT`].
pub fn discover(root: &Path, uid: u32) -> io::Result<Vec<App>> {
    let mut found = BTreeMap::new();
    scan(root, &app_slice(uid), uid, 0, &mut found)?;
    Ok(found.into_values().collect())
}

fn scan(
    root: &Path,
    rel: &str,
    uid: u32,
    depth: usize,
    found: &mut BTreeMap<String, App>,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = format!("{rel}/{name}");
        if name.ends_with(".slice") {
            // Units come and go while we scan, so a vanished slice is not an error.
            let _ = scan(root, &path, uid, depth + 1, found);
            continue;
        }
        let Some((id, desktop)) = parse_unit(&name) else {
            continue;
        };
        let processes = count_processes(&entry.path(), 0);
        let Ok(cgroup) = CgroupPath::for_user(&path, uid) else {
            continue;
        };
        if processes == 0 {
            continue;
        }
        let app = found.entry(id.clone()).or_insert(App {
            id,
            cgroups: Vec::new(),
            processes: 0,
            desktop,
        });
        if app.cgroups.len() < MAX_CGROUPS_PER_APP {
            app.cgroups.push(cgroup);
        }
        app.processes += processes;
        app.desktop |= desktop;
        app.cgroups.sort();
    }
    Ok(())
}

fn count_processes(dir: &Path, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    let own = fs::read_to_string(dir.join("cgroup.procs")).map_or(0, |s| s.lines().count());
    let nested: usize = fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| count_processes(&e.path(), depth + 1))
            .sum()
    });
    own + nested
}

/// Returns the app id and whether the unit is a desktop app, following
/// <https://systemd.io/DESKTOP_ENVIRONMENTS/>.
fn parse_unit(unit: &str) -> Option<(String, bool)> {
    let (stem, scope) = match unit.strip_suffix(".scope") {
        Some(s) => (s, true),
        None => (unit.strip_suffix(".service")?, false),
    };
    let Some(rest) = stem.strip_prefix("app-") else {
        return Some((unescape(stem), false));
    };
    // Literal dashes in the id are escaped, so a raw dash is a separator.
    let mut parts: Vec<&str> = if scope {
        rest.split('-').collect()
    } else {
        rest.split_once('@')
            .map_or(rest, |(id, _)| id)
            .split('-')
            .collect()
    };
    if scope && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 1 && ["flatpak", "gnome", "KDE", "loadguard"].contains(&parts[0]) {
        parts.remove(0);
    }
    Some((unescape(&parts.join("-")), true))
}

/// Reverses systemd's `\xNN` escaping.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i..].starts_with(b"\\x"))
            .then(|| b.get(i + 2..i + 4))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    // Control characters could corrupt terminal output.
    String::from_utf8_lossy(&out).replace(char::is_control, "?")
}

/// Finds one app by exact id, then by unambiguous case-insensitive substring.
pub fn find<'a>(apps: &'a [App], query: &str) -> Result<&'a App, String> {
    if let Some(app) = apps.iter().find(|a| a.id == query) {
        return Ok(app);
    }
    let q = query.to_lowercase();
    let hits: Vec<_> = apps
        .iter()
        .filter(|a| a.id.to_lowercase().contains(&q))
        .collect();
    match hits[..] {
        [app] => Ok(app),
        [] => Err(format!(
            "no running app matches '{query}' (see `loadguard list --all`)"
        )),
        _ => {
            let ids: Vec<_> = hits.iter().map(|a| a.id.as_str()).collect();
            Err(format!("'{query}' is ambiguous: {}", ids.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unit_names() {
        for (unit, id, desktop) in [
            ("app-zen@a1a2.service", "zen", true),
            ("app-Nextcloud@autostart.service", "Nextcloud", true),
            (
                "app-org.kde.kdeconnect.daemon@autostart.service",
                "org.kde.kdeconnect.daemon",
                true,
            ),
            (
                "app-arch\\x2dupdate\\x2dtray@autostart.service",
                "arch-update-tray",
                true,
            ),
            ("app-heroic-3189.scope", "heroic", true),
            ("app-steam-app2357570-11246.scope", "steam-app2357570", true),
            (
                "app-flatpak-com.spotify.Client-12.scope",
                "com.spotify.Client",
                true,
            ),
            ("pipewire.service", "pipewire", false),
        ] {
            assert_eq!(parse_unit(unit), Some((id.to_owned(), desktop)), "{unit}");
        }
        assert_eq!(parse_unit("dbus.socket"), None);
    }

    #[test]
    fn unescape_tolerates_malformed_input() {
        assert_eq!(unescape("a\\x"), "a\\x");
        assert_eq!(unescape("a\\xé"), "a\\xé");
        assert_eq!(unescape("a\\x1bb"), "a?b");
    }

    #[test]
    fn find_prefers_exact_and_reports_ambiguity() {
        let app = |id: &str| App {
            id: id.into(),
            cgroups: vec![],
            processes: 1,
            desktop: true,
        };
        let apps = [app("steam"), app("steam-app1"), app("Zen")];
        assert_eq!(find(&apps, "steam").unwrap().id, "steam");
        assert_eq!(find(&apps, "zen").unwrap().id, "Zen");
        assert!(find(&apps, "stea").unwrap_err().contains("ambiguous"));
        assert!(find(&apps, "nope").is_err());
    }
}
