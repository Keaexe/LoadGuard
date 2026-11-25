use clap::{Parser, Subcommand};
use loadguard_core::{
    Config, Limit, Rate,
    apps::{self, App, CGROUP_ROOT},
    client::{self, HelperProxy},
    nft,
};
use std::{
    collections::BTreeMap, os::unix::fs::MetadataExt, path::Path, process::ExitCode, str::FromStr,
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
        /// Forget the limit when the app closes, instead of keeping it across reboots
        #[arg(long)]
        until_closed: bool,
        /// Print the rules without applying anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove an app's limit, or every limit
    Clear {
        /// App id, exactly as listed
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

type Limits = BTreeMap<String, (Limit, bool)>;

fn helper() -> Result<HelperProxy<'static>, String> {
    zbus::blocking::Connection::system()
        .and_then(|c| HelperProxy::new(&c))
        .map_err(|e| format!("cannot reach the system bus: {e}"))
}

fn explain(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) if name.as_str().ends_with(".ServiceUnknown") => {
            "the LoadGuard helper is not installed".into()
        }
        zbus::Error::MethodError(_, Some(text), _) => text,
        other => other.to_string(),
    }
}

fn limits(helper: &HelperProxy) -> Result<Limits, String> {
    let rows = helper.list_limits().map_err(explain)?;
    rows.into_iter()
        .map(|(id, down, up, temp)| Ok((id, (client::decode(down, up)?, temp))))
        .collect()
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
        Command::Clear { app: Some(id), .. } => {
            helper()?.clear_limit(&id).map_err(explain)?;
            println!("{id}: limit removed");
        }
        Command::Clear { app: None, .. } => {
            helper()?.clear_all().map_err(explain)?;
            println!("All limits removed");
        }
    }
    Ok(())
}

fn rules(uid: u32, limits: &Limits, apps: &[App]) -> String {
    let entries: Vec<_> = limits
        .iter()
        .filter_map(|(id, (limit, _))| {
            let app = apps.iter().find(|a| &a.id == id)?;
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
    let row = |id: &str, procs: usize| {
        let (down, up, kept) = match limits.get(id) {
            Some((l, temp)) => (
                show(l.down),
                show(l.up),
                if *temp { "until closed" } else { "always" },
            ),
            None => ("-".into(), "-".into(), ""),
        };
        let line = format!("{id:<40} {procs:>5} {down:>12} {up:>12}  {kept}");
        println!("{}", line.trim_end());
    };
    println!("{:<40} {:>5} {:>12} {:>12}", "APP", "PROCS", "DOWN", "UP");
    for app in apps
        .iter()
        .filter(|a| all || a.desktop || limits.contains_key(&a.id))
    {
        row(&app.id, app.processes);
    }
    for id in limits
        .keys()
        .filter(|id| !apps.iter().any(|a| &a.id == *id))
    {
        row(id, 0);
    }
}
