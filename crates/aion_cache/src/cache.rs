//! High-level cache orchestrator.
//!
//! The `Cache` type ties together the manifest, artifact store, and source
//! hasher into a single interface for the build pipeline. It handles loading
//! or creating the cache, detecting file changes, storing and retrieving
//! cached ASTs, and garbage collection.

use std::path::{Path, PathBuf};

use aion_common::ContentHash;

use crate::artifact::ArtifactStore;
use crate::error::CacheError;
use crate::hasher::{ChangeSet, SourceHasher};
use crate::manifest::{CacheManifest, FileCache};

/// Subdirectory name for cached AST artifacts.
const AST_SUBDIR: &str = "ast";

/// File extension for cached AST artifacts.
const AST_EXT: &str = "ast";

/// High-level cache manager for incremental builds.
///
/// Orchestrates the cache manifest, content-addressed artifact store, and
/// source file hashing to enable skipping reparsing of unchanged files.
/// All reads are fail-safe: corruption or version mismatches result in
/// cache misses rather than errors.
pub struct Cache {
    /// Root directory for all cache files.
    cache_dir: PathBuf,

    /// The cache manifest tracking file and module state.
    manifest: CacheManifest,

    /// Content-addressed binary artifact store.
    store: ArtifactStore,

    /// Aion version string for compatibility checks.
    aion_version: String,
}

impl Cache {
    /// Loads an existing cache or creates a fresh one.
    ///
    /// If a manifest exists and is compatible with the current Aion version,
    /// it is loaded. Otherwise a new empty manifest is created. This is
    /// fail-safe: any problem with the existing cache results in starting fresh.
    pub fn load_or_create(cache_dir: &Path, aion_version: &str) -> Self {
        let manifest = CacheManifest::load(cache_dir)
            .filter(|m| m.is_compatible(aion_version))
            .unwrap_or_else(|| CacheManifest::new(aion_version));

        Self {
            cache_dir: cache_dir.to_path_buf(),
            manifest,
            store: ArtifactStore::new(cache_dir),
            aion_version: aion_version.to_string(),
        }
    }

    /// Detects which source files have changed since the last build.
    ///
    /// Reads and hashes the given files, then compares against the manifest
    /// to categorize each file as new, modified, deleted, or unchanged.
    pub fn detect_changes(&self, file_paths: &[PathBuf]) -> ChangeSet {
        let hashes = SourceHasher::hash_files(file_paths);
        SourceHasher::detect_changes(&hashes, &self.manifest)
    }

    /// Stores a cached AST artifact for a source file.
    ///
    /// Records the content hash, cache key, and defined module names in the
    /// manifest. The actual AST bytes are written to the artifact store.
    pub fn store_ast(
        &mut self,
        path: &Path,
        content_hash: ContentHash,
        ast_bytes: &[u8],
        modules_defined: Vec<String>,
    ) -> Result<String, CacheError> {
        let key = self.store.write_artifact(
            AST_SUBDIR,
            AST_EXT,
            &content_hash,
            ast_bytes,
            &self.aion_version,
        )?;

        self.manifest.files.insert(
            path.to_path_buf(),
            FileCache {
                content_hash,
                ast_cache_key: key.clone(),
                modules_defined,
            },
        );

        Ok(key)
    }

    /// Loads a cached AST artifact for a source file.
    ///
    /// Returns `None` if the file is not in the manifest, the artifact
    /// is missing, or validation fails. This is fail-safe.
    pub fn load_ast(&self, path: &Path) -> Option<Vec<u8>> {
        let fc = self.manifest.files.get(path)?;
        self.store
            .read_artifact(AST_SUBDIR, &fc.ast_cache_key, AST_EXT)
    }

    /// Removes entries for deleted files from the manifest.
    ///
    /// Should be called after detecting changes to clean up stale entries.
    pub fn remove_deleted(&mut self, deleted_paths: &[PathBuf]) {
        for path in deleted_paths {
            self.manifest.files.remove(path);
        }
    }

    /// Persists the current manifest to disk.
    pub fn save(&self) -> Result<(), CacheError> {
        self.manifest.save(&self.cache_dir)
    }

    /// Returns a reference to the current cache manifest.
    pub fn manifest(&self) -> &CacheManifest {
        &self.manifest
    }

    /// Runs garbage collection on AST artifacts.
    ///
    /// Removes any artifact files that are not referenced by the current
    /// manifest. Returns the number of files removed.
    pub fn gc(&self) -> Result<usize, CacheError> {
        let live_keys: Vec<&str> = self
            .manifest
            .files
            .values()
            .map(|fc| fc.ast_cache_key.as_str())
            .collect();
        self.store.gc(AST_SUBDIR, AST_EXT, &live_keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::load_or_create(dir.path(), "0.1.0");
        (dir, cache)
    }

    #[test]
    fn fresh_cache_has_empty_manifest() {
        let (_dir, cache) = make_cache();
        assert!(cache.manifest().files.is_empty());
        assert_eq!(cache.manifest().aion_version, "0.1.0");
    }

    #[test]
    fn load_existing_cache() {
        let dir = tempfile::tempdir().unwrap();

        // Create and save a cache
        {
            let mut cache = Cache::load_or_create(dir.path(), "0.1.0");
            let hash = ContentHash::from_bytes(b"content");
            cache
                .store_ast(
                    Path::new("src/top.v"),
                    hash,
                    b"ast bytes",
                    vec!["top".to_string()],
                )
                .unwrap();
            cache.save().unwrap();
        }

        // Reload it
        let cache = Cache::load_or_create(dir.path(), "0.1.0");
        assert_eq!(cache.manifest().files.len(), 1);
    }

    #[test]
    fn version_mismatch_creates_fresh_cache() {
        let dir = tempfile::tempdir().unwrap();

        // Save with version 0.1.0
        {
            let mut cache = Cache::load_or_create(dir.path(), "0.1.0");
            let hash = ContentHash::from_bytes(b"content");
            cache
                .store_ast(Path::new("src/top.v"), hash, b"ast", vec![])
                .unwrap();
            cache.save().unwrap();
        }

        // Load with different version — should get fresh cache
        let cache = Cache::load_or_create(dir.path(), "0.2.0");
        assert!(cache.manifest().files.is_empty());
        assert_eq!(cache.manifest().aion_version, "0.2.0");
    }

    #[test]
    fn detect_changes_with_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::load_or_create(dir.path(), "0.1.0");

        // Create a source file
        let src = dir.path().join("test.v");
        std::fs::write(&src, "module test; endmodule").unwrap();

        let cs = cache.detect_changes(&[src]);
        assert_eq!(cs.new_files.len(), 1);
        assert!(cs.modified_files.is_empty());
        assert!(cs.unchanged_files.is_empty());
    }

