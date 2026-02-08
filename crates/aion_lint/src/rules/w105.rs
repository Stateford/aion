//! W105: Incomplete sensitivity list — combinational process missing signals.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, ProcessKind, Sensitivity};

use crate::helpers::collect_read_signals;
use crate::LintRule;

/// Detects combinational processes with explicit `SignalList` sensitivity
/// that are missing signals read in the process body.
///
/// Missing signals in the sensitivity list cause simulation-synthesis
/// mismatches. Processes with `Sensitivity::All` are not checked.
pub struct IncompleteSensitivity;

impl LintRule for IncompleteSensitivity {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 105)
    }

    fn name(&self) -> &str {
        "incomplete-sensitivity"
    }

    fn description(&self) -> &str {
        "combinational process with SignalList missing signals read in body"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (_pid, process) in module.processes.iter() {
            if process.kind != ProcessKind::Combinational {
                continue;
            }

            let sens_signals = match &process.sensitivity {
                Sensitivity::SignalList(sigs) => sigs,
                Sensitivity::All | Sensitivity::EdgeList(_) => continue,
            };

            let read_signals = collect_read_signals(&process.body);
            let sens_set: std::collections::HashSet<_> = sens_signals.iter().copied().collect();

            for sig_id in &read_signals {
                if !sens_set.contains(sig_id) {
                    sink.emit(
                        Diagnostic::warning(
                            self.code(),
                            "incomplete sensitivity list",
                            process.span,
                        )
                        .with_label(Label::primary(
                            process.span,
                            "signal read in body but missing from sensitivity list",
                        ))
                        .with_help(
                            "use `always @(*)` or `always_comb` to infer the full sensitivity list",
                        ),
                    );
                    // Report once per process, not once per missing signal
                    break;
                }
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
    fn incomplete_sensitivity_fires() {
        let mut module = mk_module();
        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);
        module.signals.alloc(Signal {
            id: a,
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.signals.alloc(Signal {
            id: b,
            name: Ident::from_raw(11),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Process reads a and b but only has a in sensitivity
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Signal(SignalRef::Signal(a))),
                    rhs: Box::new(Expr::Signal(SignalRef::Signal(b))),
                    ty: TypeId::from_raw(0),
                    span: Span::DUMMY,
                },
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(vec![a]), // Missing b
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 105));
    }

    #[test]
    fn complete_sensitivity_no_warning() {
        let mut module = mk_module();
        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);
        module.signals.alloc(Signal {
            id: a,
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.signals.alloc(Signal {
            id: b,
            name: Ident::from_raw(11),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Signal(SignalRef::Signal(a))),
                    rhs: Box::new(Expr::Signal(SignalRef::Signal(b))),
                    ty: TypeId::from_raw(0),
                    span: Span::DUMMY,
                },
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(vec![a, b]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn sensitivity_all_skipped() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
