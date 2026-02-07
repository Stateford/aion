//! Source file management, span tracking, and source maps for diagnostics.
//!
//! This crate provides the [`SourceDb`] for loading and managing source files,
//! [`FileId`] and [`Span`] types for tracking source locations, and [`ResolvedSpan`]
//! for converting byte offsets to human-readable line/column coordinates.

#![warn(missing_docs)]

pub mod file_id;
pub mod resolved_span;
pub mod source_db;
pub mod source_file;
pub mod span;

pub use file_id::FileId;
pub use resolved_span::ResolvedSpan;
pub use source_db::SourceDb;
pub use source_file::SourceFile;
pub use span::Span;
