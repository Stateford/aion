//! W107: Truncation — RHS is wider than LHS in assignment.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Expr, Module, SignalRef, Statement};

use crate::helpers::{expr_width, signal_ref_width};
use crate::{LintContext, LintRule};

/// Detects assignments where the RHS is wider than the LHS, causing
/// implicit truncation of high bits.
///
/// Unlike W103 (width-mismatch) which fires on any width difference,
/// this rule specifically flags truncation where data loss occurs.
/// Widths come from the same overflow-checked helpers as W103
/// ([`signal_ref_width`] and [`expr_width`]), so neither rule trusts the
/// elaborator's placeholder result types. W107 is deliberately more
/// conservative on the RHS: bit-selects and slices are treated as unknown
/// width (see `rhs_width`), because the elaborator currently lowers indexed
/// part-selects (`a[b +: w]`) as `Slice { high: b, low: w }`, which is
/// indistinguishable from a real `[b:w]` range in the IR.
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

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        let checker = TruncationChecker {
            module,
            design,
            ctx,
            code: self.code(),
            sink,
        };

        // Check continuous assignments
        for assign in &module.assignments {
            checker.check_assign(&assign.target, &assign.value, assign.span);
        }

        // Check process body assignments
        for (_pid, process) in module.processes.iter() {
            checker.check_stmt(&process.body);
        }
    }
}

/// The per-module state W107 needs while walking assignments.
struct TruncationChecker<'m, 'c> {
    module: &'m Module,
    design: &'m Design,
    ctx: &'m LintContext<'c>,
    code: DiagnosticCode,
    sink: &'m DiagnosticSink,
}

impl TruncationChecker<'_, '_> {
    /// Reports `target = value` if the value is wider than the target.
    fn check_assign(&self, target: &SignalRef, value: &Expr, span: aion_source::Span) {
        let lhs_width = signal_ref_width(target, self.module, self.design);
        let rhs_width = rhs_width(value, self.design);
        if let (Some(lw), Some(rw)) = (lhs_width, rhs_width) {
            if rw > lw {
                let target_desc = self
                    .ctx
                    .signal_ref_name(self.module, target)
                    .map(|n| format!(" `{n}`"))
                    .unwrap_or_default();
                self.sink.emit(
                    Diagnostic::warning(
                        self.code,
                        format!(
                            "truncation: {rw}-bit value assigned to {lw}-bit target{target_desc}, losing {} bits",
                            rw - lw
                        ),
                        span,
                    )
                    .with_label(Label::primary(span, "high bits will be truncated")),
                );
            }
        }
    }

    /// Checks every assignment nested in `stmt`.
    fn check_stmt(&self, stmt: &Statement) {
        match stmt {
            Statement::Assign {
                target,
                value,
                span,
                ..
            } => self.check_assign(target, value, *span),
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                self.check_stmt(then_body);
                if let Some(else_b) = else_body {
                    self.check_stmt(else_b);
                }
            }
            Statement::Case { arms, default, .. } => {
                for arm in arms {
                    self.check_stmt(&arm.body);
                }
                if let Some(def) = default {
                    self.check_stmt(def);
                }
            }
            Statement::Block { stmts, .. } => {
                for s in stmts {
                    self.check_stmt(s);
                }
            }
            _ => {}
        }
    }
}

