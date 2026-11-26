//! Connects the window to the helper. All blocking work happens on other
//! threads, because a password prompt can take as long as the user likes.

use crate::{
    desktop::Entries,
    desktop::Launcher,
    model::{self, Helper, Row, Snapshot},
    ui::{AppRow, LaunchRow, MainWindow, Tray},
};
use loadguard_core::{
    Config, Limit, Unit,
    apps::{self, CGROUP_ROOT},
    client::{self, HelperProxy},
    config,
    launch::{self, Stage},
};
use slint::{ComponentHandle, Image, ModelRc, VecModel, Weak};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::Stdio,
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
    tray: Option<Weak<Tray>>,
    /// Indexes into `entries.launchers` shown in the launch dialog.
    launch_rows: Vec<usize>,
}

/// Everything needed to start a program limited.
#[derive(Debug)]
pub struct Plan {
    pub stage: Stage,
    pub dir: Option<PathBuf>,
}

impl Controller {
    /// `window` may be empty: the tray works without one.
    pub fn new(window: Weak<MainWindow>, entries: Entries, config: &Config) -> Self {
        Self {
            window,
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
            tray: None,
            launch_rows: Vec::new(),
        }
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        self.snapshot = snapshot;
        self.refresh();
    }

    /// Redraws the lists only if they changed, which keeps focus and scrolling.
    pub fn refresh(&mut self) {
        let up = matches!(self.snapshot.helper, Helper::Running(_));
        self.refresh_tray(up);
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

    /// Connects a newly built window and fills it.
    pub fn attach(&mut self, window: &MainWindow) {
        self.window = window.as_weak();
        self.shown = None;
        self.refresh();
    }

    fn refresh_tray(&self, up: bool) {
        let Some(tray) = self.tray.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let (limited, _) = model::rows(&self.snapshot, &self.entries, "", true);
        let lines: Vec<slint::SharedString> = limited
            .iter()
            .map(|r| {
                let l = r.limit.map(|l| l.0).unwrap_or_default();
                let rate = |arrow: char, rate: Option<loadguard_core::Rate>| {
                    rate.map(|r| format!("  {arrow} {}", model::show(Some(r), self.unit)))
                };
                let text = [rate('↓', l.down), rate('↑', l.up)]
                    .into_iter()
                    .flatten()
                    .collect::<String>();
                format!("{}{text}", r.name).into()
            })
            .collect();
        let summary = match (up, lines.len()) {
            (false, _) if matches!(self.snapshot.helper, Helper::Failed(_)) => {
                "LoadGuard: helper unavailable".to_owned()
            }
            (false, _) => "LoadGuard: helper not running".to_owned(),
            (true, 0) => "No app is limited".into(),
            (true, 1) => "1 app limited".into(),
            (true, n) => format!("{n} apps limited"),
        };
        tray.set_summary(summary.into());
        tray.set_limits(ModelRc::new(VecModel::from(lines)));
        tray.set_can_edit(up && self.backend_on);
    }

    pub fn open_launch(&mut self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        w.set_launch_mode(0);
        w.set_launch_command("".into());
        w.set_launch_down_on(true);
        w.set_launch_down_text("".into());
        w.set_launch_up_on(false);
        w.set_launch_up_text("".into());
        w.set_launch_error("".into());
        self.launch_search("");
        w.set_launch_open(true);
    }

    pub fn launch_search(&mut self, text: &str) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let text = text.trim().to_lowercase();
        self.launch_rows = (0..self.entries.launchers.len())
            .filter(|&i| {
                self.entries.launchers[i]
                    .entry
                    .name
                    .to_lowercase()
                    .contains(&text)
            })
            .collect();
        let rows: Vec<LaunchRow> = self
            .launch_rows
            .clone()
            .iter()
            .map(|&i| self.launch_row(i))
            .collect();
        w.set_launch_rows(ModelRc::new(VecModel::from(rows)));
        w.set_launch_selected(-1);
        self.launch_changed();
    }

