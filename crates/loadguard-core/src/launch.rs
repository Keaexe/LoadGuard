//! Starts a program in its own scope, limited before its first packet.
//!
//! The launcher re-executes a LoadGuard binary through `systemd-run`. Inside
//! the new scope that binary sets the limit, then becomes the program.

use crate::{
    App,
    apps::{self, CGROUP_ROOT},
    client::{self, Limits},
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{self, Command},
};

/// Hidden argument that selects the second stage in each LoadGuard binary.
pub const STAGE: &str = "run-in-scope";

/// Programs that hand the work to a process outside their own scope.
const ESCAPES: [&str; 6] = [
    "flatpak",
    "snap",
    "systemd-run",
    "gtk-launch",
    "xdg-open",
    "gio",
];

/// Explains why a program cannot be launched limited, if so.
pub fn refusal(program: &OsStr) -> Option<String> {
    let path = Path::new(program);
    let name = path.file_name()?.to_string_lossy();
    (ESCAPES.contains(&name.as_ref()) || path.starts_with("/snap/")).then(|| {
        format!(
            "{name} starts apps outside LoadGuard's control, so the limit would not hold; \
             start the app normally and limit it once it runs"
        )
    })
}

/// A free id for `program`. Saved limits reserve their id too, so a launch
/// never replaces them.
pub fn new_id(program: &OsStr, apps: &[App], limits: &Limits) -> String {
    let mut taken = apps.to_vec();
    taken.extend(limits.keys().map(|id| App {
        id: id.clone(),
        ..App::default()
    }));
    apps::launch_id(&program.to_string_lossy(), &taken)
}

/// What the second stage needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub id: String,
    pub down: u64,
    pub up: u64,
    /// Receives "ok" once the limit holds, or the reason nothing was launched.
    pub report: Option<PathBuf>,
    pub command: Vec<OsString>,
}

impl Stage {
    /// Reads the arguments after [`STAGE`].
    pub fn parse(args: &[OsString]) -> Result<Self, String> {
        let split = args.iter().position(|a| a == "--").ok_or("missing --")?;
        let (options, command) = (&args[..split], &args[split + 1..]);
        if command.is_empty() {
            return Err("missing command".into());
        }
        let mut stage = Self {
            id: String::new(),
            down: 0,
            up: 0,
            report: None,
            command: command.to_vec(),
        };
        for option in options {
            let text = option.to_str().ok_or("invalid argument")?;
            let (key, value) = text.split_once('=').ok_or("invalid argument")?;
            let number = || value.parse::<u64>().map_err(|_| format!("invalid {key}"));
            match key {
                "--id" => stage.id = value.to_owned(),
                "--down" => stage.down = number()?,
                "--up" => stage.up = number()?,
                "--report" => stage.report = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown argument {key}")),
            }
        }
        if stage.id.is_empty() {
            return Err("missing --id".into());
        }
        Ok(stage)
    }

    fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            STAGE.into(),
            format!("--id={}", self.id).into(),
            format!("--down={}", self.down).into(),
            format!("--up={}", self.up).into(),
        ];
        if let Some(report) = &self.report {
            let mut arg = OsString::from("--report=");
            arg.push(report);
            args.push(arg);
        }
        args.push("--".into());
        args.extend(self.command.iter().cloned());
        args
    }
}

/// The `systemd-run` call that starts `exe` in a new scope for `stage`.
pub fn command(exe: &Path, stage: &Stage) -> Command {
    let mut command = Command::new("systemd-run");
    command
        // Without this, systemd-run would rewrite $VARIABLES in arguments.
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--slice=app.slice",
        ])
        .arg("--expand-environment=no")
        .arg(format!(
            "--unit={}",
            apps::launch_unit(&stage.id, process::id())
        ))
        .arg("--")
        .arg(exe)
        .args(stage.args());
    command
}

