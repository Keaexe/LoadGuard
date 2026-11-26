//! What the window shows, independent of the toolkit.

use crate::desktop::Entries;
use loadguard_core::{App, Limit, Rate, Unit, client::Limits};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum Helper {
    Running(Limits),
    Stopped,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub apps: Vec<App>,
    pub helper: Helper,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub processes: usize,
    pub also_covers: Vec<String>,
    /// The limit and whether it ends when the app closes.
    pub limit: Option<(Limit, bool)>,
}

impl Row {
    pub fn detail(&self) -> String {
        match (self.limit, self.processes) {
            (Some((_, true)), _) => "Until closed".into(),
            (Some((_, false)), 0) => "Always (not running)".into(),
            (Some((_, false)), _) => "Always".into(),
            (None, 1) => "1 process".into(),
            (None, n) => format!("{n} processes"),
        }
    }
}

/// Limited apps, then other running apps, both sorted by name.
pub fn rows(snap: &Snapshot, entries: &Entries, search: &str, all: bool) -> (Vec<Row>, Vec<Row>) {
    let none = Limits::new();
    let limits = match &snap.helper {
        Helper::Running(limits) => limits,
        _ => &none,
    };
    let row = |id: &str, app: Option<&App>, limit| {
        let entry = entries.get(id, app);
        Row {
            id: id.to_owned(),
            name: entry.map_or_else(|| id.to_owned(), |e| e.name.clone()),
            icon: entry.and_then(|e| e.icon.clone()),
            processes: app.map_or(0, |a| a.processes),
            also_covers: app.map_or_else(Vec::new, |a| a.aliases.clone()),
            limit,
        }
    };
    let limited = limits
        .iter()
        .map(|(id, l)| row(id, snap.apps.iter().find(|a| a.answers_to(id)), Some(*l)));
    let running = snap
        .apps
        .iter()
        .filter(|a| (all || a.desktop) && !limits.keys().any(|id| a.answers_to(id)))
        .map(|a| row(&a.id, Some(a), None));

    let search = search.trim().to_lowercase();
    let finish = |rows: Vec<Row>| {
        let mut rows: Vec<Row> = rows
            .into_iter()
            .filter(|r| {
                [&r.id, &r.name]
                    .into_iter()
                    .chain(&r.also_covers)
                    .any(|t| t.to_lowercase().contains(&search))
            })
            .collect();
        rows.sort_by_cached_key(|r| (r.name.to_lowercase(), r.id.clone()));
        rows
    };
    (finish(limited.collect()), finish(running.collect()))
}

pub fn show(rate: Option<Rate>, unit: Unit) -> String {
    rate.map_or_else(
        || "unlimited".into(),
        |r| format!("{} {}", unit.number(r), unit.label()),
    )
}

/// Reads one direction of the editor. Untouched text keeps the exact old rate,
/// which the shown number may have rounded.
pub fn read_field(
    on: bool,
    text: &str,
    unit: Unit,
    old: Option<Rate>,
) -> Result<Option<Rate>, String> {
    if !on {
        return Ok(None);
    }
    if text.trim().is_empty() {
        return Err("enter a speed, or switch this direction off".into());
    }
    match old {
        Some(old) if unit.number(old) == text => Ok(Some(old)),
        _ => unit.parse(text).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, desktop: bool) -> App {
        App {
            id: id.into(),
            processes: 2,
            desktop,
            ..App::default()
        }
    }

    fn snapshot() -> Snapshot {
        let mut beeper = app("beeper.appimage", true);
        beeper.aliases = vec!["Beeper".into()];
        let limit = Limit {
            down: "500".parse().ok(),
            up: None,
        };
        Snapshot {
            apps: vec![
                app("zen", true),
                beeper,
                app("pipewire", false),
                app("Ark", true),
            ],
            helper: Helper::Running(
                [("Beeper", true), ("closed", false)]
                    .map(|(id, temp)| (id.to_owned(), (limit, temp)))
                    .into(),
            ),
        }
    }

    fn ids(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn splits_filters_and_sorts() {
        let e = Entries::default();
        let (limited, running) = rows(&snapshot(), &e, "", false);
        assert_eq!(ids(&limited), ["Beeper", "closed"]);
        assert_eq!(ids(&running), ["Ark", "zen"]);
        assert_eq!(limited[0].detail(), "Until closed");
        assert_eq!(limited[1].detail(), "Always (not running)");
        assert_eq!(rows(&snapshot(), &e, "", true).1.len(), 3);
        assert_eq!(ids(&rows(&snapshot(), &e, " ZE", false).1), ["zen"]);
        assert_eq!(
            ids(&rows(&snapshot(), &e, "appimage", false).0),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn untouched_field_keeps_exact_rate() {
        let old = Rate::from_bytes(1_234_567).ok();
        let text = Unit::MegaBits.number(old.unwrap());
        assert_eq!(read_field(true, &text, Unit::MegaBits, old), Ok(old));
        assert_eq!(read_field(false, "x", Unit::KiloBytes, old), Ok(None));
        assert!(read_field(true, " ", Unit::KiloBytes, old).is_err());
        assert!(read_field(true, "abc", Unit::KiloBytes, old).is_err());
    }
}
