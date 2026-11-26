//! Names and icons from desktop entries.

use loadguard_core::{App, launch};
use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub icon: Option<PathBuf>,
}

/// An installed app that can be launched.
#[derive(Debug, Clone, PartialEq)]
pub struct Launcher {
    pub entry: Entry,
    pub command: Vec<String>,
    pub dir: Option<PathBuf>,
    /// Why it cannot be launched limited, if so.
    pub refusal: Option<String>,
}

/// Desktop entries by every lower-case key an app id may match, and the
/// installed apps that can be launched.
#[derive(Default)]
pub struct Entries {
    by_key: HashMap<String, Entry>,
    pub launchers: Vec<Launcher>,
}

impl Entries {
    pub fn load() -> Self {
        Self::load_from(&data_dirs(), &language(), icon_theme().as_deref())
    }

    fn load_from(dirs: &[PathBuf], lang: &str, theme: Option<&str>) -> Self {
        let mut exact = HashMap::new();
        let mut loose = HashMap::new();
        let mut launchers: Vec<Launcher> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Earlier directories take precedence.
        for dir in dirs {
            let Ok(files) = fs::read_dir(dir.join("applications")) else {
                continue;
            };
            let mut files: Vec<PathBuf> = files.flatten().map(|f| f.path()).collect();
            files.sort();
            for path in files {
                let (Some(stem), Some("desktop")) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    path.extension().and_then(|e| e.to_str()),
                ) else {
                    continue;
                };
                let Some(raw) = fs::read_to_string(&path).ok().and_then(|t| parse(&t, lang)) else {
                    continue;
                };
                let raw_icon = raw.icon.clone();
                let entry = Entry {
                    name: raw.name.clone(),
                    icon: raw.icon.as_deref().and_then(|i| find_icon(i, dirs, theme)),
                };
                let stem = stem.to_lowercase();
                // Same file name in a later directory is shadowed, as in menus.
                if raw.visible
                    && seen.insert(stem.clone())
                    && let Some(command) = raw
                        .exec
                        .as_deref()
                        .and_then(|e| expand_exec(e, &entry.name, raw_icon.as_deref(), &path))
                {
                    let refusal = launch::refusal(OsStr::new(&command[0]));
                    launchers.push(Launcher {
                        entry: entry.clone(),
                        command,
                        dir: raw.dir.clone(),
                        refusal,
                    });
                }
                if raw.visible {
                    let last = stem.rsplit('.').next().unwrap_or_default().to_owned();
                    for key in [raw.wm_class, raw.program, Some(last)]
                        .into_iter()
                        .flatten()
                    {
                        loose
                            .entry(key.to_lowercase())
                            .or_insert_with(|| entry.clone());
                    }
                }
                exact.entry(stem).or_insert(entry);
            }
        }
        for (key, entry) in loose {
            exact.entry(key).or_insert(entry);
        }
        launchers.sort_by_cached_key(|l| l.entry.name.to_lowercase());
        launchers.dedup_by(|a, b| a.entry.name == b.entry.name && a.command == b.command);
        Self {
            by_key: exact,
            launchers,
        }
    }

    /// Looks an app up by its id, then by its other groups.
    pub fn get(&self, id: &str, app: Option<&App>) -> Option<&Entry> {
        let aliases = app.map_or(&[][..], |a| &a.aliases);
        let own = app.map(|a| a.id.as_str());
        [id].into_iter()
            .chain(own)
            .chain(aliases.iter().map(String::as_str))
            .find_map(|id| self.by_key.get(&id.to_lowercase()))
    }
}

struct Raw {
    name: String,
    exec: Option<String>,
    dir: Option<PathBuf>,
    icon: Option<String>,
    program: Option<String>,
    wm_class: Option<String>,
    visible: bool,
}

