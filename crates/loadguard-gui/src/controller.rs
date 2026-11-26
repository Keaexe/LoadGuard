//! Connects the window to the helper. All blocking work happens on other
//! threads, because a password prompt can take as long as the user likes.

use crate::{
    desktop::Entries,
    model::{self, Helper, Row, Snapshot},
    ui::{AppRow, MainWindow},
};
use loadguard_core::{
    Config, Limit, Unit,
    apps::{self, CGROUP_ROOT},
    client::{self, HelperProxy},
    config,
};
use slint::{ComponentHandle, Image, ModelRc, VecModel, Weak};
use std::{
    cell::RefCell,
    collections::HashMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc,
    thread,
    time::Duration,
};

const POLL: Duration = Duration::from_secs(2);

pub struct Controller {
    window: Weak<MainWindow>,
    entries: Entries,
    icons: HashMap<PathBuf, Option<Image>>,
    snapshot: Snapshot,
    search: String,
    unit: Unit,
    all: bool,
    backend_on: bool,
    editing: Option<Row>,
    shown: Option<(Vec<Row>, Vec<Row>, Unit)>,
}

impl Controller {
    pub fn new(window: &MainWindow, entries: Entries, config: &Config) -> Self {
        Self {
            window: window.as_weak(),
            entries,
            icons: HashMap::new(),
            snapshot: Snapshot {
                apps: Vec::new(),
                helper: Helper::Stopped,
            },
            search: String::new(),
            unit: config.unit,
            all: false,
            backend_on: config.backends.nftables,
            editing: None,
            shown: None,
        }
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        self.snapshot = snapshot;
        self.refresh();
    }

    /// Redraws the lists only if they changed, which keeps focus and scrolling.
    pub fn refresh(&mut self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let (limited, running) = model::rows(&self.snapshot, &self.entries, &self.search, self.all);
        let shown = Some((limited, running, self.unit));
        if shown != self.shown {
            if let Some((limited, running, _)) = &shown {
                let limited: Vec<_> = limited.iter().map(|r| self.ui_row(r)).collect();
                let running: Vec<_> = running.iter().map(|r| self.ui_row(r)).collect();
                w.set_limited(ModelRc::new(VecModel::from(limited)));
                w.set_running(ModelRc::new(VecModel::from(running)));
            }
            self.shown = shown;
        }
        let up = matches!(self.snapshot.helper, Helper::Running(_));
        w.set_can_edit(up && self.backend_on);
        w.set_can_start(self.backend_on && !up);
        let status = match &self.snapshot.helper {
            _ if !self.backend_on => "Limiting is switched off in the configuration".to_owned(),
            Helper::Running(_) => "Helper connected".into(),
            Helper::Stopped => "The helper is not running".into(),
            Helper::Failed(e) => format!("Helper unavailable: {e}"),
        };
        w.set_status(status.into());
        w.set_unit(i32::from(self.unit == Unit::MegaBits));
        w.set_unit_label(self.unit.label().into());
        w.set_show_all(self.all);
        w.set_searching(!self.search.trim().is_empty());
    }

    fn ui_row(&mut self, row: &Row) -> AppRow {
        let icon = row.icon.as_ref().and_then(|path| {
            let load = || Image::load_from_path(path).ok();
            self.icons.entry(path.clone()).or_insert_with(load).clone()
        });
        let limit = row.limit.map(|l| l.0).unwrap_or_default();
        AppRow {
            id: row.id.as_str().into(),
            name: row.name.as_str().into(),
            detail: row.detail().into(),
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
            letter: row
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string()
                .into(),
            limited: row.limit.is_some(),
            down: model::show(limit.down, self.unit).into(),
            up: model::show(limit.up, self.unit).into(),
        }
    }

    pub fn open(&mut self, id: &str) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let (limited, running) = model::rows(&self.snapshot, &self.entries, "", true);
        let Some(row) = limited.into_iter().chain(running).find(|r| r.id == id) else {
            w.set_error("That app has just closed.".into());
            return;
        };
        let limit = row.limit.map(|l| l.0).unwrap_or_default();
        let text = |r: Option<_>| r.map(|r| self.unit.number(r)).unwrap_or_default();
        w.set_down_on(limit.down.is_some() || row.limit.is_none());
        w.set_down_text(text(limit.down).into());
        w.set_up_on(limit.up.is_some());
        w.set_up_text(text(limit.up).into());
        w.set_until_closed(row.limit.is_none_or(|l| l.1));
        w.set_is_running(row.processes > 0);
        w.set_also_covers(row.also_covers.join(", ").into());
        w.set_editor_error("".into());
        let ui = self.ui_row(&row);
        w.set_editing(ui);
        self.editing = Some(row);
        w.set_editor_open(true);
    }

    /// Reads the editor: (id, limit, until closed), or a message for the user.
    pub fn read_editor(&self) -> Result<(String, Limit, bool), String> {
        let (Some(w), Some(row)) = (self.window.upgrade(), &self.editing) else {
            return Err("nothing is being edited".into());
        };
        let old = row.limit.map(|l| l.0).unwrap_or_default();
        let read = model::read_field;
        let down = read(w.get_down_on(), &w.get_down_text(), self.unit, old.down)
            .map_err(|e| format!("Download: {e}"))?;
        let up = read(w.get_up_on(), &w.get_up_text(), self.unit, old.up)
            .map_err(|e| format!("Upload: {e}"))?;
        if down.is_none() && up.is_none() {
            return Err("Limit at least one direction, or remove the limit.".into());
        }
        Ok((row.id.clone(), Limit { down, up }, w.get_until_closed()))
    }
}

