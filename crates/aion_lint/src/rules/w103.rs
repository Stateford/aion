//! W103: Width mismatch — LHS and RHS of assignment have different bit widths.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Expr, Module, SignalRef, Statement};
use aion_source::Span;

use crate::helpers::{expr_width, signal_ref_width};
use crate::{LintContext, LintRule};

/// Detects assignments where the LHS and RHS have different bit widths.
///
/// This checks both continuous assignments and assignments within processes.
/// Widths come from the shared, overflow-checked helpers
/// [`signal_ref_width`] and [`expr_width`]; when either side's width is
/// unknown the assignment is skipped rather than guessed.
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

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        let checker = WidthChecker {
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

        // Check assignments in processes
        for (_pid, process) in module.processes.iter() {
            checker.check_stmt(&process.body);
        }
    }
}

/// The per-module state W103 needs while walking assignments.
struct WidthChecker<'m, 'c> {
    module: &'m Module,
    design: &'m Design,
    ctx: &'m LintContext<'c>,
    code: DiagnosticCode,
    sink: &'m DiagnosticSink,
}

impl WidthChecker<'_, '_> {
    /// Reports `target = value` if both widths are known and differ.
    fn check_assign(&self, target: &SignalRef, value: &Expr, span: Span) {
        let lhs_width = signal_ref_width(target, self.module, self.design);
        let rhs_width = expr_width(value, self.design);
        if let (Some(lw), Some(rw)) = (lhs_width, rhs_width) {
            if lw != rw {
                let target_desc = self
                    .ctx
                    .signal_ref_name(self.module, target)
                    .map(|n| format!(" in assignment to `{n}`"))
                    .unwrap_or_default();
                self.sink.emit(
                    Diagnostic::warning(
                        self.code,
                        format!("width mismatch{target_desc}: LHS is {lw} bits, RHS is {rw} bits"),
                        span,
                    )
                    .with_label(Label::primary(span, "mismatched widths")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_util::empty_ctx;
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
                kind: aion_ir::AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        assert_eq!(
            diags.len(),
            1,
            "4-bit literal assigned to 8-bit should still fire W103"
        );
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
        WidthMismatch.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        sink.take_all()
    }

    #[test]
    fn ascending_lhs_slice_matching_width_no_warning() {
        // `assign y[0:3] = 4'hF;` on `output [0:7] y` (high < low).
        let (mut module, mut types) = mk_module_with_types();
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
    fn ascending_lhs_slice_mismatch_reports_real_width() {
        let (mut module, mut types) = mk_module_with_types();
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
        assert!(diags[0].message.contains("LHS is 4 bits, RHS is 8 bits"));
    }

    #[test]
    fn ascending_rhs_slice_no_panic() {
        // VHDL `y <= a(0 to 3);` lowers to an Expr::Slice with high < low.
        let (mut module, mut types) = mk_module_with_types();
        let a = add_vec_signal(&mut module, &mut types, 8);
        let y = add_vec_signal(&mut module, &mut types, 4);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: literal_slice(a, 0, 3),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    #[test]
    fn part_select_lowering_no_panic() {
        // `a[0 +: 8]` lowers to Slice { high: 0, low: 8 } (see LINT-4). The
        // width is not meaningful yet, but the rule must not panic.
        let (mut module, mut types) = mk_module_with_types();
        let a = add_vec_signal(&mut module, &mut types, 16);
        let y = add_vec_signal(&mut module, &mut types, 8);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(y),
            value: literal_slice(a, 0, 8),
            span: Span::DUMMY,
        });
        let _ = run_rule(module, types);
    }

    #[test]
    fn huge_replication_no_panic_no_warning() {
        // `assign y = {1000000000{8'hFF}};` overflows u32 -> width unknown.
        let (mut module, mut types) = mk_module_with_types();
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
    fn overflowing_lhs_concat_no_panic_no_warning() {
        let (mut module, mut types) = mk_module_with_types();
        let y = add_vec_signal(&mut module, &mut types, 8);
        let wide = SignalRef::Slice {
            signal: y,
            high: u32::MAX - 1,
            low: 0,
        };
        module.assignments.push(Assignment {
            target: SignalRef::Concat(vec![wide.clone(), wide]),
            value: Expr::Literal(LogicVec::from_u64(0xFF, 8)),
            span: Span::DUMMY,
        });
        assert!(run_rule(module, types).is_empty());
    }

    // ---- Target names in messages (issue #39) ----

    fn run_named(target: impl Fn(SignalId) -> SignalRef) -> Vec<aion_diagnostics::Diagnostic> {
        let interner = aion_common::Interner::new();
        let db = aion_source::SourceDb::new();
        let (mut module, mut types) = mk_module_with_types();
        let ty8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: interner.get_or_intern("result"),
            ty: ty8,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: target(sig_id),
            value: Expr::Literal(LogicVec::from_u64(0x5, 3)),
            span: Span::DUMMY,
        });
        let design = mk_design(module, types);
        let sink = DiagnosticSink::new();
        let ctx = LintContext::new(&interner, &db);
        WidthMismatch.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn message_names_the_target_signal() {
        let diags = run_named(SignalRef::Signal);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "width mismatch in assignment to `result`: LHS is 8 bits, RHS is 3 bits"
        );
    }

    #[test]
    fn message_names_a_slice_target() {
        let diags = run_named(|s| SignalRef::Slice {
            signal: s,
            high: 7,
            low: 4,
        });
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "width mismatch in assignment to `result[7:4]`: LHS is 4 bits, RHS is 3 bits"
        );
    }

    #[test]
    fn constant_target_has_no_name_clause() {
        let diags = run_named(|_| SignalRef::Const(LogicVec::all_zero(8)));
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "width mismatch: LHS is 8 bits, RHS is 3 bits"
        );
    }
}
