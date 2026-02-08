//! Error types for cache operations.

use std::path::PathBuf;

/// Errors that can occur during cache operations.
///
/// Most cache operations are fail-safe: errors result in cache misses
/// rather than hard failures. This enum is used for internal error
/// propagation within the cache subsystem.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// An I/O error occurred while reading or writing cache files.
    #[error("cache I/O error at {path}: {source}")]
    Io {
        /// The path that caused the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The cache manifest could not be parsed as valid JSON.
    #[error("failed to parse cache manifest: {reason}")]
    ManifestParse {
        /// Description of the parse failure.
        reason: String,
    },

    /// An artifact file has an invalid or missing header.
    #[error("invalid artifact header in {path}: {reason}")]
    InvalidHeader {
        /// The artifact file path.
        path: PathBuf,
        /// Description of the header problem.
        reason: String,
    },

    /// The stored checksum does not match the computed checksum of the payload.
    #[error("checksum mismatch in {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// The artifact file path.
        path: PathBuf,
        /// The expected checksum from the header.
        expected: String,
        /// The actual checksum computed from the payload.
        actual: String,
    },

    /// The artifact format version does not match the current version.
    #[error("version mismatch in {path}: expected {expected}, got {actual}")]
    VersionMismatch {
        /// The artifact file path.
        path: PathBuf,
        /// The expected format version.
        expected: u32,
        /// The actual format version found in the file.
        actual: u32,
    },

    /// A serialization or deserialization error occurred.
    #[error("serialization error: {reason}")]
    Serialization {
        /// Description of the serialization failure.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_display() {
        let err = CacheError::Io {
            path: PathBuf::from("/tmp/cache/manifest.json"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
        };
        let msg = err.to_string();
        assert!(msg.contains("cache I/O error"));
        assert!(msg.contains("manifest.json"));
    }

    #[test]
    fn manifest_parse_display() {
        let err = CacheError::ManifestParse {
            reason: "unexpected EOF".to_string(),
        };
        assert!(err.to_string().contains("unexpected EOF"));
    }

    #[test]
    fn invalid_header_display() {
        let err = CacheError::InvalidHeader {
            path: PathBuf::from("bad.ast"),
            reason: "missing magic bytes".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("invalid artifact header"));
        assert!(msg.contains("missing magic bytes"));
    }

    #[test]
    fn checksum_mismatch_display() {
        let err = CacheError::ChecksumMismatch {
            path: PathBuf::from("file.ast"),
            expected: "aabb".to_string(),
            actual: "ccdd".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("checksum mismatch"));
        assert!(msg.contains("aabb"));
        assert!(msg.contains("ccdd"));
    }

    #[test]
    fn version_mismatch_display() {
        let err = CacheError::VersionMismatch {
            path: PathBuf::from("old.ast"),
            expected: 2,
            actual: 1,
        };
        let msg = err.to_string();
        assert!(msg.contains("version mismatch"));
        assert!(msg.contains("expected 2"));
        assert!(msg.contains("got 1"));
    }

    #[test]
    fn serialization_error_display() {
        let err = CacheError::Serialization {
            reason: "invalid bincode data".to_string(),
        };
        assert!(err.to_string().contains("invalid bincode data"));
    }
}
