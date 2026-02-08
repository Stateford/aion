//! W106: Latch inferred — combinational process missing else/default branches.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, ProcessKind};

use crate::helpers::stmt_has_full_else_coverage;
use crate::LintRule;

/// Detects combinational processes where `if` statements lack `else` branches
/// or `case` statements lack `default` arms, which causes latch inference.
///
/// Unintentional latch inference is a common source of synthesis issues
/// and simulation-synthesis mismatches.
pub struct LatchInferred;

impl LintRule for LatchInferred {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 106)
    }

    fn name(&self) -> &str {
        "latch-inferred"
    }

    fn description(&self) -> &str {
        "combinational process has if without else or case without default"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (_pid, process) in module.processes.iter() {
            if process.kind != ProcessKind::Combinational {
                continue;
            }

            if !stmt_has_full_else_coverage(&process.body) {
                sink.emit(
                    Diagnostic::warning(
                        self.code(),
                        "latch inferred in combinational process",
                        process.span,
                    )
                    .with_label(Label::primary(
                        process.span,
                        "incomplete if/case branches may infer a latch",
                    ))
                    .with_help(
                        "add else/default branches to cover all paths, or use always_ff for sequential logic",
                    ),
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
    fn latch_inferred_if_without_else() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: None,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        LatchInferred.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 106));
    }

    #[test]
    fn no_latch_if_with_else() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        LatchInferred.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn latch_inferred_case_without_default() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Case {
                subject: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                arms: vec![CaseArm {
                    patterns: vec![Expr::Literal(LogicVec::from_bool(true))],
                    body: Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(1)),
                        value: Expr::Literal(LogicVec::from_bool(true)),
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                }],
                default: None,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        LatchInferred.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn sequential_process_skipped() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: None,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(2),
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        LatchInferred.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