thread_local! {
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
    CONTROLLER.with(|c| c.borrow_mut().as_mut().map(f))
}

fn take_snapshot(uid: u32, helper: &Result<HelperProxy<'static>, String>, start: bool) -> Snapshot {
    let apps = apps::discover(Path::new(CGROUP_ROOT), uid).unwrap_or_default();
    let helper = match helper {
        Err(e) => Helper::Failed(e.clone()),
        // Polling must not restart a helper that was stopped on purpose.
        Ok(h) if !start && !client::is_running(h) => Helper::Stopped,
        Ok(h) => client::limits(h).map_or_else(Helper::Failed, Helper::Running),
    };
    Snapshot { apps, helper }
}

type Poke = mpsc::Sender<bool>;

/// Polls until the window is gone. A message polls at once; `true` also starts
/// the helper.
fn spawn_poller(uid: u32) -> Poke {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut helper = client::connect();
        let mut start = true;
        loop {
            if helper.is_err() {
                helper = client::connect();
            }
            let snapshot = take_snapshot(uid, &helper, start);
            let sent = slint::invoke_from_event_loop(move || {
                with(|c| c.update(snapshot));
            });
            if sent.is_err() {
                return;
            }
            start = match rx.recv_timeout(POLL) {
                Ok(start) => start,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
        }
    });
    tx
}

fn open_editor(window: &Weak<MainWindow>) -> Option<String> {
    let w = window.upgrade().filter(|w| w.get_editor_open())?;
    Some(w.get_editing().id.into())
}

/// Runs a helper call in the background and reports back in the window.
fn act(
    window: &Weak<MainWindow>,
    poke: &Poke,
    call: impl FnOnce(&HelperProxy<'static>) -> Result<(), String> + Send + 'static,
) {
    let (window, poke) = (window.clone(), poke.clone());
    if let Some(w) = window.upgrade() {
        w.set_busy(true);
        w.set_editor_error("".into());
        w.set_error("".into());
    }
    // The editor may be closed, or show another app, by the time this finishes.
    let editor = open_editor(&window);
    thread::spawn(move || {
        let result = client::connect().and_then(|h| call(&h));
        let _ = poke.send(false);
        let _ = window.upgrade_in_event_loop(move |w| {
            w.set_busy(false);
            let same = editor.is_some() && editor == open_editor(&w.as_weak());
            match result {
                Ok(()) if same => w.set_editor_open(false),
                Ok(()) => {}
                Err(e) if same => w.set_editor_error(e.into()),
                Err(e) => w.set_error(e.into()),
            }
        });
    });
}

pub fn run() -> Result<(), String> {
    let path = Config::default_path().ok_or("cannot locate the configuration directory")?;
    let config = Config::load(&path)?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    // Links the window to its desktop entry.
    let _ = slint::set_xdg_app_id("io.github.keaexe.LoadGuard");
    let window = MainWindow::new().map_err(|e| e.to_string())?;
    let controller = Controller::new(&window, Entries::load(), &config);
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller));
    with(Controller::refresh);
    let poke = spawn_poller(uid);
    let weak = Rc::new(window.as_weak());

    window.on_search(|text| {
        with(|c| {
            c.search = text.into();
            c.refresh();
        });
    });
    let w = weak.clone();
    window.on_set_unit(move |i| {
        let unit = if i == 1 {
            Unit::MegaBits
        } else {
            Unit::KiloBytes
        };
        with(|c| {
            c.unit = unit;
            c.refresh();
        });
        if let (Err(e), Some(w)) = (config::save_unit(&path, unit), w.upgrade()) {
            w.set_error(e.into());
        }
    });
    window.on_toggle_all(|| {
        with(|c| {
            c.all = !c.all;
            c.refresh();
        });
    });
    window.on_open(|id| {
        with(|c| c.open(&id));
    });
    let p = poke.clone();
    window.on_start_helper(move || {
        let _ = p.send(true);
    });
    let (w, p) = (weak.clone(), poke.clone());
    window.on_apply(move || {
        let Some(win) = w.upgrade() else { return };
        // Enter in a field also lands here.
        if win.get_busy() {
            return;
        }
        match with(|c| c.read_editor()) {
            Some(Ok((id, limit, until_closed))) => act(&w, &p, move |h| {
                let (d, u) = (client::encode(limit.down), client::encode(limit.up));
                h.set_limit(&id, d, u, until_closed)
                    .map_err(client::explain)
            }),
            Some(Err(e)) => win.set_editor_error(e.into()),
            None => {}
        }
    });
    let (w, p) = (weak.clone(), poke.clone());
    window.on_remove(move |id| {
        act(&w, &p, move |h| h.clear_limit(&id).map_err(client::explain));
    });
    let (w, p) = (weak, poke);
    window.on_clear_all(move || act(&w, &p, |h| h.clear_all().map_err(client::explain)));

    window.run().map_err(|e| e.to_string())
}
