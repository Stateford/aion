//! C202: Missing doc — module with no associated documentation.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module};

use crate::helpers::is_duplicate_specialization;
use crate::{LintContext, LintRule};

/// Checks for modules (Verilog/SystemVerilog `module`, VHDL `entity`) that
/// have no documentation comment directly above their declaration.
///
/// The rule reads the source text preceding the declaration, skipping blank
/// lines and lines that sit between a header comment and the declaration by
/// convention — Verilog compiler directives (`` `timescale ``), attribute
/// instances (`(* keep *)`) and VHDL context clauses (`library` / `use` /
/// `context`). The module counts as documented if the first remaining line is
/// a comment: a `//` or `--` line, or the end of a `/* ... */` block.
///
/// Modules whose name starts with `_` are treated as internal and are not
/// flagged, and modules without source text (e.g. synthesized ones) are
/// skipped. Each declaration is reported once even when it is elaborated
/// with several parameterizations.
pub struct MissingDoc;

impl LintRule for MissingDoc {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Convention, 202)
    }

    fn name(&self) -> &str {
        "missing-doc"
    }

    fn description(&self) -> &str {
        "module lacks documentation"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        let name = ctx.name(module.name);
        if name.starts_with('_') || is_duplicate_specialization(module, design) {
            return;
        }
        let Some(before) = ctx.file_text_before(module.span) else {
            return;
        };
        if has_preceding_doc_comment(before) {
            return;
        }

        let is_vhdl = ctx
            .source_db()
            .try_get_file(module.span.file)
            .and_then(|f| f.path.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("vhd") || e.eq_ignore_ascii_case("vhdl"));
        let (kind, comment) = if is_vhdl {
            ("entity", "--")
        } else {
            ("module", "//")
        };

        sink.emit(
            Diagnostic::warning(
                self.code(),
                format!("{kind} `{name}` has no documentation comment"),
                module.span,
            )
            .with_label(Label::primary(
                module.span,
                format!("`{name}` is declared without a preceding comment"),
            ))
            .with_help(format!(
                "add a `{comment}` comment directly above the declaration describing what `{name}` does"
            )),
        );
    }
}

/// Returns true if the text preceding a declaration ends with a comment,
/// ignoring blank lines, compiler directives, attribute instances and VHDL
/// context clauses in between.
fn has_preceding_doc_comment(before: &str) -> bool {
    for line in before.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("//") || line.starts_with("--") || line.ends_with("*/") {
            return true;
        }
        if is_transparent_line(line) {
            continue;
        }
        return false;
    }
    false
}

/// Returns true for lines conventionally placed between a module's header
/// comment and its declaration.
fn is_transparent_line(line: &str) -> bool {
    if line.starts_with('`') || (line.starts_with("(*") && line.ends_with("*)")) {
        return true;
    }
    let lower = line.to_ascii_lowercase();
    ["library ", "use ", "context "]
        .iter()
        .any(|kw| lower.starts_with(kw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_util::empty_ctx;
    use aion_common::{ContentHash, Ident, Interner};
    use aion_ir::*;
    use aion_source::{SourceDb, Span};

    fn mk_module() -> Module {
        Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(0),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(&[]),
        }
    }

    fn mk_design(module: Module) -> Design {
        let mut modules = Arena::new();
        let top = modules.alloc(module);
        Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        }
    }

    /// Lints a single module named `name` whose declaration starts at the
    /// first occurrence of `decl` in `src` (stored as `file`).
    fn lint_source(file: &str, src: &str, decl: &str, name: &str) -> Vec<Diagnostic> {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let fid = db.add_source(file, src.to_string());
        let start = src.find(decl).expect("decl present") as u32;
        let mut module = mk_module();
        module.name = interner.get_or_intern(name);
        module.span = Span::new(fid, start, src.len() as u32);
        let design = mk_design(module);
        let ctx = LintContext::new(&interner, &db);
        let sink = DiagnosticSink::new();
        MissingDoc.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn undocumented_verilog_module_fires() {
        let src = "module top(input a, output y);\n  assign y = a;\nendmodule\n";
        let diags = lint_source("top.v", src, "module top", "top");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].code,
            DiagnosticCode::new(Category::Convention, 202)
        );
        assert_eq!(
            diags[0].message,
            "module `top` has no documentation comment"
        );
        assert!(diags[0].help.iter().any(|h| h.contains("`//`")));
    }

    #[test]
    fn line_comment_above_module_is_documentation() {
        let src = "// A pass-through buffer.\nmodule top(input a, output y);\nendmodule\n";
        assert!(lint_source("top.v", src, "module top", "top").is_empty());
    }

    #[test]
    fn block_comment_and_directives_are_accepted() {
        let src =
            "/**\n * Counter.\n */\n`timescale 1ns/1ps\n\n(* keep *)\nmodule top; endmodule\n";
        assert!(lint_source("top.v", src, "module top", "top").is_empty());
    }

    #[test]
    fn code_between_comment_and_module_does_not_count() {
        let src = "// helper\nmodule a; endmodule\nmodule top; endmodule\n";
        let diags = lint_source("top.v", src, "module top", "top");
        assert_eq!(diags.len(), 1, "{diags:#?}");
    }

    #[test]
    fn vhdl_comment_above_context_clause_documents_entity() {
        let src = "-- Blinks an LED.\nlibrary ieee;\nuse ieee.std_logic_1164.all;\n\nentity blink is\nend entity;\n";
        assert!(lint_source("blink.vhd", src, "entity blink", "blink").is_empty());
    }

    #[test]
    fn undocumented_vhdl_entity_uses_vhdl_wording() {
        let src = "library ieee;\nuse ieee.std_logic_1164.all;\nentity blink is\nend entity;\n";
        let diags = lint_source("blink.vhd", src, "entity blink", "blink");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "entity `blink` has no documentation comment"
        );
        assert!(diags[0].help.iter().any(|h| h.contains("`--`")));
    }

    #[test]
    fn underscore_module_is_exempt() {
        let src = "module _internal; endmodule\n";
        assert!(lint_source("t.v", src, "module _internal", "_internal").is_empty());
    }

    #[test]
    fn module_without_source_text_is_skipped() {
        let module = mk_module();
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingDoc.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn specializations_report_once() {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let src = "module sub; endmodule\n";
        let fid = db.add_source("sub.v", src.to_string());
        let mut module = mk_module();
        module.name = interner.get_or_intern("sub");
        module.span = Span::new(fid, 0, src.len() as u32);
        let second = module.clone();
        let mut design = mk_design(module);
        let second_id = design.modules.alloc(second);
        let ctx = LintContext::new(&interner, &db);
        let sink = DiagnosticSink::new();
        MissingDoc.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        MissingDoc.check_module(design.modules.get(second_id), &design, &ctx, &sink);
        assert_eq!(sink.take_all().len(), 1);
    }

    #[test]
    fn preceding_comment_detection() {
        assert!(has_preceding_doc_comment("// doc\n"));
        assert!(has_preceding_doc_comment("-- doc\n\n\n"));
        assert!(!has_preceding_doc_comment(""));
        assert!(!has_preceding_doc_comment("endmodule\n"));
        assert!(!has_preceding_doc_comment("`timescale 1ns/1ps\n"));
    }

    #[test]
    fn rule_metadata() {
        let rule = MissingDoc;
        assert_eq!(rule.name(), "missing-doc");
        assert_eq!(rule.code(), DiagnosticCode::new(Category::Convention, 202));
        assert_eq!(rule.default_severity(), Severity::Warning);
    }
}
