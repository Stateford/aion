//! E105: Port mismatch — cell instance connections don't match module ports.

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{CellKind, Design, Module};

use crate::helpers::{check_cell_port_match, PortMatchIssue};
use crate::LintRule;

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

    fn check_module(&self, module: &Module, design: &Design, sink: &DiagnosticSink) {
        for (_cid, cell) in module.cells.iter() {
            let target_mod_id = match &cell.kind {
                CellKind::Instance { module, .. } => *module,
                _ => continue,
            };

            let target_module = design.modules.get(target_mod_id);
            let issues = check_cell_port_match(cell, target_module);

            for issue in issues {
                match issue {
                    PortMatchIssue::ExtraConnection(conn) => {
                        sink.emit(
                            Diagnostic::error(
                                self.code(),
                                "connection to non-existent port",
                                cell.span,
                            )
                            .with_label(Label::primary(
                                cell.span,
                                format!(
                                    "port '{}' does not exist on target module",
                                    conn.port_name.as_raw()
                                ),
                            )),
                        );
                    }
                    PortMatchIssue::MissingConnection(port_name) => {
                        sink.emit(
                            Diagnostic::error(self.code(), "missing port connection", cell.span)
                                .with_label(Label::primary(
                                    cell.span,
                                    format!("port '{}' is not connected", port_name.as_raw()),
                                ))
                                .with_help("connect all ports of the instantiated module"),
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
        PortMismatch.check_module(design.modules.get(top), &design, &sink);
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
        PortMismatch.check_module(design.modules.get(top), &design, &sink);
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
        PortMismatch.check_module(design.modules.get(top), &design, &sink);
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
        PortMismatch.check_module(design.modules.get(top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
