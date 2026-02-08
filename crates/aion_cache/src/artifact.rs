//! Content-addressed binary artifact storage.
//!
//! Artifacts (cached ASTs, elaborated IR, netlists) are stored as binary files
//! in subdirectories of the cache. Each artifact has a header containing magic
//! bytes, format version, and a checksum for integrity validation.

use std::path::{Path, PathBuf};

use aion_common::ContentHash;
use serde::{Deserialize, Serialize};

use crate::error::CacheError;

/// Magic bytes identifying an Aion cache artifact.
const ARTIFACT_MAGIC: [u8; 4] = *b"AION";

/// Current artifact format version. Increment on breaking changes to
/// the header or payload format.
const ARTIFACT_FORMAT_VERSION: u32 = 1;

/// Header prepended to every cached artifact for validation.
///
/// Contains magic bytes to identify the file format, a version number
/// for forward/backward compatibility checks, and a checksum to detect
/// corruption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactHeader {
    /// Magic bytes: must be `b"AION"`.
    pub magic: [u8; 4],

    /// Artifact format version.
    pub format_version: u32,

    /// Aion version that produced this artifact.
    pub aion_version: String,

    /// Content hash of the payload data (for integrity checks).
    pub checksum: ContentHash,
}

/// Content-addressed store for binary artifacts.
///
/// Manages reading and writing artifacts in subdirectories of the cache.
/// Each artifact is stored at `<cache_dir>/<subdir>/<key>.<ext>` with a
/// validated binary header.
pub struct ArtifactStore {
    /// Root cache directory.
    cache_dir: PathBuf,
}

