use clap::{Parser, Subcommand};
use loadguard_core::{
    Config, Limit, Rate,
    apps::{self, App, CGROUP_ROOT},
    client::{self, Limits, connect as helper, explain, limits},
    nft,
};
use std::{
    env, ffi::OsString, os::unix::fs::MetadataExt, os::unix::process::CommandExt, path::Path,
    process, process::ExitCode, str::FromStr,
};

/// Limit the download and upload speed of running apps.
#[derive(Parser)]
#[command(name = "loadguard", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show running apps and their limits
    List {
        /// Include background services
        #[arg(long)]
        all: bool,
    },
    /// Limit a running app; a direction you leave out keeps its current limit
    Limit {
        /// App id, or any unambiguous part of it
        app: String,
        /// Download limit, such as 500K or 20Mbit (KB/s if no unit), or "none"
        #[arg(short, long, required_unless_present = "up")]
        down: Option<RateArg>,
        /// Upload limit, or "none"
        #[arg(short, long)]
        up: Option<RateArg>,
        /// Keep the limit when the app or the computer restarts
        #[arg(long)]
        always: bool,
        /// Forget the limit when the app closes (the default for new limits)
        #[arg(long, conflicts_with = "always")]
        until_closed: bool,
        /// Print the rules without applying anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Launch a program with a limit that applies from its first packet
    Run {
        /// Download limit, such as 500K or 20Mbit (KB/s if no unit)
        #[arg(short, long, required_unless_present = "up")]
        down: Option<Rate>,
        /// Upload limit
        #[arg(short, long)]
        up: Option<Rate>,
        /// Program and its arguments
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Second stage of `run`, inside the new scope
    #[command(name = "run-in-scope", hide = true)]
    RunInScope {
        #[arg(long)]
        id: String,
        #[arg(long, default_value_t = 0)]
        down: u64,
        #[arg(long, default_value_t = 0)]
        up: u64,
        #[arg(required = true, last = true)]
        command: Vec<OsString>,
    },
    /// Remove an app's limit, or every limit
    Clear {
        /// App id, or any unambiguous part of it
        #[arg(required_unless_present = "all")]
        app: Option<String>,
        #[arg(long, conflicts_with = "app")]
        all: bool,
    },
}

#[derive(Clone, Copy)]
struct RateArg(Option<Rate>);

impl FromStr for RateArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "none" {
            Ok(Self(None))
        } else {
            s.parse().map(|r| Self(Some(r)))
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    let path = Config::default_path().ok_or("cannot locate the configuration directory")?;
    let config = Config::load(&path)?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    let apps = || {
        apps::discover(Path::new(CGROUP_ROOT), uid).map_err(|e| {
            format!("cannot read app cgroups (cgroup v2 and systemd are required): {e}")
        })
    };
    let current = || helper().and_then(|h| limits(&h));

    match command {
        Command::List { all } => {
            let limits = current().unwrap_or_else(|e| {
                eprintln!("note: {e}; limits are not shown");
                Limits::new()
            });
            list(&apps()?, &limits, all);
        }
        Command::Limit {
            app,
            down,
            up,
            always,
            until_closed,
            dry_run,
        } => {
            if !config.backends.nftables {
                return Err(format!(
                    "the nftables backend is disabled in {}",
                    path.display()
                ));
            }
            let apps = apps()?;
            let id = apps::find(&apps, &app)?.id.clone();
            let mut limits = match current() {
                Err(_) if dry_run => Limits::new(),
                other => other?,
            };
            // A changed limit keeps its duration unless told otherwise.
            let until_closed = until_closed || (!always && limits.get(&id).is_none_or(|l| l.1));
            let mut limit = limits.get(&id).map(|l| l.0).unwrap_or_default();
            limit.down = down.map_or(limit.down, |r| r.0);
            limit.up = up.map_or(limit.up, |r| r.0);
            let (d, u) = (client::encode(limit.down), client::encode(limit.up));
            if d == 0 && u == 0 {
                return Err(format!("nothing left to limit; use `loadguard clear {id}`"));
            }
            if dry_run {
                limits.insert(id, (limit, until_closed));
                print!("{}", rules(uid, &limits, &apps));
            } else {
                helper()?
                    .set_limit(&id, d, u, until_closed)
                    .map_err(explain)?;
                println!(
                    "{id}: download {}, upload {}",
                    show(limit.down),
                    show(limit.up)
                );
            }
        }
        Command::Run { down, up, command } => {
            if !config.backends.nftables {
                return Err(format!(
                    "the nftables backend is disabled in {}",
                    path.display()
                ));
            }
            // Fail before launching anything if the helper is unavailable.
            let limits = current()?;
            return Err(launch(down, up, &command, &apps()?, &limits));
        }
        Command::RunInScope {
            id,
            down,
            up,
            command,
        } => {
            return Err(launch_in_scope(&id, down, up, &command, &apps()?));
        }
        Command::Clear {
            app: Some(query), ..
        } => {
            let helper = helper()?;
            // Saved limits can be cleared even if apps cannot be listed.
            let id = find_limit(&query, &limits(&helper)?, &apps().unwrap_or_default())?;
            helper.clear_limit(&id).map_err(explain)?;
            println!("{id}: limit removed");
        }
        Command::Clear { app: None, .. } => {
            helper()?.clear_all().map_err(explain)?;
            println!("All limits removed");
        }
    }
    Ok(())
}

/// Moves into a new scope. Only returns on failure.
fn launch(
    down: Option<Rate>,
    up: Option<Rate>,
    command: &[OsString],
    apps: &[App],
    limits: &Limits,
) -> String {
    let program = command[0].to_string_lossy();
    let name = program.rsplit('/').next().unwrap_or_default();
    if ["flatpak", "snap", "systemd-run", "gtk-launch", "xdg-open"].contains(&name) {
        return format!(
            "'{name}' starts programs outside its own group, so the limit would not hold; \
             start the app normally and use `loadguard limit`"
        );
    }
    // Saved limits reserve their id too, so a launch never replaces them.
    let mut taken = apps.to_vec();
    taken.extend(limits.keys().map(|id| App {
        id: id.clone(),
        ..App::default()
    }));
    let id = apps::launch_id(&program, &taken);
    if apps.iter().any(|a| a.id.eq_ignore_ascii_case(name)) {
        eprintln!(
            "warning: {name} is already running; if it reuses that window, \
             this limit will not affect it"
        );
    }
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return e.to_string(),
    };
    let error = process::Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--slice=app.slice",
            "--expand-environment=no",
        ])
        .arg(format!("--unit={}", apps::launch_unit(&id, process::id())))
        .arg("--")
        .arg(exe)
        .arg("run-in-scope")
        .arg(format!("--id={id}"))
        .arg(format!("--down={}", client::encode(down)))
        .arg(format!("--up={}", client::encode(up)))
        .arg("--")
        .args(command)
        .exec();
    format!("cannot run systemd-run: {error}")
}

