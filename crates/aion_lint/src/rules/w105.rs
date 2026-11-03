//! W105: Incomplete sensitivity list — combinational process missing signals.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, ProcessKind, Sensitivity, SignalId, SignalKind};

use crate::helpers::collect_read_signals;
use crate::{LintContext, LintRule};

/// Detects combinational processes with explicit `SignalList` sensitivity
/// that are missing signals read in the process body.
///
/// Missing signals in the sensitivity list cause simulation-synthesis
/// mismatches. Processes with `Sensitivity::All` are not checked. Reads of
/// constants and VHDL process variables are ignored: they are not signals,
/// so they can never appear in a sensitivity list.
pub struct IncompleteSensitivity;

/// Returns `true` if `id` is a constant or a VHDL process variable, i.e.
/// storage that does not belong in a sensitivity list.
fn is_non_signal(module: &Module, id: SignalId) -> bool {
    (id.as_raw() as usize) < module.signals.len()
        && matches!(
            module.signals.get(id).kind,
            SignalKind::Const | SignalKind::Variable
        )
}

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

    fn check_module(
        &self,
        module: &Module,
        _design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
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

            // Report once per process, naming every missing signal in a
            // stable (alphabetical) order.
            let mut missing: Vec<&str> = read_signals
                .iter()
                .filter(|id| !sens_set.contains(id) && !is_non_signal(module, **id))
                .map(|id| ctx.signal_name(module, *id))
                .collect();
            if missing.is_empty() {
                continue;
            }
            missing.sort_unstable();
            missing.dedup();
            let list = missing
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let (noun, verb) = if missing.len() == 1 {
                ("signal", "is")
            } else {
                ("signals", "are")
            };
            sink.emit(
                Diagnostic::warning(
                    self.code(),
                    format!("incomplete sensitivity list: {noun} {list} {verb} read but not listed"),
                    process.span,
                )
                .with_label(Label::primary(
                    process.span,
                    format!("{list} {verb} read in the body but missing from the sensitivity list"),
                ))
                .with_help(format!(
                    "add {list} to the sensitivity list, or use `always @(*)` / `always_comb` / `process (all)` to infer it"
                )),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_util::empty_ctx;
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
                kind: aion_ir::AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(vec![a]), // Missing b
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(
            design.modules.get(design.top),
            &design,
            &empty_ctx(),
            &sink,
        );
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
                kind: aion_ir::AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(vec![a, b]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(
            design.modules.get(design.top),
            &design,
            &empty_ctx(),
            &sink,
        );
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
                kind: aion_ir::AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(
            design.modules.get(design.top),
            &design,
            &empty_ctx(),
            &sink,
        );
        assert!(sink.take_all().is_empty());
    }
    #[test]
    fn variables_and_constants_not_required_in_sensitivity() {
        // VHDL: process (a) variable v; constant K; begin y <= a + v + K; end
        let mut module = mk_module();
        let a = SignalId::from_raw(0);
        let v = SignalId::from_raw(1);
        let k = SignalId::from_raw(2);
        for (id, kind) in [
            (a, SignalKind::Wire),
            (v, SignalKind::Variable),
            (k, SignalKind::Const),
        ] {
            module.signals.alloc(Signal {
                id,
                name: Ident::from_raw(10 + id.as_raw()),
                ty: TypeId::from_raw(0),
                kind,
                init: None,
                clock_domain: None,
                span: Span::DUMMY,
            });
        }
        let add = |l: Expr, r: Expr| Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(l),
            rhs: Box::new(r),
            ty: TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let rd = |id| Expr::Signal(SignalRef::Signal(id));
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(3)),
                value: add(add(rd(a), rd(v)), rd(k)),
                kind: aion_ir::AssignKind::NonBlocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(vec![a]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        IncompleteSensitivity.check_module(
            design.modules.get(design.top),
            &design,
            &empty_ctx(),
            &sink,
        );
        assert!(sink.take_all().is_empty());
    }

    // ---- Missing signals are named (issue #39) ----

    /// Builds a combinational process over signals `names` that reads all of
    /// them but lists only `listed` in its sensitivity list.
    fn run_named(names: &[&str], listed: &[usize]) -> Vec<aion_diagnostics::Diagnostic> {
        let interner = aion_common::Interner::new();
        let db = aion_source::SourceDb::new();
        let mut module = mk_module();
        let ids: Vec<SignalId> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                module.signals.alloc(Signal {
                    id: SignalId::from_raw(i as u32),
                    name: interner.get_or_intern(n),
                    ty: TypeId::from_raw(0),
                    kind: SignalKind::Wire,
                    init: None,
                    clock_domain: None,
                    span: Span::DUMMY,
                })
            })
            .collect();
        let out = module.signals.alloc(Signal {
            id: SignalId::from_raw(names.len() as u32),
            name: interner.get_or_intern("y"),
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
                target: SignalRef::Signal(out),
                value: Expr::Concat(
                    ids.iter()
                        .map(|id| Expr::Signal(SignalRef::Signal(*id)))
                        .collect(),
                ),
                kind: aion_ir::AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::SignalList(listed.iter().map(|i| ids[*i]).collect()),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        let ctx = LintContext::new(&interner, &db);
        IncompleteSensitivity.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn single_missing_signal_is_named() {
        let diags = run_named(&["a", "b"], &[0]);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "incomplete sensitivity list: signal `b` is read but not listed"
        );
        assert!(diags[0].help.iter().any(|h| h.contains("add `b`")));
    }

    #[test]
    fn all_missing_signals_are_named_in_order() {
        let diags = run_named(&["sel", "b", "a"], &[]);
        assert_eq!(diags.len(), 1, "reported once per process");
        assert_eq!(
            diags[0].message,
            "incomplete sensitivity list: signals `a`, `b`, `sel` are read but not listed"
        );
    }

    #[test]
    fn complete_named_list_is_silent() {
        assert!(run_named(&["a", "b"], &[0, 1]).is_empty());
    }
}
