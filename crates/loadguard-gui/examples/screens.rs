//! Renders the real window offscreen with sample data and checks its behaviour.
//! Usage: screens <output directory>

use loadguard_core::{App, Config, Limit, Unit};
use loadguard_gui::{
    controller::Controller,
    desktop::{Entries, Entry, Launcher},
    model::{Helper, Snapshot},
    ui::MainWindow,
};
use slint::{
    ComponentHandle, LogicalPosition, Model, PhysicalSize, Rgb8Pixel, SharedString,
    platform::{
        Key, Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use std::{cell::RefCell, fs, path::Path, rc::Rc, time::Instant};

struct Offscreen(Rc<MinimalSoftwareWindow>, Instant);
impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> std::time::Duration {
        self.1.elapsed()
    }
}

struct Shots<'a>(Rc<MinimalSoftwareWindow>, &'a Path);
impl Shots<'_> {
    fn take(&self, name: &str) {
        slint::platform::update_timers_and_animations();
        let size = self.0.size();
        let mut buf = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
        self.0.request_redraw();
        self.0.draw_if_needed(|r| {
            r.render(&mut buf, size.width as usize);
        });
        let mut out = format!("P6 {} {} 255\n", size.width, size.height).into_bytes();
        out.extend(buf.iter().flat_map(|p| [p.r, p.g, p.b]));
        fs::write(self.1.join(format!("{name}.ppm")), out).expect("write");
    }
    fn click(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        self.0
            .dispatch_event(WindowEvent::PointerMoved { position });
        self.0
            .dispatch_event(WindowEvent::PointerPressed { position, button });
        self.0
            .dispatch_event(WindowEvent::PointerReleased { position, button });
    }
    fn key(&self, text: impl Into<SharedString>) {
        let text = text.into();
        self.0
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        self.0.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("usage: screens <directory>")?;
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(adapter.clone(), Instant::now())))?;
    let shots = Shots(adapter.clone(), Path::new(&dir));
    let w = MainWindow::new()?;
    w.show()?;
    adapter.set_size(PhysicalSize::new(820, 640));
    adapter.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let app = |id: &str, processes, aliases: &[&str]| App {
        id: id.into(),
        processes,
        desktop: true,
        aliases: aliases.iter().map(|a| (*a).into()).collect(),
        ..App::default()
    };
    let limit = |d: &str, u: &str| Limit {
        down: d.parse().ok(),
        up: u.parse().ok(),
    };
    let apps = vec![
        app("beeper.appimage", 9, &["Beeper"]),
        app("cef", 16, &["org.chromium.Chromium"]),
        app("com.heroicgameslauncher.hgl", 7, &["heroic"]),
        app("Nextcloud", 1, &[]),
        app("org.mozilla.thunderbird", 9, &[]),
        app("steam", 18, &[]),
        app("zen", 19, &[]),
    ];
    let running = Snapshot {
        apps: apps.clone(),
        helper: Helper::Running(
            [
                ("steam".to_owned(), (limit("2500", ""), false)),
                ("beeper.appimage".to_owned(), (limit("100", "100"), true)),
            ]
            .into(),
        ),
    };
    let c = Rc::new(RefCell::new(Controller::new(
        &w,
        Entries::load(),
        &Config::default(),
    )));
    c.borrow_mut().update(running.clone());
    let opener = c.clone();
    w.on_open(move |id| opener.borrow_mut().open(&id));
    let applied = Rc::new(RefCell::new(None));
    let (reader, sink) = (c.clone(), applied.clone());
    w.on_apply(move || *sink.borrow_mut() = Some(reader.borrow().read_editor()));
    let cleared = Rc::new(RefCell::new(0));
    let count = cleared.clone();
    w.on_clear_all(move || *count.borrow_mut() += 1);

    shots.take("main");

    c.borrow_mut().open("beeper.appimage");
    shots.take("editor");
    // The download field has focus with its text selected, so typing replaces it.
    for ch in ["2", "5", "0"] {
        shots.key(ch);
    }
    assert_eq!(w.get_down_text(), "250");
    shots.key(Key::Return);
    let got = applied.take().expect("Enter applies").expect("valid");
    assert_eq!(got.0, "beeper.appimage");
    assert_eq!(got.1, limit("250", "100"));
    assert!(got.2, "until closed");
    shots.key(Key::Escape);
    assert!(!w.get_editor_open(), "Escape closes the editor");

    c.borrow_mut().open("zen");
    shots.take("editor-new");
    assert!(w.get_until_closed(), "new limits end with the app");
    w.invoke_apply();
    let error = applied
        .take()
        .expect("applied")
        .expect_err("no speed entered");
    w.set_editor_error(error.into());
    shots.take("editor-error");
    // The window behind must not react while the editor is open.
    shots.click(770.0, 88.0);
    assert!(
        !w.get_confirm_open(),
        "the editor blocks the window behind it"
    );
    w.set_editor_open(false);

    shots.click(770.0, 88.0);
    assert!(w.get_confirm_open(), "Clear all asks first");
    assert_eq!(*cleared.borrow(), 0);
    shots.take("confirm");
    shots.key(Key::Return);
    assert!(!w.get_confirm_open());
    assert_eq!(*cleared.borrow(), 0, "Enter must not confirm by accident");

    c.borrow_mut().update(Snapshot {
        apps: apps.clone(),
        helper: Helper::Stopped,
    });
    w.set_error("not authorized".into());
    shots.take("stopped");
    assert!(!w.get_can_edit());
    w.set_error("".into());

    let config = Config {
        unit: Unit::MegaBits,
        ..Config::default()
    };
    *c.borrow_mut() = Controller::new(&w, Entries::load(), &config);
    c.borrow_mut().update(running);
    adapter.set_size(PhysicalSize::new(560, 480));
    shots.take("small");
    c.borrow_mut().open("beeper.appimage");
    w.set_editor_error("Download: 'x' is not a valid rate (try 500K or 20Mbit)".into());
    shots.take("small-editor");
    w.set_editor_open(false);
    adapter.set_size(PhysicalSize::new(820, 640));
    w.set_scheme(slint::language::ColorScheme::Dark);
    shots.take("dark");
    c.borrow_mut().open("steam");
    shots.take("dark-editor");
    // Launch dialog, with installed apps that do not depend on this machine.
    w.set_scheme(slint::language::ColorScheme::Light);
    let launcher = |name: &str, command: &[&str], refusal: Option<&str>| Launcher {
        entry: Entry {
            name: name.into(),
            icon: None,
        },
        command: command.iter().map(|c| (*c).into()).collect(),
        dir: None,
        refusal: refusal.map(Into::into),
    };
    let mut entries = Entries::default();
    entries.launchers = vec![
        launcher("Flatpak App", &["flatpak", "run", "x"], Some("no")),
        launcher("Shell", &["sh", "-c", "exit"], None),
        launcher("Zen Browser", &["/usr/bin/zen"], None),
    ];
    *c.borrow_mut() = Controller::new(&w, entries, &Config::default());
    let mut snapshot = Snapshot {
        apps: vec![app("sh", 1, &[])],
        ..Snapshot {
            apps: vec![],
            helper: Helper::Stopped,
        }
    };
    snapshot.helper = Helper::Running([("sh".to_owned(), (limit("1", ""), false))].into());
    c.borrow_mut().update(snapshot);
    let launcher_c = c.clone();
    w.on_launch_changed(move || launcher_c.borrow_mut().launch_changed());
    let search_c = c.clone();
    w.on_launch_search(move |t| search_c.borrow_mut().launch_search(&t));
    c.borrow_mut().open_launch();
    assert!(w.get_launch_open());
    assert_eq!(w.get_launch_rows().row_count(), 3);
    assert!(
        !w.get_launch_rows().row_data(0).unwrap().available,
        "Flatpak is refused"
    );
    assert!(c.borrow().read_launch().is_err(), "nothing chosen yet");
    for ch in ["s", "H"] {
        shots.key(ch);
    }
    assert_eq!(
        w.get_launch_rows().row_count(),
        1,
        "typing searches installed apps"
    );
    w.set_launch_selected(0);
    w.invoke_launch_changed();
    assert!(w.get_launch_warning().contains("already running"));
    w.set_launch_down_text("300".into());
    let plan = c.borrow().read_launch().expect("valid launch");
    assert_eq!(plan.stage.command, ["sh", "-c", "exit"]);
    assert_eq!(plan.stage.down, 300_000);
    assert_eq!(plan.stage.id, "sh-2", "a saved limit's id is never reused");
    shots.take("launch");
    w.set_launch_mode(1);
    w.set_launch_command("no-such-program-lg --x".into());
    assert!(c.borrow().read_launch().unwrap_err().contains("not found"));
    w.set_launch_command("flatpak run org.x.Y".into());
    assert!(
        c.borrow()
            .read_launch()
            .unwrap_err()
            .contains("outside LoadGuard")
    );
    w.set_launch_command("sh -c 'echo \"a b\"'".into());
    w.set_launch_up_on(true);
    w.set_launch_up_text("2".into());
    w.set_launch_error("Download: enter a speed, or switch this direction off".into());
    let plan = c.borrow().read_launch().expect("valid command");
    assert_eq!(plan.stage.command, ["sh", "-c", "echo \"a b\""]);
    assert_eq!(plan.stage.up, 2_000);
    shots.take("launch-command");
    w.set_launch_mode(0);
    shots.take("launch-back");
    assert_eq!(w.get_launch_rows().row_count(), 3, "a fresh search field shows every app");
    adapter.set_size(PhysicalSize::new(560, 480));
    shots.take("launch-small");
    adapter.set_size(PhysicalSize::new(820, 640));
    shots.key(Key::Escape);
    assert!(!w.get_launch_open());
    println!("ok");
    Ok(())
}