    fn launch_row(&mut self, index: usize) -> LaunchRow {
        let launcher = self.entries.launchers[index].clone();
        let icon = launcher.entry.icon.as_ref().and_then(|path| {
            let load = || Image::load_from_path(path).ok();
            self.icons.entry(path.clone()).or_insert_with(load).clone()
        });
        let name = launcher.entry.name;
        LaunchRow {
            letter: name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string()
                .into(),
            name: name.into(),
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
            note: if launcher.refusal.is_some() {
                "Can't be launched limited: limit it once it runs"
            } else {
                ""
            }
            .into(),
            available: launcher.refusal.is_none(),
        }
    }

    fn selected_launcher(&self) -> Option<&Launcher> {
        let w = self.window.upgrade()?;
        let index = usize::try_from(w.get_launch_selected()).ok()?;
        self.entries.launchers.get(*self.launch_rows.get(index)?)
    }

    /// The program the dialog would start, as (command, working directory).
    fn launch_command(&self) -> Result<(Vec<String>, Option<PathBuf>), String> {
        let w = self.window.upgrade().ok_or("no window")?;
        if w.get_launch_mode() == 0 {
            let launcher = self.selected_launcher().ok_or("Choose an app to launch.")?;
            return Ok((launcher.command.clone(), launcher.dir.clone()));
        }
        let words = launch::split(&w.get_launch_command()).map_err(|e| format!("Command: {e}"))?;
        if words.is_empty() || words[0].is_empty() {
            return Err("Enter a command to launch.".into());
        }
        Ok((words, None))
    }

    /// Warns when the chosen app already runs: it may hand over to that copy.
    pub fn launch_changed(&mut self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let warning = self.launch_command().ok().and_then(|(command, _)| {
            let program = Path::new(&command[0])
                .file_name()?
                .to_string_lossy()
                .into_owned();
            let target = match self.selected_launcher() {
                Some(l) if w.get_launch_mode() == 0 => Some(l.entry.clone()),
                _ => self.entries.get(&program, None).cloned(),
            };
            let running = self.snapshot.apps.iter().find(|app| {
                app.answers_to(&program)
                    || target
                        .as_ref()
                        .is_some_and(|t| self.entries.get(&app.id, Some(app)) == Some(t))
            })?;
            let name = target.map_or_else(|| running.id.clone(), |t| t.name);
            Some(format!(
                "{name} is already running. If it reuses that window, this limit won't affect it."
            ))
        });
        w.set_launch_warning(warning.unwrap_or_default().into());
    }

