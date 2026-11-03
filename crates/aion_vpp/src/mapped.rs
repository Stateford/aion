//! Text buffers that remember, for every byte range, where the text came from.
//!
//! The preprocessor builds its output by copying runs of source text, macro
//! bodies and macro arguments into a [`MappedText`]. Each run is recorded as a
//! [`Seg`] whose [`Origin`] says either "these bytes are an exact copy of file
//! `F` starting at offset `O`" or "these bytes were synthesized". That record
//! is what lets every token lexed from the output be mapped back to a real
//! span in the `SourceDb`.

use aion_source::{FileId, Span};
use std::ops::Range;

/// Where a run of bytes in a [`MappedText`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The bytes are a byte-for-byte copy of `file`'s text starting at `offset`.
    Real {
        /// The file the bytes were copied from.
        file: FileId,
        /// Byte offset in `file` of the first byte of the run.
        offset: u32,
    },
    /// The bytes were produced by the preprocessor itself (stringification,
    /// `__LINE__`, separators, predefines without a backing file) and do not
    /// appear verbatim in any source file.
    Synthetic,
}

/// One contiguous run of bytes with a single origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seg {
    /// Offset of the run within the owning buffer.
    pub start: u32,
    /// Length of the run in bytes.
    pub len: u32,
    /// Where the bytes came from.
    pub origin: Origin,
    /// The outermost macro invocation that produced these bytes, or `None`
    /// for text copied directly from a source file.
    pub site: Option<Span>,
}

impl Seg {
    /// Offset one past the last byte of the run within the owning buffer.
    pub fn end(&self) -> u32 {
        self.start + self.len
    }
}

/// A string together with a byte-range-to-origin map.
#[derive(Clone, Debug, Default)]
pub(crate) struct MappedText {
    /// The text itself.
    pub text: String,
    /// Origin runs, sorted by `start`, covering `text` without gaps.
    pub segs: Vec<Seg>,
}

impl MappedText {
    /// Wraps an entire file's content as a single real run.
    pub fn from_file(file: FileId, content: &str) -> Self {
        let mut m = Self::default();
        m.push(content, Origin::Real { file, offset: 0 }, None);
        m
    }

    /// Wraps synthesized text as a single synthetic run.
    pub fn synthetic(text: &str, site: Option<Span>) -> Self {
        let mut m = Self::default();
        m.push(text, Origin::Synthetic, site);
        m
    }

