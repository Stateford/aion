//! Simulation error types for the event-driven HDL simulator.
//!
//! All errors that can occur during simulation construction or execution
//! are represented as variants of [`SimError`].

use std::io;

/// Errors that can occur during simulation setup or execution.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// The design has no top-level module set.
    #[error("design has no top-level module")]
    NoTopModule,

    /// A referenced module was not found in the design.
    #[error("module with ID {0} not found in design")]
    ModuleNotFound(u32),

    /// An expression could not be evaluated during simulation.
    #[error("evaluation error: {reason}")]
    EvalError {
        /// Description of what went wrong during evaluation.
        reason: String,
    },

    /// A signal reference could not be resolved.
    #[error("invalid signal reference: {reason}")]
    InvalidSignalRef {
        /// Description of why the signal reference is invalid.
        reason: String,
    },

    /// Division by zero encountered during expression evaluation.
    #[error("division by zero")]
    DivisionByZero,

    /// An unsupported construct was encountered during simulation.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// Description of the unsupported construct.
        reason: String,
    },

    /// Simulation was terminated by a `$finish` system task.
    #[error("simulation finished at {time_fs} fs")]
    Finished {
        /// Time in femtoseconds when `$finish` was called.
        time_fs: u64,
    },

    /// An assertion failed during simulation.
    #[error("assertion failed at {time_fs} fs: {message}")]
    AssertionFailed {
        /// Time in femtoseconds when the assertion failed.
        time_fs: u64,
        /// The assertion failure message.
        message: String,
    },

    /// An I/O error occurred while writing waveform data.
    #[error("waveform I/O error: {0}")]
    WaveformIo(#[from] io::Error),

    /// The simulation exceeded the configured time limit.
    #[error("time limit exceeded: {limit_fs} fs")]
    TimeLimitExceeded {
        /// The time limit in femtoseconds.
        limit_fs: u64,
    },

    /// Too many delta cycles at a single time step, indicating a combinational loop.
    #[error("delta cycle limit exceeded at {fs} fs (max {max_deltas} deltas)")]
    DeltaCycleLimit {
        /// The time in femtoseconds where the limit was hit.
        fs: u64,
        /// The maximum number of delta cycles allowed.
        max_deltas: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_top_module_display() {
        let e = SimError::NoTopModule;
        assert_eq!(e.to_string(), "design has no top-level module");
    }

    #[test]
    fn module_not_found_display() {
        let e = SimError::ModuleNotFound(42);
        assert_eq!(e.to_string(), "module with ID 42 not found in design");
    }

    #[test]
    fn eval_error_display() {
        let e = SimError::EvalError {
            reason: "unknown function".into(),
        };
        assert_eq!(e.to_string(), "evaluation error: unknown function");
    }

    #[test]
    fn invalid_signal_ref_display() {
        let e = SimError::InvalidSignalRef {
            reason: "out of range".into(),
        };
        assert_eq!(e.to_string(), "invalid signal reference: out of range");
    }

    #[test]
    fn division_by_zero_display() {
        let e = SimError::DivisionByZero;
        assert_eq!(e.to_string(), "division by zero");
    }

    #[test]
    fn unsupported_display() {
        let e = SimError::Unsupported {
            reason: "real types".into(),
        };
        assert_eq!(e.to_string(), "unsupported: real types");
    }

    #[test]
    fn finished_display() {
        let e = SimError::Finished { time_fs: 1000 };
        assert_eq!(e.to_string(), "simulation finished at 1000 fs");
    }

    #[test]
    fn assertion_failed_display() {
        let e = SimError::AssertionFailed {
            time_fs: 500,
            message: "count != 3".into(),
        };
        assert_eq!(e.to_string(), "assertion failed at 500 fs: count != 3");
    }

    #[test]
    fn waveform_io_display() {
        let e = SimError::WaveformIo(io::Error::new(io::ErrorKind::NotFound, "file not found"));
        assert!(e.to_string().contains("waveform I/O error"));
    }

    #[test]
    fn time_limit_exceeded_display() {
        let e = SimError::TimeLimitExceeded {
            limit_fs: 1_000_000,
        };
        assert_eq!(e.to_string(), "time limit exceeded: 1000000 fs");
    }

    #[test]
    fn delta_cycle_limit_display() {
        let e = SimError::DeltaCycleLimit {
            fs: 100,
            max_deltas: 10000,
        };
        assert_eq!(
            e.to_string(),
            "delta cycle limit exceeded at 100 fs (max 10000 deltas)"
        );
    }
}
