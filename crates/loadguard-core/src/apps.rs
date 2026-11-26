//! Finds running apps from the cgroups systemd creates for them.

use crate::cgroup::{CgroupPath, app_slice};
use std::{collections::BTreeMap, fs, io, path::Path};

pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Users control the tree below their app slice, and the root helper walks it.
const MAX_DEPTH: usize = 16;
const MAX_CGROUPS_PER_APP: usize = 64;
/// Folders visited per scan. The helper scans every second as root, and a user
/// can create as many cgroups as they like, so the work must stay bounded.
const MAX_DIRS: usize = 4096;
const MAX_PIDS_PER_UNIT: usize = 4096;
/// Process names and parents read per scan, for grouping.
const MAX_STAT_READS: usize = 4096;
const MAX_MOVERS: usize = 32;

/// One app, possibly spread over several cgroups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct App {
    pub id: String,
    /// Ids of other groups that turned out to belong to this app.
    pub aliases: Vec<String>,
    pub cgroups: Vec<CgroupPath>,
    pub processes: usize,
    /// Follows the desktop `app-*` naming, as opposed to a background service.
    pub desktop: bool,
}

impl App {
    /// Whether a limit saved under `id` concerns this app.
    pub fn answers_to(&self, id: &str) -> bool {
        self.id == id || self.aliases.iter().any(|a| a == id)
    }
}

struct Unit {
    name: String,
    id: String,
    desktop: bool,
    cgroup: CgroupPath,
    pids: Vec<u32>,
}

/// Lists apps with at least one process. `root` is normally [`CGROUP_ROOT`].
pub fn discover(root: &Path, uid: u32) -> io::Result<Vec<App>> {
    discover_in(root, Path::new("/proc"), uid)
}

fn discover_in(root: &Path, proc: &Path, uid: u32) -> io::Result<Vec<App>> {
    let mut units = Vec::new();
    let mut dirs = MAX_DIRS;
    scan(root, &app_slice(uid), uid, 0, &mut units, &mut dirs)?;
    let owners = find_owners(&units, proc);

    let mut found: BTreeMap<String, App> = BTreeMap::new();
    for (i, unit) in units.iter().enumerate() {
        let id = &units[owners[i]].id;
        let app = found.entry(id.clone()).or_insert_with(|| App {
            id: id.clone(),
            ..App::default()
        });
        if unit.id != *id && !app.aliases.contains(&unit.id) {
            app.aliases.push(unit.id.clone());
        }
        if app.cgroups.len() < MAX_CGROUPS_PER_APP {
            app.cgroups.push(unit.cgroup.clone());
        }
        app.processes += unit.pids.len();
        app.desktop |= unit.desktop;
    }
    let mut apps: Vec<App> = found.into_values().collect();
    // An id that still exists on its own is not an alias.
    let ids: Vec<String> = apps.iter().map(|a| a.id.clone()).collect();
    for app in &mut apps {
        app.aliases.retain(|a| !ids.contains(a));
        app.aliases.sort();
        app.cgroups.sort();
    }
    Ok(apps)
}

/// Maps each unit to the unit it belongs to, usually itself.
///
/// Chromium and Electron move their main process to a new `app-NAME-PID.scope`
/// but leave earlier children behind. So a scope whose own PID has same-named
/// children elsewhere belongs there. The reverse, a child in another group, is
/// an ordinary launch (a terminal, Steam) and must not be merged.
fn find_owners(units: &[Unit], proc: &Path) -> Vec<usize> {
    let mut owners: Vec<usize> = (0..units.len()).collect();
    let movers: Vec<(usize, u32)> = units
        .iter()
        .enumerate()
        .filter_map(|(i, u)| {
            let stem = u.name.strip_prefix("app-")?.strip_suffix(".scope")?;
            let pid = stem.rsplit_once('-')?.1.parse().ok()?;
            let moved = !stem.starts_with("loadguard-") && u.pids.contains(&pid);
            moved.then_some((i, pid))
        })
        .take(MAX_MOVERS)
        .collect();
    if movers.is_empty() {
        return owners;
    }
    // (unit, parent pid, name) of every process in a desktop unit.
    let processes: Vec<(usize, u32, String)> = units
        .iter()
        .enumerate()
        .filter(|(_, u)| u.desktop)
        .flat_map(|(i, u)| u.pids.iter().map(move |pid| (i, *pid)))
        .take(MAX_STAT_READS)
        .filter_map(|(i, pid)| read_stat(proc, pid).map(|(name, parent)| (i, parent, name)))
        .collect();
    for (unit, pid) in movers {
        let Some((name, _)) = read_stat(proc, pid) else {
            continue;
        };
        let mut children: BTreeMap<usize, usize> = BTreeMap::new();
        for (i, parent, child) in &processes {
            if *i != unit && *parent == pid && *child == name {
                *children.entry(*i).or_default() += 1;
            }
        }
        if let Some((target, _)) = children.into_iter().max_by_key(|(_, n)| *n) {
            // Follow earlier merges, and never create a cycle.
            let target = owners[target];
            if target != unit {
                for owner in &mut owners {
                    if *owner == unit {
                        *owner = target;
                    }
                }
            }
        }
    }
    owners
}

