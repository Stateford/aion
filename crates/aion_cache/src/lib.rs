//! Incremental compilation cache management.
//!
//! This crate provides content-hash-based caching for parsed ASTs and other
//! intermediate artifacts, enabling fast incremental rebuilds when only a subset
//! of source files have changed.

#![warn(missing_docs)]
