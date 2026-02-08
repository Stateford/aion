//! Source file hashing and change detection.
//!
//! Computes content hashes for source files and compares them against the
//! cache manifest to identify which files are new, modified, deleted,
//! or unchanged since the last build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aion_common::ContentHash;

use crate::error::CacheError;
use crate::manifest::CacheManifest;

/// Result of comparing current source file hashes against the cache manifest.
///
/// Categorizes all files into new (never seen), modified (hash changed),
/// deleted (in manifest but not on disk), and unchanged (hash matches).
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// Files that are not present in the cache manifest.
    pub new_files: Vec<PathBuf>,

    /// Files whose content hash differs from the manifest.
    pub modified_files: Vec<PathBuf>,

    /// Files present in the manifest but not in the current file set.
    pub deleted_files: Vec<PathBuf>,

    /// Files whose content hash matches the manifest.
    pub unchanged_files: Vec<PathBuf>,
}

impl ChangeSet {
    /// Returns `true` if there are no changes (no new, modified, or deleted files).
    pub fn is_empty(&self) -> bool {
        self.new_files.is_empty() && self.modified_files.is_empty() && self.deleted_files.is_empty()
    }

    /// Returns the total number of files that need reprocessing (new + modified).
    pub fn dirty_count(&self) -> usize {
        self.new_files.len() + self.modified_files.len()
    }
}

/// Utility for computing content hashes of source files and detecting changes.
pub struct SourceHasher;

