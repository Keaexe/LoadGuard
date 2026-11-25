//! Generates the nftables ruleset. The whole table is replaced atomically, so
//! the result always reflects the complete desired state.

use crate::{CgroupPath, Limit, Rate};
use std::fmt::Write;

pub const TABLE: &str = "inet loadguard";

/// A limit and every cgroup that shares it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub cgroups: Vec<CgroupPath>,
    pub limit: Limit,
}

/// Removes every LoadGuard rule; succeeds even if there are none.
pub fn clear() -> String {
    format!("add table {TABLE}\ndelete table {TABLE}\n")
}

/// `exempt_loopback` leaves localhost traffic alone; only tests disable it.
pub fn ruleset(entries: &[Entry], exempt_loopback: bool) -> String {
    let mut chains = String::new();
    let mut input = String::new();
    let mut output = String::new();

    for (i, entry) in entries.iter().enumerate() {
        for (rate, name, hook) in [
            (entry.limit.down, format!("down{i}"), &mut input),
            (entry.limit.up, format!("up{i}"), &mut output),
        ] {
            let Some(rate) = rate else { continue };
            let _ = writeln!(
                chains,
                "\tchain {name} {{\n\t\tlimit rate over {} bytes/second burst {} bytes counter drop\n\t}}",
                rate.bytes(),
                burst(rate)
            );
            // One chain per app, so all of its cgroups share a single budget.
            for cg in &entry.cgroups {
                let _ = writeln!(
                    hook,
                    "\t\tsocket cgroupv2 level {} \"{cg}\" jump {name}",
                    cg.level()
                );
            }
        }
    }

    let mut out = clear();
    let _ = writeln!(out, "table {TABLE} {{");
    out += &chains;
    for (name, rules, lo) in [("input", input, "iif"), ("output", output, "oif")] {
        let _ = writeln!(
            out,
            "\tchain {name} {{\n\t\ttype filter hook {name} priority filter; policy accept;"
        );
        if exempt_loopback {
            let _ = writeln!(out, "\t\t{lo} \"lo\" accept");
        }
        out += &rules;
        out += "\t}\n";
    }
    out += "}\n";
    out
}

/// A packet larger than the burst can never pass, and segmentation offload
/// produces packets of up to 64 KiB plus headers, hence the floor.
fn burst(rate: Rate) -> u64 {
    (rate.bytes() / 8).clamp(128 * 1024, 4 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgroup::app_slice;

    fn entry(limit: Limit) -> Entry {
        let cg = |n| CgroupPath::for_user(&format!("{}/{n}", app_slice(1000)), 1000).unwrap();
        Entry {
            cgroups: vec![cg("a.scope"), cg("b.scope")],
            limit,
        }
    }

    #[test]
    fn cgroups_of_one_app_share_a_chain() {
        let r = ruleset(
            &[entry(Limit {
                down: "500".parse().ok(),
                up: None,
            })],
            true,
        );
        assert_eq!(r.matches("jump down0").count(), 2);
        assert_eq!(r.matches("limit rate over 500000 bytes/second").count(), 1);
        assert!(!r.contains("up0"));
        assert!(r.contains("iif \"lo\" accept"));
    }

    #[test]
    fn only_touches_its_own_table() {
        let r = ruleset(&[], true);
        assert!(!r.contains("flush"));
        assert!(
            r.lines()
                .filter(|l| l.contains("table"))
                .all(|l| l.contains(TABLE))
        );
    }
}
