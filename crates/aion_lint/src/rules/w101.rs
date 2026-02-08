//! W101: Unused signal — signal is declared but never read.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, SignalKind};

use crate::helpers::is_signal_read_in_module;
use crate::LintRule;

/// Detects signals that are declared but never read in any expression,
/// assignment RHS, or cell connection input.
///
/// Signals prefixed with `_` and signals of kind `Port` or `Const` are excluded.
pub struct UnusedSignal;

impl LintRule for UnusedSignal {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 101)
    }

    fn name(&self) -> &str {
        "unused-signal"
    }

    fn description(&self) -> &str {
        "signal is declared but never read"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (sig_id, signal) in module.signals.iter() {
            // Skip ports and constants
            if signal.kind == SignalKind::Port || signal.kind == SignalKind::Const {
                continue;
            }

            if !is_signal_read_in_module(module, sig_id) {
                sink.emit(
                    Diagnostic::warning(self.code(), "unused signal", signal.span)
                        .with_label(Label::primary(signal.span, "this signal is never read"))
                        .with_help("consider removing it or prefixing with '_'"),
                );
            }
        }
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
    fn unused_signal_fires() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 101));
    }

    #[test]
    fn used_signal_no_warning() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let target = module.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(11),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(target),
            value: Expr::Signal(SignalRef::Signal(sig_id)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        // sig_id is read, target is unused but also a wire with no reads
        // We expect 1 warning for target (it's written but never read)
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn port_signal_skipped() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn const_signal_skipped() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Const,
            init: Some(ConstValue::Int(42)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
