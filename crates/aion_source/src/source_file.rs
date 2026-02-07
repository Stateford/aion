//! Source file representation with line-start indexing for fast line/column lookup.

use crate::file_id::FileId;
use aion_common::ContentHash;
use std::path::PathBuf;

/// A source file loaded into the compilation session.
///
/// Stores the file's content along with precomputed line-start offsets for
/// efficient line/column resolution during diagnostic rendering.
pub struct SourceFile {
    /// The unique identifier for this file within the [`SourceDb`](crate::SourceDb).
    pub id: FileId,
    /// The filesystem path of this file (or a synthetic name for in-memory sources).
    pub path: PathBuf,
    /// The full text content of the file.
    pub content: String,
    /// Byte offsets of each line start (the first entry is always 0).
    line_starts: Vec<u32>,
    /// Hash of the file content for cache invalidation.
    pub content_hash: ContentHash,
}

impl SourceFile {
    /// Creates a new `SourceFile` with precomputed line starts and content hash.
    pub fn new(id: FileId, path: PathBuf, content: String) -> Self {
        let line_starts = compute_line_starts(&content);
        let content_hash = ContentHash::from_bytes(content.as_bytes());
        Self {
            id,
            path,
            content,
            line_starts,
            content_hash,
        }
    }

    /// Converts a byte offset into 1-indexed (line, column) coordinates.
    ///
    /// Uses binary search on the precomputed line-start offsets for efficient lookup.
    pub fn line_col(&self, byte_offset: u32) -> (u32, u32) {
        let line_idx = match self.line_starts.binary_search(&byte_offset) {
            Ok(idx) => idx,
            Err(idx) => idx - 1,
        };
        let line = (line_idx as u32) + 1;
        let col = byte_offset - self.line_starts[line_idx] + 1;
        (line, col)
    }

    /// Returns a substring of the file content between byte offsets.
    pub fn snippet(&self, start: u32, end: u32) -> &str {
        &self.content[start as usize..end as usize]
    }
}

/// Computes the byte offsets of each line start in the given content.
fn compute_line_starts(content: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push((i + 1) as u32);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(content: &str) -> SourceFile {
        SourceFile::new(
            FileId::from_raw(0),
            PathBuf::from("test.vhd"),
            content.to_string(),
        )
    }

    #[test]
    fn line_starts_computation() {
        let f = make_file("abc\ndef\nghi");
        assert_eq!(f.line_starts, vec![0, 4, 8]);
    }

    #[test]
    fn line_col_resolution() {
        let f = make_file("abc\ndef\nghi");
        // 'a' is at offset 0 → line 1, col 1
        assert_eq!(f.line_col(0), (1, 1));
        // 'd' is at offset 4 → line 2, col 1
        assert_eq!(f.line_col(4), (2, 1));
        // 'e' is at offset 5 → line 2, col 2
        assert_eq!(f.line_col(5), (2, 2));
        // 'g' is at offset 8 → line 3, col 1
        assert_eq!(f.line_col(8), (3, 1));
    }

    #[test]
    fn snippet_extraction() {
        let f = make_file("hello world");
        assert_eq!(f.snippet(0, 5), "hello");
        assert_eq!(f.snippet(6, 11), "world");
    }

    #[test]
    fn empty_file() {
        let f = make_file("");
        assert_eq!(f.line_starts, vec![0]);
        assert_eq!(f.line_col(0), (1, 1));
    }

    #[test]
    fn content_hash_computed() {
        let f = make_file("test content");
        let expected = ContentHash::from_bytes(b"test content");
        assert_eq!(f.content_hash, expected);
    }
}
