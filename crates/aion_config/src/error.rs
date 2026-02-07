//! Error types for configuration loading and validation.

/// Errors that can occur when loading or validating an `aion.toml` configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// An I/O error occurred while reading the configuration file.
    #[error("failed to read configuration: {0}")]
    IoError(#[from] std::io::Error),

    /// The TOML content could not be parsed.
    #[error("failed to parse configuration: {0}")]
    ParseError(String),

    /// A referenced target name does not exist in the configuration.
    #[error("unknown target '{0}'")]
    UnknownTarget(String),

    /// A required field is missing from the configuration.
    #[error("missing required field: {0}")]
    MissingField(String),

    /// A configuration value failed validation.
    #[error("validation error: {0}")]
    ValidationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unknown_target() {
        let err = ConfigError::UnknownTarget("nonexistent".to_string());
        assert_eq!(format!("{err}"), "unknown target 'nonexistent'");
    }

    #[test]
    fn display_missing_field() {
        let err = ConfigError::MissingField("project.name".to_string());
        assert_eq!(format!("{err}"), "missing required field: project.name");
    }

    #[test]
    fn display_parse_error() {
        let err = ConfigError::ParseError("expected '=' at line 3".to_string());
        assert_eq!(
            format!("{err}"),
            "failed to parse configuration: expected '=' at line 3"
        );
    }

    #[test]
    fn display_validation_error() {
        let err = ConfigError::ValidationError("device not supported".to_string());
        assert_eq!(format!("{err}"), "validation error: device not supported");
    }

    #[test]
    fn display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = ConfigError::IoError(io_err);
        let display = format!("{err}");
        assert!(display.starts_with("failed to read configuration:"));
    }
}
