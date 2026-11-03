//! Diagnostic rendering backends for human-readable and machine-readable output.

use crate::diagnostic::Diagnostic;
use crate::label::LabelStyle;
use crate::severity::Severity;
use aion_source::SourceDb;

/// ANSI escape code for bold red text.
const BOLD_RED: &str = "\x1b[1;31m";
/// ANSI escape code for bold yellow text.
const BOLD_YELLOW: &str = "\x1b[1;33m";
/// ANSI escape code for bold cyan text.
const BOLD_CYAN: &str = "\x1b[1;36m";
/// ANSI escape code for bold blue text.
const BOLD_BLUE: &str = "\x1b[1;34m";
/// ANSI escape code for bold text (default color).
const BOLD: &str = "\x1b[1m";
/// ANSI escape code to reset all formatting.
const RESET: &str = "\x1b[0m";

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

    /// Returns the ANSI color code for the given severity, or empty strings when color is off.
    fn severity_style(&self, severity: &Severity) -> (&'static str, &'static str) {
        if !self.color {
            return ("", "");
        }
        let start = match severity {
            Severity::Error => BOLD_RED,
            Severity::Warning => BOLD_YELLOW,
            Severity::Note => BOLD,
            Severity::Help => BOLD_CYAN,
        };
        (start, RESET)
    }

    /// Returns the ANSI bold blue code for line numbers and pipes, or empty strings when color is off.
    fn blue(&self) -> (&'static str, &'static str) {
        if self.color {
            (BOLD_BLUE, RESET)
        } else {
            ("", "")
        }
    }
}

impl DiagnosticRenderer for TerminalRenderer {
    fn render(&self, diag: &Diagnostic, source_db: &SourceDb) -> String {
        let mut out = String::new();
        let (sev_on, sev_off) = self.severity_style(&diag.severity);
        let (blue_on, blue_off) = self.blue();

        // Header line: severity[CODE]: message
        out.push_str(&format!(
            "{sev_on}{}[{}]{sev_off}: {}\n",
            diag.severity, diag.code, diag.message
        ));

        // Location line
        if !diag.primary_span.is_dummy() {
            // The span is clamped to the file and snapped to UTF-8 character
            // boundaries so a bad span from an upstream stage can never make
            // rendering panic.
            let file = source_db.get_file(diag.primary_span.file);
            let content = file.content.as_str();
            let start = floor_char_boundary(content, diag.primary_span.start as usize);
            let end = ceil_char_boundary(content, diag.primary_span.end as usize).max(start);
            let span = aion_source::Span::new(diag.primary_span.file, start as u32, end as u32);

            let resolved = source_db.resolve_span(span);
            out.push_str(&format!("  {blue_on}-->{blue_off} {resolved}\n"));

            // Source line with underline
            let (line, _) = file.line_col(start as u32);
            let line_num = format!("{line}");
            let padding = " ".repeat(line_num.len());

            // Find the line content
            let line_content = get_source_line(content, start as u32);

            out.push_str(&format!("{blue_on}{padding} |{blue_off}\n"));
            out.push_str(&format!("{blue_on}{line_num} |{blue_off} {line_content}\n"));

            // Underline, measured in characters so it lines up under
            // non-ASCII text (identical to byte counts for ASCII).
            let line_start = content[..start].rfind('\n').map_or(0, |pos| pos + 1);
            let span_len = content[start..end].chars().count().max(1);
            let carets = "^".repeat(span_len);
            let col_padding = " ".repeat(content[line_start..start].chars().count());

            // Find primary label message
            let primary_msg = diag
                .labels
                .iter()
                .find(|l| l.style == LabelStyle::Primary)
                .map(|l| format!(" {}", l.message))
                .unwrap_or_default();

            out.push_str(&format!(
                "{blue_on}{padding} |{blue_off} {col_padding}{sev_on}{carets}{primary_msg}{sev_off}\n"
            ));
        }

        // Notes
        for note in &diag.notes {
            out.push_str(&format!("   {blue_on}={blue_off} note: {note}\n"));
        }

        // Help
        for help in &diag.help {
            out.push_str(&format!("   {blue_on}={blue_off} help: {help}\n"));
        }

        out
    }
}