/// Name and parent of a process, which any user may read.
fn read_stat(proc: &Path, pid: u32) -> Option<(String, u32)> {
    let stat = fs::read(proc.join(pid.to_string()).join("stat")).ok()?;
    let stat = String::from_utf8_lossy(&stat);
    // The name may itself contain parentheses and spaces.
    let (head, tail) = stat.rsplit_once(')')?;
    let name = head.split_once('(')?.1.to_owned();
    let parent = tail.split_whitespace().nth(1)?.parse().ok()?;
    Some((name, parent))
}

fn scan(
    root: &Path,
    rel: &str,
    uid: u32,
    depth: usize,
    units: &mut Vec<Unit>,
    dirs: &mut usize,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if *dirs == 0 {
            return Ok(());
        }
        *dirs -= 1;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = format!("{rel}/{name}");
        if name.ends_with(".slice") {
            // Units come and go while we scan, so a vanished slice is not an error.
            let _ = scan(root, &path, uid, depth + 1, units, dirs);
            continue;
        }
        let Some((id, desktop)) = parse_unit(&name) else {
            continue;
        };
        let Ok(cgroup) = CgroupPath::for_user(&path, uid) else {
            continue;
        };
        let mut pids = Vec::new();
        collect_pids(&entry.path(), 0, &mut pids, dirs);
        if !pids.is_empty() {
            units.push(Unit {
                name,
                id,
                desktop,
                cgroup,
                pids,
            });
        }
    }
    Ok(())
}

fn collect_pids(dir: &Path, depth: usize, pids: &mut Vec<u32>, dirs: &mut usize) {
    if depth > MAX_DEPTH || pids.len() >= MAX_PIDS_PER_UNIT {
        return;
    }
    let own = fs::read_to_string(dir.join("cgroup.procs")).unwrap_or_default();
    let room = MAX_PIDS_PER_UNIT - pids.len();
    pids.extend(own.lines().filter_map(|l| l.parse::<u32>().ok()).take(room));
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if *dirs == 0 {
                return;
            }
            *dirs -= 1;
            collect_pids(&entry.path(), depth + 1, pids, dirs);
        }
    }
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

/// An app id for a launched program, made of characters that are safe in unit names.
pub fn launch_id(program: &str, apps: &[App]) -> String {
    let name = program.rsplit('/').next().unwrap_or_default();
    let base: String = name
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = base.trim_start_matches(['.', '-']);
    let base = if base.is_empty() { "app" } else { base };
    let taken = |id: &str| apps.iter().any(|a| a.answers_to(id));
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or_default()
}