/// Returns the RHS width W107 is willing to trust, or `None` if unknown.
///
/// Delegates to the shared [`expr_width`] helper, but treats `Expr::Slice` and
/// `Expr::Index` (including inside concatenations and replications) as
/// unknown. The IR cannot yet distinguish a part-select `a[b +: w]` (lowered as
/// `Slice { high: b, low: w }`) from a genuine `a[b:w]` range, so trusting
/// slice bounds here produces false truncation warnings on correct designs
/// such as `assign y = a[0 +: 8];` or byte-lane concatenations. Remove this
/// restriction once the elaborator lowers part-selects to real bounds.
fn rhs_width(expr: &Expr, design: &Design) -> Option<u32> {
    match expr {
        Expr::Slice { .. } | Expr::Index { .. } => None,
        Expr::Concat(exprs) => exprs
            .iter()
            .try_fold(0u32, |total, e| total.checked_add(rhs_width(e, design)?)),
        Expr::Repeat { expr, count, .. } => rhs_width(expr, design)?.checked_mul(*count),
        _ => expr_width(expr, design),
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
        Truncation.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        Truncation.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        Truncation.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    // ---- Issue #38 regressions: panic-proof widths ----

    fn add_vec_signal(module: &mut Module, types: &mut TypeDb, width: u32) -> SignalId {
        let ty = types.intern(Type::BitVec {
            width,
            signed: false,
        });
        let n = module.signals.len() as u32;
        module.signals.alloc(Signal {
            id: SignalId::from_raw(n),
            name: Ident::from_raw(10 + n),
            ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        })
    }

    fn literal_slice(sig: SignalId, high: u64, low: u64) -> Expr {
        Expr::Slice {
            expr: Box::new(Expr::Signal(SignalRef::Signal(sig))),
            high: Box::new(Expr::Literal(LogicVec::from_u64(high, 32))),
            low: Box::new(Expr::Literal(LogicVec::from_u64(low, 32))),
            span: Span::DUMMY,
        }
    }

    fn run_rule(module: Module, types: TypeDb) -> Vec<aion_diagnostics::Diagnostic> {
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        Truncation.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        sink.take_all()
    }

    fn eq_expr(lhs: SignalId, rhs: SignalId, ty: aion_ir::ids::TypeId) -> Expr {
        Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(lhs))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(rhs))),
            ty,
            span: Span::DUMMY,
        }
    }

    #[test]
    fn ascending_lhs_slice_truncation_reports_real_width() {
        // `assign y[0:3] = 8'hFF;` on `output [0:7] y`.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Slice {
                signal: y,
                high: 0,
                low: 3,
            },
            value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
            span: Span::DUMMY,
        });
        let diags = run_rule(module, types);
        assert_eq!(diags.len(), 1);
        assert!(diags[0]
            .message
            .contains("8-bit value assigned to 4-bit target"));
    }

    #[test]
    fn ascending_lhs_slice_no_truncation() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Slice {
                signal: y,
                high: 0,
                low: 3,
            },
            value: Expr::Literal(LogicVec::from_u64(0xF, 4)),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn ascending_and_part_select_rhs_no_panic() {
        // VHDL `a(0 to 3)` and Verilog `a[0 +: 8]` both lower to slices with
        // high < low; neither may panic.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let a = add_vec_signal(&mut module, &mut types, 16);
        let y = add_vec_signal(&mut module, &mut types, 4);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: literal_slice(a, 0, 3),
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: literal_slice(a, 0, 8),
            span: Span::DUMMY,
        });
        let diags = run_rule(module, types);
        // Slices on the RHS are treated as unknown width (part-selects are
        // lowered as `Slice { high: base, low: width }`), so neither fires.
        assert!(diags.is_empty(), "{diags:#?}");
    }

    #[test]
    fn part_select_rhs_no_false_truncation() {
        // `assign y = a[24 +: 8];` lowers to Slice{24, 8} (17 "bits").
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let a = add_vec_signal(&mut module, &mut types, 32);
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: literal_slice(a, 24, 8),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn byte_lane_concat_of_part_selects_no_false_truncation() {
        // `{a[0 +: 8], a[8 +: 8], a[16 +: 8], a[24 +: 8]}` into 32 bits.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let a = add_vec_signal(&mut module, &mut types, 32);
        let y = add_vec_signal(&mut module, &mut types, 32);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: Expr::Concat(vec![
                literal_slice(a, 0, 8),
                literal_slice(a, 8, 8),
                literal_slice(a, 16, 8),
                literal_slice(a, 24, 8),
            ]),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn slice_rhs_into_one_bit_target_no_false_truncation() {
        // Constant-index memory writes (`mem[1] <= a[7:0]`) and unsupported
        // targets lower to 1-bit targets; a slice RHS must not trigger W107.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let a = add_vec_signal(&mut module, &mut types, 8);
        let mem = add_vec_signal(&mut module, &mut types, 32);
        module.assignments.push(Assignment {
            target: SignalRef::Slice {
                signal: mem,
                high: 1,
                low: 1,
            },
            value: literal_slice(a, 7, 0),
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Const(LogicVec::all_zero(1)),
            value: literal_slice(a, 7, 0),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn literal_concat_still_detects_truncation() {
        // Non-slice RHS widths are still trusted, including inside concats.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: Expr::Concat(vec![
                Expr::Literal(LogicVec::from_u64(0xAB, 8)),
                Expr::Literal(LogicVec::from_u64(0xCD, 8)),
            ]),
            span: Span::DUMMY,
        });
        let diags = run_rule(module, types);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("losing 8 bits"));
    }

    #[test]
    fn huge_replication_no_panic_no_warning() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: Expr::Repeat {
                expr: Box::new(Expr::Literal(LogicVec::from_u64(0xFF, 8))),
                count: 1_000_000_000,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn overflowing_rhs_concat_no_panic_no_warning() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 8);
        let big = Expr::Repeat {
            expr: Box::new(Expr::Literal(LogicVec::from_u64(0xF, 4))),
            count: u32::MAX / 4,
            span: Span::DUMMY,
        };
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: Expr::Concat(vec![big.clone(), big]),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn placeholder_type_ignored_regardless_of_port_order() {
        // `assign y = a == b;` - the comparison carries the TypeId(0)
        // placeholder. Whether TypeId(0) happens to be the 8-bit type (a
        // declared first) or the 1-bit type (y declared first), W107 must
        // give the same verdict: no truncation.
        for wide_first in [true, false] {
            let mut module = mk_module();
            let mut types = TypeDb::new();
            let (a, b, y) = if wide_first {
                let a = add_vec_signal(&mut module, &mut types, 8);
                let b = add_vec_signal(&mut module, &mut types, 8);
                let y = add_vec_signal(&mut module, &mut types, 1);
                (a, b, y)
            } else {
                let y = add_vec_signal(&mut module, &mut types, 1);
                let a = add_vec_signal(&mut module, &mut types, 8);
                let b = add_vec_signal(&mut module, &mut types, 8);
                (a, b, y)
            };
            module.assignments.push(Assignment {
                target: SignalRef::Signal(y),
                value: eq_expr(a, b, aion_ir::ids::TypeId::from_raw(0)),
                span: Span::DUMMY,
            });
            let diags = run_rule(module, types);
            assert!(diags.is_empty(), "wide_first={wide_first}: {diags:#?}");
        }
    }

    #[test]
    fn resolved_binary_type_still_detects_truncation() {
        // A non-placeholder result type is trusted: 8-bit sum into 4 bits.
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let y = add_vec_signal(&mut module, &mut types, 4);
        let a = add_vec_signal(&mut module, &mut types, 8);
        let ty8 = module.signals.get(a).ty;
        assert_ne!(ty8, aion_ir::ids::TypeId::from_raw(0));
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: Expr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(Expr::Signal(SignalRef::Signal(a))),
                rhs: Box::new(Expr::Signal(SignalRef::Signal(a))),
                ty: ty8,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });
        let diags = run_rule(module, types);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("losing 4 bits"));
    }

    // ---- Target names in messages (issue #39) ----

    #[test]
    fn message_names_the_target_signal() {
        let interner = aion_common::Interner::new();
        let db = aion_source::SourceDb::new();
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let ty4 = types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: interner.get_or_intern("nibble"),
            ty: ty4,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0xA5, 8)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        let ctx = LintContext::new(&interner, &db);
        Truncation.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "truncation: 8-bit value assigned to 4-bit target `nibble`, losing 4 bits"
        );
    }

    #[test]
    fn constant_target_message_has_no_name() {
        let mut module = mk_module();
        module.assignments.push(Assignment {
            target: SignalRef::Const(LogicVec::all_zero(2)),
            value: Expr::Literal(LogicVec::from_u64(0xA5, 8)),
            span: Span::DUMMY,
        });
        let diags = run_rule(module, TypeDb::new());
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "truncation: 8-bit value assigned to 2-bit target, losing 6 bits"
        );
    }
}