/// Clamps `offset` to `content.len()` and moves it back to the start of the
/// UTF-8 character it falls inside, if any.
fn floor_char_boundary(content: &str, offset: usize) -> usize {
    let mut offset = offset.min(content.len());
    while !content.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Clamps `offset` to `content.len()` and moves it forward to the end of the
/// UTF-8 character it falls inside, if any.
fn ceil_char_boundary(content: &str, offset: usize) -> usize {
    let mut offset = offset.min(content.len());
    while !content.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Extracts the line of source code containing the given byte offset.
///
/// An offset past the end of `content` is clamped to the end, and an offset
/// inside a multi-byte character is snapped back to that character's start.
fn get_source_line(content: &str, byte_offset: u32) -> &str {
    let offset = floor_char_boundary(content, byte_offset as usize);
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

    #[test]
    fn render_error_with_color() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("test.vhd", "signal foo : std_logic\n".to_string());

        let code = DiagnosticCode::new(Category::Error, 101);
        let span = aion_source::Span::new(file_id, 22, 23);
        let diag = Diagnostic::error(code, "expected ';'", span)
            .with_label(Label::primary(span, "expected ';' here"));

        let renderer = TerminalRenderer::new(true, 80);
        let output = renderer.render(&diag, &source_db);

        // Severity header wrapped in bold red
        assert!(output.contains("\x1b[1;31merror[E101]\x1b[0m: expected ';'"));
        // Arrow in bold blue
        assert!(output.contains("\x1b[1;34m-->\x1b[0m test.vhd:1:23"));
        // Line number in bold blue
        assert!(output.contains("\x1b[1;34m1 |\x1b[0m signal foo : std_logic"));
        // Carets in severity color (bold red)
        assert!(output.contains("\x1b[1;31m^ expected ';' here\x1b[0m"));
    }

    #[test]
    fn render_warning_with_color() {
        let source_db = SourceDb::new();
        let code = DiagnosticCode::new(Category::Warning, 201);
        let diag = Diagnostic::warning(code, "unused signal", aion_source::Span::DUMMY)
            .with_note("signal 'foo' is declared but never read")
            .with_help("consider removing it or prefixing with '_'");

        let renderer = TerminalRenderer::new(true, 80);
        let output = renderer.render(&diag, &source_db);

        // Severity header wrapped in bold yellow
        assert!(output.contains("\x1b[1;33mwarning[W201]\x1b[0m: unused signal"));
        // Note and help '=' in bold blue
        assert!(output.contains("\x1b[1;34m=\x1b[0m note:"));
        assert!(output.contains("\x1b[1;34m=\x1b[0m help:"));
    }

    #[test]
    fn render_no_color_unchanged() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("test.vhd", "signal foo : std_logic\n".to_string());

        let code = DiagnosticCode::new(Category::Error, 101);
        let span = aion_source::Span::new(file_id, 22, 23);
        let diag = Diagnostic::error(code, "expected ';'", span)
            .with_label(Label::primary(span, "expected ';' here"))
            .with_note("a note")
            .with_help("some help");

        let renderer = TerminalRenderer::new(false, 80);
        let output = renderer.render(&diag, &source_db);

        // No ANSI escape codes present
        assert!(
            !output.contains("\x1b["),
            "color: false must not emit ANSI escape codes"
        );
    }

    #[test]
    fn get_source_line_mid_char_offset() {
        let content = "wire a\u{2019};\nnext";
        // Offset 7 is inside the 3-byte U+2019 (bytes 6..9).
        assert_eq!(get_source_line(content, 7), "wire a\u{2019};");
        assert_eq!(get_source_line(content, 8), "wire a\u{2019};");
    }

    #[test]
    fn get_source_line_out_of_bounds_offset() {
        let content = "abc\ndef";
        assert_eq!(get_source_line(content, 7), "def");
        assert_eq!(get_source_line(content, 1000), "def");
        assert_eq!(get_source_line("", 5), "");
    }

    #[test]
    fn char_boundary_helpers() {
        let s = "a\u{e9}b"; // bytes: a(0) é(1..3) b(3)
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(ceil_char_boundary(s, 2), 3);
        assert_eq!(floor_char_boundary(s, 99), s.len());
        assert_eq!(ceil_char_boundary(s, 99), s.len());
        assert_eq!(floor_char_boundary(s, 3), 3);
    }

    #[test]
    fn render_mid_char_span_does_not_panic() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("t.v", "wire caf\u{e9};\n".to_string());
        let code = DiagnosticCode::new(Category::Error, 100);
        // Span 9..10 starts inside the 2-byte 'é' (bytes 8..10).
        let diag = Diagnostic::error(code, "bad", aion_source::Span::new(file_id, 9, 10));
        let output = TerminalRenderer::new(false, 80).render(&diag, &source_db);
        assert!(output.contains("wire caf\u{e9};"));
        // Snapped to the start of 'é': 8 columns of padding, one caret.
        assert!(output.contains(&format!(" | {}^\n", " ".repeat(8))));
    }

    #[test]
    fn render_out_of_bounds_and_inverted_spans_do_not_panic() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("t.v", "abc".to_string());
        let code = DiagnosticCode::new(Category::Error, 100);
        let renderer = TerminalRenderer::new(false, 80);
        for (start, end) in [(3, 50), (40, 50), (2, 1), (u32::MAX, u32::MAX)] {
            let diag = Diagnostic::error(code, "bad", aion_source::Span::new(file_id, start, end));
            let output = renderer.render(&diag, &source_db);
            assert!(output.contains("abc"), "span {start}..{end}: {output}");
        }
    }

    #[test]
    fn render_carets_count_characters() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("t.v", "\u{e9} \u{2019};".to_string());
        let code = DiagnosticCode::new(Category::Error, 100);
        // U+2019 is bytes 3..6; after the 2-byte 'é' and a space it is column 3.
        let diag = Diagnostic::error(code, "bad", aion_source::Span::new(file_id, 3, 6));
        let output = TerminalRenderer::new(false, 80).render(&diag, &source_db);
        assert!(output.ends_with(" |   ^\n"), "{output}");
    }
}
