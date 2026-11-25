//! Desired limits, and the loop that keeps the firewall in line with them.

use loadguard_core::{Limit, apps, nft};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

const MAX_LIMITS: usize = 128;

pub trait Firewall: Send {
    fn apply(&mut self, rules: &str) -> Result<(), String>;
    fn has_table(&mut self, uid: u32) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub limit: Limit,
    /// Forget the limit once the app is gone, and never write it to disk.
    pub until_closed: bool,
}

#[derive(Default)]
struct User {
    limits: BTreeMap<String, Entry>,
    /// What the firewall holds; `None` if unknown.
    applied: Option<Vec<nft::Entry>>,
    last_error: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    apps: BTreeMap<String, Limit>,
}

pub struct Engine<F> {
    cgroup_root: PathBuf,
    state_dir: PathBuf,
    firewall: F,
    users: BTreeMap<u32, User>,
}

impl<F: Firewall> Engine<F> {
    /// Loads saved limits. Broken files are reported and skipped.
    pub fn new(cgroup_root: &Path, state_dir: &Path, firewall: F) -> io::Result<Self> {
        let mut users = BTreeMap::new();
        for file in fs::read_dir(state_dir)?.flatten() {
            let path = file.path();
            let Some(uid) = uid_of(&path) else { continue };
            let saved = fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| toml::from_str::<Saved>(&s).map_err(|e| e.to_string()));
            match saved {
                Ok(saved) => {
                    let limits = saved
                        .apps
                        .into_iter()
                        .map(|(id, limit)| {
                            (
                                id,
                                Entry {
                                    limit,
                                    until_closed: false,
                                },
                            )
                        })
                        .collect();
                    users.insert(
                        uid,
                        User {
                            limits,
                            ..User::default()
                        },
                    );
                }
                Err(e) => eprintln!("ignoring {}: {e}", path.display()),
            }
        }
        Ok(Self {
            cgroup_root: cgroup_root.into(),
            state_dir: state_dir.into(),
            firewall,
            users,
        })
    }

    pub fn list(&self, uid: u32) -> Vec<(String, Entry)> {
        self.users.get(&uid).map_or_else(Vec::new, |u| {
            u.limits.iter().map(|(id, e)| (id.clone(), *e)).collect()
        })
    }

    pub fn set(&mut self, uid: u32, app: &str, entry: Entry) -> Result<(), String> {
        validate_id(app)?;
        if entry.until_closed && !self.discover(uid).iter().any(|a| a.answers_to(app)) {
            return Err(format!("'{app}' is not running"));
        }
        self.change(uid, |limits| {
            limits.insert(app.to_owned(), entry);
            if limits.len() > MAX_LIMITS {
                return Err(format!("at most {MAX_LIMITS} limits are allowed"));
            }
            Ok(())
        })
    }

    pub fn clear(&mut self, uid: u32, app: &str) -> Result<(), String> {
        self.change(uid, |limits| {
            limits
                .remove(app)
                .map(drop)
                .ok_or_else(|| format!("no limit for '{app}'"))
        })
    }

    pub fn clear_all(&mut self, uid: u32) -> Result<(), String> {
        self.change(uid, |limits| {
            limits.clear();
            Ok(())
        })
    }

    /// Applies and saves a change, or restores the previous limits.
    fn change(
        &mut self,
        uid: u32,
        edit: impl FnOnce(&mut BTreeMap<String, Entry>) -> Result<(), String>,
    ) -> Result<(), String> {
        let old = self
            .users
            .get(&uid)
            .map(|u| u.limits.clone())
            .unwrap_or_default();
        let mut new = old.clone();
        edit(&mut new)?;
        self.users.entry(uid).or_default().limits = new;
        // A cgroup can vanish between the scan and the load, so retry.
        let mut result = Err(String::new());
        for _ in 0..3 {
            result = self.reconcile(uid);
            if result.is_ok() {
                break;
            }
        }
        let result = result.and_then(|()| self.save(uid));
        if result.is_err() {
            let user = self.users.entry(uid).or_default();
            user.limits = old;
            user.applied = None;
            let _ = self.reconcile(uid);
        }
        result
    }

    /// Brings every user's rules up to date. Called periodically.
    pub fn tick(&mut self, verify: bool) {
        let uids: Vec<u32> = self.users.keys().copied().collect();
        for uid in uids {
            if verify
                && self.users[&uid]
                    .applied
                    .as_ref()
                    .is_some_and(|a| !a.is_empty())
                && !self.firewall.has_table(uid)
            {
                eprintln!("rules of user {uid} were removed externally, restoring them");
                self.users.entry(uid).or_default().applied = None;
            }
            let error = self.reconcile(uid).err();
            let user = self.users.entry(uid).or_default();
            if error.is_some() && error != user.last_error {
                eprintln!("user {uid}: {}", error.as_deref().unwrap_or_default());
            }
            user.last_error = error;
        }
        self.users.retain(|_, u| {
            !u.limits.is_empty() || u.applied.as_ref().is_none_or(|a| !a.is_empty())
        });
    }

    fn reconcile(&mut self, uid: u32) -> Result<(), String> {
        let apps = self.discover(uid);
        let Some(user) = self.users.get_mut(&uid) else {
            return Ok(());
        };
        user.limits
            .retain(|id, e| !e.until_closed || apps.iter().any(|a| a.answers_to(id)));
        let wanted: Vec<nft::Entry> = user
            .limits
            .iter()
            .filter_map(|(id, e)| {
                let app = apps.iter().find(|a| a.answers_to(id))?;
                Some(nft::Entry {
                    cgroups: app.cgroups.clone(),
                    limit: e.limit,
                })
            })
            .collect();
        if user.applied.as_ref() == Some(&wanted) {
            return Ok(());
        }
        let rules = if wanted.is_empty() {
            nft::clear(uid)
        } else {
            nft::ruleset(uid, &wanted, true)
        };
        user.applied = None;
        self.firewall.apply(&rules)?;
        user.applied = Some(wanted);
        Ok(())
    }

    /// A logged-out user has no app slice, which simply means no apps.
    fn discover(&self, uid: u32) -> Vec<apps::App> {
        apps::discover(&self.cgroup_root, uid).unwrap_or_default()
    }

    fn save(&self, uid: u32) -> Result<(), String> {
        let path = self.state_dir.join(format!("{uid}.toml"));
        let err = |e: io::Error| format!("cannot save limits: {e}");
        let apps: BTreeMap<_, _> = self.users[&uid]
            .limits
            .iter()
            .filter(|(_, e)| !e.until_closed)
            .map(|(id, e)| (id.clone(), e.limit))
            .collect();
        if apps.is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(err(e)),
                _ => Ok(()),
            };
        }
        let text = toml::to_string_pretty(&Saved { apps }).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text).map_err(err)?;
        fs::rename(&tmp, &path).map_err(err)
    }
}