    pub fn read_launch(&self) -> Result<Plan, String> {
        let w = self.window.upgrade().ok_or("no window")?;
        let (command, dir) = self.launch_command()?;
        let program = OsString::from(&command[0]);
        if let Some(refusal) = launch::refusal(&program) {
            return Err(refusal);
        }
        if launch::find_program(&program).is_none() {
            return Err(format!("{} was not found.", command[0]));
        }
        let read = |on, text: slint::SharedString, what| {
            model::read_field(on, &text, self.unit, None).map_err(|e| format!("{what}: {e}"))
        };
        let down = read(w.get_launch_down_on(), w.get_launch_down_text(), "Download")?;
        let up = read(w.get_launch_up_on(), w.get_launch_up_text(), "Upload")?;
        if down.is_none() && up.is_none() {
            return Err("Limit at least one direction.".into());
        }
        let Helper::Running(limits) = &self.snapshot.helper else {
            return Err("The helper is not running.".into());
        };
        let stage = Stage {
            id: launch::new_id(&program, &self.snapshot.apps, limits),
            down: client::encode(down),
            up: client::encode(up),
            report: None,
            command: command.into_iter().map(OsString::from).collect(),
        };
        Ok(Plan { stage, dir })
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
            // The event loop may not exist yet at startup; only its end stops polling.
            loop {
                let copy = snapshot.clone();
                match slint::invoke_from_event_loop(move || {
                    with(|c| c.update(copy));
                }) {
                    Ok(()) => break,
                    Err(slint::EventLoopError::NoEventLoopProvider) => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => return,
                }
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

/// Where the second stage reports back, private to this user.
fn report_path() -> Result<PathBuf, String> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("XDG_RUNTIME_DIR is not set")?;
    let dir = base.join("loadguard");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(dir.join(format!("launch-{}-{unique}", std::process::id())))
}

/// Starts the program and waits until it is limited, or the reason it is not.
fn start(mut plan: Plan) -> Result<std::process::Child, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let report = report_path()?;
    plan.stage.report = Some(report.clone());
    let mut command = launch::command(&exe, &plan.stage);
    command.stdin(Stdio::null());
    if let Some(dir) = plan.dir.or_else(|| env::var_os("HOME").map(PathBuf::from)) {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot run systemd-run: {e}"))?;
    // A password prompt may keep the second stage waiting for a long time.
    let outcome = loop {
        if let Ok(text) = fs::read_to_string(&report) {
            break match text.split_once('\n') {
                None if text == "ok" => Ok(()),
                Some((_, error)) => Err(error.to_owned()),
                None => Err(text),
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break fs::read_to_string(&report).map_or_else(
                    |_| Err(format!("the launch failed ({status})")),
                    |text| {
                        if text == "ok" {
                            Ok(())
                        } else {
                            Err(text.lines().skip(1).collect())
                        }
                    },
                );
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = fs::remove_file(&report);
    outcome.map(|()| child)
}

fn launch_in_background(window: &Weak<MainWindow>, poke: &Poke, plan: Plan) {
    let (window, poke) = (window.clone(), poke.clone());
    if let Some(w) = window.upgrade() {
        w.set_busy(true);
        w.set_launch_error("".into());
    }
    thread::spawn(move || {
        let result = start(plan);
        let _ = poke.send(false);
        let ok = result.is_ok();
        let error = result.as_ref().err().cloned();
        let _ = window.upgrade_in_event_loop(move |w| {
            w.set_busy(false);
            match error {
                None => w.set_launch_open(false),
                Some(e) => w.set_launch_error(e.into()),
            }
        });
        // Reap the program when it exits, so it does not linger as a zombie.
        if ok && let Ok(mut child) = result {
            let _ = child.wait();
            let _ = poke.send(false);
        }
    });
}

/// Whether a desktop panel can show tray icons right now.
fn tray_host_available() -> bool {
    let check = || -> zbus::Result<bool> {
        let conn = zbus::blocking::Connection::session()?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )?;
        proxy.get_property("IsStatusNotifierHostRegistered")
    };
    check().unwrap_or(false)
}

const INSTANCE_NAME: &str = "io.github.keaexe.LoadGuard.Gui";
const INSTANCE_PATH: &str = "/io/github/keaexe/LoadGuard/Gui";

struct Instance;

#[zbus::interface(name = "io.github.keaexe.LoadGuard.Gui")]
impl Instance {
    /// Shows the window of the running copy.
    fn activate(&self) {
        let _ = slint::invoke_from_event_loop(|| {
            show_window();
        });
    }
}

/// Becomes the only copy, or asks the running one to show itself. Returns
/// `None` when another copy took over.
fn claim_instance() -> Option<Option<zbus::blocking::Connection>> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.serve_at(INSTANCE_PATH, Instance))
        .and_then(|b| b.build());
    // Without a session bus there is nothing to coordinate with.
    let Ok(conn) = conn else { return Some(None) };
    match conn.request_name_with_flags(INSTANCE_NAME, RequestNameFlags::DoNotQueue.into()) {
        // zbus reports an owned name as an error rather than as a reply.
        Ok(RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            let shown =
                zbus::blocking::Proxy::new(&conn, INSTANCE_NAME, INSTANCE_PATH, INSTANCE_NAME)
                    .and_then(|p| p.call_method("Activate", &()).map(drop));
            match shown {
                Ok(()) => None,
                Err(e) => {
                    eprintln!(
                        "note: could not reach the running LoadGuard ({e}); starting another"
                    );
                    Some(Some(conn))
                }
            }
        }
        _ => Some(Some(conn)),
    }
}

/// Waits for a panel, which may start after LoadGuard at login.
fn wait_for_tray(seconds: u32) -> bool {
    for _ in 0..seconds {
        if tray_host_available() {
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    tray_host_available()
}

/// What building the window needs; kept so it can be built when first shown.
struct Setup {
    poke: Poke,
    config_path: PathBuf,
}

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
    /// Built only when first shown: an existing window would appear as an
    /// empty one while LoadGuard sits in the tray.
    static WINDOW: RefCell<Option<MainWindow>> = const { RefCell::new(None) };
}

/// Shows the window, building it first if needed.
pub fn show_window() -> Option<MainWindow> {
    if WINDOW.with(|w| w.borrow().is_none()) {
        let window = SETUP.with(|s| s.borrow().as_ref().map(build_window))?;
        match window {
            Ok(window) => WINDOW.with(|w| *w.borrow_mut() = Some(window)),
            Err(e) => {
                eprintln!("error: cannot open the window: {e}");
                return None;
            }
        }
    }
    let window = WINDOW.with(|w| w.borrow().as_ref().map(ComponentHandle::clone_strong))?;
    let _ = window.show();
    Some(window)
}

fn build_window(setup: &Setup) -> Result<MainWindow, String> {
    let window = MainWindow::new().map_err(|e| e.to_string())?;
    with(|c| c.attach(&window));
    let poke = setup.poke.clone();
    let path = setup.config_path.clone();
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
    let (w, p) = (weak.clone(), poke.clone());
    window.on_clear_all(move || act(&w, &p, |h| h.clear_all().map_err(client::explain)));

    window.on_open_launch(|| with(Controller::open_launch).unwrap_or_default());
    window.on_launch_search(|text| with(|c| c.launch_search(&text)).unwrap_or_default());
    window.on_launch_changed(|| with(Controller::launch_changed).unwrap_or_default());
    let (w, p) = (weak, poke);
    window.on_launch_start(move || {
        let Some(win) = w.upgrade() else { return };
        if win.get_busy() {
            return;
        }
        match with(|c| c.read_launch()) {
            Some(Ok(plan)) => launch_in_background(&w, &p, plan),
            Some(Err(e)) => win.set_launch_error(e.into()),
            None => {}
        }
    });

    if let Some(path) = crate::autostart::path() {
        window.set_autostart(crate::autostart::is_enabled(&path));
        let w = window.as_weak();
        window.on_set_autostart(move |on| {
            let Some(w) = w.upgrade() else { return };
            let result = env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| crate::autostart::set(&path, on, &exe));
            if let Err(e) = result {
                w.set_error(e.into());
            }
            w.set_autostart(crate::autostart::is_enabled(&path));
        });
    }
    Ok(window)
}

pub fn run(hidden: bool) -> Result<(), String> {
    let Some(_instance) = claim_instance() else {
        return Ok(());
    };
    let path = Config::default_path().ok_or("cannot locate the configuration directory")?;
    let config = Config::load(&path)?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();

    // Without a panel to hold the icon, closing the window quits.
    let has_tray = if hidden {
        wait_for_tray(30)
    } else {
        tray_host_available()
    };
    if hidden && !has_tray {
        // Limits keep working without the GUI; there is just nowhere to show it.
        eprintln!("note: no system tray appeared, so LoadGuard is not staying in the background");
        return Ok(());
    }

    // Links the window to its desktop entry.
    let _ = slint::set_xdg_app_id("io.github.keaexe.LoadGuard");
    let controller = Controller::new(Weak::default(), Entries::load(), &config);
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller));
    let poke = spawn_poller(uid);
    SETUP.with(|s| {
        *s.borrow_mut() = Some(Setup {
            poke,
            config_path: path,
        })
    });

    let tray = if has_tray { Tray::new().ok() } else { None };
    if let Some(tray) = &tray {
        with(|c| {
            c.tray = Some(tray.as_weak());
            c.refresh();
        });
        tray.on_open(|| {
            show_window();
        });
        tray.on_launch(|| {
            if show_window().is_some() {
                with(Controller::open_launch);
            }
        });
        tray.on_clear_all(|| {
            if let Some(w) = show_window() {
                w.set_confirm_open(true);
            }
        });
        tray.on_quit(|| {
            let _ = slint::quit_event_loop();
        });
    }
    if !hidden || tray.is_none() {
        show_window().ok_or("cannot open the window")?;
    }
    slint::run_event_loop().map_err(|e| e.to_string())
}
