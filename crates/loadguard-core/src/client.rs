//! D-Bus client for the privileged helper. Rates are bytes per second; 0 means unlimited.

use crate::{Limit, Rate};
use std::collections::BTreeMap;

pub const BUS_NAME: &str = "io.github.keaexe.LoadGuard1";
pub const OBJECT_PATH: &str = "/io/github/keaexe/LoadGuard1";

#[zbus::proxy(
    interface = "io.github.keaexe.LoadGuard1",
    default_service = "io.github.keaexe.LoadGuard1",
    default_path = "/io/github/keaexe/LoadGuard1",
    gen_async = false,
    blocking_name = "HelperProxy"
)]
pub trait Helper {
    fn set_limit(&self, app: &str, down: u64, up: u64, until_closed: bool) -> zbus::Result<()>;
    fn clear_limit(&self, app: &str) -> zbus::Result<()>;
    fn clear_all(&self) -> zbus::Result<()>;
    /// Returns (app, down, up, until_closed) for the calling user.
    fn list_limits(&self) -> zbus::Result<Vec<(String, u64, u64, bool)>>;
}

/// Limits by app id, with whether each ends when its app closes.
pub type Limits = BTreeMap<String, (Limit, bool)>;

pub fn connect() -> Result<HelperProxy<'static>, String> {
    zbus::blocking::Connection::system()
        .and_then(|c| HelperProxy::new(&c))
        .map_err(|e| format!("cannot reach the system bus: {e}"))
}

/// Whether the helper is currently running; never starts it.
pub fn is_running(helper: &HelperProxy) -> bool {
    zbus::names::BusName::try_from(BUS_NAME).is_ok_and(|name| {
        zbus::blocking::fdo::DBusProxy::new(helper.inner().connection())
            .and_then(|bus| bus.name_has_owner(name).map_err(Into::into))
            .unwrap_or(false)
    })
}

/// Starts the helper if needed.
pub fn limits(helper: &HelperProxy) -> Result<Limits, String> {
    let rows = helper.list_limits().map_err(explain)?;
    rows.into_iter()
        .map(|(id, down, up, temp)| Ok((id, (decode(down, up)?, temp))))
        .collect()
}

pub fn explain(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) if name.as_str().ends_with(".ServiceUnknown") => {
            "the LoadGuard helper is not installed".into()
        }
        zbus::Error::MethodError(_, Some(text), _) => text,
        other => other.to_string(),
    }
}

pub fn encode(rate: Option<Rate>) -> u64 {
    rate.map_or(0, Rate::bytes)
}

/// Rejects out-of-range rates and limits that limit nothing.
pub fn decode(down: u64, up: u64) -> Result<Limit, String> {
    let rate = |v| {
        if v == 0 {
            Ok(None)
        } else {
            Rate::from_bytes(v).map(Some)
        }
    };
    let limit = Limit {
        down: rate(down)?,
        up: rate(up)?,
    };
    if limit == Limit::default() {
        return Err("at least one direction must be limited".into());
    }
    Ok(limit)
}
