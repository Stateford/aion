//! E105: Port mismatch — cell instance connections don't match module ports.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{CellKind, Design, Module};

use crate::helpers::{check_cell_port_match, PortMatchIssue};
use crate::{LintContext, LintRule};

/// Detects cell instances whose connections don't match the instantiated
/// module's port list.
///
/// This checks for extra connections (port name not in module) and
/// missing connections (module port not connected).
pub struct PortMismatch;

impl LintRule for PortMismatch {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Error, 105)
    }

    fn name(&self) -> &str {
        "port-mismatch"
    }

    fn description(&self) -> &str {
        "cell instance connections don't match module ports"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        for (_cid, cell) in module.cells.iter() {
            let target_mod_id = match &cell.kind {
                CellKind::Instance { module, .. } => *module,
                _ => continue,
            };
            // A dangling module ID is an elaboration bug; skip rather than panic.
            if target_mod_id.as_raw() as usize >= design.modules.len() {
                continue;
            }

            let target_module = design.modules.get(target_mod_id);
            let target_name = ctx.name(target_module.name);
            let instance_name = ctx.name(cell.name);
            let issues = check_cell_port_match(cell, target_module);

            for issue in issues {
                match issue {
                    PortMatchIssue::ExtraConnection(conn) => {
                        let port = ctx.name(conn.port_name);
                        sink.emit(
                            Diagnostic::error(
                                self.code(),
                                format!(
                                    "connection to non-existent port `{port}` in instance `{instance_name}` of module `{target_name}`"
                                ),
                                cell.span,
                            )
                            .with_label(Label::primary(
                                cell.span,
                                format!("port `{port}` does not exist on module `{target_name}`"),
                            ))
                            .with_help(format!(
                                "remove the connection or use one of the ports declared by `{target_name}`"
                            )),
                        );
                    }
                    PortMatchIssue::MissingConnection(port_name) => {
                        let port = ctx.name(port_name);
                        sink.emit(
                            Diagnostic::error(
                                self.code(),
                                format!(
                                    "missing port connection: port `{port}` of module `{target_name}` is not connected in instance `{instance_name}`"
                                ),
                                cell.span,
                            )
                            .with_label(Label::primary(
                                cell.span,
                                format!("port `{port}` is not connected"),
                            ))
                            .with_help(format!(
                                "connect port `{port}` of instance `{instance_name}` to a signal"
                            )),
                        );
                    }
                }
            }
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

    fn mk_module_with_id(id: u32) -> Module {
        Module {
            id: ModuleId::from_raw(id),
            name: Ident::from_raw(id),
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

    #[test]
    fn missing_port_fires() {
        // Target module has one port
        let mut target = mk_module_with_id(1);
        target.ports.push(Port {
            id: PortId::from_raw(0),
            name: Ident::from_raw(100),
            direction: PortDirection::Input,
            ty: TypeId::from_raw(0),
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        });

        // Instantiating module has cell with no connections
        let mut parent = mk_module_with_id(0);
        parent.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: vec![],
            },
            connections: vec![], // Missing the port connection
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        let top = modules.alloc(parent);
        modules.alloc(target);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };

        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("missing port"));
    }

    #[test]
    fn extra_port_fires() {
        // Target module has no ports
        let target = mk_module_with_id(1);

        // Instantiating module has cell with a connection
        let mut parent = mk_module_with_id(0);
        parent.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: vec![],
            },
            connections: vec![Connection {
                port_name: Ident::from_raw(100),
                direction: PortDirection::Input,
                signal: SignalRef::Signal(SignalId::from_raw(0)),
            }],
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        let top = modules.alloc(parent);
        modules.alloc(target);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };

        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("non-existent"));
    }

    #[test]
    fn matching_ports_no_error() {
        let mut target = mk_module_with_id(1);
        let port_name = Ident::from_raw(100);
        target.ports.push(Port {
            id: PortId::from_raw(0),
            name: port_name,
            direction: PortDirection::Input,
            ty: TypeId::from_raw(0),
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        });

        let mut parent = mk_module_with_id(0);
        parent.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: vec![],
            },
            connections: vec![Connection {
                port_name,
                direction: PortDirection::Input,
                signal: SignalRef::Signal(SignalId::from_raw(0)),
            }],
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        let top = modules.alloc(parent);
        modules.alloc(target);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };

        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    #[test]
    fn non_instance_cell_skipped() {
        let mut module = mk_module_with_id(0);
        module.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::And { width: 1 },
            connections: vec![],
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        let top = modules.alloc(module);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };
        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }

    // ---- Real names in messages (issue #39, LINT-17) ----

    use aion_common::Interner;
    use aion_source::SourceDb;

    /// Builds `top` instantiating `sub` (ports `a`, `b`) as `u0` with
    /// connections to the given port names, and returns E105's diagnostics.
    fn run_named(connected: &[&str]) -> Vec<Diagnostic> {
        let interner = Interner::new();
        let mut sub = mk_module_with_id(1);
        sub.name = interner.get_or_intern("sub");
        for (i, p) in ["a", "b"].iter().enumerate() {
            sub.ports.push(Port {
                id: PortId::from_raw(i as u32),
                name: interner.get_or_intern(p),
                direction: PortDirection::Input,
                ty: TypeId::from_raw(0),
                signal: SignalId::from_raw(i as u32),
                span: Span::DUMMY,
            });
        }
        let mut top = mk_module_with_id(0);
        top.name = interner.get_or_intern("top");
        top.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: interner.get_or_intern("u0"),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: vec![],
            },
            connections: connected
                .iter()
                .map(|p| Connection {
                    port_name: interner.get_or_intern(p),
                    direction: PortDirection::Input,
                    signal: SignalRef::Const(aion_common::LogicVec::all_zero(1)),
                })
                .collect(),
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        let top_id = modules.alloc(top);
        modules.alloc(sub);
        let design = Design {
            modules,
            top: top_id,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };
        let db = SourceDb::new();
        let ctx = LintContext::new(&interner, &db);
        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top_id), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn missing_port_message_names_port_module_and_instance() {
        // Repro from issue #39 printed "port '3' is not connected".
        let diags = run_named(&["a"]);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "missing port connection: port `b` of module `sub` is not connected in instance `u0`"
        );
        assert_eq!(diags[0].labels[0].message, "port `b` is not connected");
    }

    #[test]
    fn extra_port_message_names_port_and_module() {
        let diags = run_named(&["a", "b", "c"]);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "connection to non-existent port `c` in instance `u0` of module `sub`"
        );
        assert_eq!(
            diags[0].labels[0].message,
            "port `c` does not exist on module `sub`"
        );
    }

    #[test]
    fn unresolvable_names_never_print_raw_ids() {
        // `missing_port_fires` uses raw IDs 100/200 unknown to the interner.
        let mut target = mk_module_with_id(1);
        target.ports.push(Port {
            id: PortId::from_raw(0),
            name: Ident::from_raw(100),
            direction: PortDirection::Input,
            ty: TypeId::from_raw(0),
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        });
        let mut parent = mk_module_with_id(0);
        parent.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: vec![],
            },
            connections: vec![],
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        let top = modules.alloc(parent);
        modules.alloc(target);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };
        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1);
        assert!(!diags[0].message.contains("100"), "{}", diags[0].message);
        assert!(diags[0].message.contains(crate::UNKNOWN_NAME));
    }

    #[test]
    fn dangling_instance_target_is_skipped_without_panic() {
        let mut parent = mk_module_with_id(0);
        parent.cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(200),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(42),
                params: vec![],
            },
            connections: vec![],
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        let top = modules.alloc(parent);
        let design = Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        };
        let sink = DiagnosticSink::new();
        PortMismatch.check_module(design.modules.get(top), &design, &empty_ctx(), &sink);
        assert!(sink.take_all().is_empty());
    }
}
