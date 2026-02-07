//! Diagnostic rendering backends for human-readable and machine-readable output.

use crate::diagnostic::Diagnostic;
use crate::label::LabelStyle;
use aion_source::SourceDb;

/// Trait for rendering diagnostics into formatted output strings.
///
/// Implementations format diagnostics for different output targets:
/// terminal (human-readable), JSON, and SARIF (machine-readable).
pub trait DiagnosticRenderer {
    /// Renders a single diagnostic into a formatted string.
    fn render(&self, diag: &Diagnostic, source_db: &SourceDb) -> String;
}

/// Renders diagnostics in a rustc-style terminal format.
///
/// Produces output like:
/// ```text
/// error[E101]: unexpected token
///   --> src/top.vhd:10:5
///    |
/// 10 | signal foo : std_logic
///    |                       ^ expected ';'
///    |
///    = note: ...
///    = help: ...
/// ```
pub struct TerminalRenderer {
    /// Whether to use ANSI color codes in output.
    pub color: bool,
    /// The terminal width for line wrapping.
    pub width: u16,
}

impl TerminalRenderer {
    /// Creates a new terminal renderer.
    pub fn new(color: bool, width: u16) -> Self {
        Self { color, width }
    }
}

impl DiagnosticRenderer for TerminalRenderer {
    fn render(&self, diag: &Diagnostic, source_db: &SourceDb) -> String {
        let mut out = String::new();

        // Header line: severity[CODE]: message
        out.push_str(&format!(
            "{}[{}]: {}\n",
            diag.severity, diag.code, diag.message
        ));

        // Location line
        if !diag.primary_span.is_dummy() {
            let resolved = source_db.resolve_span(diag.primary_span);
            out.push_str(&format!("  --> {resolved}\n"));

            // Source line with underline
            let file = source_db.get_file(diag.primary_span.file);
            let (line, col) = file.line_col(diag.primary_span.start);
            let line_num = format!("{line}");
            let padding = " ".repeat(line_num.len());

            // Find the line content
            let line_content = get_source_line(&file.content, diag.primary_span.start);

            out.push_str(&format!("{padding} |\n"));
            out.push_str(&format!("{line_num} | {line_content}\n"));

            // Underline
            let span_len = (diag.primary_span.end - diag.primary_span.start).max(1) as usize;
            let carets = "^".repeat(span_len);
            let col_padding = " ".repeat((col as usize).saturating_sub(1));

            // Find primary label message
            let primary_msg = diag
                .labels
                .iter()
                .find(|l| l.style == LabelStyle::Primary)
                .map(|l| format!(" {}", l.message))
                .unwrap_or_default();

            out.push_str(&format!("{padding} | {col_padding}{carets}{primary_msg}\n"));
        }

        // Notes
        for note in &diag.notes {
            out.push_str(&format!("   = note: {note}\n"));
        }

        // Help
        for help in &diag.help {
            out.push_str(&format!("   = help: {help}\n"));
        }

        out
    }
}

/// Extracts the line of source code containing the given byte offset.
fn get_source_line(content: &str, byte_offset: u32) -> &str {
    let offset = byte_offset as usize;
    let start = content[..offset].rfind('\n').map_or(0, |pos| pos + 1);
    let end = content[offset..]
        .find('\n')
        .map_or(content.len(), |pos| offset + pos);
    &content[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::{Category, DiagnosticCode};
    use crate::label::Label;

    #[test]
    fn render_error_with_span() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("test.vhd", "signal foo : std_logic\n".to_string());

        let code = DiagnosticCode::new(Category::Error, 101);
        let span = aion_source::Span::new(file_id, 22, 23);
        let diag = Diagnostic::error(code, "expected ';'", span)
            .with_label(Label::primary(span, "expected ';' here"));

        let renderer = TerminalRenderer::new(false, 80);
        let output = renderer.render(&diag, &source_db);

        assert!(output.contains("error[E101]: expected ';'"));
        assert!(output.contains("--> test.vhd:1:23"));
        assert!(output.contains("signal foo : std_logic"));
        assert!(output.contains("^"));
    }

    #[test]
    fn render_warning_with_notes() {
        let source_db = SourceDb::new();
        let code = DiagnosticCode::new(Category::Warning, 201);
        let diag = Diagnostic::warning(code, "unused signal", aion_source::Span::DUMMY)
            .with_note("signal 'foo' is declared but never read")
            .with_help("consider removing it or prefixing with '_'");

        let renderer = TerminalRenderer::new(false, 80);
        let output = renderer.render(&diag, &source_db);

        assert!(output.contains("warning[W201]: unused signal"));
        assert!(output.contains("= note: signal 'foo' is declared but never read"));
        assert!(output.contains("= help: consider removing it or prefixing with '_'"));
    }

    #[test]
    fn render_dummy_span_no_source() {
        let source_db = SourceDb::new();
        let code = DiagnosticCode::new(Category::Error, 999);
        let diag = Diagnostic::error(code, "general error", aion_source::Span::DUMMY);

        let renderer = TerminalRenderer::new(false, 80);
        let output = renderer.render(&diag, &source_db);

        assert!(output.contains("error[E999]: general error"));
        assert!(!output.contains("-->"));
    }
}