impl SourceHasher {
    /// Computes the content hash of a single file.
    ///
    /// Reads the file and returns its XXH3-128 content hash.
    pub fn hash_file(path: &Path) -> Result<ContentHash, CacheError> {
        let content = std::fs::read(path).map_err(|e| CacheError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(ContentHash::from_bytes(&content))
    }

    /// Computes content hashes for multiple files.
    ///
    /// Returns a map from path to content hash. Any files that cannot be read
    /// are silently skipped (they will appear as deleted in the change set).
    pub fn hash_files(paths: &[PathBuf]) -> HashMap<PathBuf, ContentHash> {
        let mut hashes = HashMap::with_capacity(paths.len());
        for path in paths {
            if let Ok(hash) = Self::hash_file(path) {
                hashes.insert(path.clone(), hash);
            }
        }
        hashes
    }

    /// Compares current file hashes against the cache manifest to detect changes.
    ///
    /// Files are categorized as new (not in manifest), modified (hash changed),
    /// deleted (in manifest but not in current set), or unchanged.
    pub fn detect_changes(
        current_hashes: &HashMap<PathBuf, ContentHash>,
        manifest: &CacheManifest,
    ) -> ChangeSet {
        let mut new_files = Vec::new();
        let mut modified_files = Vec::new();
        let mut unchanged_files = Vec::new();

        for (path, hash) in current_hashes {
            match manifest.files.get(path) {
                Some(fc) if fc.content_hash == *hash => {
                    unchanged_files.push(path.clone());
                }
                Some(_) => {
                    modified_files.push(path.clone());
                }
                None => {
                    new_files.push(path.clone());
                }
            }
        }

        let deleted_files: Vec<PathBuf> = manifest
            .files
            .keys()
            .filter(|p| !current_hashes.contains_key(*p))
            .cloned()
            .collect();

        // Sort for deterministic ordering in tests
        new_files.sort();
        modified_files.sort();
        unchanged_files.sort();
        let mut deleted_files = deleted_files;
        deleted_files.sort();

        ChangeSet {
            new_files,
            modified_files,
            deleted_files,
            unchanged_files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FileCache;
    use std::collections::HashMap;

    #[test]
    fn hash_file_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.v");
        std::fs::write(&path, "module top; endmodule").unwrap();

        let h1 = SourceHasher::hash_file(&path).unwrap();
        let h2 = SourceHasher::hash_file(&path).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_file_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.v");
        let path_b = dir.path().join("b.v");
        std::fs::write(&path_a, "module a; endmodule").unwrap();
        std::fs::write(&path_b, "module b; endmodule").unwrap();

        let h1 = SourceHasher::hash_file(&path_a).unwrap();
        let h2 = SourceHasher::hash_file(&path_b).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_file_nonexistent_errors() {
        let result = SourceHasher::hash_file(Path::new("/nonexistent/file.v"));
        assert!(result.is_err());
    }

    #[test]
    fn hash_files_multiple() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.v");
        let path_b = dir.path().join("b.v");
        std::fs::write(&path_a, "module a; endmodule").unwrap();
        std::fs::write(&path_b, "module b; endmodule").unwrap();

        let paths = vec![path_a.clone(), path_b.clone()];
        let hashes = SourceHasher::hash_files(&paths);
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains_key(&path_a));
        assert!(hashes.contains_key(&path_b));
    }

    #[test]
    fn detect_changes_all_new() {
        let manifest = CacheManifest::new("0.1.0");
        let mut hashes = HashMap::new();
        hashes.insert(
            PathBuf::from("src/a.v"),
            ContentHash::from_bytes(b"module a"),
        );
        hashes.insert(
            PathBuf::from("src/b.v"),
            ContentHash::from_bytes(b"module b"),
        );

        let cs = SourceHasher::detect_changes(&hashes, &manifest);
        assert_eq!(cs.new_files.len(), 2);
        assert!(cs.modified_files.is_empty());
        assert!(cs.deleted_files.is_empty());
        assert!(cs.unchanged_files.is_empty());
        assert_eq!(cs.dirty_count(), 2);
    }

    #[test]
    fn detect_changes_all_unchanged() {
        let hash = ContentHash::from_bytes(b"content");
        let mut manifest = CacheManifest::new("0.1.0");
        manifest.files.insert(
            PathBuf::from("src/a.v"),
            FileCache {
                content_hash: hash,
                ast_cache_key: "key1".to_string(),
                modules_defined: vec![],
            },
        );

        let mut hashes = HashMap::new();
        hashes.insert(PathBuf::from("src/a.v"), hash);

        let cs = SourceHasher::detect_changes(&hashes, &manifest);
        assert!(cs.new_files.is_empty());
        assert!(cs.modified_files.is_empty());
        assert!(cs.deleted_files.is_empty());
        assert_eq!(cs.unchanged_files.len(), 1);
        assert!(cs.is_empty());
    }

    #[test]
    fn detect_changes_modified() {
        let old_hash = ContentHash::from_bytes(b"old content");
        let new_hash = ContentHash::from_bytes(b"new content");

        let mut manifest = CacheManifest::new("0.1.0");
        manifest.files.insert(
            PathBuf::from("src/a.v"),
            FileCache {
                content_hash: old_hash,
                ast_cache_key: "key1".to_string(),
                modules_defined: vec![],
            },
        );

        let mut hashes = HashMap::new();
        hashes.insert(PathBuf::from("src/a.v"), new_hash);

        let cs = SourceHasher::detect_changes(&hashes, &manifest);
        assert!(cs.new_files.is_empty());
        assert_eq!(cs.modified_files.len(), 1);
        assert!(cs.deleted_files.is_empty());
        assert!(!cs.is_empty());
    }

    #[test]
    fn detect_changes_deleted() {
        let hash = ContentHash::from_bytes(b"content");
        let mut manifest = CacheManifest::new("0.1.0");
        manifest.files.insert(
            PathBuf::from("src/deleted.v"),
            FileCache {
                content_hash: hash,
                ast_cache_key: "key1".to_string(),
                modules_defined: vec![],
            },
        );

        let hashes = HashMap::new(); // no current files

        let cs = SourceHasher::detect_changes(&hashes, &manifest);
        assert!(cs.new_files.is_empty());
        assert!(cs.modified_files.is_empty());
        assert_eq!(cs.deleted_files.len(), 1);
        assert_eq!(cs.deleted_files[0], PathBuf::from("src/deleted.v"));
    }
}
