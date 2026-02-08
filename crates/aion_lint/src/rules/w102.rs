//! W102: Undriven signal — signal is never assigned or driven.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, PortDirection, SignalKind};

use crate::helpers::is_signal_driven_in_module;
use crate::LintRule;

/// Detects signals that are declared but never driven by any assignment,
/// process, or cell output connection.
///
/// Input ports and constants are excluded since they are driven externally
/// or by definition.
pub struct UndrivenSignal;

impl LintRule for UndrivenSignal {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 102)
    }

    fn name(&self) -> &str {
        "undriven-signal"
    }

    fn description(&self) -> &str {
        "signal is never assigned or driven"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (sig_id, signal) in module.signals.iter() {
            // Skip constants (driven by definition)
            if signal.kind == SignalKind::Const {
                continue;
            }

            // Skip input ports (driven externally)
            if signal.kind == SignalKind::Port {
                let is_input = module
                    .ports
                    .iter()
                    .any(|p| p.signal == sig_id && p.direction == PortDirection::Input);
                if is_input {
                    continue;
                }
            }

            if !is_signal_driven_in_module(module, sig_id) {
                sink.emit(
                    Diagnostic::warning(self.code(), "undriven signal", signal.span)
                        .with_label(Label::primary(signal.span, "this signal is never assigned"))
                        .with_help("assign a value or connect to a driver"),
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
    fn undriven_signal_fires() {
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
        UndrivenSignal.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 102));
    }

    #[test]
    fn driven_signal_no_warning() {
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
        UndrivenSignal.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn input_port_skipped() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.ports.push(Port {
            id: PortId::from_raw(0),
            name: Ident::from_raw(10),
            direction: PortDirection::Input,
            ty: TypeId::from_raw(0),
            signal: sig_id,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UndrivenSignal.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn output_port_undriven_fires() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.ports.push(Port {
            id: PortId::from_raw(0),
            name: Ident::from_raw(10),
            direction: PortDirection::Output,
            ty: TypeId::from_raw(0),
            signal: sig_id,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UndrivenSignal.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn const_signal_skipped() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Const,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UndrivenSignal.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
