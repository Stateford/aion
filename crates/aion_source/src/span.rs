//! Byte-offset ranges within source files for tracking source locations.

use crate::file_id::FileId;
use serde::{Deserialize, Serialize};

/// A byte offset range within a source file.
///
/// Spans track the location of AST nodes, IR nodes, and diagnostics back to
/// their origin in source code. The `start` is inclusive and `end` is exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Span {
    /// The source file this span belongs to.
    pub file: FileId,
    /// Byte offset of the start of the span (inclusive).
    pub start: u32,
    /// Byte offset of the end of the span (exclusive).
    pub end: u32,
}

impl Span {
    /// A dummy span used when no source location is available.
    pub const DUMMY: Span = Span {
        file: FileId::DUMMY,
        start: 0,
        end: 0,
    };

    /// Creates a new span in the given file with the given byte range.
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        Self { file, start, end }
    }

    /// Merges two spans in the same file, producing a span that covers both.
    ///
    /// Takes the minimum start and maximum end of the two spans.
    ///
    /// # Panics
    ///
    /// Panics if the two spans are from different files.
    pub fn merge(self, other: Span) -> Span {
        assert_eq!(
            self.file, other.file,
            "cannot merge spans from different files"
        );
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Returns the length of this span in bytes.
    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Returns `true` if this span has zero length.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Returns `true` if this is the dummy span.
    pub fn is_dummy(&self) -> bool {
        self.file == FileId::DUMMY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct() {
        let f = FileId::from_raw(0);
        let s = Span::new(f, 10, 20);
        assert_eq!(s.file, f);
        assert_eq!(s.start, 10);
        assert_eq!(s.end, 20);
    }

    #[test]
    fn merge_spans() {
        let f = FileId::from_raw(0);
        let a = Span::new(f, 5, 15);
        let b = Span::new(f, 10, 25);
        let m = a.merge(b);
        assert_eq!(m.start, 5);
        assert_eq!(m.end, 25);
    }

    #[test]
    fn merge_order_independent() {
        let f = FileId::from_raw(0);
        let a = Span::new(f, 5, 15);
        let b = Span::new(f, 10, 25);
        assert_eq!(a.merge(b), b.merge(a));
    }

    #[test]
    fn len_and_empty() {
        let f = FileId::from_raw(0);
        let s = Span::new(f, 10, 20);
        assert_eq!(s.len(), 10);
        assert!(!s.is_empty());

        let empty = Span::new(f, 5, 5);
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn dummy_span() {
        assert!(Span::DUMMY.is_dummy());
        let f = FileId::from_raw(0);
        assert!(!Span::new(f, 0, 0).is_dummy());
    }

    #[test]
    fn serde_roundtrip() {
        let f = FileId::from_raw(1);
        let s = Span::new(f, 10, 20);
        let json = serde_json::to_string(&s).unwrap();
        let back: Span = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
