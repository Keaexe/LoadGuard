use crate::engine::Firewall;
use loadguard_core::nft;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub struct Nftables {
    program: &'static str,
}

impl Nftables {
    pub fn new() -> Result<Self, String> {
        ["/usr/bin/nft", "/usr/sbin/nft"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .map(|program| Self { program })
            .ok_or_else(|| "nft is not installed".into())
    }

    fn run(&self, args: &[&str], input: &str) -> Result<String, String> {
        let mut child = Command::new(self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run nft: {e}"))?;
        // Dropping stdin closes it. A write error means nft exited early, and
        // its status below says why.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "nft failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    /// Removes the tables of every user, and nothing else.
    pub fn cleanup(&mut self) -> Result<(), String> {
        let tables = self.run(&["list", "tables", "inet"], "")?;
        for line in tables.lines() {
            let uid = line
                .trim()
                .strip_prefix("table inet loadguard_")
                .and_then(|u| u.parse().ok());
            if let Some(uid) = uid {
                self.apply(&nft::clear(uid))?;
            }
        }
        Ok(())
    }
}

impl Firewall for Nftables {
    fn apply(&mut self, rules: &str) -> Result<(), String> {
        self.run(&["-f", "-"], rules).map(drop)
    }

    fn has_table(&mut self, uid: u32) -> bool {
        let table = nft::table(uid);
        let args: Vec<&str> = ["list", "table"]
            .into_iter()
            .chain(table.split(' '))
            .collect();
        self.run(&args, "").is_ok()
    }
}
