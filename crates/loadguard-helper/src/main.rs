//! Privileged service. Callers name an app and a rate; they can never supply
//! cgroup paths, rules or a user id, and only ever affect their own apps.

mod engine;
mod nftables;

use engine::{Engine, Entry};
use loadguard_core::{apps::CGROUP_ROOT, client};
use nftables::Nftables;
use std::{
    collections::HashMap,
    env, fs,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread,
    time::Duration,
};
use zbus::{Connection, fdo, message::Header, zvariant::Value};

const ACTION: &str = "io.github.keaexe.LoadGuard.manage";
const POLL: Duration = Duration::from_secs(1);
/// Check every this many polls that nobody flushed our rules.
const VERIFY_EVERY: u32 = 10;

type Shared = Arc<Mutex<Engine<Nftables>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Service {
    engine: Shared,
    /// Users who authenticated, with the login they did so in.
    authorized: Mutex<HashMap<u32, u64>>,
}

/// Identifies a login: the user's slice is recreated, with a new inode, after a
/// full logout.
fn login_id(uid: u32) -> Option<u64> {
    fs::metadata(format!("{CGROUP_ROOT}/user.slice/user-{uid}.slice"))
        .ok()
        .map(|m| m.ino())
}

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority",
    gen_blocking = false
)]
trait Authority {
    #[allow(clippy::type_complexity)]
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

fn sender(header: &Header<'_>) -> fdo::Result<String> {
    header
        .sender()
        .map(ToString::to_string)
        .ok_or_else(|| fdo::Error::AccessDenied("unknown caller".into()))
}

async fn caller_uid(conn: &Connection, header: &Header<'_>) -> fdo::Result<u32> {
    let name = sender(header)?;
    let name = zbus::names::BusName::try_from(name.as_str())
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;
    fdo::DBusProxy::new(conn)
        .await?
        .get_connection_unix_user(name)
        .await
}

impl Service {
    /// Returns the caller's uid, asking for a password once per login.
    async fn authorize(&self, conn: &Connection, header: &Header<'_>) -> fdo::Result<u32> {
        let uid = caller_uid(conn, header).await?;
        let login = login_id(uid);
        if login.is_some() && lock(&self.authorized).get(&uid).copied() == login {
            return Ok(uid);
        }
        let name = sender(header)?;
        let subject = (
            "system-bus-name",
            HashMap::from([("name", Value::from(name.as_str()))]),
        );
        let allow_interaction = 1;
        let (authorized, _, _) = AuthorityProxy::new(conn)
            .await?
            .check_authorization(&subject, ACTION, HashMap::new(), allow_interaction, "")
            .await?;
        if !authorized {
            return Err(fdo::Error::AccessDenied("not authorized".into()));
        }
        if let Some(login) = login {
            lock(&self.authorized).insert(uid, login);
        }
        Ok(uid)
    }
}

#[zbus::interface(name = "io.github.keaexe.LoadGuard1")]
impl Service {
    async fn set_limit(
        &self,
        app: &str,
        down: u64,
        up: u64,
        until_closed: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let limit = client::decode(down, up).map_err(fdo::Error::InvalidArgs)?;
        let uid = self.authorize(conn, &header).await?;
        lock(&self.engine)
            .set(
                uid,
                app,
                Entry {
                    limit,
                    until_closed,
                },
            )
            .map_err(fdo::Error::Failed)
    }

    async fn clear_limit(
        &self,
        app: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let uid = self.authorize(conn, &header).await?;
        lock(&self.engine)
            .clear(uid, app)
            .map_err(fdo::Error::Failed)
    }

    async fn clear_all(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let uid = self.authorize(conn, &header).await?;
        lock(&self.engine)
            .clear_all(uid)
            .map_err(fdo::Error::Failed)
    }

    /// Needs no password: callers only see their own limits.
    async fn list_limits(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<Vec<(String, u64, u64, bool)>> {
        let uid = caller_uid(conn, &header).await?;
        Ok(lock(&self.engine)
            .list(uid)
            .into_iter()
            .map(|(id, e)| {
                let Entry {
                    limit,
                    until_closed,
                } = e;
                (
                    id,
                    client::encode(limit.down),
                    client::encode(limit.up),
                    until_closed,
                )
            })
            .collect())
    }
}

fn run() -> Result<(), String> {
    let mut firewall = Nftables::new()?;
    firewall.cleanup()?;
    if env::args().nth(1).as_deref() == Some("--cleanup") {
        return Ok(());
    }
    let state: PathBuf = env::var_os("STATE_DIRECTORY")
        .ok_or("STATE_DIRECTORY is not set; start this through systemd")?
        .into();
    let engine = Engine::new(CGROUP_ROOT.as_ref(), &state, firewall).map_err(|e| e.to_string())?;
    let engine = Arc::new(Mutex::new(engine));
    let service = Service {
        engine: engine.clone(),
        authorized: Mutex::default(),
    };
    let _conn = zbus::blocking::connection::Builder::system()
        .and_then(|b| b.name(client::BUS_NAME))
        .and_then(|b| b.serve_at(client::OBJECT_PATH, service))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    let mut polls = 0u32;
    loop {
        lock(&engine).tick(polls.is_multiple_of(VERIFY_EVERY));
        polls = polls.wrapping_add(1);
        thread::sleep(POLL);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