/// The scope for an id from [`launch_id`], which [`discover`] maps back to that id.
pub fn launch_unit(id: &str, pid: u32) -> String {
    format!("app-loadguard-{}-{pid}.scope", id.replace('-', "\\x2d"))
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
    if let Some(app) = apps.iter().find(|a| a.answers_to(query)) {
        return Ok(app);
    }
    let q = query.to_lowercase();
    let hits: Vec<_> = apps
        .iter()
        .filter(|a| {
            [&a.id]
                .into_iter()
                .chain(&a.aliases)
                .any(|id| id.to_lowercase().contains(&q))
        })
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

    type FakeUnit<'a> = (&'a str, &'a [(u32, u32, &'a str)]);

    /// Units as (name, [(pid, parent, process name)]), returned as (id, aliases, cgroups).
    fn discover_fake(tag: &str, units: &[FakeUnit]) -> Vec<(String, Vec<String>, usize)> {
        let dir = std::env::temp_dir().join(format!("lg-apps-{}-{tag}", std::process::id()));
        for (unit, procs) in units {
            let cg = dir.join("cg").join(app_slice(1000)).join(unit);
            fs::create_dir_all(&cg).unwrap();
            let pids: Vec<String> = procs.iter().map(|p| p.0.to_string()).collect();
            fs::write(cg.join("cgroup.procs"), pids.join("\n")).unwrap();
            for (pid, parent, name) in *procs {
                let p = dir.join("proc").join(pid.to_string());
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("stat"), format!("{pid} ({name}) S {parent} 1 1")).unwrap();
            }
        }
        let apps = discover_in(&dir.join("cg"), &dir.join("proc"), 1000).unwrap();
        fs::remove_dir_all(dir).unwrap();
        apps.into_iter()
            .map(|a| (a.id, a.aliases, a.cgroups.len()))
            .collect()
    }

    #[test]
    fn merges_a_main_process_that_moved_itself() {
        let apps = discover_fake(
            "moved",
            &[
                (
                    "app-beeper.appimage@autostart.service",
                    &[(10, 1, "wrapper"), (12, 11, "a) b")],
                ),
                ("app-Beeper-11.scope", &[(11, 10, "a) b")]),
            ],
        );
        assert_eq!(apps, [("beeper.appimage".into(), vec!["Beeper".into()], 2)]);
    }

    #[test]
    fn follows_apps_started_with_run() {
        let apps = discover_fake(
            "run",
            &[
                ("app-loadguard-chromium-20.scope", &[(22, 20, "chromium")]),
                ("app-org.chromium.Chromium-20.scope", &[(20, 1, "chromium")]),
            ],
        );
        assert_eq!(
            apps,
            [("chromium".into(), vec!["org.chromium.Chromium".into()], 2)]
        );
    }

    #[test]
    fn keeps_ordinary_launches_separate() {
        let apps = discover_fake(
            "launch",
            &[
                // Started from a terminal, by Steam, and by a same-named shell.
                ("app-kitty@1.service", &[(30, 1, "kitty"), (31, 30, "sh")]),
                ("app-loadguard-wget-32.scope", &[(32, 31, "wget")]),
                ("app-steam@1.service", &[(40, 1, "steam")]),
                ("app-steam-app1-41.scope", &[(41, 40, "steam")]),
                ("app-loadguard-sh-50.scope", &[(50, 31, "sh")]),
                ("app-other-51.scope", &[(51, 50, "sh")]),
                // The moved process launched something with a different name.
                ("app-browser-60.scope", &[(60, 1, "browser")]),
                ("app-player@1.service", &[(61, 60, "player")]),
            ],
        );
        assert_eq!(apps.len(), 8);
        assert!(apps.iter().all(|a| a.1.is_empty() && a.2 == 1));
    }

    #[test]
    fn scanning_a_hostile_tree_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("lg-apps-{}-many", std::process::id()));
        let slice = dir.join(app_slice(1000));
        for i in 0..MAX_DIRS + 500 {
            let unit = slice.join(format!("app-x{i}@1.service"));
            fs::create_dir_all(&unit).unwrap();
            fs::write(unit.join("cgroup.procs"), "1\n").unwrap();
        }
        let started = std::time::Instant::now();
        let apps = discover_in(&dir, &dir.join("proc"), 1000).unwrap();
        assert!(apps.len() < MAX_DIRS + 1, "{} units scanned", apps.len());
        assert!(started.elapsed().as_secs() < 5);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launch_ids_are_unique_and_round_trip() {
        let app = |id: &str| App {
            id: id.into(),
            processes: 1,
            desktop: true,
            ..App::default()
        };
        assert_eq!(launch_id("/usr/bin/wget", &[]), "wget");
        assert_eq!(launch_id("wget", &[app("wget"), app("wget-2")]), "wget-3");
        assert_eq!(launch_id("./a b;\u{e9}@.x", &[]), "a_b___.x");
        assert_eq!(launch_id("", &[]), "app");
        for id in ["wget", "wget-2", "my-app.x_y", "flatpak-x"] {
            assert_eq!(
                parse_unit(&launch_unit(id, 42)),
                Some((id.to_owned(), true))
            );
        }
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
            processes: 1,
            desktop: true,
            ..App::default()
        };
        let apps = [app("steam"), app("steam-app1"), app("Zen")];
        assert_eq!(find(&apps, "steam").unwrap().id, "steam");
        assert_eq!(find(&apps, "zen").unwrap().id, "Zen");
        assert!(find(&apps, "stea").unwrap_err().contains("ambiguous"));
        assert!(find(&apps, "nope").is_err());
    }
}
