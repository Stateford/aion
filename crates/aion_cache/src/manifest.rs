//! Cache manifest that tracks per-file and per-module cache state.
//!
//! The manifest is stored as `manifest.json` in the cache directory. It records
//! content hashes for every source file, enabling fast detection of changed files
//! without reparsing. Module-level dependency tracking is defined here but
//! populated in later phases.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aion_common::ContentHash;
use serde::{Deserialize, Serialize};

use crate::error::CacheError;

/// Name of the manifest file within the cache directory.
const MANIFEST_FILE: &str = "manifest.json";

/// Top-level cache manifest tracking all cached source files and modules.
///
/// Serialized as `manifest.json` in the cache directory. Contains per-file
/// content hashes, cached AST keys, and (in later phases) module dependency
/// edges for transitive invalidation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheManifest {
    /// Aion version that produced this cache. Invalidate on version change.
    pub aion_version: String,

    /// Per-source-file cache state, keyed by path relative to project root.
    pub files: HashMap<PathBuf, FileCache>,

    /// Per-module dependency edges (populated in Phase 1+).
    pub module_deps: HashMap<String, ModuleCacheEntry>,

    /// Per-target place-and-route state (populated in Phase 1+).
    pub targets: HashMap<String, TargetCache>,
}

/// Cached state for a single source file.
///
/// Stores the content hash at the time the file was last parsed, the key
/// used to locate the cached AST artifact, and the list of modules defined
/// in the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCache {
    /// Content hash of the source file when it was last parsed.
    pub content_hash: ContentHash,

    /// Key in the `ast/` artifact directory for the cached AST.
    pub ast_cache_key: String,

    /// Module names defined in this file (stored as strings, not interned).
    pub modules_defined: Vec<String>,
}

/// Cached dependency information for a single module (Phase 1+).
///
/// Tracks interface and body hashes separately so that body-only changes
/// can skip re-elaboration of instantiators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleCacheEntry {
    /// Hash of the module's interface (ports, parameters).
    pub interface_hash: ContentHash,

    /// Hash of the module's body (behavioral code).
    pub body_hash: ContentHash,

    /// Names of modules that this module instantiates.
    pub dependencies: Vec<String>,

    /// Cache key for the elaborated AionIR artifact.
    pub air_cache_key: String,

    /// Cache key for the synthesized netlist artifact (if synthesized).
    pub synth_cache_key: Option<String>,
}

/// Cached place-and-route state for a specific target device (Phase 1+).
///
/// Tracks the device target string and associated placement/routing artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetCache {
    /// Target device identifier (e.g. "ep4ce6e22c8").
    pub device: String,

    /// Cache key for the placed design artifact.
    pub placed_cache_key: Option<String>,

    /// Cache key for the routed design artifact.
    pub routed_cache_key: Option<String>,
}

impl CacheManifest {
    /// Creates a new, empty cache manifest for the given Aion version.
    pub fn new(aion_version: &str) -> Self {
        Self {
            aion_version: aion_version.to_string(),
            files: HashMap::new(),
            module_deps: HashMap::new(),
            targets: HashMap::new(),
        }
    }

    /// Loads the manifest from the cache directory, returning `None` if
    /// the file doesn't exist or can't be parsed.
    ///
    /// This is fail-safe: any error results in `None` (cache miss),
    /// triggering a full rebuild.
    pub fn load(cache_dir: &Path) -> Option<Self> {
        let path = cache_dir.join(MANIFEST_FILE);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Saves the manifest to the cache directory.
    ///
    /// Creates the cache directory if it doesn't exist.
    pub fn save(&self, cache_dir: &Path) -> Result<(), CacheError> {
        std::fs::create_dir_all(cache_dir).map_err(|e| CacheError::Io {
            path: cache_dir.to_path_buf(),
            source: e,
        })?;
        let path = cache_dir.join(MANIFEST_FILE);
        let json = serde_json::to_string_pretty(self).map_err(|e| CacheError::Serialization {
            reason: e.to_string(),
        })?;
        std::fs::write(&path, json).map_err(|e| CacheError::Io { path, source: e })
    }

    /// Returns `true` if this manifest was produced by a compatible Aion version.
    pub fn is_compatible(&self, current_version: &str) -> bool {
        self.aion_version == current_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manifest_is_empty() {
        let m = CacheManifest::new("0.1.0");
        assert_eq!(m.aion_version, "0.1.0");
        assert!(m.files.is_empty());
        assert!(m.module_deps.is_empty());
        assert!(m.targets.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = CacheManifest::new("0.1.0");
        m.files.insert(
            PathBuf::from("src/top.v"),
            FileCache {
                content_hash: ContentHash::from_bytes(b"top.v content"),
                ast_cache_key: "abc123".to_string(),
                modules_defined: vec!["top".to_string()],
            },
        );
        m.save(dir.path()).unwrap();

        let loaded = CacheManifest::load(dir.path()).unwrap();
        assert_eq!(loaded.aion_version, "0.1.0");
        assert_eq!(loaded.files.len(), 1);
        let fc = &loaded.files[&PathBuf::from("src/top.v")];
        assert_eq!(fc.ast_cache_key, "abc123");
        assert_eq!(fc.modules_defined, vec!["top"]);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(CacheManifest::load(dir.path()).is_none());
    }

    #[test]
    fn load_corrupt_json_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), "not valid json {{{").unwrap();
        assert!(CacheManifest::load(dir.path()).is_none());
    }

    #[test]
    fn is_compatible_same_version() {
        let m = CacheManifest::new("0.1.0");
        assert!(m.is_compatible("0.1.0"));
    }

    #[test]
    fn is_compatible_different_version() {
        let m = CacheManifest::new("0.1.0");
        assert!(!m.is_compatible("0.2.0"));
    }

    #[test]
    fn serde_file_cache_roundtrip() {
        let fc = FileCache {
            content_hash: ContentHash::from_bytes(b"test"),
            ast_cache_key: "key1".to_string(),
            modules_defined: vec!["mod_a".to_string(), "mod_b".to_string()],
        };
        let json = serde_json::to_string(&fc).unwrap();
        let back: FileCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ast_cache_key, "key1");
        assert_eq!(back.modules_defined.len(), 2);
    }

    #[test]
    fn serde_module_cache_entry_roundtrip() {
        let entry = ModuleCacheEntry {
            interface_hash: ContentHash::from_bytes(b"iface"),
            body_hash: ContentHash::from_bytes(b"body"),
            dependencies: vec!["sub_a".to_string()],
            air_cache_key: "air1".to_string(),
            synth_cache_key: Some("synth1".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ModuleCacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.air_cache_key, "air1");
        assert_eq!(back.synth_cache_key, Some("synth1".to_string()));
    }

    #[test]
    fn serde_target_cache_roundtrip() {
        let tc = TargetCache {
            device: "ep4ce6e22c8".to_string(),
            placed_cache_key: Some("placed1".to_string()),
            routed_cache_key: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: TargetCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.device, "ep4ce6e22c8");
        assert!(back.routed_cache_key.is_none());
    }

    #[test]
    fn save_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("deeply").join("nested").join("cache");
        let m = CacheManifest::new("0.1.0");
        m.save(&nested).unwrap();
        assert!(nested.join("manifest.json").exists());
    }
}
