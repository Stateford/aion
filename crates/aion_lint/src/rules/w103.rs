//! W103: Width mismatch — LHS and RHS of assignment have different bit widths.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::ids::TypeId;
use aion_ir::{Design, Expr, Module, SignalRef, Statement};

use crate::LintRule;

/// Detects assignments where the LHS and RHS have different bit widths.
///
/// This checks both continuous assignments and assignments within processes.
/// Width information is obtained from the `TypeDb`.
pub struct WidthMismatch;

impl LintRule for WidthMismatch {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 103)
    }

    fn name(&self) -> &str {
        "width-mismatch"
    }

    fn description(&self) -> &str {
        "LHS and RHS of assignment have different bit widths"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, design: &Design, sink: &DiagnosticSink) {
        // Check continuous assignments
        for assign in &module.assignments {
            let lhs_width = signal_ref_width(&assign.target, module, design);
            let rhs_width = expr_width(&assign.value, design);
            if let (Some(lw), Some(rw)) = (lhs_width, rhs_width) {
                if lw != rw {
                    sink.emit(
                        Diagnostic::warning(
                            self.code(),
                            format!("width mismatch: LHS is {lw} bits, RHS is {rw} bits"),
                            assign.span,
                        )
                        .with_label(Label::primary(assign.span, "mismatched widths")),
                    );
                }
            }
        }

        // Check assignments in processes
        for (_pid, process) in module.processes.iter() {
            check_stmt_widths(&process.body, module, design, self.code(), sink);
        }
    }
}

/// Returns the bit width of a signal reference, if known.
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

/// Returns the bit width of an expression, if known from its type annotation.
///
/// Returns `None` for context-dependent widths such as VHDL `(others => '0')`
/// aggregates (which lower to 1-bit all-zero/all-one literals) and small integer
/// literals used in arithmetic expressions.
fn expr_width(expr: &Expr, design: &Design) -> Option<u32> {
    match expr {
        Expr::Signal(sref) => match sref {
            SignalRef::Slice { high, low, .. } => Some(high - low + 1),
            SignalRef::Const(lv) => Some(lv.width()),
            _ => None, // Would need module context to resolve signal types
        },
        Expr::Literal(lv) => {
            // 1-bit all-zero or all-one literals are context-dependent in VHDL:
            // they represent `(others => '0')` or `(others => '1')` aggregates
            // whose actual width matches the assignment target.
            if lv.width() == 1 && (lv.is_all_zero() || lv.is_all_one()) {
                return None;
            }
            Some(lv.width())
        }
        Expr::Unary { ty, .. }
        | Expr::Binary { ty, .. }
        | Expr::Ternary { ty, .. }
        | Expr::FuncCall { ty, .. } => {
            // TypeId::from_raw(0) is used as a placeholder for unresolved types
            // throughout elaboration. Since this ID may coincidentally alias with a
            // real type (e.g., std_logic → Bit → 1 bit), treat it as unknown to
            // avoid false width-mismatch warnings.
            if *ty == TypeId::from_raw(0) {
                return None;
            }
            design.types.bit_width(*ty)
        }
        Expr::Concat(exprs) => {
            let mut total = 0u32;
            for e in exprs {
                total += expr_width(e, design)?;
            }
            Some(total)
        }
        Expr::Repeat { expr, count, .. } => expr_width(expr, design).map(|w| w * count),
        Expr::Index { .. } => Some(1), // Single-bit index
        Expr::Slice { high, low, .. } => {
            // If high/low are literals, we can compute width
            if let (Expr::Literal(h), Expr::Literal(l)) = (high.as_ref(), low.as_ref()) {
                let hv = h.to_u64()?;
                let lv = l.to_u64()?;
                Some((hv - lv + 1) as u32)
            } else {
                None
            }
        }
    }
}

fn check_stmt_widths(
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
            let lhs_width = signal_ref_width(target, module, design);
            let rhs_width = expr_width(value, design);
            if let (Some(lw), Some(rw)) = (lhs_width, rhs_width) {
                if lw != rw {
                    sink.emit(
                        Diagnostic::warning(
                            code,
                            format!("width mismatch: LHS is {lw} bits, RHS is {rw} bits"),
                            *span,
                        )
                        .with_label(Label::primary(*span, "mismatched widths")),
                    );
                }
            }
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            check_stmt_widths(then_body, module, design, code, sink);
            if let Some(else_b) = else_body {
                check_stmt_widths(else_b, module, design, code, sink);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                check_stmt_widths(&arm.body, module, design, code, sink);
            }
            if let Some(def) = default {
                check_stmt_widths(def, module, design, code, sink);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                check_stmt_widths(s, module, design, code, sink);
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

    fn mk_module_with_types() -> (Module, TypeDb) {
        let module = Module {
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
        };
        let types = TypeDb::new();
        (module, types)
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
    fn width_mismatch_fires() {
        let (mut module, mut types) = mk_module_with_types();
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
        // RHS is a 4-bit literal
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0xF, 4)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("8 bits"));
        assert!(diags[0].message.contains("4 bits"));
    }

    #[test]
    fn matching_widths_no_warning() {
        let (mut module, mut types) = mk_module_with_types();
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn width_mismatch_in_process() {
        let (mut module, mut types) = mk_module_with_types();
        let ty4 = types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: ty4,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(sig_id),
                value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn others_zero_no_width_mismatch() {
        // (others => '0') lowers to LogicVec::all_zero(1). When assigned to an
        // 8-bit signal, W103 should NOT fire because the width is context-dependent.
        let (mut module, mut types) = mk_module_with_types();
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
        // RHS is 1-bit all-zero (represents (others => '0'))
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::all_zero(1)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        assert!(
            sink.take_all().is_empty(),
            "1-bit all-zero should not trigger W103 (context-dependent width)"
        );
    }

    #[test]
    fn others_one_no_width_mismatch() {
        // (others => '1') lowers to LogicVec::all_one(1).
        let (mut module, mut types) = mk_module_with_types();
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
            value: Expr::Literal(LogicVec::all_one(1)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        assert!(
            sink.take_all().is_empty(),
            "1-bit all-one should not trigger W103 (context-dependent width)"
        );
    }

    #[test]
    fn explicit_multibit_literal_still_fires() {
        // A 4-bit literal (not 1-bit context-dependent) should still trigger W103
        let (mut module, mut types) = mk_module_with_types();
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(
            diags.len(),
            1,
            "4-bit literal assigned to 8-bit should still fire W103"
        );
    }
}
