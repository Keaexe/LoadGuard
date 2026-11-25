use clap::{Parser, Subcommand};
use loadguard_core::{
    Config, Limit, Rate,
    apps::{self, App, CGROUP_ROOT},
    nft,
};
use std::{os::unix::fs::MetadataExt, path::Path, process::ExitCode};

/// Limit the download and upload speed of running apps.
#[derive(Parser)]
#[command(name = "loadguard", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show running apps and their saved limits
    List {
        /// Include background services
        #[arg(long)]
        all: bool,
    },
    /// Limit a running app
    Limit {
        /// App id, or any unambiguous part of it
        app: String,
        /// Download limit, such as 500K or 20Mbit (KB/s if no unit)
        #[arg(short, long, required_unless_present = "up")]
        down: Option<Rate>,
        /// Upload limit
        #[arg(short, long)]
        up: Option<Rate>,
        /// Print the rules without applying or saving anything
        #[arg(long)]
        dry_run: bool,
        /// Save the limit to the configuration without applying it
        #[arg(long, conflicts_with = "dry_run")]
        save: bool,
    },
    /// Remove an app's limit, or every limit
    Clear {
        /// App id, exactly as saved
        #[arg(required_unless_present = "all")]
        app: Option<String>,
        #[arg(long, conflicts_with = "app")]
        all: bool,
        /// Print the rules without applying or saving anything
        #[arg(long)]
        dry_run: bool,
        /// Remove the saved limit without changing active rules
        #[arg(long, conflicts_with = "dry_run")]
        save: bool,
    },
    /// Print the rules for all saved limits
    Rules,
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
    let mut config = Config::load(&path)?;
    let apps = || {
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| e.to_string())?
            .uid();
        apps::discover(Path::new(CGROUP_ROOT), uid).map_err(|e| {
            format!("cannot read app cgroups (cgroup v2 and systemd are required): {e}")
        })
    };

    match command {
        Command::List { all } => list(&apps()?, &config, all),
        Command::Rules => print!("{}", rules(&config, &apps()?)?),
        Command::Limit {
            app,
            down,
            up,
            dry_run,
            save,
        } => {
            let apps = apps()?;
            let id = apps::find(&apps, &app)?.id.clone();
            config.apps.insert(id, Limit { down, up });
            finish(&config, &path, dry_run, save, || rules(&config, &apps))?;
        }
        Command::Clear {
            app, dry_run, save, ..
        } => {
            match app {
                Some(id) => {
                    config
                        .apps
                        .remove(&id)
                        .ok_or_else(|| format!("no saved limit for '{id}'"))?;
                }
                None => config.apps.clear(),
            }
            let apps = apps()?;
            finish(&config, &path, dry_run, save, || rules(&config, &apps))?;
        }
    }
    Ok(())
}

fn finish(
    config: &Config,
    path: &Path,
    dry_run: bool,
    save: bool,
    rules: impl Fn() -> Result<String, String>,
) -> Result<(), String> {
    if dry_run {
        print!("{}", rules()?);
        Ok(())
    } else if save {
        config.save(path)?;
        println!("Saved to {}. Nothing has been applied.", path.display());
        Ok(())
    } else {
        Err(
            "applying limits needs the privileged helper, which does not exist yet; \
             use --dry-run or --save"
                .into(),
        )
    }
}

fn rules(config: &Config, apps: &[App]) -> Result<String, String> {
    if !config.backends.nftables {
        return Err("the nftables backend is disabled in the configuration".into());
    }
    let entries: Vec<_> = config
        .apps
        .iter()
        .filter_map(|(id, limit)| {
            let app = apps.iter().find(|a| &a.id == id)?;
            Some(nft::Entry {
                cgroups: app.cgroups.clone(),
                limit: *limit,
            })
        })
        .collect();
    Ok(if entries.is_empty() {
        nft::clear()
    } else {
        nft::ruleset(&entries, true)
    })
}

fn list(apps: &[App], config: &Config, all: bool) {
    let show = |r: Option<Rate>| r.map_or("-".to_owned(), |r| r.to_string());
    println!(
        "{:<40} {:>5} {:>12} {:>12}",
        "APP", "PROCS", "SAVED DOWN", "SAVED UP"
    );
    for app in apps
        .iter()
        .filter(|a| all || a.desktop || config.apps.contains_key(&a.id))
    {
        let l = config.apps.get(&app.id).copied().unwrap_or_default();
        println!(
            "{:<40} {:>5} {:>12} {:>12}",
            app.id,
            app.processes,
            show(l.down),
            show(l.up)
        );
    }
}
