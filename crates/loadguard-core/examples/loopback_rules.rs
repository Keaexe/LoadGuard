//! Prints rules for one cgroup with loopback limited. Used by scripts/test-limits.sh.
//! Usage: loopback_rules <uid> <cgroup> <rate> <down|up>

use loadguard_core::{CgroupPath, Limit, nft};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [uid, path, rate, direction] = &args[..] else {
        return Err("usage: loopback_rules <uid> <cgroup> <rate> <down|up>".into());
    };
    let uid = uid.parse().map_err(|_| "bad uid")?;
    let rate = Some(rate.parse()?);
    let limit = match direction.as_str() {
        "down" => Limit {
            down: rate,
            up: None,
        },
        "up" => Limit {
            down: None,
            up: rate,
        },
        _ => return Err("direction must be down or up".into()),
    };
    let entry = nft::Entry {
        cgroups: vec![CgroupPath::for_user(path, uid)?],
        limit,
    };
    print!("{}", nft::ruleset(&[entry], false));
    Ok(())
}
