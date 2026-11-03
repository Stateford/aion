//! W101: Unused signal — signal is declared but never read.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, SignalKind};

use crate::helpers::{dynamic_index_reads, is_signal_read_in_module};
use crate::{LintContext, LintRule};

/// Detects signals that are declared but never read in any expression,
/// assignment RHS, sensitivity list, or cell connection input.
///
/// Signals whose name starts with `_` are conventionally unused and are not
/// reported; neither are signals of kind `Port` or `Const`. A signal that only
/// appears inside the index of a dynamically indexed assignment target
/// (`mem[addr] <= d`) counts as read, even though AionIR keeps no record of
/// that index (see the source-text fallback in `dynamic_index_reads`).
pub struct UnusedSignal;

impl LintRule for UnusedSignal {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Warning, 101)
    }

    fn name(&self) -> &str {
        "unused-signal"
    }

    fn description(&self) -> &str {
        "signal is declared but never read"
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
        // Computed lazily: only needed once an unread candidate is found.
        let mut index_reads = None;
        for (sig_id, signal) in module.signals.iter() {
            // Skip ports and constants
            if signal.kind == SignalKind::Port || signal.kind == SignalKind::Const {
                continue;
            }

            let name = ctx.name(signal.name);
            // Scoped names (`proc.var`) are judged by their last segment.
            let base = name.rsplit('.').next().unwrap_or(name);
            if base.starts_with('_') {
                continue;
            }

            if is_signal_read_in_module(module, sig_id) {
                continue;
            }

            let index_reads = index_reads.get_or_insert_with(|| dynamic_index_reads(module, ctx));
            if index_reads.contains(&base.to_ascii_lowercase()) {
                continue;
            }

            sink.emit(
                Diagnostic::warning(self.code(), format!("unused signal `{name}`"), signal.span)
                    .with_label(Label::primary(
                        signal.span,
                        format!("`{name}` is never read"),
                    ))
                    .with_help(format!(
                        "remove `{name}`, or rename it to `_{base}` to mark it as intentionally unused"
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
    fn unused_signal_fires() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagnosticCode::new(Category::Warning, 101));
    }

    #[test]
    fn used_signal_no_warning() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let target = module.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(11),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(target),
            value: Expr::Signal(SignalRef::Signal(sig_id)),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        // sig_id is read, target is unused but also a wire with no reads
        // We expect 1 warning for target (it's written but never read)
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn port_signal_skipped() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn const_signal_skipped() {
        let mut module = mk_module();
        module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(10),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Const,
            init: Some(ConstValue::Int(42)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        UnusedSignal.check_module(design.modules.get(design.top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    // ---- Names, `_` exemption, sensitivity lists, dynamic indices (issue #39) ----

    use aion_common::Interner;
    use aion_source::SourceDb;

    fn add_named(module: &mut Module, interner: &Interner, name: &str) -> SignalId {
        let id = SignalId::from_raw(module.signals.len() as u32);
        module.signals.alloc(Signal {
            id,
            name: interner.get_or_intern(name),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        })
    }

    fn run_with(module: Module, interner: &Interner, db: &SourceDb) -> Vec<Diagnostic> {
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        let ctx = LintContext::new(interner, db);
        UnusedSignal.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn message_names_the_signal() {
        let interner = Interner::new();
        let mut module = mk_module();
        add_named(&mut module, &interner, "spare");
        let diags = run_with(module, &interner, &SourceDb::new());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "unused signal `spare`");
        assert!(diags[0].help.iter().any(|h| h.contains("`_spare`")));
        assert!(!diags[0].message.chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn underscore_prefix_is_exempt() {
        // Repro from issue #39: `wire _unused;` must not warn.
        let interner = Interner::new();
        let mut module = mk_module();
        add_named(&mut module, &interner, "_unused");
        // VHDL process variables are scoped `proc.name`.
        add_named(&mut module, &interner, "p_main._tmp");
        // Not exempt: underscore inside the name only.
        add_named(&mut module, &interner, "not_exempt");
        let diags = run_with(module, &interner, &SourceDb::new());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].message, "unused signal `not_exempt`");
    }

    #[test]
    fn clock_used_only_in_edge_list_is_read() {
        // Repro from issue #39:
        //   wire gclk; assign gclk = clk & en; always @(posedge gclk) q <= d;
        let interner = Interner::new();
        let mut module = mk_module();
        let gclk = add_named(&mut module, &interner, "gclk");
        let q = add_named(&mut module, &interner, "q");
        let d = add_named(&mut module, &interner, "d");
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(q),
                value: Expr::Signal(SignalRef::Signal(d)),
                kind: AssignKind::NonBlocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: gclk,
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });
        let diags = run_with(module, &interner, &SourceDb::new());
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(msgs, vec!["unused signal `q`"]);
    }

    #[test]
    fn dynamic_index_of_write_counts_as_read() {
        // `mem[addr] <= d;` is lowered to a whole-signal target on `mem`.
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "mem[addr] <= d;";
        let fid = db.add_source("t.v", text.to_string());
        let mut module = mk_module();
        let mem = add_named(&mut module, &interner, "mem");
        add_named(&mut module, &interner, "addr");
        let d = add_named(&mut module, &interner, "d");
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(mem),
                value: Expr::Signal(SignalRef::Signal(d)),
                kind: AssignKind::NonBlocking,
                span: Span::new(fid, 0, text.len() as u32),
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let diags = run_with(module, &interner, &db);
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(msgs, vec!["unused signal `mem`"], "{diags:#?}");
    }
}
