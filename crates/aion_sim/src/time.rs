//! Simulation time representation with femtosecond precision and delta cycles.
//!
//! [`SimTime`] tracks both wall-clock simulation time (in femtoseconds) and
//! the delta cycle index within a single time step, enabling correct ordering
//! of events in the simulation kernel.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Femtoseconds per picosecond.
pub const FS_PER_PS: u64 = 1_000;
/// Femtoseconds per nanosecond.
pub const FS_PER_NS: u64 = 1_000_000;
/// Femtoseconds per microsecond.
pub const FS_PER_US: u64 = 1_000_000_000;
/// Femtoseconds per millisecond.
pub const FS_PER_MS: u64 = 1_000_000_000_000;

/// A simulation time point with femtosecond resolution and delta cycle tracking.
///
/// Events are ordered first by femtosecond timestamp, then by delta cycle index.
/// Delta cycles represent instantaneous signal propagation steps within a single
/// time step, following IEEE 1364/1800 simulation semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimTime {
    /// Wall-clock simulation time in femtoseconds.
    pub fs: u64,
    /// Delta cycle index within the current time step.
    pub delta: u32,
}

impl SimTime {
    /// Creates a time point at time zero, delta zero.
    pub fn zero() -> Self {
        Self { fs: 0, delta: 0 }
    }

    /// Creates a time from a nanosecond value with delta 0.
    pub fn from_ns(ns: u64) -> Self {
        Self {
            fs: ns * FS_PER_NS,
            delta: 0,
        }
    }

    /// Creates a time from a picosecond value with delta 0.
    pub fn from_ps(ps: u64) -> Self {
        Self {
            fs: ps * FS_PER_PS,
            delta: 0,
        }
    }

    /// Creates a time from a femtosecond value with delta 0.
    pub fn from_fs(fs: u64) -> Self {
        Self { fs, delta: 0 }
    }

    /// Returns the next delta cycle at the same wall-clock time.
    pub fn next_delta(&self) -> Self {
        Self {
            fs: self.fs,
            delta: self.delta + 1,
        }
    }

    /// Advances to a new wall-clock time, resetting the delta counter.
    pub fn advance_to(&self, new_fs: u64) -> Self {
        debug_assert!(
            new_fs >= self.fs,
            "cannot advance backwards: {} -> {}",
            self.fs,
            new_fs
        );
        Self {
            fs: new_fs,
            delta: 0,
        }
    }

    /// Converts the femtosecond timestamp to nanoseconds (truncated).
    pub fn to_ns(&self) -> u64 {
        self.fs / FS_PER_NS
    }
}

impl Default for SimTime {
    fn default() -> Self {
        Self::zero()
    }
}

impl Ord for SimTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.fs.cmp(&other.fs).then(self.delta.cmp(&other.delta))
    }
}

impl PartialOrd for SimTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for SimTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fs = self.fs;
        if fs == 0 {
            write!(f, "0 fs")?;
        } else if fs >= FS_PER_MS && fs.is_multiple_of(FS_PER_MS) {
            write!(f, "{} ms", fs / FS_PER_MS)?;
        } else if fs >= FS_PER_US && fs.is_multiple_of(FS_PER_US) {
            write!(f, "{} us", fs / FS_PER_US)?;
        } else if fs >= FS_PER_NS && fs.is_multiple_of(FS_PER_NS) {
            write!(f, "{} ns", fs / FS_PER_NS)?;
        } else if fs >= FS_PER_PS && fs.is_multiple_of(FS_PER_PS) {
            write!(f, "{} ps", fs / FS_PER_PS)?;
        } else {
            write!(f, "{fs} fs")?;
        }
        if self.delta > 0 {
            write!(f, "+d{}", self.delta)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_time() {
        let t = SimTime::zero();
        assert_eq!(t.fs, 0);
        assert_eq!(t.delta, 0);
    }

    #[test]
    fn from_ns() {
        let t = SimTime::from_ns(10);
        assert_eq!(t.fs, 10_000_000);
        assert_eq!(t.delta, 0);
    }

    #[test]
    fn from_ps() {
        let t = SimTime::from_ps(500);
        assert_eq!(t.fs, 500_000);
    }

    #[test]
    fn from_fs() {
        let t = SimTime::from_fs(42);
        assert_eq!(t.fs, 42);
    }

    #[test]
    fn next_delta() {
        let t = SimTime::from_ns(5);
        let t2 = t.next_delta();
        assert_eq!(t2.fs, t.fs);
        assert_eq!(t2.delta, 1);
        let t3 = t2.next_delta();
        assert_eq!(t3.delta, 2);
    }

    #[test]
    fn advance_to() {
        let t = SimTime { fs: 100, delta: 5 };
        let t2 = t.advance_to(200);
        assert_eq!(t2.fs, 200);
        assert_eq!(t2.delta, 0);
    }

    #[test]
    fn to_ns() {
        let t = SimTime::from_ns(42);
        assert_eq!(t.to_ns(), 42);
        // Truncation
        let t2 = SimTime::from_fs(1_500_000);
        assert_eq!(t2.to_ns(), 1);
    }

    #[test]
    fn ordering_by_fs() {
        let a = SimTime::from_ns(1);
        let b = SimTime::from_ns(2);
        assert!(a < b);
    }

    #[test]
    fn ordering_by_delta() {
        let a = SimTime { fs: 100, delta: 0 };
        let b = SimTime { fs: 100, delta: 1 };
        assert!(a < b);
    }

    #[test]
    fn ordering_fs_takes_precedence() {
        let a = SimTime { fs: 200, delta: 0 };
        let b = SimTime { fs: 100, delta: 99 };
        assert!(a > b);
    }

    #[test]
    fn display_zero() {
        assert_eq!(SimTime::zero().to_string(), "0 fs");
    }

    #[test]
    fn display_ns() {
        assert_eq!(SimTime::from_ns(10).to_string(), "10 ns");
    }

    #[test]
    fn display_ps() {
        assert_eq!(SimTime::from_ps(500).to_string(), "500 ps");
    }

    #[test]
    fn display_us() {
        let t = SimTime::from_fs(5 * FS_PER_US);
        assert_eq!(t.to_string(), "5 us");
    }

    #[test]
    fn display_ms() {
        let t = SimTime::from_fs(2 * FS_PER_MS);
        assert_eq!(t.to_string(), "2 ms");
    }

    #[test]
    fn display_fs_fractional() {
        let t = SimTime::from_fs(1500);
        assert_eq!(t.to_string(), "1500 fs");
    }

    #[test]
    fn display_with_delta() {
        let t = SimTime {
            fs: FS_PER_NS,
            delta: 3,
        };
        assert_eq!(t.to_string(), "1 ns+d3");
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(SimTime::default(), SimTime::zero());
    }

    #[test]
    fn serde_roundtrip() {
        let t = SimTime {
            fs: 12345,
            delta: 7,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: SimTime = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
