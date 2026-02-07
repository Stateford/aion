//! Frequency values with unit parsing and display.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A frequency value stored in Hertz.
///
/// Supports parsing from strings like "50MHz", "100KHz", "1GHz", "48000Hz",
/// and bare numeric values (interpreted as Hz). Displays using the most
/// appropriate unit for readability.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frequency(f64);

impl Frequency {
    /// Creates a new frequency from a value in Hertz.
    pub fn new(hz: f64) -> Self {
        Self(hz)
    }

    /// Returns the frequency in Hertz.
    pub fn hz(&self) -> f64 {
        self.0
    }

    /// Returns the frequency in kilohertz.
    pub fn khz(&self) -> f64 {
        self.0 / 1_000.0
    }

    /// Returns the frequency in megahertz.
    pub fn mhz(&self) -> f64 {
        self.0 / 1_000_000.0
    }

    /// Returns the frequency in gigahertz.
    pub fn ghz(&self) -> f64 {
        self.0 / 1_000_000_000.0
    }
}

impl fmt::Debug for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frequency({self})")
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hz = self.0;
        if hz >= 1_000_000_000.0 {
            write!(f, "{}GHz", hz / 1_000_000_000.0)
        } else if hz >= 1_000_000.0 {
            write!(f, "{}MHz", hz / 1_000_000.0)
        } else if hz >= 1_000.0 {
            write!(f, "{}KHz", hz / 1_000.0)
        } else {
            write!(f, "{hz}Hz")
        }
    }
}

/// Error type for parsing frequency strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFrequencyError {
    /// The input string that failed to parse.
    pub input: String,
}

impl fmt::Display for ParseFrequencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid frequency: '{}'", self.input)
    }
}

impl std::error::Error for ParseFrequencyError {}

impl FromStr for Frequency {
    type Err = ParseFrequencyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let err = || ParseFrequencyError {
            input: s.to_string(),
        };

        // Try suffixed formats (case-insensitive)
        let lower = s.to_ascii_lowercase();
        if let Some(num) = lower.strip_suffix("ghz") {
            let val: f64 = num.trim().parse().map_err(|_| err())?;
            return Ok(Frequency(val * 1_000_000_000.0));
        }
        if let Some(num) = lower.strip_suffix("mhz") {
            let val: f64 = num.trim().parse().map_err(|_| err())?;
            return Ok(Frequency(val * 1_000_000.0));
        }
        if let Some(num) = lower.strip_suffix("khz") {
            let val: f64 = num.trim().parse().map_err(|_| err())?;
            return Ok(Frequency(val * 1_000.0));
        }
        if let Some(num) = lower.strip_suffix("hz") {
            let val: f64 = num.trim().parse().map_err(|_| err())?;
            return Ok(Frequency(val));
        }

        // Bare number — interpreted as Hz
        let val: f64 = s.parse().map_err(|_| err())?;
        Ok(Frequency(val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ghz() {
        let f: Frequency = "1GHz".parse().unwrap();
        assert_eq!(f.hz(), 1_000_000_000.0);
    }

    #[test]
    fn parse_mhz() {
        let f: Frequency = "50MHz".parse().unwrap();
        assert_eq!(f.hz(), 50_000_000.0);
    }

    #[test]
    fn parse_khz() {
        let f: Frequency = "100KHz".parse().unwrap();
        assert_eq!(f.hz(), 100_000.0);
    }

    #[test]
    fn parse_hz() {
        let f: Frequency = "48000Hz".parse().unwrap();
        assert_eq!(f.hz(), 48_000.0);
    }

    #[test]
    fn parse_bare_number() {
        let f: Frequency = "25000000".parse().unwrap();
        assert_eq!(f.hz(), 25_000_000.0);
    }

    #[test]
    fn parse_case_insensitive() {
        let f: Frequency = "50mhz".parse().unwrap();
        assert_eq!(f.hz(), 50_000_000.0);
    }

    #[test]
    fn parse_invalid() {
        let r = "not_a_freq".parse::<Frequency>();
        assert!(r.is_err());
    }

    #[test]
    fn accessor_methods() {
        let f = Frequency::new(1_000_000_000.0);
        assert_eq!(f.hz(), 1_000_000_000.0);
        assert_eq!(f.khz(), 1_000_000.0);
        assert_eq!(f.mhz(), 1_000.0);
        assert_eq!(f.ghz(), 1.0);
    }

    #[test]
    fn display_selects_best_unit() {
        assert_eq!(format!("{}", Frequency::new(1_000_000_000.0)), "1GHz");
        assert_eq!(format!("{}", Frequency::new(50_000_000.0)), "50MHz");
        assert_eq!(format!("{}", Frequency::new(100_000.0)), "100KHz");
        assert_eq!(format!("{}", Frequency::new(44_100.0)), "44.1KHz");
        assert_eq!(format!("{}", Frequency::new(500.0)), "500Hz");
    }
}
