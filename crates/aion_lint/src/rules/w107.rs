//! W107: Truncation — RHS is wider than LHS in assignment.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Expr, Module, SignalRef, Statement};

use crate::LintRule;

/// Detects assignments where the RHS is wider than the LHS, causing
/// implicit truncation of high bits.
///
/// Unlike W103 (width-mismatch) which fires on any width difference,
/// this rule specifically flags truncation where data loss occurs.
pub struct Truncation;

impl LintRule for Truncation {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 107)
    }

    fn name(&self) -> &str {
        "truncation"
    }

    fn description(&self) -> &str {
        "RHS wider than LHS in assignment causes truncation"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, design: &Design, sink: &DiagnosticSink) {
        // Check continuous assignments
        for assign in &module.assignments {
            check_truncation(
                &assign.target,
                &assign.value,
                assign.span,
                module,
                design,
                self.code(),
                sink,
            );
        }

        // Check process body assignments
        for (_pid, process) in module.processes.iter() {
            check_stmt_truncation(&process.body, module, design, self.code(), sink);
        }
    }
}

fn signal_ref_width(sref: &SignalRef, module: &Module, design: &Design) -> Option<u32> {
    match sref {
        SignalRef::Signal(id) => {
            let signal = module.signals.get(*id);
            design.types.bit_width(signal.ty)
        }
        SignalRef::Slice { high, low, .. } => Some(high - low + 1),
        SignalRef::Concat(refs) => {
            let mut total = 0u32;
            for r in refs {
                total += signal_ref_width(r, module, design)?;
            }
            Some(total)
        }
        SignalRef::Const(lv) => Some(lv.width()),
    }
}

fn expr_width(expr: &Expr, design: &Design) -> Option<u32> {
    match expr {
        Expr::Literal(lv) => Some(lv.width()),
        Expr::Unary { ty, .. }
        | Expr::Binary { ty, .. }
        | Expr::Ternary { ty, .. }
        | Expr::FuncCall { ty, .. } => design.types.bit_width(*ty),
        Expr::Concat(exprs) => {
            let mut total = 0u32;
            for e in exprs {
                total += expr_width(e, design)?;
            }
            Some(total)
        }
        Expr::Repeat { expr, count, .. } => expr_width(expr, design).map(|w| w * count),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_truncation(
    target: &SignalRef,
    value: &Expr,
    span: aion_source::Span,
    module: &Module,
    design: &Design,
    code: DiagnosticCode,
    sink: &DiagnosticSink,
) {
    let lhs_width = signal_ref_width(target, module, design);
    let rhs_width = expr_width(value, design);
    if let (Some(lw), Some(rw)) = (lhs_width, rhs_width) {
        if rw > lw {
            sink.emit(
                Diagnostic::warning(
                    code,
                    format!(
                        "truncation: {rw}-bit value assigned to {lw}-bit target, losing {} bits",
                        rw - lw
                    ),
                    span,
                )
                .with_label(Label::primary(span, "high bits will be truncated")),
            );
        }
    }
}

fn check_stmt_truncation(
    stmt: &Statement,
    module: &Module,
    design: &Design,
    code: DiagnosticCode,
    sink: &DiagnosticSink,
) {
    match stmt {
        Statement::Assign {
            target,
            value,
            span,
            ..
        } => {
            check_truncation(target, value, *span, module, design, code, sink);
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            check_stmt_truncation(then_body, module, design, code, sink);
            if let Some(else_b) = else_body {
                check_stmt_truncation(else_b, module, design, code, sink);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                check_stmt_truncation(&arm.body, module, design, code, sink);
            }
            if let Some(def) = default {
                check_stmt_truncation(def, module, design, code, sink);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                check_stmt_truncation(s, module, design, code, sink);
            }
        }
        _ => {}
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

    fn mk_design(module: Module, types: TypeDb) -> Design {
        let mut modules = Arena::new();
        let top = modules.alloc(module);
        Design {
            modules,
            top,
            types,
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn truncation_fires() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let ty4 = types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: ty4,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        Truncation.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("truncation"));
        assert!(diags[0].message.contains("losing 4 bits"));
    }

    #[test]
    fn no_truncation_same_width() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let ty8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: ty8,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        Truncation.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn no_truncation_rhs_narrower() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let ty8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: ty8,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0xF, 4)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        Truncation.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
