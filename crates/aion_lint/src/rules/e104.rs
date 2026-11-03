//! E104: Multiple drivers — wire signal driven by more than one source.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, SignalKind};

use crate::helpers::count_drivers;
use crate::{LintContext, LintRule};

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

    fn check_module(
        &self,
        module: &Module,
        _design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        for (sig_id, signal) in module.signals.iter() {
            if signal.kind != SignalKind::Wire {
                continue;
            }

            let driver_count = count_drivers(module, sig_id);
            if driver_count > 1 {
                let name = ctx.name(signal.name);
                sink.emit(
                    Diagnostic::error(
                        self.code(),
                        format!("signal `{name}` has {driver_count} concurrent drivers"),
                        signal.span,
                    )
                    .with_label(Label::primary(
                        signal.span,
                        format!("multiple drivers on wire `{name}`"),
                    ))
                    .with_help(format!("ensure `{name}` has exactly one driver")),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_util::empty_ctx;
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
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    // ---- Signal names in messages (issue #39) ----

    #[test]
    fn message_names_the_wire() {
        let interner = aion_common::Interner::new();
        let db = aion_source::SourceDb::new();
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: interner.get_or_intern("bus"),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        for v in [true, false, true] {
            module.assignments.push(Assignment {
                target: SignalRef::Signal(sig_id),
                value: Expr::Literal(LogicVec::from_bool(v)),
                span: Span::DUMMY,
            });
        }
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        let ctx = LintContext::new(&interner, &db);
        MultipleDrivers.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "signal `bus` has 3 concurrent drivers");
        assert_eq!(diags[0].labels[0].message, "multiple drivers on wire `bus`");
    }
}
