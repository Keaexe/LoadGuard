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

/// Unit used to show rates and to read bare numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    #[serde(rename = "KB/s")]
    KiloBytes,
    #[serde(rename = "Mbit/s")]
    MegaBits,
}

impl Unit {
    pub fn label(self) -> &'static str {
        match self {
            Self::KiloBytes => "KB/s",
            Self::MegaBits => "Mbit/s",
        }
    }

    fn bytes(self) -> f64 {
        match self {
            Self::KiloBytes => 1e3,
            Self::MegaBits => 125e3,
        }
    }

    /// The number alone, with at most three decimals.
    pub fn number(self, rate: Rate) -> String {
        let text = format!("{:.3}", rate.bytes() as f64 / self.bytes());
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }

    /// Reads a number in this unit, or a rate with its own unit. Accepts a decimal comma.
    pub fn parse(self, text: &str) -> Result<Rate, String> {
        let text = text.trim().replace(',', ".");
        if text.ends_with(|c: char| c.is_ascii_digit()) {
            format!("{text}{}", self.label()).parse()
        } else {
            text.parse()
        }
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
    fn units_format_and_parse() {
        let r = Rate::from_bytes(2_500_000).unwrap();
        assert_eq!(Unit::KiloBytes.number(r), "2500");
        assert_eq!(Unit::MegaBits.number(r), "20");
        assert_eq!(Unit::MegaBits.parse("20").unwrap(), r);
        assert_eq!(Unit::MegaBits.parse("0,5").unwrap().bytes(), 62_500);
        assert_eq!(Unit::MegaBits.parse("500 KB").unwrap().bytes(), 500_000);
        assert!(Unit::KiloBytes.parse("").is_err());
        assert!(Unit::KiloBytes.parse("0").is_err());
    }

    #[test]
    fn display_round_trips() {
        for n in [1_000, 1_234, 500_000, 2_500_000, 3_000_000] {
            let r = Rate::from_bytes(n).unwrap();
            assert_eq!(r.to_string().parse::<Rate>().unwrap(), r);
        }
    }
}
