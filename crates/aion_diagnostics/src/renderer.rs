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
            let resolved = source_db.resolve_span(diag.primary_span);
            out.push_str(&format!("  {blue_on}-->{blue_off} {resolved}\n"));

            // Source line with underline
            let file = source_db.get_file(diag.primary_span.file);
            let (line, _) = file.line_col(diag.primary_span.start);
            let line_num = format!("{line}");
            let padding = " ".repeat(line_num.len());

            // Find the line content
            let content = file.content.as_str();
            let line_content = get_source_line(content, diag.primary_span.start);

            out.push_str(&format!("{blue_on}{padding} |{blue_off}\n"));
            out.push_str(&format!("{blue_on}{line_num} |{blue_off} {line_content}\n"));

            // Underline. Columns are measured in characters (not bytes) so that
            // carets line up under multi-byte UTF-8 text; offsets that fall
            // inside a character are clamped to its start.
            let start = floor_char_boundary(content, diag.primary_span.start as usize);
            let line_end = content[start..]
                .find('\n')
                .map_or(content.len(), |pos| start + pos);
            let end =
                floor_char_boundary(content, diag.primary_span.end as usize).clamp(start, line_end);
            let line_start = content[..start].rfind('\n').map_or(0, |pos| pos + 1);
            let char_col = content[line_start..start].chars().count();
            let span_len = content[start..end].chars().count().max(1);
            let carets = "^".repeat(span_len);
            let col_padding = " ".repeat(char_col);

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

/// Returns the largest char boundary of `s` that is `<= index`, clamping
/// `index` to `s.len()` first.
///
/// Equivalent to `str::floor_char_boundary`, implemented here so that the
/// renderer never slices a string in the middle of a UTF-8 sequence, whatever
/// byte offsets a diagnostic carries.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Extracts the line of source code containing the given byte offset.
///
/// Never panics: an offset past the end of `content` or inside a multi-byte
/// character is clamped to the nearest preceding character boundary.
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
    fn floor_char_boundary_clamps() {
        let s = "a\u{2018}b"; // 'a', 3-byte quote, 'b'
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 1);
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 3), 1);
        assert_eq!(floor_char_boundary(s, 4), 4);
        assert_eq!(floor_char_boundary(s, 99), s.len());
        assert_eq!(floor_char_boundary("", 3), 0);
    }

    #[test]
    fn get_source_line_mid_char_and_out_of_range() {
        let content = "first\nx <= \u{2018}1\u{2019};\nlast";
        // Offset 12 is inside the 3-byte U+2018 (bytes 11..14).
        assert_eq!(get_source_line(content, 12), "x <= \u{2018}1\u{2019};");
        assert_eq!(get_source_line(content, 13), "x <= \u{2018}1\u{2019};");
        // Past the end clamps to the last line.
        assert_eq!(get_source_line(content, 1000), "last");
    }

    #[test]
    fn render_mid_char_span_does_not_panic() {
        let mut source_db = SourceDb::new();
        let src = "x <= \u{2018}1\u{2019};\n";
        let file_id = source_db.add_source("smart.vhd", src.to_string());
        let code = DiagnosticCode::new(Category::Error, 100);
        // Spans starting and ending inside multi-byte characters.
        for (start, end) in [(6, 7), (7, 10), (5, 13), (13, 13)] {
            let span = aion_source::Span::new(file_id, start, end);
            let diag = Diagnostic::error(code, "unrecognized character", span);
            let output = TerminalRenderer::new(false, 80).render(&diag, &source_db);
            assert!(output.contains("x <= \u{2018}1\u{2019};"), "{output}");
            assert!(output.contains('^'));
        }
    }

    #[test]
    fn render_carets_align_under_multibyte_text() {
        let mut source_db = SourceDb::new();
        let src = "\u{e9}\u{e9} <= \u{2018}1;\n";
        let file_id = source_db.add_source("t.vhd", src.to_string());
        // U+2018 is bytes 8..11; it is the 6th character (char column 6).
        let span = aion_source::Span::new(file_id, 8, 11);
        let diag = Diagnostic::error(DiagnosticCode::new(Category::Error, 100), "bad", span);
        let output = TerminalRenderer::new(false, 80).render(&diag, &source_db);
        let caret_line = output.lines().find(|l| l.contains('^')).unwrap();
        // "  | " prefix, then 6 spaces of padding, then exactly one caret.
        assert!(caret_line.ends_with("|       ^"), "{caret_line:?}");
    }

    #[test]
    fn render_multiline_span_underlines_first_line_only() {
        let mut source_db = SourceDb::new();
        let file_id = source_db.add_source("t.vhd", "ab\ncd\n".to_string());
        let span = aion_source::Span::new(file_id, 1, 5);
        let diag = Diagnostic::error(DiagnosticCode::new(Category::Error, 100), "bad", span);
        let output = TerminalRenderer::new(false, 80).render(&diag, &source_db);
        let caret_line = output.lines().find(|l| l.contains('^')).unwrap();
        assert_eq!(caret_line.matches('^').count(), 1);
    }
}