/// Limits this process's scope, then becomes `command`. Only returns on failure.
fn launch_in_scope(id: &str, down: u64, up: u64, command: &[OsString], apps: &[App]) -> String {
    let own = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let own = own.trim().strip_prefix("0::/").unwrap_or_default();
    let covered = apps
        .iter()
        .any(|a| a.answers_to(id) && a.cgroups.iter().any(|c| c.as_str() == own));
    if !covered {
        return "could not enter a separate group, so nothing was launched".into();
    }
    if let Err(e) = helper().and_then(|h| h.set_limit(id, down, up, true).map_err(explain)) {
        return format!("{e}; nothing was launched");
    }
    let error = process::Command::new(&command[0])
        .args(&command[1..])
        .exec();
    format!("cannot start {}: {error}", command[0].to_string_lossy())
}

/// Finds the limit meant by `query`: an exact id, then an exact name of a limited
/// app in any case, then an unambiguous part of one.
fn find_limit(query: &str, limits: &Limits, apps: &[App]) -> Result<String, String> {
    if limits.contains_key(query) {
        return Ok(query.to_owned());
    }
    if query.is_empty() {
        return Err("the app name is empty".into());
    }
    let query_lower = query.to_lowercase();
    let matching = |test: &dyn Fn(&str) -> bool| -> Vec<&String> {
        limits
            .keys()
            .filter(|id| {
                let mut names = vec![id.as_str()];
                for app in apps.iter().filter(|a| a.answers_to(id)) {
                    names.push(&app.id);
                    names.extend(app.aliases.iter().map(String::as_str));
                }
                names.iter().any(|n| test(&n.to_lowercase()))
            })
            .collect()
    };
    let mut hits = matching(&|name| name == query_lower);
    if hits.is_empty() {
        hits = matching(&|name| name.contains(&query_lower));
    }
    match hits[..] {
        [id] => Ok(id.clone()),
        [] => Err(format!("no limit matches '{query}' (see `loadguard list`)")),
        _ => {
            let ids: Vec<_> = hits.iter().map(|id| id.as_str()).collect();
            Err(format!("'{query}' is ambiguous: {}", ids.join(", ")))
        }
    }
}

