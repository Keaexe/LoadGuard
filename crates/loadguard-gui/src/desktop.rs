//! Names and icons from desktop entries.

use loadguard_core::App;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub icon: Option<PathBuf>,
}

/// Desktop entries by every lower-case key an app id may match.
#[derive(Default)]
pub struct Entries(HashMap<String, Entry>);

impl Entries {
    pub fn load() -> Self {
        Self::load_from(&data_dirs(), &language(), icon_theme().as_deref())
    }

    fn load_from(dirs: &[PathBuf], lang: &str, theme: Option<&str>) -> Self {
        let mut exact = HashMap::new();
        let mut loose = HashMap::new();
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
                let entry = Entry {
                    name: raw.name,
                    icon: raw.icon.and_then(|i| find_icon(&i, dirs, theme)),
                };
                let stem = stem.to_lowercase();
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
        Self(exact)
    }

    /// Looks an app up by its id, then by its other groups.
    pub fn get(&self, id: &str, app: Option<&App>) -> Option<&Entry> {
        let aliases = app.map_or(&[][..], |a| &a.aliases);
        let own = app.map(|a| a.id.as_str());
        [id].into_iter()
            .chain(own)
            .chain(aliases.iter().map(String::as_str))
            .find_map(|id| self.0.get(&id.to_lowercase()))
    }
}

struct Raw {
    name: String,
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
    Some(Raw {
        name: name.replace(char::is_control, " "),
        icon: get("Icon"),
        program,
        wm_class: get("StartupWMClass"),
        visible: get("NoDisplay").as_deref() != Some("true")
            && get("Hidden").as_deref() != Some("true"),
    })
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
        fs::remove_dir_all(dir).unwrap();
    }
}
