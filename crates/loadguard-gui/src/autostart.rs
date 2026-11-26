//! Starting hidden in the tray at login, through an XDG autostart entry.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const FILE: &str = "io.github.keaexe.LoadGuard.desktop";
/// Marks the entry as ours, so a file the user wrote is never removed.
const MARKER: &str = "X-LoadGuard-Autostart=true";

pub fn path() -> Option<PathBuf> {
    let absolute = |v| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    let config = env::var_os("XDG_CONFIG_HOME")
        .and_then(absolute)
        .or_else(|| {
            env::var_os("HOME")
                .and_then(absolute)
                .map(|h| h.join(".config"))
        })?;
    Some(config.join("autostart").join(FILE))
}

pub fn is_enabled(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|text| {
        let hidden = text.lines().any(|l| l.trim() == "Hidden=true");
        text.contains(MARKER) && !hidden
    })
}

pub fn set(path: &Path, enabled: bool, exe: &Path) -> Result<(), String> {
    let err = |e: io::Error| format!("{}: {e}", path.display());
    if !enabled {
        return match fs::read_to_string(path) {
            Ok(text) if text.contains(MARKER) => fs::remove_file(path).map_err(err),
            Ok(_) => Err(format!(
                "{} was not made by LoadGuard; remove it yourself",
                path.display()
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err(e)),
        };
    }
    let exe = exe.to_str().filter(|p| {
        p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c))
    });
    let Some(exe) = exe else {
        return Err("LoadGuard is installed in a folder it cannot start from at login".into());
    };
    // An installed copy is found on PATH, which survives package updates.
    let exec = if Path::new(exe).parent() == Some(Path::new("/usr/bin")) {
        "loadguard-gui"
    } else {
        exe
    };
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=LoadGuard\nComment=Keeps LoadGuard in the system tray\n\
         Exec={exec} --hidden\nIcon=io.github.keaexe.LoadGuard\nTerminal=false\nNoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n{MARKER}\n"
    );
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(err)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(err)?;
    fs::rename(&tmp, path).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_removes_only_its_own_entry() {
        let dir = env::temp_dir().join(format!("lg-autostart-{}", std::process::id()));
        let path = dir.join("autostart").join(FILE);
        assert!(!is_enabled(&path));
        set(&path, true, Path::new("/usr/bin/loadguard-gui")).unwrap();
        assert!(is_enabled(&path));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=loadguard-gui --hidden\n")
        );
        set(&path, true, Path::new("/opt/lg/loadguard-gui")).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=/opt/lg/loadguard-gui --hidden\n")
        );
        assert!(set(&path, true, Path::new("/opt/my lg/loadguard-gui")).is_err());
        set(&path, false, Path::new("")).unwrap();
        assert!(!path.exists());
        set(&path, false, Path::new("")).unwrap();
        fs::write(&path, "[Desktop Entry]\nExec=mine\n").unwrap();
        assert!(!is_enabled(&path));
        assert!(set(&path, false, Path::new("")).is_err());
        assert!(path.exists(), "a file the user wrote is kept");
        fs::write(&path, format!("{MARKER}\nHidden=true\n")).unwrap();
        assert!(!is_enabled(&path), "disabled in the desktop's settings");
        fs::remove_dir_all(dir).unwrap();
    }
}
