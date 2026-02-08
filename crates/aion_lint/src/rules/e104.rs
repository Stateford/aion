//! E104: Multiple drivers — wire signal driven by more than one source.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, SignalKind};

use crate::helpers::count_drivers;
use crate::LintRule;

/// Detects wire signals that are driven by multiple concurrent sources.
///
/// In synthesis, a wire with multiple drivers creates a short circuit.
/// Only `Wire` signals are checked; `Reg` signals can be assigned
/// in different processes under certain conditions.
pub struct MultipleDrivers;

impl LintRule for MultipleDrivers {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Error, 104)
    }

    fn name(&self) -> &str {
        "multiple-drivers"
    }

    fn description(&self) -> &str {
        "wire signal driven by more than one concurrent source"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (sig_id, signal) in module.signals.iter() {
            if signal.kind != SignalKind::Wire {
                continue;
            }

            let driver_count = count_drivers(module, sig_id);
            if driver_count > 1 {
                sink.emit(
                    Diagnostic::error(
                        self.code(),
                        format!("signal has {} concurrent drivers", driver_count),
                        signal.span,
                    )
                    .with_label(Label::primary(signal.span, "multiple drivers on this wire"))
                    .with_help("ensure each wire has exactly one driver"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, LogicVec};
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
    fn multiple_drivers_fires() {
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
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_bool(true)),
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_bool(false)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("2 concurrent drivers"));
    }

    #[test]
    fn single_driver_no_error() {
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
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_bool(true)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn reg_signal_skipped() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_bool(true)),
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_bool(false)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