    #[test]
    fn store_and_load_ast() {
        let (_dir, mut cache) = make_cache();
        let path = Path::new("src/top.v");
        let hash = ContentHash::from_bytes(b"top.v source");
        let ast_data = b"serialized ast data";

        cache
            .store_ast(path, hash, ast_data, vec!["top".to_string()])
            .unwrap();

        let loaded = cache.load_ast(path).unwrap();
        assert_eq!(loaded, ast_data);
    }

    #[test]
    fn load_ast_cache_miss() {
        let (_dir, cache) = make_cache();
        assert!(cache.load_ast(Path::new("nonexistent.v")).is_none());
    }

    #[test]
    fn remove_deleted_files() {
        let (_dir, mut cache) = make_cache();
        let path = PathBuf::from("src/deleted.v");
        let hash = ContentHash::from_bytes(b"content");
        cache.store_ast(&path, hash, b"ast", vec![]).unwrap();
        assert_eq!(cache.manifest().files.len(), 1);

        cache.remove_deleted(&[path]);
        assert!(cache.manifest().files.is_empty());
    }

    #[test]
    fn save_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::load_or_create(dir.path(), "0.1.0");
        let hash = ContentHash::from_bytes(b"content");
        cache
            .store_ast(Path::new("src/a.v"), hash, b"ast", vec![])
            .unwrap();
        cache.save().unwrap();

        // Verify manifest file exists
        assert!(dir.path().join("manifest.json").exists());
    }

    #[test]
    fn gc_removes_stale_artifacts() {
        let (_dir, mut cache) = make_cache();

        // Store two ASTs
        let hash_a = ContentHash::from_bytes(b"file A");
        cache
            .store_ast(Path::new("a.v"), hash_a, b"ast A", vec![])
            .unwrap();

        let hash_b = ContentHash::from_bytes(b"file B");
        cache
            .store_ast(Path::new("b.v"), hash_b, b"ast B", vec![])
            .unwrap();

        // Remove b.v from manifest (simulating deletion)
        cache.remove_deleted(&[PathBuf::from("b.v")]);

        // GC should remove b's artifact
        let removed = cache.gc().unwrap();
        assert_eq!(removed, 1);

        // a.v still loadable
        assert!(cache.load_ast(Path::new("a.v")).is_some());
    }

    #[test]
    fn full_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join(".aion-cache");

        // First build: everything is new
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file_a = src_dir.join("a.v");
        let file_b = src_dir.join("b.v");
        std::fs::write(&file_a, "module a; endmodule").unwrap();
        std::fs::write(&file_b, "module b; endmodule").unwrap();

        {
            let mut cache = Cache::load_or_create(&cache_dir, "0.1.0");
            let cs = cache.detect_changes(&[file_a.clone(), file_b.clone()]);
            assert_eq!(cs.new_files.len(), 2);

            // "Parse" and store
            for path in &cs.new_files {
                let hash = SourceHasher::hash_file(path).unwrap();
                let ast_bytes = format!("ast for {}", path.display());
                cache
                    .store_ast(path, hash, ast_bytes.as_bytes(), vec![])
                    .unwrap();
            }
            cache.save().unwrap();
        }

        // Second build: nothing changed
        {
            let cache = Cache::load_or_create(&cache_dir, "0.1.0");
            let cs = cache.detect_changes(&[file_a.clone(), file_b.clone()]);
            assert!(cs.is_empty());
            assert_eq!(cs.unchanged_files.len(), 2);
        }

        // Third build: modify a.v
        std::fs::write(&file_a, "module a_modified; endmodule").unwrap();
        {
            let cache = Cache::load_or_create(&cache_dir, "0.1.0");
            let cs = cache.detect_changes(&[file_a.clone(), file_b.clone()]);
            assert_eq!(cs.modified_files.len(), 1);
            assert_eq!(cs.unchanged_files.len(), 1);
        }
    }

    #[test]
    fn detect_changes_with_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join(".aion-cache");

        let file_a = dir.path().join("a.v");
        std::fs::write(&file_a, "module a; endmodule").unwrap();

        // First build
        {
            let mut cache = Cache::load_or_create(&cache_dir, "0.1.0");
            let hash = SourceHasher::hash_file(&file_a).unwrap();
            cache.store_ast(&file_a, hash, b"ast", vec![]).unwrap();
            cache.save().unwrap();
        }

        // Second build: file_a no longer in list (deleted)
        {
            let cache = Cache::load_or_create(&cache_dir, "0.1.0");
            let cs = cache.detect_changes(&[]);
            assert_eq!(cs.deleted_files.len(), 1);
        }
    }
}
