//! Stand-in for polkit on a private test bus. Allows or denies everything and
//! prints each request. Usage: mock_polkit <allow|deny>

use std::collections::HashMap;
use zbus::zvariant::OwnedValue;

struct Mock(bool);

#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Mock {
    fn check_authorization(
        &self,
        subject: (String, HashMap<String, OwnedValue>),
        action_id: &str,
        _details: HashMap<String, String>,
        flags: u32,
        _cancellation_id: &str,
    ) -> (bool, bool, HashMap<String, String>) {
        println!("asked {} {action_id} flags={flags}", subject.0);
        (self.0, false, HashMap::new())
    }
}

fn main() -> zbus::Result<()> {
    let allow = std::env::args().nth(1).as_deref() == Some("allow");
    let _conn = zbus::blocking::connection::Builder::system()?
        .name("org.freedesktop.PolicyKit1")?
        .serve_at("/org/freedesktop/PolicyKit1/Authority", Mock(allow))?
        .build()?;
    loop {
        std::thread::park();
    }
}
