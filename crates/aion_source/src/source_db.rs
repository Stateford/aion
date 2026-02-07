//! Central database of all source files in a compilation session.

use crate::file_id::FileId;
use crate::resolved_span::ResolvedSpan;
use crate::source_file::SourceFile;
use crate::span::Span;
use std::io;
use std::path::{Path, PathBuf};

/// The source database, owning all loaded source text and resolving
/// [`FileId`] + byte offsets to line/column coordinates for diagnostics.
pub struct SourceDb {
    files: Vec<SourceFile>,
}

impl SourceDb {
    /// Creates an empty source database.
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Loads a source file from the filesystem and returns its [`FileId`].
    pub fn load_file(&mut self, path: &Path) -> Result<FileId, io::Error> {
        let content = std::fs::read_to_string(path)?;
        let id = FileId::from_raw(self.files.len() as u32);
        let file = SourceFile::new(id, path.to_path_buf(), content);
        self.files.push(file);
        Ok(id)
    }

    /// Adds a source file from an in-memory string (useful for tests).
    ///
    /// The `name` parameter is used as the file path in diagnostics.
    pub fn add_source(&mut self, name: impl Into<PathBuf>, content: String) -> FileId {
        let id = FileId::from_raw(self.files.len() as u32);
        let file = SourceFile::new(id, name.into(), content);
        self.files.push(file);
        id
    }

    /// Returns the [`SourceFile`] for the given [`FileId`].
    ///
    /// # Panics
    ///
    /// Panics if the `FileId` is invalid.
    pub fn get_file(&self, id: FileId) -> &SourceFile {
        &self.files[id.as_raw() as usize]
    }

    /// Resolves a [`Span`] to human-readable line/column coordinates.
    pub fn resolve_span(&self, span: Span) -> ResolvedSpan {
        let file = self.get_file(span.file);
        let (start_line, start_col) = file.line_col(span.start);
        let (end_line, end_col) = file.line_col(span.end.saturating_sub(1).max(span.start));
        ResolvedSpan {
            file_path: file.path.clone(),
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }

    /// Returns the source text corresponding to a [`Span`].
    pub fn snippet(&self, span: Span) -> &str {
        let file = self.get_file(span.file);
        file.snippet(span.start, span.end)
    }
}

impl Default for SourceDb {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_get() {
        let mut db = SourceDb::new();
        let id = db.add_source("test.vhd", "hello world".to_string());
        let file = db.get_file(id);
        assert_eq!(file.content, "hello world");
    }

    #[test]
    fn resolve_span() {
        let mut db = SourceDb::new();
        let id = db.add_source("test.vhd", "abc\ndef\nghi".to_string());
        let span = Span::new(id, 4, 7); // "def"
        let resolved = db.resolve_span(span);
        assert_eq!(resolved.file_path, PathBuf::from("test.vhd"));
        assert_eq!(resolved.start_line, 2);
        assert_eq!(resolved.start_col, 1);
        assert_eq!(resolved.end_line, 2);
        assert_eq!(resolved.end_col, 3);
    }

    #[test]
    fn snippet() {
        let mut db = SourceDb::new();
        let id = db.add_source("test.vhd", "hello world".to_string());
        let span = Span::new(id, 0, 5);
        assert_eq!(db.snippet(span), "hello");
    }

    #[test]
    fn multiple_files() {
        let mut db = SourceDb::new();
        let id1 = db.add_source("a.vhd", "file one".to_string());
        let id2 = db.add_source("b.vhd", "file two".to_string());
        assert_ne!(id1, id2);
        assert_eq!(db.get_file(id1).content, "file one");
        assert_eq!(db.get_file(id2).content, "file two");
    }

    #[test]
    fn load_file_from_disk() {
        let dir = std::env::temp_dir().join("aion_source_test");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test_load.vhd");
        std::fs::write(&file_path, "entity top is end;").unwrap();

        let mut db = SourceDb::new();
        let id = db.load_file(&file_path).unwrap();
        assert_eq!(db.get_file(id).content, "entity top is end;");

        std::fs::remove_dir_all(&dir).ok();
    }
}