    /// Returns the length of the text in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Appends `s` with the given origin, coalescing with the previous run
    /// when the two are contiguous.
    pub fn push(&mut self, s: &str, origin: Origin, site: Option<Span>) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len() as u32;
        let len = s.len() as u32;
        self.text.push_str(s);
        if let Some(last) = self.segs.last_mut() {
            if last.site == site && last.end() == start {
                let contiguous = match (last.origin, origin) {
                    (
                        Origin::Real {
                            file: f1,
                            offset: o1,
                        },
                        Origin::Real {
                            file: f2,
                            offset: o2,
                        },
                    ) => f1 == f2 && o1 + last.len == o2,
                    (Origin::Synthetic, Origin::Synthetic) => true,
                    _ => false,
                };
                if contiguous {
                    last.len += len;
                    return;
                }
            }
        }
        self.segs.push(Seg {
            start,
            len,
            origin,
            site,
        });
    }

    /// Appends `src[range]`, preserving origins. When `site` is `Some`, it
    /// replaces the site of every copied run.
    pub fn push_slice(&mut self, src: &MappedText, range: Range<usize>, site: Option<Span>) {
        let end = range.end.min(src.len());
        if range.start >= end {
            return;
        }
        let Some(mut idx) = src.seg_index(range.start) else {
            return;
        };
        while idx < src.segs.len() {
            let seg = src.segs[idx];
            if seg.start as usize >= end {
                break;
            }
            let a = range.start.max(seg.start as usize);
            let b = end.min(seg.end() as usize);
            let origin = match seg.origin {
                Origin::Real { file, offset } => Origin::Real {
                    file,
                    offset: offset + (a as u32 - seg.start),
                },
                Origin::Synthetic => Origin::Synthetic,
            };
            if let Some(piece) = src.text.get(a..b) {
                self.push(piece, origin, site.or(seg.site));
            }
            idx += 1;
        }
    }

    /// Returns a copy of `self[range]` with its origins.
    pub fn slice(&self, range: Range<usize>) -> MappedText {
        let mut m = MappedText::default();
        m.push_slice(self, range, None);
        m
    }

    /// Returns `range` with leading and trailing ASCII whitespace removed.
    pub fn trim_range(&self, range: Range<usize>) -> Range<usize> {
        let bytes = self.text.as_bytes();
        let mut a = range.start.min(bytes.len());
        let mut b = range.end.min(bytes.len());
        while a < b && bytes[a].is_ascii_whitespace() {
            a += 1;
        }
        while b > a && bytes[b - 1].is_ascii_whitespace() {
            b -= 1;
        }
        a..b
    }

    /// Returns a copy with leading and trailing ASCII whitespace removed.
    pub fn trimmed(&self) -> MappedText {
        self.slice(self.trim_range(0..self.len()))
    }

    /// Returns the index of the run containing byte `pos`, if any.
    pub fn seg_index(&self, pos: usize) -> Option<usize> {
        let pos = u32::try_from(pos).ok()?;
        let idx = self.segs.partition_point(|s| s.end() <= pos);
        (idx < self.segs.len() && self.segs[idx].start <= pos).then_some(idx)
    }

    /// Returns the site of the run containing byte `pos`, if any.
    pub fn site_at(&self, pos: usize) -> Option<Span> {
        self.seg_index(pos).and_then(|i| self.segs[i].site)
    }

    /// Maps `[a, b)` to a real span when those bytes are an exact copy of one
    /// contiguous region of a single file. Returns `None` otherwise.
    pub fn exact_span(&self, a: usize, b: usize) -> Option<Span> {
        if a >= b {
            // Zero-width: anchor at the run containing `a`, or ending at `a`.
            let idx = self
                .seg_index(a)
                .or_else(|| a.checked_sub(1).and_then(|p| self.seg_index(p)))?;
            let seg = self.segs[idx];
            return match seg.origin {
                Origin::Real { file, offset } => {
                    let o = offset + (a as u32).saturating_sub(seg.start).min(seg.len);
                    Some(Span::new(file, o, o))
                }
                Origin::Synthetic => None,
            };
        }
        let first = self.seg_index(a)?;
        let seg = self.segs[first];
        let Origin::Real { file, offset } = seg.origin else {
            return None;
        };
        let start = offset + (a as u32 - seg.start);
        let mut idx = first;
        let mut run_end_offset = offset + seg.len;
        while (self.segs[idx].end() as usize) < b {
            let next = idx + 1;
            let n = self.segs.get(next)?;
            match n.origin {
                Origin::Real { file: f, offset: o } if f == file && o == run_end_offset => {
                    run_end_offset = o + n.len;
                    idx = next;
                }
                _ => return None,
            }
        }
        let last = self.segs[idx];
        let Origin::Real { offset: o, .. } = last.origin else {
            return None;
        };
        Some(Span::new(file, start, o + (b as u32 - last.start)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: u32) -> FileId {
        FileId::from_raw(n)
    }

    #[test]
    fn from_file_maps_identity() {
        let m = MappedText::from_file(f(0), "hello world");
        assert_eq!(m.exact_span(6, 11), Some(Span::new(f(0), 6, 11)));
        assert_eq!(m.exact_span(11, 11), Some(Span::new(f(0), 11, 11)));
    }

    #[test]
    fn push_coalesces_contiguous_real_runs() {
        let mut m = MappedText::default();
        m.push(
            "ab",
            Origin::Real {
                file: f(0),
                offset: 4,
            },
            None,
        );
        m.push(
            "cd",
            Origin::Real {
                file: f(0),
                offset: 6,
            },
            None,
        );
        assert_eq!(m.segs.len(), 1);
        assert_eq!(m.exact_span(1, 3), Some(Span::new(f(0), 5, 7)));
    }

    #[test]
    fn non_contiguous_runs_are_not_exact() {
        let mut m = MappedText::default();
        m.push(
            "8",
            Origin::Real {
                file: f(0),
                offset: 10,
            },
            None,
        );
        m.push(
            "'b0",
            Origin::Real {
                file: f(0),
                offset: 40,
            },
            None,
        );
        assert_eq!(m.exact_span(0, 1), Some(Span::new(f(0), 10, 11)));
        assert_eq!(m.exact_span(1, 4), Some(Span::new(f(0), 40, 43)));
        assert_eq!(m.exact_span(0, 4), None);
    }

    #[test]
    fn synthetic_is_never_exact() {
        let m = MappedText::synthetic("\"x\"", None);
        assert_eq!(m.exact_span(0, 3), None);
        assert_eq!(m.exact_span(0, 0), None);
    }

    #[test]
    fn push_slice_preserves_offsets_and_overrides_site() {
        let src = MappedText::from_file(f(2), "0123456789");
        let site = Span::new(f(0), 1, 3);
        let mut m = MappedText::default();
        m.push_slice(&src, 3..6, Some(site));
        assert_eq!(m.text, "345");
        assert_eq!(m.exact_span(0, 3), Some(Span::new(f(2), 3, 6)));
        assert_eq!(m.site_at(1), Some(site));
    }

    #[test]
    fn trim_and_out_of_range_are_safe() {
        let m = MappedText::from_file(f(0), "  ab  ");
        assert_eq!(m.trimmed().text, "ab");
        assert_eq!(m.trim_range(0..100), 2..4);
        assert_eq!(m.seg_index(100), None);
        assert_eq!(m.exact_span(50, 60), None);
        let empty = MappedText::default();
        assert_eq!(empty.exact_span(0, 0), None);
        assert!(empty.slice(0..5).text.is_empty());
    }
}
