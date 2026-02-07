//! Human-readable resolved source locations with line/column coordinates.

use std::fmt;
use std::path::PathBuf;

/// A span resolved to human-readable line/column coordinates.
///
/// All line and column values are 1-indexed for display to users.
/// Produced by [`SourceDb::resolve_span`](crate::SourceDb::resolve_span).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpan {
    /// The filesystem path of the source file.
    pub file_path: PathBuf,
    /// The starting line number (1-indexed).
    pub start_line: u32,
    /// The starting column number (1-indexed).
    pub start_col: u32,
    /// The ending line number (1-indexed).
    pub end_line: u32,
    /// The ending column number (1-indexed).
    pub end_col: u32,
}

impl fmt::Display for ResolvedSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.file_path.display(),
            self.start_line,
            self.start_col
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format() {
        let rs = ResolvedSpan {
            file_path: PathBuf::from("src/top.vhd"),
            start_line: 10,
            start_col: 5,
            end_line: 10,
            end_col: 15,
        };
        assert_eq!(format!("{rs}"), "src/top.vhd:10:5");
    }

    #[test]
    fn equality_with_different_values() {
        let a = ResolvedSpan {
            file_path: PathBuf::from("a.vhd"),
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 5,
        };
        let b = ResolvedSpan {
            file_path: PathBuf::from("b.vhd"),
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 5,
        };
        assert_ne!(a, b);
        assert_eq!(a, a.clone());
    }

    #[test]
    fn display_multiline_span() {
        let rs = ResolvedSpan {
            file_path: PathBuf::from("design.sv"),
            start_line: 5,
            start_col: 3,
            end_line: 12,
            end_col: 20,
        };
        // Display only shows start position
        assert_eq!(format!("{rs}"), "design.sv:5:3");
    }
}
