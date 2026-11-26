//! Stands in for a desktop panel on a private session bus: accepts the tray
//! icon, prints its menu, then picks one entry once <go-file> exists. Usage: tray_probe <entry> <go-file>

use std::{collections::HashMap, sync::mpsc, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    interface,
    message::Header,
    zvariant::{OwnedValue, Value},
};

struct Watcher(mpsc::Sender<String>);

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        // Items pass either their bus name or their object path.
        let target = if service.starts_with('/') {
            format!("{sender}{service}")
        } else {
            format!("{service}/StatusNotifierItem")
        };
        let _ = self.0.send(target);
    }
    fn register_status_notifier_host(&self, _service: &str) {}
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        Vec::new()
    }
}

/// (icon name, pixmaps, title, description)
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn labels(node: &Layout, out: &mut Vec<(i32, String, bool)>) {
    let label = node
        .1
        .get("label")
        .and_then(|v| String::try_from(v.clone()).ok());
    let enabled = node
        .1
        .get("enabled")
        .is_none_or(|v| bool::try_from(v).unwrap_or(true));
    let separator = node
        .1
        .get("type")
        .and_then(|v| String::try_from(v.clone()).ok())
        .is_some_and(|t| t == "separator");
    if separator {
        out.push((node.0, "---".into(), false));
    } else if let Some(label) = label {
        out.push((node.0, label, enabled));
    }
    for child in &node.2 {
        if let Ok(child) = Value::from(child.clone()).downcast::<Layout>() {
            labels(&child, out);
        }
    }
}

fn main() -> zbus::Result<()> {
    let pick = std::env::args().nth(1).unwrap_or_default();
    let (tx, rx) = mpsc::channel();
    let _watcher: Connection = Builder::session()?
        .name("org.kde.StatusNotifierWatcher")?
        .serve_at("/StatusNotifierWatcher", Watcher(tx))?
        .build()?;
    println!("watcher ready");
    let item = rx
        .recv_timeout(Duration::from_secs(20))
        .map_err(|_| zbus::Error::Failure("no tray icon registered".into()))?;
    let (service, path) = item.split_at(item.find('/').unwrap_or(item.len()));
    let conn = Connection::session()?;
    let sni = Proxy::new(
        &conn,
        service.to_owned(),
        path.to_owned(),
        "org.kde.StatusNotifierItem",
    )?;
    let title: String = sni.get_property("Title")?;
    let tooltip: ToolTip = sni.get_property("ToolTip")?;
    let pixmaps: Vec<(i32, i32, Vec<u8>)> = sni.get_property("IconPixmap")?;
    println!(
        "title={title:?} tooltip={:?} icon={:?}",
        tooltip.2,
        pixmaps.first().map(|p| (p.0, p.1))
    );
    let menu_path: zbus::zvariant::OwnedObjectPath = sni.get_property("Menu")?;
    let menu = Proxy::new(
        &conn,
        service.to_owned(),
        menu_path,
        "com.canonical.dbusmenu",
    )?;
    let (_, layout): (u32, Layout) =
        menu.call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))?;
    let mut entries = Vec::new();
    labels(&layout, &mut entries);
    for (_, label, enabled) in &entries {
        println!("menu: {label}{}", if *enabled { "" } else { " (disabled)" });
    }
    if let Some(go) = std::env::args().nth(2) {
        while !std::path::Path::new(&go).exists() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let id = entries
        .iter()
        .find(|e| e.1 == pick)
        .map(|e| e.0)
        .ok_or_else(|| zbus::Error::Failure(format!("no entry {pick}")))?;
    let now = 0u32;
    menu.call::<_, _, ()>("Event", &(id, "clicked", Value::from(0i32), now))?;
    println!("picked {pick}");
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}