fn rules(uid: u32, limits: &Limits, apps: &[App]) -> String {
    let entries: Vec<_> = limits
        .iter()
        .filter_map(|(id, (limit, _))| {
            let app = apps.iter().find(|a| a.answers_to(id))?;
            Some(nft::Entry {
                cgroups: app.cgroups.clone(),
                limit: *limit,
            })
        })
        .collect();
    nft::ruleset(uid, &entries, true)
}

fn show(rate: Option<Rate>) -> String {
    rate.map_or_else(|| "unlimited".into(), |r| r.to_string())
}

fn list(apps: &[App], limits: &Limits, all: bool) {
    let row = |name: &str, procs: usize, entry: Option<&(Limit, bool)>| {
        let (down, up, kept) = match entry {
            Some((l, temp)) => {
                let kept = if *temp { "until closed" } else { "always" };
                (show(l.down), show(l.up), kept)
            }
            None => ("-".into(), "-".into(), ""),
        };
        let line = format!("{name:<40} {procs:>5} {down:>12} {up:>12}  {kept}");
        println!("{}", line.trim_end());
    };
    println!("{:<40} {:>5} {:>12} {:>12}", "APP", "PROCS", "DOWN", "UP");
    for app in apps {
        // A limit saved under an alias gets its own row, named as it must be cleared.
        let mut own: Vec<_> = limits.iter().filter(|(id, _)| app.answers_to(id)).collect();
        own.sort_by_key(|(id, _)| **id != app.id);
        if own.is_empty() && (all || app.desktop) {
            row(&app.id, app.processes, None);
        }
        for (id, entry) in own {
            row(id, app.processes, Some(entry));
        }
        if !app.aliases.is_empty() && (all || app.desktop) {
            println!("  also covers: {}", app.aliases.join(", "));
        }
    }
    for (id, entry) in limits {
        if !apps.iter().any(|a| a.answers_to(id)) {
            row(id, 0, Some(entry));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_limit_to_clear() {
        let limits: Limits = ["beeper.appimage", "steam", "steam-app1", "org.x.Closed"]
            .map(|id| (id.to_owned(), (Limit::default(), false)))
            .into();
        let apps = [
            App {
                id: "beeper.appimage".into(),
                aliases: vec!["Beeper".into(), "Chat".into()],
                ..App::default()
            },
            App {
                id: "zen".into(),
                ..App::default()
            },
        ];
        let find = |q| find_limit(q, &limits, &apps);
        assert_eq!(find("beeper").unwrap(), "beeper.appimage");
        assert_eq!(find("chat").unwrap(), "beeper.appimage");
        assert_eq!(find("steam").unwrap(), "steam");
        assert_eq!(find("STEAM").unwrap(), "steam");
        assert_eq!(find("app1").unwrap(), "steam-app1");
        assert_eq!(find("closed").unwrap(), "org.x.Closed");
        assert!(find("stea").unwrap_err().contains("ambiguous"));
        assert!(find("zen").is_err(), "running, but not limited");
        assert!(find("").unwrap_err().contains("empty"));
    }
}
