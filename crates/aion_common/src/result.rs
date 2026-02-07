//! Common result and error types for the Aion toolchain.

/// The standard result type for fallible internal operations.
///
/// `Ok` contains the result value (which may be partial or degraded after
/// error recovery). `Err` indicates an unrecoverable internal error (a bug
/// in Aion), not a user-facing error. User errors are reported through
/// [`DiagnosticSink`](aion_diagnostics) and the operation still returns `Ok`.
pub type AionResult<T> = Result<T, InternalError>;

/// An internal compiler error indicating a bug in Aion, not a user input problem.
///
/// These errors should never occur during normal operation. If one does occur,
/// it means there is a logic error in the compiler that should be fixed.
#[derive(Debug, thiserror::Error)]
#[error("internal compiler error: {message}")]
pub struct InternalError {
    /// Description of the internal error.
    pub message: String,
}

impl InternalError {
    /// Creates a new internal error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<String> for InternalError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format() {
        let err = InternalError::new("something broke");
        assert_eq!(format!("{err}"), "internal compiler error: something broke");
    }

    #[test]
    fn ok_path() {
        let r: AionResult<i32> = Ok(42);
        assert!(r.is_ok());
        assert_eq!(r.ok(), Some(42));
    }

    #[test]
    fn err_path() {
        let r: AionResult<i32> = Err(InternalError::new("test error"));
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert_eq!(err.message, "test error");
    }

    #[test]
    fn from_string() {
        let err: InternalError = "from string".to_string().into();
        assert_eq!(err.message, "from string");
    }
}
