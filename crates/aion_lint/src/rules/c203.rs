//! C203: Magic number — literal values used directly in expressions.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Expr, Module, Statement};

use crate::LintRule;

/// Detects "magic numbers" — literal values wider than 1 bit and not
/// 0 or 1 used directly in expressions.
///
/// Magic numbers make code harder to read and maintain. They should be
/// replaced with named constants or parameters.
pub struct MagicNumber;

impl LintRule for MagicNumber {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Convention, 203)
    }

    fn name(&self) -> &str {
        "magic-number"
    }

    fn description(&self) -> &str {
        "literal value used directly in expression instead of named constant"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        // Check process bodies for magic numbers in expressions
        for (_pid, process) in module.processes.iter() {
            check_stmt_magic_numbers(&process.body, self.code(), sink);
        }

        // Check continuous assignment RHS
        for assign in &module.assignments {
            check_expr_magic_numbers(&assign.value, self.code(), sink);
        }
    }
}

fn check_stmt_magic_numbers(stmt: &Statement, code: DiagnosticCode, sink: &DiagnosticSink) {
    match stmt {
        Statement::Assign { value, .. } => {
            check_expr_magic_numbers(value, code, sink);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            check_expr_magic_numbers(condition, code, sink);
            check_stmt_magic_numbers(then_body, code, sink);
            if let Some(else_b) = else_body {
                check_stmt_magic_numbers(else_b, code, sink);
            }
        }
        Statement::Case {
            subject,
            arms,
            default,
            ..
        } => {
            check_expr_magic_numbers(subject, code, sink);
            for arm in arms {
                // Case arm patterns are often literal — skip them
                check_stmt_magic_numbers(&arm.body, code, sink);
            }
            if let Some(def) = default {
                check_stmt_magic_numbers(def, code, sink);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                check_stmt_magic_numbers(s, code, sink);
            }
        }
        _ => {}
    }
}

fn check_expr_magic_numbers(expr: &Expr, code: DiagnosticCode, sink: &DiagnosticSink) {
    match expr {
        Expr::Literal(lv) => {
            // Skip 1-bit literals and values 0/1
            if lv.width() <= 1 {
                return;
            }
            if lv.is_all_zero() || lv.is_all_one() {
                return;
            }
            // This is a magic number
            // We don't have a span on Expr::Literal, so we use DUMMY
            sink.emit(
                Diagnostic::warning(code, "magic number in expression", aion_source::Span::DUMMY)
                    .with_label(Label::primary(
                        aion_source::Span::DUMMY,
                        "consider using a named constant",
                    )),
            );
        }
        Expr::Unary { operand, .. } => {
            check_expr_magic_numbers(operand, code, sink);
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr_magic_numbers(lhs, code, sink);
            check_expr_magic_numbers(rhs, code, sink);
        }
        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            check_expr_magic_numbers(condition, code, sink);
            check_expr_magic_numbers(true_val, code, sink);
            check_expr_magic_numbers(false_val, code, sink);
        }
        Expr::FuncCall { args, .. } => {
            for arg in args {
                check_expr_magic_numbers(arg, code, sink);
            }
        }
        Expr::Concat(exprs) => {
            for e in exprs {
                check_expr_magic_numbers(e, code, sink);
            }
        }
        Expr::Repeat { expr, .. } => {
            check_expr_magic_numbers(expr, code, sink);
        }
        Expr::Index { expr, index, .. } => {
            check_expr_magic_numbers(expr, code, sink);
            check_expr_magic_numbers(index, code, sink);
        }
        Expr::Slice {
            expr, high, low, ..
        } => {
            check_expr_magic_numbers(expr, code, sink);
            check_expr_magic_numbers(high, code, sink);
            check_expr_magic_numbers(low, code, sink);
        }
        Expr::Signal(_) => {}
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
    fn magic_number_fires() {
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_u64(42, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("magic number"));
    }

    #[test]
    fn zero_literal_no_warning() {
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_u64(0, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn one_bit_literal_no_warning() {
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_bool(true)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn all_ones_no_warning() {
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn magic_in_process() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(0xDEAD, 16)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
    }
}
