//! W108: Dead logic — unreachable code after `Finish` or always-true/false conditions.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Expr, Module, Statement};

use crate::LintRule;

/// Detects dead logic: code after `$finish`, or `if`/`case` with
/// always-true or always-false literal conditions.
///
/// Dead logic wastes synthesis resources and may indicate bugs.
pub struct DeadLogic;

impl LintRule for DeadLogic {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 108)
    }

    fn name(&self) -> &str {
        "dead-logic"
    }

    fn description(&self) -> &str {
        "code after Finish or always-true/false conditions"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (_pid, process) in module.processes.iter() {
            check_dead_logic(&process.body, self.code(), sink);
        }
    }
}

fn check_dead_logic(stmt: &Statement, code: DiagnosticCode, sink: &DiagnosticSink) {
    match stmt {
        Statement::Block { stmts, .. } => {
            let mut after_finish = false;
            for s in stmts {
                if after_finish {
                    if let Some(span) = stmt_span(s) {
                        sink.emit(
                            Diagnostic::warning(code, "dead logic after $finish", span)
                                .with_label(Label::primary(span, "unreachable code")),
                        );
                    }
                    break; // Only report once per block
                }
                if matches!(s, Statement::Finish { .. }) {
                    after_finish = true;
                }
                check_dead_logic(s, code, sink);
            }
        }
        Statement::If {
            condition,
            then_body,
            else_body,
            span,
            ..
        } => {
            if let Some(is_true) = is_literal_bool(condition) {
                sink.emit(
                    Diagnostic::warning(
                        code,
                        if is_true {
                            "condition is always true"
                        } else {
                            "condition is always false"
                        },
                        *span,
                    )
                    .with_label(Label::primary(*span, "constant condition")),
                );
            }
            check_dead_logic(then_body, code, sink);
            if let Some(else_b) = else_body {
                check_dead_logic(else_b, code, sink);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                check_dead_logic(&arm.body, code, sink);
            }
            if let Some(def) = default {
                check_dead_logic(def, code, sink);
            }
        }
        _ => {}
    }
}

/// Returns the span of a statement, if it has one.
fn stmt_span(stmt: &Statement) -> Option<aion_source::Span> {
    match stmt {
        Statement::Assign { span, .. }
        | Statement::If { span, .. }
        | Statement::Case { span, .. }
        | Statement::Block { span, .. }
        | Statement::Wait { span, .. }
        | Statement::Assertion { span, .. }
        | Statement::Display { span, .. }
        | Statement::Finish { span } => Some(*span),
        Statement::Nop => None,
    }
}

/// Checks if an expression is a literal boolean (all-zeros = false, all-ones = true).
fn is_literal_bool(expr: &Expr) -> Option<bool> {
    if let Expr::Literal(lv) = expr {
        if lv.is_all_zero() {
            return Some(false);
        }
        if lv.is_all_one() {
            return Some(true);
        }
    }
    None
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
    fn dead_logic_after_finish() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Finish { span: Span::DUMMY },
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_bool(true)),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        DeadLogic.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("dead logic after $finish"));
    }

    #[test]
    fn always_true_condition() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::If {
                condition: Expr::Literal(LogicVec::from_bool(true)),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
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
        DeadLogic.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("always true"));
    }

    #[test]
    fn always_false_condition() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::If {
                condition: Expr::Literal(LogicVec::from_u64(0, 1)),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
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
        DeadLogic.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("always false"));
    }

    #[test]
    fn normal_code_no_warning() {
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
        DeadLogic.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