impl ArtifactStore {
    /// Creates a new artifact store rooted at the given cache directory.
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
        }
    }

    /// Ensures that the subdirectory for the given artifact type exists.
    pub fn ensure_dirs(&self, subdir: &str) -> Result<(), CacheError> {
        let dir = self.cache_dir.join(subdir);
        std::fs::create_dir_all(&dir).map_err(|e| CacheError::Io {
            path: dir,
            source: e,
        })
    }

    /// Returns the file path for an artifact with the given key.
    pub fn artifact_path(&self, subdir: &str, key: &str, ext: &str) -> PathBuf {
        self.cache_dir.join(subdir).join(format!("{key}.{ext}"))
    }

    /// Writes an artifact to the store and returns the cache key.
    ///
    /// The key is derived from the content hash of the data. The artifact
    /// is written with a binary header containing magic bytes, format version,
    /// and a checksum for later validation.
    pub fn write_artifact(
        &self,
        subdir: &str,
        ext: &str,
        hash: &ContentHash,
        data: &[u8],
        aion_version: &str,
    ) -> Result<String, CacheError> {
        self.ensure_dirs(subdir)?;

        let key = hash.to_string();
        let path = self.artifact_path(subdir, &key, ext);

        let header = ArtifactHeader {
            magic: ARTIFACT_MAGIC,
            format_version: ARTIFACT_FORMAT_VERSION,
            aion_version: aion_version.to_string(),
            checksum: ContentHash::from_bytes(data),
        };

        let header_bytes = bincode::serde::encode_to_vec(&header, bincode::config::standard())
            .map_err(|e| CacheError::Serialization {
                reason: e.to_string(),
            })?;

        // Write: 4-byte header length (little-endian) + header + payload
        let header_len = header_bytes.len() as u32;
        let mut output = Vec::with_capacity(4 + header_bytes.len() + data.len());
        output.extend_from_slice(&header_len.to_le_bytes());
        output.extend_from_slice(&header_bytes);
        output.extend_from_slice(data);

        std::fs::write(&path, &output).map_err(|e| CacheError::Io { path, source: e })?;

        Ok(key)
    }

    /// Reads an artifact from the store, validating its header.
    ///
    /// Returns `None` if the file doesn't exist, the header is invalid,
    /// the format version doesn't match, or the checksum doesn't verify.
    /// This is fail-safe: corruption results in a cache miss.
    pub fn read_artifact(&self, subdir: &str, key: &str, ext: &str) -> Option<Vec<u8>> {
        let path = self.artifact_path(subdir, key, ext);
        let raw = std::fs::read(&path).ok()?;

        // Need at least 4 bytes for the header length
        if raw.len() < 4 {
            return None;
        }

        let header_len = u32::from_le_bytes(raw[..4].try_into().ok()?) as usize;
        if raw.len() < 4 + header_len {
            return None;
        }

        let header: ArtifactHeader =
            bincode::serde::decode_from_slice(&raw[4..4 + header_len], bincode::config::standard())
                .ok()?
                .0;

        // Validate magic
        if header.magic != ARTIFACT_MAGIC {
            return None;
        }

        // Validate format version
        if header.format_version != ARTIFACT_FORMAT_VERSION {
            return None;
        }

        let payload = &raw[4 + header_len..];

        // Validate checksum
        let actual_checksum = ContentHash::from_bytes(payload);
        if actual_checksum != header.checksum {
            return None;
        }

        Some(payload.to_vec())
    }

    /// Removes artifacts that are not in the set of live keys.
    ///
    /// Scans the subdirectory for files with the given extension and deletes
    /// any whose stem (filename without extension) is not in `live_keys`.
    /// Returns the number of files removed.
    pub fn gc(&self, subdir: &str, ext: &str, live_keys: &[&str]) -> Result<usize, CacheError> {
        let dir = self.cache_dir.join(subdir);
        if !dir.exists() {
            return Ok(0);
        }

        let mut removed = 0;
        let entries = std::fs::read_dir(&dir).map_err(|e| CacheError::Io {
            path: dir.clone(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| CacheError::Io {
                path: dir.clone(),
                source: e,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !live_keys.contains(&stem) {
                        std::fs::remove_file(&path).map_err(|e| CacheError::Io {
                            path: path.clone(),
                            source: e,
                        })?;
                        removed += 1;
                    }
                }
            }
        }

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> (tempfile::TempDir, ArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn write_and_read_roundtrip() {
        let (_dir, store) = make_store();
        let data = b"hello artifact world";
        let hash = ContentHash::from_bytes(data);
        let key = store
            .write_artifact("ast", "ast", &hash, data, "0.1.0")
            .unwrap();

        let read_back = store.read_artifact("ast", &key, "ast").unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn read_missing_returns_none() {
        let (_dir, store) = make_store();
        assert!(store.read_artifact("ast", "nonexistent", "ast").is_none());
    }

    #[test]
    fn read_corrupt_data_returns_none() {
        let (_dir, store) = make_store();
        store.ensure_dirs("ast").unwrap();
        let path = store.artifact_path("ast", "corrupt", "ast");
        std::fs::write(&path, b"garbage data").unwrap();
        assert!(store.read_artifact("ast", "corrupt", "ast").is_none());
    }

    #[test]
    fn read_wrong_magic_returns_none() {
        let (_dir, store) = make_store();
        store.ensure_dirs("ast").unwrap();

        // Write a valid-looking file but with wrong magic
        let header = ArtifactHeader {
            magic: *b"BAAD",
            format_version: ARTIFACT_FORMAT_VERSION,
            aion_version: "0.1.0".to_string(),
            checksum: ContentHash::from_bytes(b"data"),
        };
        let header_bytes =
            bincode::serde::encode_to_vec(&header, bincode::config::standard()).unwrap();
        let header_len = header_bytes.len() as u32;
        let mut output = Vec::new();
        output.extend_from_slice(&header_len.to_le_bytes());
        output.extend_from_slice(&header_bytes);
        output.extend_from_slice(b"data");

        let path = store.artifact_path("ast", "badmagic", "ast");
        std::fs::write(&path, &output).unwrap();
        assert!(store.read_artifact("ast", "badmagic", "ast").is_none());
    }

    #[test]
    fn read_wrong_version_returns_none() {
        let (_dir, store) = make_store();
        store.ensure_dirs("ast").unwrap();

        let payload = b"data";
        let header = ArtifactHeader {
            magic: ARTIFACT_MAGIC,
            format_version: 999,
            aion_version: "0.1.0".to_string(),
            checksum: ContentHash::from_bytes(payload),
        };
        let header_bytes =
            bincode::serde::encode_to_vec(&header, bincode::config::standard()).unwrap();
        let header_len = header_bytes.len() as u32;
        let mut output = Vec::new();
        output.extend_from_slice(&header_len.to_le_bytes());
        output.extend_from_slice(&header_bytes);
        output.extend_from_slice(payload);

        let path = store.artifact_path("ast", "oldver", "ast");
        std::fs::write(&path, &output).unwrap();
        assert!(store.read_artifact("ast", "oldver", "ast").is_none());
    }

    #[test]
    fn read_checksum_mismatch_returns_none() {
        let (_dir, store) = make_store();
        store.ensure_dirs("ast").unwrap();

        // Write with correct checksum for "data" but actual payload is "tampered"
        let header = ArtifactHeader {
            magic: ARTIFACT_MAGIC,
            format_version: ARTIFACT_FORMAT_VERSION,
            aion_version: "0.1.0".to_string(),
            checksum: ContentHash::from_bytes(b"data"),
        };
        let header_bytes =
            bincode::serde::encode_to_vec(&header, bincode::config::standard()).unwrap();
        let header_len = header_bytes.len() as u32;
        let mut output = Vec::new();
        output.extend_from_slice(&header_len.to_le_bytes());
        output.extend_from_slice(&header_bytes);
        output.extend_from_slice(b"tampered");

        let path = store.artifact_path("ast", "mismatch", "ast");
        std::fs::write(&path, &output).unwrap();
        assert!(store.read_artifact("ast", "mismatch", "ast").is_none());
    }

    #[test]
    fn artifact_path_format() {
        let (_dir, store) = make_store();
        let path = store.artifact_path("ast", "abc123", "ast");
        assert!(path.ends_with("ast/abc123.ast"));
    }

    #[test]
    fn read_truncated_header_returns_none() {
        let (_dir, store) = make_store();
        store.ensure_dirs("ast").unwrap();
        let path = store.artifact_path("ast", "truncated", "ast");
        // Only 2 bytes — not enough for header length
        std::fs::write(&path, b"AB").unwrap();
        assert!(store.read_artifact("ast", "truncated", "ast").is_none());
    }

    #[test]
    fn gc_removes_stale_artifacts() {
        let (_dir, store) = make_store();

        let data_a = b"artifact A";
        let hash_a = ContentHash::from_bytes(data_a);
        let key_a = store
            .write_artifact("ast", "ast", &hash_a, data_a, "0.1.0")
            .unwrap();

        let data_b = b"artifact B";
        let hash_b = ContentHash::from_bytes(data_b);
        let _key_b = store
            .write_artifact("ast", "ast", &hash_b, data_b, "0.1.0")
            .unwrap();

        // Keep only key_a, GC should remove key_b
        let removed = store.gc("ast", "ast", &[key_a.as_str()]).unwrap();
        assert_eq!(removed, 1);

        // key_a still readable, key_b is gone
        assert!(store.read_artifact("ast", &key_a, "ast").is_some());
    }

    #[test]
    fn gc_nonexistent_dir_returns_zero() {
        let (_dir, store) = make_store();
        let removed = store.gc("nonexistent", "ast", &[]).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn gc_preserves_all_live_keys() {
        let (_dir, store) = make_store();

        let data = b"keep me";
        let hash = ContentHash::from_bytes(data);
        let key = store
            .write_artifact("ast", "ast", &hash, data, "0.1.0")
            .unwrap();

        let removed = store.gc("ast", "ast", &[key.as_str()]).unwrap();
        assert_eq!(removed, 0);
        assert!(store.read_artifact("ast", &key, "ast").is_some());
    }

    #[test]
    fn write_large_payload() {
        let (_dir, store) = make_store();
        let data: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let hash = ContentHash::from_bytes(&data);
        let key = store
            .write_artifact("ast", "ast", &hash, &data, "0.1.0")
            .unwrap();
        let read_back = store.read_artifact("ast", &key, "ast").unwrap();
        assert_eq!(read_back, data);
    }
}
