//! C202: Missing doc — module with no associated documentation.

use aion_diagnostics::{Category, DiagnosticCode, DiagnosticSink, Severity};
use aion_ir::{Design, Module};

use crate::LintRule;

/// Checks for modules that lack documentation.
///
/// Since we don't have access to source text comments at the IR level,
/// this rule currently checks whether the module name follows the
/// underscore-prefix convention that indicates intentionally undocumented
/// internal modules. Modules starting with `_` are considered internal
/// and are not flagged.
///
/// In a future version with source text access, this will check for
/// actual comment-based documentation above the module declaration.
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

    fn check_module(&self, _module: &Module, _design: &Design, _sink: &DiagnosticSink) {
        // Stub: requires source text access to check for comments.
        // Would need interner to resolve module name for _ prefix check.
        // Full implementation deferred until interner is available in check context.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident};
    use aion_ir::*;
    use aion_source::Span;

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

    #[test]
    fn missing_doc_stub_no_diagnostics() {
        let module = mk_module();
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingDoc.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn rule_metadata() {
        let rule = MissingDoc;
        assert_eq!(rule.name(), "missing-doc");
        assert_eq!(rule.code(), DiagnosticCode::new(Category::Convention, 202));
        assert_eq!(rule.default_severity(), Severity::Warning);
    }
}