fn parse(text: &str, lang: &str) -> Option<Raw> {
    let mut fields = HashMap::new();
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            fields.entry(key.trim()).or_insert(value.trim());
        }
    }
    let get = |key: &str| {
        fields
            .get(key)
            .map(|v| (*v).to_owned())
            .filter(|v| !v.is_empty())
    };
    let short = lang.split('_').next().unwrap_or_default();
    let name = get(&format!("Name[{lang}]"))
        .or_else(|| get(&format!("Name[{short}]")))
        .or_else(|| get("Name"))?;
    // Generic runners would match unrelated apps.
    let program = get("Exec")
        .and_then(|e| {
            let first = e.split_whitespace().next()?.trim_matches('"').to_owned();
            Some(first.rsplit('/').next()?.to_owned())
        })
        .filter(|p| {
            !["env", "sh", "bash", "flatpak", "steam", "python3", "java"].contains(&p.as_str())
        });
    let installed = get("TryExec").is_none_or(|t| launch::find_program(OsStr::new(&t)).is_some());
    let application = get("Type").as_deref() == Some("Application");
    Some(Raw {
        exec: get("Exec").filter(|_| installed && application),
        dir: get("Path").map(PathBuf::from).filter(|p| p.is_absolute()),
        name: name.replace(char::is_control, " "),
        icon: get("Icon"),
        program,
        wm_class: get("StartupWMClass"),
        visible: get("NoDisplay").as_deref() != Some("true")
            && get("Hidden").as_deref() != Some("true"),
    })
}

/// Turns an Exec value into arguments, following the desktop entry spec:
/// file and URL codes are dropped, since nothing is opened.
fn expand_exec(exec: &str, name: &str, icon: Option<&str>, file: &Path) -> Option<Vec<String>> {
    // Values may escape characters once more for the file format itself.
    let unescaped = exec.replace("\\s", " ").replace("\\\\", "\\");
    let mut out = Vec::new();
    for word in launch::split(&unescaped).ok()? {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            "%i" => {
                if let Some(icon) = icon {
                    out.extend(["--icon".to_owned(), icon.to_owned()]);
                }
            }
            _ => {
                let mut text = String::new();
                let mut chars = word.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        text.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('%') => text.push('%'),
                        Some('c') => text.push_str(name),
                        Some('k') => text.push_str(&file.to_string_lossy()),
                        _ => {}
                    }
                }
                out.push(text);
            }
        }
    }
    (!out.is_empty() && !out[0].is_empty()).then_some(out)
}

