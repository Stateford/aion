//! Error types for JTAG programming and device detection.

/// Errors that can occur while detecting or programming FPGA devices.
#[derive(Debug, thiserror::Error)]
pub enum FlashError {
    /// The external programmer executable could not be found.
    #[error(
        "programmer executable '{path}' not found (install openFPGALoader or use --loader-path)"
    )]
    LoaderNotFound {
        /// The executable path or name that was attempted.
        path: String,
    },

    /// The external programmer ran but exited unsuccessfully.
    #[error("programmer failed ({status}): {stderr}")]
    LoaderFailed {
        /// Human-readable exit status (e.g. `exit status: 1`).
        status: String,
        /// Captured standard-error output from the programmer.
        stderr: String,
    },

    /// The bitstream file to program does not exist on disk.
    #[error("bitstream file '{path}' not found; run `aion build --format rbf` to generate it")]
    BitstreamNotFound {
        /// The path that was checked.
        path: String,
    },

    /// A device was detected, but it does not match the configured target.
    #[error("connected device mismatch: expected {expected}, found {found}")]
    DeviceMismatch {
        /// The part number expected from the project configuration.
        expected: String,
        /// Description of the device(s) actually detected on the chain.
        found: String,
    },

    /// An I/O error occurred while invoking the programmer.
    #[error("failed to run programmer: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_not_found_message_includes_hint() {
        let err = FlashError::LoaderNotFound {
            path: "openFPGALoader".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("openFPGALoader"));
        assert!(msg.contains("--loader-path"));
    }

    #[test]
    fn loader_failed_message_includes_status_and_stderr() {
        let err = FlashError::LoaderFailed {
            status: "exit status: 1".to_string(),
            stderr: "JTAG init failed".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("exit status: 1"));
        assert!(msg.contains("JTAG init failed"));
    }

    #[test]
    fn bitstream_not_found_message_includes_build_hint() {
        let err = FlashError::BitstreamNotFound {
            path: "build/de0_nano/blinky.rbf".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("build/de0_nano/blinky.rbf"));
        assert!(msg.contains("aion build --format rbf"));
    }

    #[test]
    fn device_mismatch_message_names_both_sides() {
        let err = FlashError::DeviceMismatch {
            expected: "EP4CE22F17C6".to_string(),
            found: "XC7A35T".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("EP4CE22F17C6"));
        assert!(msg.contains("XC7A35T"));
    }

    #[test]
    fn io_error_converts_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: FlashError = io.into();
        assert!(err.to_string().contains("denied"));
    }
}
