//! W104: Missing reset — sequential process has no reset logic.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, ProcessKind, Sensitivity, Statement};

use crate::LintRule;

/// Detects sequential processes (flip-flop style) that have no reset signal
/// in their sensitivity list or body.
///
/// A sequential process without reset can lead to unknown initial states
/// and unreliable behavior after power-on.
pub struct MissingReset;

impl LintRule for MissingReset {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 104)
    }

    fn name(&self) -> &str {
        "missing-reset"
    }

    fn description(&self) -> &str {
        "sequential process has no reset in sensitivity list or body"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (_pid, process) in module.processes.iter() {
            if process.kind != ProcessKind::Sequential {
                continue;
            }

            // Check if sensitivity list has more than one edge (clock + reset)
            let has_reset_in_sensitivity = match &process.sensitivity {
                Sensitivity::EdgeList(edges) => edges.len() > 1,
                _ => false,
            };

            // Check if body contains an if/case that could be a reset check
            let has_reset_in_body = body_has_reset_pattern(&process.body);

            if !has_reset_in_sensitivity && !has_reset_in_body {
                sink.emit(
                    Diagnostic::warning(
                        self.code(),
                        "sequential process has no reset",
                        process.span,
                    )
                    .with_label(Label::primary(process.span, "no reset signal detected"))
                    .with_help(
                        "add an asynchronous or synchronous reset to ensure known initial state",
                    ),
                );
            }
        }
    }
}

/// Checks if the body has a pattern like `if (reset) ... else ...` at the top level,
/// which indicates synchronous reset.
fn body_has_reset_pattern(stmt: &Statement) -> bool {
    match stmt {
        Statement::If { else_body, .. } => {
            // An if at the top level with an else branch suggests reset logic
            else_body.is_some()
        }
        Statement::Block { stmts, .. } => {
            // Check first statement in block
            stmts.first().is_some_and(body_has_reset_pattern)
        }
        _ => false,
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
    fn missing_reset_fires() {
        let mut module = mk_module();
        let clk = SignalId::from_raw(0);
        module.signals.alloc(Signal {
            id: clk,
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(1)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: clk,
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingReset.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 104));
    }

    #[test]
    fn async_reset_no_warning() {
        let mut module = mk_module();
        let clk = SignalId::from_raw(0);
        let rst = SignalId::from_raw(1);
        module.signals.alloc(Signal {
            id: clk,
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.signals.alloc(Signal {
            id: rst,
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
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![
                EdgeSensitivity {
                    signal: clk,
                    edge: Edge::Posedge,
                },
                EdgeSensitivity {
                    signal: rst,
                    edge: Edge::Posedge,
                },
            ]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingReset.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn sync_reset_no_warning() {
        let mut module = mk_module();
        let clk = SignalId::from_raw(0);
        module.signals.alloc(Signal {
            id: clk,
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Body: if (rst) ... else ...  (synchronous reset pattern)
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_u64(0, 1)),
                    span: Span::DUMMY,
                }),
                else_body: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_u64(1, 1)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: clk,
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingReset.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn combinational_process_skipped() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MissingReset.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
