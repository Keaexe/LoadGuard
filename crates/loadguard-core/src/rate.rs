//! Bandwidth values. Units are decimal: 1 KB = 1000 bytes.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A validated rate, stored in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rate(u64);

impl Rate {
    pub const MIN: u64 = 1_000;
    pub const MAX: u64 = 100_000_000_000;

    pub fn from_bytes(bytes: u64) -> Result<Self, String> {
        if (Self::MIN..=Self::MAX).contains(&bytes) {
            Ok(Self(bytes))
        } else {
            Err("rate must be between 1 KB/s and 100 GB/s".into())
        }
    }

    pub fn bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for Rate {
    type Err = String;

    /// Accepts `500` (KB/s), `500K`, `1.5MB/s`, `20Mbit`, `20 Mbit/s`, `20Mbps`.
    fn from_str(input: &str) -> Result<Self, String> {
        let s = input.trim().to_ascii_lowercase();
        let split = s
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len());
        let (number, unit) = s.split_at(split);
        let value: f64 = number
            .parse()
            .map_err(|_| format!("'{input}' is not a valid rate (try 500K or 20Mbit)"))?;
        let unit = unit.trim();
        let factor = match unit.strip_suffix("/s").unwrap_or(unit) {
            "" | "k" | "kb" => 1e3,
            "b" => 1.0,
            "m" | "mb" => 1e6,
            "g" | "gb" => 1e9,
            "kbit" | "kbps" => 125.0,
            "mbit" | "mbps" => 125e3,
            "gbit" | "gbps" => 125e6,
            other => return Err(format!("unknown unit '{other}' (use KB, MB, kbit or Mbit)")),
        };
        Self::from_bytes((value * factor).round() as u64)
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        if b.is_multiple_of(1_000_000) {
            write!(f, "{} MB/s", b / 1_000_000)
        } else if b.is_multiple_of(1_000) {
            write!(f, "{} KB/s", b / 1_000)
        } else {
            write!(f, "{b} B/s")
        }
    }
}

impl TryFrom<String> for Rate {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl From<Rate> for String {
    fn from(r: Rate) -> Self {
        r.to_string()
    }
}

/// Limits for one app. A missing direction is unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down: Option<Rate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up: Option<Rate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> u64 {
        s.parse::<Rate>().unwrap().bytes()
    }

    #[test]
    fn parses_units() {
        assert_eq!(bytes("500"), 500_000);
        assert_eq!(bytes("500KB/s"), 500_000);
        assert_eq!(bytes("1.5M"), 1_500_000);
        assert_eq!(bytes("20 Mbit/s"), 2_500_000);
        assert_eq!(bytes("8kbps"), 1_000);
    }

    #[test]
    fn rejects_bad_rates() {
        for s in [
            "",
            "0",
            "-5",
            "K",
            "1.2.3M",
            "5 parsecs",
            "inf",
            "999B",
            "1e30",
            "999999999G",
        ] {
            assert!(s.parse::<Rate>().is_err(), "{s} should be rejected");
        }
    }

    #[test]
    fn display_round_trips() {
        for n in [1_000, 1_234, 500_000, 2_500_000, 3_000_000] {
            let r = Rate::from_bytes(n).unwrap();
            assert_eq!(r.to_string().parse::<Rate>().unwrap(), r);
        }
    }
}
