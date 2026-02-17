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
            check_expr_magic_numbers(&assign.value, self.code(), assign.span, sink);
        }
    }
}

fn check_stmt_magic_numbers(stmt: &Statement, code: DiagnosticCode, sink: &DiagnosticSink) {
    match stmt {
        Statement::Assign { value, span, .. } => {
            check_expr_magic_numbers(value, code, *span, sink);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
            span,
        } => {
            check_expr_magic_numbers(condition, code, *span, sink);
            check_stmt_magic_numbers(then_body, code, sink);
            if let Some(else_b) = else_body {
                check_stmt_magic_numbers(else_b, code, sink);
            }
        }
        Statement::Case {
            subject,
            arms,
            default,
            span,
        } => {
            check_expr_magic_numbers(subject, code, *span, sink);
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

/// Formats a `LogicVec` literal value as a human-readable string (e.g. `0x2A`).
fn format_literal_value(lv: &aion_common::LogicVec) -> String {
    if let Some(val) = lv.to_u64() {
        format!("0x{val:X}")
    } else {
        // Fallback to binary representation for values that don't fit u64
        format!("{lv}")
    }
}

fn check_expr_magic_numbers(
    expr: &Expr,
    code: DiagnosticCode,
    context_span: aion_source::Span,
    sink: &DiagnosticSink,
) {
    match expr {
        Expr::Literal(lv) => {
            // Skip 1-bit literals and values 0/1
            if lv.width() <= 1 {
                return;
            }
            if lv.is_all_zero() || lv.is_all_one() {
                return;
            }
            let value_str = format_literal_value(lv);
            let width = lv.width();
            sink.emit(
                Diagnostic::warning(
                    code,
                    format!("magic number `{value_str}` ({width}-bit) in expression"),
                    context_span,
                )
                .with_label(Label::primary(
                    context_span,
                    format!("literal `{value_str}` used directly"),
                ))
                .with_help("replace with a named constant or parameter for clarity"),
            );
        }
        Expr::Unary { operand, span, .. } => {
            check_expr_magic_numbers(operand, code, *span, sink);
        }
        Expr::Binary { lhs, rhs, span, .. } => {
            check_expr_magic_numbers(lhs, code, *span, sink);
            check_expr_magic_numbers(rhs, code, *span, sink);
        }
        Expr::Ternary {
            condition,
            true_val,
            false_val,
            span,
            ..
        } => {
            check_expr_magic_numbers(condition, code, *span, sink);
            check_expr_magic_numbers(true_val, code, *span, sink);
            check_expr_magic_numbers(false_val, code, *span, sink);
        }
        Expr::FuncCall { args, span, .. } => {
            for arg in args {
                check_expr_magic_numbers(arg, code, *span, sink);
            }
        }
        Expr::Concat(exprs) => {
            for e in exprs {
                check_expr_magic_numbers(e, code, context_span, sink);
            }
        }
        Expr::Repeat { expr, span, .. } => {
            check_expr_magic_numbers(expr, code, *span, sink);
        }
        Expr::Index { expr, span, .. } => {
            // Only check the base expression — index positions are structural
            // (e.g. `count[2]` is a bit-select, not a magic number)
            check_expr_magic_numbers(expr, code, *span, sink);
        }
        Expr::Slice { expr, span, .. } => {
            // Only check the base expression — high/low bounds are structural
            // (e.g. `data[7:0]` is a range-select, not a magic number)
            check_expr_magic_numbers(expr, code, *span, sink);
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
        assert!(diags[0].message.contains("0x2A"));
        assert!(diags[0].message.contains("8-bit"));
        assert!(!diags[0].labels.is_empty());
        assert!(diags[0].labels[0].message.contains("0x2A"));
        assert!(!diags[0].help.is_empty());
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
    fn bit_index_no_warning() {
        // `count[2]` — the index literal is structural, not a magic number
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Index {
                expr: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
                index: Box::new(Expr::Literal(LogicVec::from_u64(2, 4))),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        MagicNumber.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn slice_bounds_no_warning() {
        // `data[7:0]` — high/low bounds are structural, not magic numbers
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Slice {
                expr: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
                high: Box::new(Expr::Literal(LogicVec::from_u64(7, 4))),
                low: Box::new(Expr::Literal(LogicVec::from_u64(0, 4))),
                span: Span::DUMMY,
            },
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
        assert!(diags[0].message.contains("0xDEAD"));
        assert!(diags[0].message.contains("16-bit"));
        assert!(!diags[0].help.is_empty());
    }
}
