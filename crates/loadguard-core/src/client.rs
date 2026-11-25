//! D-Bus client for the privileged helper. Rates are bytes per second; 0 means unlimited.

use crate::{Limit, Rate};

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
