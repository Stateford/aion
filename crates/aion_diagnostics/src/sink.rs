//! Thread-safe diagnostic accumulator for parallel compilation stages.

use crate::diagnostic::Diagnostic;
use crate::severity::Severity;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// A thread-safe accumulator for diagnostics emitted during compilation.
///
/// Multiple threads can emit diagnostics concurrently via [`emit`](Self::emit).
/// The error count is tracked atomically for fast `has_errors` checks without
/// locking the diagnostic vector.
pub struct DiagnosticSink {
    diagnostics: Mutex<Vec<Diagnostic>>,
    error_count: AtomicUsize,
}

impl DiagnosticSink {
    /// Creates a new empty diagnostic sink.
    pub fn new() -> Self {
        Self {
            diagnostics: Mutex::new(Vec::new()),
            error_count: AtomicUsize::new(0),
        }
    }

    /// Emits a diagnostic into the sink.
    ///
    /// If the diagnostic has [`Severity::Error`], the error count is incremented atomically.
    pub fn emit(&self, diag: Diagnostic) {
        if diag.severity == Severity::Error {
            self.error_count.fetch_add(1, Ordering::Relaxed);
        }
        let mut diagnostics = self.diagnostics.lock().unwrap();
        diagnostics.push(diag);
    }

    /// Returns `true` if any error-severity diagnostics have been emitted.
    pub fn has_errors(&self) -> bool {
        self.error_count.load(Ordering::Relaxed) > 0
    }

    /// Returns the number of error-severity diagnostics emitted so far.
    pub fn error_count(&self) -> usize {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Takes all accumulated diagnostics, leaving the sink empty.
    pub fn take_all(&self) -> Vec<Diagnostic> {
        let mut diagnostics = self.diagnostics.lock().unwrap();
        std::mem::take(&mut *diagnostics)
    }

    /// Returns a snapshot of all accumulated diagnostics without draining.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let diagnostics = self.diagnostics.lock().unwrap();
        diagnostics.clone()
    }
}

impl Default for DiagnosticSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::{Category, DiagnosticCode};
    use aion_source::Span;

    fn make_error() -> Diagnostic {
        Diagnostic::error(
            DiagnosticCode::new(Category::Error, 101),
            "test error",
            Span::DUMMY,
        )
    }

    fn make_warning() -> Diagnostic {
        Diagnostic::warning(
            DiagnosticCode::new(Category::Warning, 201),
            "test warning",
            Span::DUMMY,
        )
    }

    #[test]
    fn empty_sink() {
        let sink = DiagnosticSink::new();
        assert!(!sink.has_errors());
        assert_eq!(sink.error_count(), 0);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn emit_error() {
        let sink = DiagnosticSink::new();
        sink.emit(make_error());
        assert!(sink.has_errors());
        assert_eq!(sink.error_count(), 1);
    }

    #[test]
    fn emit_warning_not_error() {
        let sink = DiagnosticSink::new();
        sink.emit(make_warning());
        assert!(!sink.has_errors());
        assert_eq!(sink.error_count(), 0);
        assert_eq!(sink.diagnostics().len(), 1);
    }

    #[test]
    fn take_all_drains() {
        let sink = DiagnosticSink::new();
        sink.emit(make_error());
        sink.emit(make_warning());
        let all = sink.take_all();
        assert_eq!(all.len(), 2);
        assert!(sink.take_all().is_empty());
        // Error count is NOT reset by take_all (it's an atomic counter)
        assert_eq!(sink.error_count(), 1);
    }

    #[test]
    fn thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let sink = Arc::new(DiagnosticSink::new());
        let mut handles = Vec::new();

        for _ in 0..10 {
            let sink = Arc::clone(&sink);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    sink.emit(make_error());
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(sink.error_count(), 1000);
        assert_eq!(sink.diagnostics().len(), 1000);
    }
}