fn find_icon(icon: &str, dirs: &[PathBuf], theme: Option<&str>) -> Option<PathBuf> {
    if icon.starts_with('/') {
        return Some(PathBuf::from(icon)).filter(|p| supported(p) && p.is_file());
    }
    if icon.contains('/') {
        return None;
    }
    for dir in dirs {
        for size in [
            "64x64", "48x48", "128x128", "256x256", "scalable", "512x512", "32x32",
        ] {
            for ext in ["png", "svg"] {
                let path = dir.join(format!("icons/hicolor/{size}/apps/{icon}.{ext}"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    // Some icons exist only in the user's theme. Themes use either directory order.
    if let Some(theme) = theme {
        for dir in dirs {
            let base = dir.join("icons").join(theme);
            for sub in [
                "apps/scalable",
                "scalable/apps",
                "apps/64",
                "64x64/apps",
                "apps/48",
                "48x48/apps",
            ] {
                for ext in ["svg", "png"] {
                    let path = base.join(format!("{sub}/{icon}.{ext}"));
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
    }
    dirs.iter()
        .flat_map(|d| {
            ["icons", "pixmaps"].into_iter().flat_map(move |sub| {
                ["png", "svg"].map(|ext| d.join(format!("{sub}/{icon}.{ext}")))
            })
        })
        .find(|p| p.is_file())
}

/// The icon theme chosen in KDE, or else in GTK.
fn icon_theme() -> Option<String> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let read = |file: &str, section: &str, key: &str| {
        let text = fs::read_to_string(config.join(file)).ok()?;
        let mut here = false;
        text.lines().map(str::trim).find_map(|line| {
            if line.starts_with('[') {
                here = line == section;
            }
            let (k, v) = line.split_once('=').filter(|_| here)?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    };
    read("kdeglobals", "[Icons]", "Theme")
        .or_else(|| read("gtk-4.0/settings.ini", "[Settings]", "gtk-icon-theme-name"))
        .or_else(|| read("gtk-3.0/settings.ini", "[Settings]", "gtk-icon-theme-name"))
        .filter(|t| !t.is_empty() && !t.contains('/') && t != "..")
}

fn supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "png" || e == "svg")
}

fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let system = env::var_os("XDG_DATA_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(env::split_paths(&system))
        .filter(|p| p.is_absolute())
        .collect()
}

fn language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| env::var(v).ok().filter(|v| !v.is_empty()))
        .map(|l| l.split(['.', '@']).next().unwrap_or_default().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ids_names_and_icons() {
        let dir = env::temp_dir().join(format!("lg-desktop-{}", std::process::id()));
        let apps = dir.join("applications");
        let icons = dir.join("icons/hicolor/48x48/apps");
        fs::create_dir_all(&apps).unwrap();
        fs::create_dir_all(&icons).unwrap();
        fs::write(icons.join("zen.png"), "").unwrap();
        let write = |name: &str, body: &str| {
            fs::write(
                apps.join(name),
                format!("[Desktop Entry]\n{body}\n[Desktop Action x]\nName=No"),
            )
            .unwrap();
        };
        write(
            "zen.desktop",
            "Name=Zen Browser\nName[fr]=Navigateur Zen\nIcon=zen",
        );
        write(
            "org.x.Mail.desktop",
            "Name=Mail\nIcon=mail-icon\nExec=/usr/bin/mailer %u\nStartupWMClass=MailWin",
        );
        write("hidden.desktop", "Name=Hidden\nNoDisplay=true\nExec=secret");
        write("game.desktop", "Name=Game\nExec=steam steam://1");
        write(
            "flat.desktop",
            "Type=Application\nName=Flat\nExec=/usr/bin/flatpak run org.x.Flat",
        );
        write(
            "gone.desktop",
            "Type=Application\nName=Gone\nTryExec=no-such-program-lg\nExec=gone",
        );
        write(
            "tool.desktop",
            "Type=Application\nName=Tool\nIcon=zen\nPath=/tmp\nExec=\"/opt/my tool/run\" --name=%c %i --open %U 100%%",
        );
        let themed = dir.join("icons/mine/apps/scalable");
        fs::create_dir_all(&themed).unwrap();
        fs::write(themed.join("mail-icon.svg"), "").unwrap();
        let e = Entries::load_from(std::slice::from_ref(&dir), "fr_BE", Some("mine"));
        assert!(e.get("mail", None).unwrap().icon.is_some());
        assert_eq!(e.get("zen", None).unwrap().name, "Navigateur Zen");
        assert!(e.get("ZEN", None).unwrap().icon.is_some());
        for id in ["org.x.Mail", "mail", "mailer", "mailwin"] {
            assert_eq!(e.get(id, None).unwrap().name, "Mail", "{id}");
        }
        assert!(e.get("secret", None).is_none());
        assert!(e.get("steam", None).is_none());
        let app = App {
            id: "cef".into(),
            aliases: vec!["Mail".into()],
            ..App::default()
        };
        assert_eq!(e.get("cef", Some(&app)).unwrap().name, "Mail");
        let launchers: Vec<_> = e
            .launchers
            .iter()
            .map(|l| (l.entry.name.as_str(), l.refusal.is_some()))
            .collect();
        assert_eq!(launchers, [("Flat", true), ("Tool", false)]);
        let tool = &e.launchers[1];
        assert_eq!(
            tool.command,
            [
                "/opt/my tool/run",
                "--name=Tool",
                "--icon",
                "zen",
                "--open",
                "100%"
            ]
        );
        assert_eq!(tool.dir.as_deref(), Some(Path::new("/tmp")));
        fs::remove_dir_all(dir).unwrap();
    }
}
