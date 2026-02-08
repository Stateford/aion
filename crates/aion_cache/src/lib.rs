//! Incremental compilation cache management.
//!
//! This crate provides content-hash-based caching for parsed ASTs and other
//! intermediate artifacts, enabling fast incremental rebuilds when only a subset
//! of source files have changed.
//!
//! # Architecture
//!
//! The cache system has four layers:
//!
//! - **Manifest** (`manifest.json`) — tracks per-file content hashes, cached
//!   AST keys, and module dependency edges. Stored as human-readable JSON.
//! - **Artifact store** — content-addressed binary files in subdirectories
//!   (`ast/`, `air/`, `synth/`). Each artifact has a validated header with
//!   magic bytes, format version, and integrity checksum.
//! - **Source hasher** — computes XXH3-128 content hashes for source files
//!   and compares them against the manifest to produce a `ChangeSet`.
//! - **Cache** — high-level orchestrator that ties everything together for
//!   the build pipeline.
//!
//! # Fail-Safe Design
//!
//! All cache reads are fail-safe: missing files, corruption, version mismatches,
//! and checksum failures all result in cache misses (returning `None`) rather
//! than hard errors. A full rebuild is always safe and correct.

#![warn(missing_docs)]

pub mod artifact;
pub mod cache;
pub mod error;
pub mod hasher;
pub mod manifest;

pub use artifact::{ArtifactHeader, ArtifactStore};
pub use cache::Cache;
pub use error::CacheError;
pub use hasher::{ChangeSet, SourceHasher};
pub use manifest::{CacheManifest, FileCache, ModuleCacheEntry, TargetCache};
