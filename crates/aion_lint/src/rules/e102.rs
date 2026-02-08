//! E102: Non-synthesizable — constructs that cannot be synthesized to hardware.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, ProcessKind, Statement};

use crate::LintRule;

/// Detects non-synthesizable constructs in the design.
///
/// This includes:
/// - `Initial` process kind (simulation only)
/// - `Wait`, `Display`, `Finish` statements in non-initial processes
pub struct NonSynthesizable;

impl LintRule for NonSynthesizable {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Error, 102)
    }

    fn name(&self) -> &str {
        "non-synthesizable"
    }

    fn description(&self) -> &str {
        "constructs that cannot be synthesized to hardware"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        for (_pid, process) in module.processes.iter() {
            // Initial processes are simulation-only
            if process.kind == ProcessKind::Initial {
                sink.emit(
                    Diagnostic::error(
                        self.code(),
                        "initial blocks are not synthesizable",
                        process.span,
                    )
                    .with_label(Label::primary(process.span, "simulation-only construct"))
                    .with_help("remove initial block or move to a testbench"),
                );
                continue;
            }

            // Check for non-synthesizable statements in non-initial processes
            check_non_synth_stmts(&process.body, self.code(), sink);
        }
    }
}

fn check_non_synth_stmts(stmt: &Statement, code: DiagnosticCode, sink: &DiagnosticSink) {
    match stmt {
        Statement::Wait { span, .. } => {
            sink.emit(
                Diagnostic::error(code, "wait statements are not synthesizable", *span)
                    .with_label(Label::primary(*span, "simulation-only construct")),
            );
        }
        Statement::Display { span, .. } => {
            sink.emit(
                Diagnostic::error(code, "$display is not synthesizable", *span)
                    .with_label(Label::primary(*span, "simulation-only construct")),
            );
        }
        Statement::Finish { span } => {
            sink.emit(
                Diagnostic::error(code, "$finish is not synthesizable", *span)
                    .with_label(Label::primary(*span, "simulation-only construct")),
            );
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            check_non_synth_stmts(then_body, code, sink);
            if let Some(else_b) = else_body {
                check_non_synth_stmts(else_b, code, sink);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                check_non_synth_stmts(&arm.body, code, sink);
            }
            if let Some(def) = default {
                check_non_synth_stmts(def, code, sink);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                check_non_synth_stmts(s, code, sink);
            }
        }
        Statement::Assign { .. } | Statement::Assertion { .. } | Statement::Nop => {}
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
    fn initial_block_fires() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NonSynthesizable.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("initial"));
    }

    #[test]
    fn wait_in_comb_fires() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Wait {
                duration: None,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NonSynthesizable.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("wait"));
    }

    #[test]
    fn display_in_seq_fires() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Display {
                format: "test".to_string(),
                args: vec![],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NonSynthesizable.check_module(design.modules.get(design.top), &design, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("$display"));
    }

    #[test]
    fn normal_process_no_error() {
        let mut module = mk_module();
        module.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NonSynthesizable.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