fn uid_of(path: &Path) -> Option<u32> {
    (path.extension()? == "toml").then_some(())?;
    path.file_stem()?.to_str()?.parse().ok()
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 255 || id.chars().any(char::is_control) {
        return Err("invalid app id".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadguard_core::cgroup::app_slice;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    };

    #[derive(Default)]
    struct Fake {
        log: Vec<String>,
        failures: usize,
        missing: bool,
    }
    impl Firewall for Arc<Mutex<Fake>> {
        fn apply(&mut self, rules: &str) -> Result<(), String> {
            let mut f = self.lock().unwrap();
            if f.failures > 0 {
                f.failures -= 1;
                return Err("boom".into());
            }
            f.log.push(rules.into());
            Ok(())
        }
        fn has_table(&mut self, _: u32) -> bool {
            !self.lock().unwrap().missing
        }
    }

    struct Env {
        dir: PathBuf,
        fw: Arc<Mutex<Fake>>,
    }
    impl Env {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("lg-engine-{}-{n}", std::process::id()));
            fs::create_dir_all(dir.join("state")).unwrap();
            Self {
                dir,
                fw: Arc::default(),
            }
        }
        fn engine(&self) -> Engine<Arc<Mutex<Fake>>> {
            Engine::new(
                &self.dir.join("cg"),
                &self.dir.join("state"),
                self.fw.clone(),
            )
            .unwrap()
        }
        fn unit(&self, name: &str) -> PathBuf {
            self.dir.join("cg").join(app_slice(1000)).join(name)
        }
        fn start(&self, name: &str) {
            fs::create_dir_all(self.unit(name)).unwrap();
            fs::write(self.unit(name).join("cgroup.procs"), "1\n").unwrap();
        }
        fn stop(&self, name: &str) {
            fs::remove_dir_all(self.unit(name)).unwrap();
        }
        fn applied(&self) -> usize {
            self.fw.lock().unwrap().log.len()
        }
        fn last(&self) -> String {
            self.fw.lock().unwrap().log.last().unwrap().clone()
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn entry(until_closed: bool) -> Entry {
        Entry {
            limit: Limit {
                down: "500".parse().ok(),
                up: None,
            },
            until_closed,
        }
    }

    #[test]
    fn follows_restarts_and_avoids_needless_reloads() {
        let env = Env::new();
        let mut e = env.engine();
        env.start("app-zen@1.service");
        e.set(1000, "zen", entry(false)).unwrap();
        e.tick(false);
        assert_eq!(env.applied(), 1);
        env.stop("app-zen@1.service");
        e.tick(false);
        assert!(!env.last().contains("limit rate"));
        env.start("app-zen@2.service");
        e.tick(false);
        assert!(env.last().contains("app-zen@2.service"));
    }

    #[test]
    fn until_closed_is_forgotten_and_never_saved() {
        let env = Env::new();
        let mut e = env.engine();
        assert!(e.set(1000, "zen", entry(true)).is_err());
        env.start("app-zen@1.service");
        e.set(1000, "zen", entry(true)).unwrap();
        assert!(env.engine().list(1000).is_empty());
        env.stop("app-zen@1.service");
        e.tick(false);
        env.start("app-zen@2.service");
        e.tick(false);
        assert!(e.list(1000).is_empty());
        assert!(!env.last().contains("limit rate"));
    }

    #[test]
    fn saved_limits_survive_restart_until_cleared() {
        let env = Env::new();
        env.engine().set(1000, "zen", entry(false)).unwrap();
        let mut e = env.engine();
        assert_eq!(e.list(1000).len(), 1);
        assert!(e.list(1001).is_empty());
        env.start("app-zen@1.service");
        e.tick(false);
        assert!(env.last().contains("limit rate"));
        e.clear_all(1000).unwrap();
        assert!(env.engine().list(1000).is_empty());
    }

    #[test]
    fn failure_rolls_back() {
        let env = Env::new();
        let mut e = env.engine();
        env.start("app-zen@1.service");
        env.fw.lock().unwrap().failures = 2;
        e.set(1000, "zen", entry(false)).unwrap(); // retried
        env.fw.lock().unwrap().failures = 3;
        e.clear(1000, "zen").unwrap_err();
        assert_eq!(e.list(1000).len(), 1);
        assert!(env.last().contains("limit rate"));
        assert_eq!(env.engine().list(1000).len(), 1);
    }

    #[test]
    fn restores_externally_removed_rules() {
        let env = Env::new();
        let mut e = env.engine();
        env.start("app-zen@1.service");
        e.set(1000, "zen", entry(false)).unwrap();
        env.fw.lock().unwrap().missing = true;
        e.tick(true);
        assert_eq!(env.applied(), 2);
    }

    #[test]
    fn rejects_bad_ids() {
        let env = Env::new();
        let mut e = env.engine();
        for id in ["", "a\nb", &"x".repeat(256)] {
            assert!(e.set(1000, id, entry(false)).is_err());
        }
    }
}
