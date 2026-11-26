//! Validated cgroup paths. The privileged helper will rely on this check, so it
//! is deliberately strict.

use std::fmt;

/// A cgroup inside one user's `app.slice`, relative to the cgroup root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CgroupPath(String);

impl CgroupPath {
    /// Accepts `path` only if it is strictly below the app slice of `uid`.
    pub fn for_user(path: &str, uid: u32) -> Result<Self, String> {
        let bad = |why: &str| Err(format!("invalid cgroup path {path:?}: {why}"));
        let Some(rest) = path.strip_prefix(&format!("{}/", app_slice(uid))) else {
            return bad("outside the user's app slice");
        };
        if rest
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return bad("empty or relative component");
        }
        let allowed = |c: char| c.is_ascii_alphanumeric() || "/-_.@:\\+~".contains(c);
        if !path.chars().all(allowed) {
            return bad("unsupported character");
        }
        // nft reads backslashes literally, but only systemd's own \xNN escapes
        // are needed, so nothing else is accepted.
        let mut rest = path;
        while let Some(at) = rest.find('\\') {
            let escape = rest.get(at + 1..at + 4).unwrap_or_default();
            let mut chars = escape.chars();
            let valid = chars.next() == Some('x') && chars.all(|c| c.is_ascii_hexdigit());
            if escape.len() != 3 || !valid {
                return bad("backslash outside a \\xNN escape");
            }
            rest = &rest[at + 4..];
        }
        Ok(Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Depth below the cgroup root, as nftables counts it.
    pub fn level(&self) -> usize {
        self.0.split('/').count()
    }
}

impl fmt::Display for CgroupPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn app_slice(uid: u32) -> String {
    format!("user.slice/user-{uid}.slice/user@{uid}.service/app.slice")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_app_units() {
        let p = format!("{}/app-arch\\x2dupdate@autostart.service", app_slice(1000));
        assert_eq!(CgroupPath::for_user(&p, 1000).unwrap().level(), 5);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let base = app_slice(1000);
        for p in [
            "system.slice/sshd.service".to_owned(),
            format!("/{base}/a.scope"),
            base.clone(),
            format!("{base}/"),
            format!("{base}/../x.scope"),
            format!("{base}/a//b"),
            format!("{base}/a\" accept; #"),
            format!("{base}/a\nflush ruleset"),
            format!("{base}/evil\\"),
            format!("{base}/a\\x2"),
            format!("{base}/a\\y2d.scope"),
            format!("{base}/a\\xg1.scope"),
        ] {
            assert!(CgroupPath::for_user(&p, 1000).is_err(), "{p:?}");
        }
        assert!(CgroupPath::for_user(&format!("{base}/a.scope"), 1001).is_err());
    }
}
