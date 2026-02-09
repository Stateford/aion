//! Timing constraint types parsed from SDC/XDC files.
//!
//! This module defines the data structures that represent timing constraints
//! such as clock definitions, input/output delays, false paths, multicycle
//! paths, and maximum delay paths. These constraints drive static timing
//! analysis and timing-driven placement.

use aion_common::Ident;
use serde::{Deserialize, Serialize};

/// A collection of timing constraints for a design.
///
/// Populated by the SDC/XDC parser and consumed by the STA engine.
/// Contains all clock definitions, I/O delay specifications, and
/// path exceptions needed for timing analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimingConstraints {
    /// Clock definitions (`create_clock` commands).
    pub clocks: Vec<ClockConstraint>,
    /// Input delay constraints (`set_input_delay` commands).
    pub input_delays: Vec<IoDelay>,
    /// Output delay constraints (`set_output_delay` commands).
    pub output_delays: Vec<IoDelay>,
    /// False path exceptions that exclude paths from timing analysis.
    pub false_paths: Vec<FalsePath>,
    /// Multicycle path exceptions allowing more than one clock cycle.
    pub multicycle_paths: Vec<MulticyclePath>,
    /// Maximum delay constraints on specific paths.
    pub max_delay_paths: Vec<MaxDelayPath>,
}

impl TimingConstraints {
    /// Creates an empty set of timing constraints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the clock constraint with the given name, if any.
    pub fn find_clock(&self, name: Ident) -> Option<&ClockConstraint> {
        self.clocks.iter().find(|c| c.name == name)
    }

    /// Returns the number of defined clocks.
    pub fn clock_count(&self) -> usize {
        self.clocks.len()
    }
}

/// A clock constraint from a `create_clock` SDC command.
///
/// Defines a periodic clock signal with a given period and optional
/// waveform (rise/fall edge times within the period).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockConstraint {
    /// The name of the clock domain.
    pub name: Ident,
    /// Clock period in nanoseconds.
    pub period_ns: f64,
    /// The port or pin that the clock is applied to.
    pub port: Ident,
    /// Optional waveform specification: (rise_time_ns, fall_time_ns).
    /// If `None`, defaults to 50% duty cycle: (0, period/2).
    pub waveform: Option<(f64, f64)>,
}

impl ClockConstraint {
    /// Returns the target frequency in MHz for this clock.
    pub fn frequency_mhz(&self) -> f64 {
        if self.period_ns > 0.0 {
            1000.0 / self.period_ns
        } else {
            0.0
        }
    }

    /// Returns the duty cycle as a fraction (0.0 to 1.0).
    pub fn duty_cycle(&self) -> f64 {
        match self.waveform {
            Some((rise, fall)) => {
                if self.period_ns > 0.0 {
                    let high_time = if fall > rise {
                        fall - rise
                    } else {
                        self.period_ns - rise + fall
                    };
                    high_time / self.period_ns
                } else {
                    0.5
                }
            }
            None => 0.5,
        }
    }
}

/// An input or output delay constraint from `set_input_delay`/`set_output_delay`.
///
/// Specifies the external delay between a port and its associated clock,
/// used to constrain the timing at the design boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoDelay {
    /// The port that this delay applies to.
    pub port: Ident,
    /// The reference clock for this delay.
    pub clock: Ident,
    /// The delay value in nanoseconds.
    pub delay_ns: f64,
}

/// A false path exception from `set_false_path`.
///
/// Excludes the specified paths from timing analysis entirely. Paths
/// matching any `from` endpoint to any `to` endpoint are not checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FalsePath {
    /// Source endpoints (clocks, ports, or cells).
    pub from: Vec<Ident>,
    /// Destination endpoints (clocks, ports, or cells).
    pub to: Vec<Ident>,
}

/// A multicycle path exception from `set_multicycle_path`.
///
/// Allows the specified paths to take more than one clock cycle for
/// data propagation. The `cycles` field specifies how many clock
/// periods are allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MulticyclePath {
    /// Source endpoints.
    pub from: Vec<Ident>,
    /// Destination endpoints.
    pub to: Vec<Ident>,
    /// Number of clock cycles allowed for data propagation.
    pub cycles: u32,
}