/// Runs inside the new scope: limits it, then becomes the program. Only
/// returns on failure, after reporting it.
pub fn enter(stage: &Stage, uid: u32) -> String {
    let error = prepare(stage, uid).err().unwrap_or_else(|| {
        report(stage, "ok");
        let error = Command::new(&stage.command[0])
            .args(&stage.command[1..])
            .exec();
        format!(
            "cannot start {}: {error}",
            stage.command[0].to_string_lossy()
        )
    });
    report(stage, &format!("error\n{error}"));
    error
}

fn prepare(stage: &Stage, uid: u32) -> Result<(), String> {
    let program = &stage.command[0];
    if find_program(program).is_none() {
        return Err(format!("{} was not found", program.to_string_lossy()));
    }
    let own = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let own = own
        .trim()
        .strip_prefix("0::/")
        .unwrap_or_default()
        .to_owned();
    let apps = apps::discover(Path::new(CGROUP_ROOT), uid).unwrap_or_default();
    let covered = apps
        .iter()
        .any(|a| a.answers_to(&stage.id) && a.cgroups.iter().any(|c| c.as_str() == own));
    if !covered {
        return Err("could not enter a separate group, so nothing was launched".into());
    }
    client::connect()
        .and_then(|h| {
            h.set_limit(&stage.id, stage.down, stage.up, true)
                .map_err(client::explain)
        })
        .map_err(|e| format!("{e}; nothing was launched"))
}

fn report(stage: &Stage, text: &str) {
    if let Some(path) = &stage.report {
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
}

/// Resolves a program the way exec does, so a missing one is reported
/// before anything is said to have launched.
pub fn find_program(program: &OsStr) -> Option<PathBuf> {
    let executable = |p: &Path| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if program.as_bytes().contains(&b'/') {
        return Some(PathBuf::from(program)).filter(|p| executable(p));
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(program))
        .find(|p| executable(p))
}

/// Splits a command line like a shell would, without running one: quotes and
/// backslashes are understood, but variables, globs and pipes are not.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unclosed quote".into()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("unclosed quote".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("unclosed quote".into()),
                    }
                }
            }
            '\\' => {
                started = true;
                word.push(chars.next().ok_or("nothing after backslash")?);
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_arguments_round_trip() {
        let stage = Stage {
            id: "my-app".into(),
            down: 500_000,
            up: 0,
            report: Some("/run/x y".into()),
            command: vec!["sh".into(), "-c".into(), "--".into(), "$HOME".into()],
        };
        assert_eq!(Stage::parse(&stage.args()[1..]).unwrap(), stage);
        let no_report = Stage {
            report: None,
            ..stage
        };
        assert_eq!(Stage::parse(&no_report.args()[1..]).unwrap(), no_report);
        for bad in [
            &["--id=x"][..],
            &["--id=x", "--"],
            &["--", "ls"],
            &["--id=x", "--down=a", "--", "ls"],
            &["--x=1", "--", "ls"],
        ] {
            let args: Vec<OsString> = bad.iter().map(Into::into).collect();
            assert!(Stage::parse(&args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn refuses_programs_that_escape() {
        assert!(refusal(OsStr::new("/usr/bin/flatpak")).is_some());
        assert!(refusal(OsStr::new("/snap/bin/firefox")).is_some());
        assert!(refusal(OsStr::new("wget")).is_none());
    }

    #[test]
    fn splits_like_a_shell_without_running_one() {
        let ok = |s: &str| split(s).unwrap();
        assert_eq!(
            ok(" wget  -O 'a b' \"c \\\"d\\\"\" e\\ f $HOME ''"),
            ["wget", "-O", "a b", "c \"d\"", "e f", "$HOME", ""]
        );
        assert_eq!(ok("a;b|c"), ["a;b|c"]);
        assert!(ok("   ").is_empty());
        assert!(split("'open").is_err());
        assert!(split("x\\").is_err());
    }

    #[test]
    fn finds_programs_like_exec() {
        assert!(find_program(OsStr::new("sh")).is_some());
        assert!(find_program(OsStr::new("/bin/sh")).is_some());
        assert!(find_program(OsStr::new("/etc/hostname")).is_none());
        assert!(find_program(OsStr::new("no-such-program-lg")).is_none());
    }
}