/// A maximum delay constraint from `set_max_delay`.
///
/// Constrains the total delay from source to destination endpoints
/// to not exceed the specified value. Used for paths that don't
/// belong to a regular clock domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaxDelayPath {
    /// Source endpoints.
    pub from: Vec<Ident>,
    /// Destination endpoints.
    pub to: Vec<Ident>,
    /// Maximum allowed delay in nanoseconds.
    pub delay_ns: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;

    fn make_interner() -> Interner {
        Interner::new()
    }

    #[test]
    fn constraints_default_empty() {
        let tc = TimingConstraints::new();
        assert!(tc.clocks.is_empty());
        assert!(tc.input_delays.is_empty());
        assert!(tc.output_delays.is_empty());
        assert!(tc.false_paths.is_empty());
        assert!(tc.multicycle_paths.is_empty());
        assert!(tc.max_delay_paths.is_empty());
        assert_eq!(tc.clock_count(), 0);
    }

    #[test]
    fn clock_constraint_frequency() {
        let interner = make_interner();
        let clk = ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk_port"),
            waveform: None,
        };
        assert!((clk.frequency_mhz() - 100.0).abs() < 0.001);
    }

    #[test]
    fn clock_constraint_duty_cycle_default() {
        let interner = make_interner();
        let clk = ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk_port"),
            waveform: None,
        };
        assert!((clk.duty_cycle() - 0.5).abs() < 0.001);
    }

    #[test]
    fn clock_constraint_duty_cycle_custom() {
        let interner = make_interner();
        let clk = ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk_port"),
            waveform: Some((0.0, 7.0)),
        };
        assert!((clk.duty_cycle() - 0.7).abs() < 0.001);
    }

    #[test]
    fn clock_constraint_zero_period() {
        let interner = make_interner();
        let clk = ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 0.0,
            port: interner.get_or_intern("clk_port"),
            waveform: None,
        };
        assert_eq!(clk.frequency_mhz(), 0.0);
    }

    #[test]
    fn find_clock_by_name() {
        let interner = make_interner();
        let name = interner.get_or_intern("sys_clk");
        let mut tc = TimingConstraints::new();
        tc.clocks.push(ClockConstraint {
            name,
            period_ns: 8.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });
        assert!(tc.find_clock(name).is_some());
        assert_eq!(tc.find_clock(name).unwrap().period_ns, 8.0);
    }

    #[test]
    fn find_clock_not_found() {
        let interner = make_interner();
        let tc = TimingConstraints::new();
        let name = interner.get_or_intern("nonexistent");
        assert!(tc.find_clock(name).is_none());
    }

    #[test]
    fn io_delay_construction() {
        let interner = make_interner();
        let d = IoDelay {
            port: interner.get_or_intern("data_in"),
            clock: interner.get_or_intern("clk"),
            delay_ns: 2.5,
        };
        assert_eq!(d.delay_ns, 2.5);
    }

    #[test]
    fn false_path_construction() {
        let interner = make_interner();
        let fp = FalsePath {
            from: vec![interner.get_or_intern("clk_a")],
            to: vec![interner.get_or_intern("clk_b")],
        };
        assert_eq!(fp.from.len(), 1);
        assert_eq!(fp.to.len(), 1);
    }

    #[test]
    fn multicycle_path_construction() {
        let interner = make_interner();
        let mp = MulticyclePath {
            from: vec![interner.get_or_intern("slow_reg")],
            to: vec![interner.get_or_intern("fast_reg")],
            cycles: 3,
        };
        assert_eq!(mp.cycles, 3);
    }

    #[test]
    fn max_delay_path_construction() {
        let interner = make_interner();
        let md = MaxDelayPath {
            from: vec![interner.get_or_intern("src")],
            to: vec![interner.get_or_intern("dst")],
            delay_ns: 15.0,
        };
        assert_eq!(md.delay_ns, 15.0);
    }

    #[test]
    fn constraints_serde_roundtrip() {
        let interner = make_interner();
        let mut tc = TimingConstraints::new();
        tc.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk_port"),
            waveform: Some((0.0, 5.0)),
        });
        tc.input_delays.push(IoDelay {
            port: interner.get_or_intern("din"),
            clock: interner.get_or_intern("clk"),
            delay_ns: 1.5,
        });
        let json = serde_json::to_string(&tc).unwrap();
        let restored: TimingConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.clocks.len(), 1);
        assert_eq!(restored.input_delays.len(), 1);
    }

    #[test]
    fn clock_count() {
        let interner = make_interner();
        let mut tc = TimingConstraints::new();
        assert_eq!(tc.clock_count(), 0);
        tc.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk1"),
            period_ns: 10.0,
            port: interner.get_or_intern("p1"),
            waveform: None,
        });
        tc.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk2"),
            period_ns: 5.0,
            port: interner.get_or_intern("p2"),
            waveform: None,
        });
        assert_eq!(tc.clock_count(), 2);
    }
}
