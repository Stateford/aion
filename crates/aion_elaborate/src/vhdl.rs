//! VHDL-2008 entity+architecture elaboration.
//!
//! Transforms a parsed entity and architecture pair into an IR
//! [`Module`](aion_ir::module::Module), handling generics, ports, architecture
//! signals, processes, concurrent assignments, and component instantiations.

use std::collections::HashMap;

use aion_common::{ContentHash, Ident};
use aion_ir::arena::Arena;
use aion_ir::cell::{Cell, CellKind, Connection};
use aion_ir::ids::{CellId, ModuleId, ProcessId, SignalId, TypeId};
use aion_ir::module::{Assignment, Module, Parameter};
use aion_ir::port::{Port, PortDirection};
use aion_ir::process::{Edge, EdgeSensitivity, Process, ProcessKind, Sensitivity};
use aion_ir::signal::{Signal, SignalKind};
use aion_ir::ConstValue;
use aion_vhdl_parser::ast::{self as vhdl_ast, PortMode};

use crate::const_eval::{self, ConstEnv};
use crate::context::ElaborationContext;
use crate::errors;
use crate::expr::{lower_vhdl_expr, lower_vhdl_to_signal_ref, SignalEnv};
use crate::registry::ModuleEntry;
use crate::stmt::lower_vhdl_stmt;
use crate::types;

/// Elaborates a VHDL entity+architecture pair into an IR module.
pub fn elaborate_vhdl_entity(
    entity: &vhdl_ast::EntityDecl,
    arch: &vhdl_ast::ArchitectureDecl,
    generic_overrides: &[(Ident, ConstValue)],
    ctx: &mut ElaborationContext<'_>,
) -> ModuleId {
    let mut const_env = ConstEnv::new();
    let mut ir_params = Vec::new();
    apply_vhdl_generics(
        entity,
        generic_overrides,
        &mut const_env,
        &mut ir_params,
        ctx,
    );

    let mut signals: Arena<SignalId, Signal> = Arena::new();
    let mut sig_env = SignalEnv::new();
    let mut ports = Vec::new();

    elaborate_vhdl_ports(
        entity,
        &const_env,
        &mut signals,
        &mut sig_env,
        &mut ports,
        ctx,
    );

    // Architecture declarations
    for decl in &arch.decls {
        elaborate_vhdl_decl(decl, &const_env, &mut signals, &mut sig_env, ctx);
    }

    let mut cells: Arena<CellId, Cell> = Arena::new();
    let mut processes: Arena<ProcessId, Process> = Arena::new();
    let mut assignments = Vec::new();

    for stmt in &arch.stmts {
        elaborate_vhdl_concurrent(
            stmt,
            &const_env,
            &mut signals,
            &mut sig_env,
            &mut cells,
            &mut processes,
            &mut assignments,
            ctx,
        );
    }

    let content_hash = ContentHash::from_bytes(
        &format!(
            "{}:{}:{}",
            ctx.interner.resolve(entity.name),
            ctx.interner.resolve(arch.name),
            generic_overrides
                .iter()
                .map(|(k, v)| format!("{}={:?}", ctx.interner.resolve(*k), v))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes(),
    );

    let module = Module {
        id: ModuleId::from_raw(0),
        name: entity.name,
        span: entity.span,
        params: ir_params,
        ports,
        signals,
        cells,
        processes,
        assignments,
        clock_domains: Vec::new(),
        content_hash,
    };

    let mid = ctx.design.modules.alloc(module);
    ctx.design.source_map.insert_module(mid, entity.span);
    mid
}

/// Applies entity generics and overrides.
fn apply_vhdl_generics(
    entity: &vhdl_ast::EntityDecl,
    overrides: &[(Ident, ConstValue)],
    const_env: &mut ConstEnv,
    ir_params: &mut Vec<Parameter>,
    ctx: &mut ElaborationContext<'_>,
) {
    let override_map: HashMap<_, _> = overrides.iter().cloned().collect();

    if let Some(ref generics) = entity.generics {
        for gen in &generics.decls {
            for &name in &gen.names {
                let value = if let Some(ov) = override_map.get(&name) {
                    ov.clone()
                } else if let Some(ref default) = gen.default {
                    const_eval::eval_vhdl_expr(
                        default,
                        ctx.source_db,
                        ctx.interner,
                        const_env,
                        ctx.sink,
                    )
                    .unwrap_or(ConstValue::Int(0))
                } else {
                    ConstValue::Int(0)
                };
                const_env.insert(name, value.clone());
                ir_params.push(Parameter {
                    name,
                    ty: TypeId::from_raw(0),
                    value,
                    span: gen.span,
                });
            }
        }
    }
}

/// Elaborates the port list of a VHDL entity.
fn elaborate_vhdl_ports(
    entity: &vhdl_ast::EntityDecl,
    const_env: &ConstEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    ports: &mut Vec<Port>,
    ctx: &mut ElaborationContext<'_>,
) {
    if let Some(ref port_list) = entity.ports {
        for iface in &port_list.decls {
            let dir = match iface.mode {
                Some(PortMode::In) | None => PortDirection::Input,
                Some(PortMode::Out) | Some(PortMode::Buffer) => PortDirection::Output,
                Some(PortMode::Inout) | Some(PortMode::Linkage) => PortDirection::InOut,
            };
            let ty = types::resolve_vhdl_type(
                &iface.ty,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            let kind = SignalKind::Port;

            for &name in &iface.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name,
                    ty,
                    kind,
                    init: None,
                    clock_domain: None,
                    span: iface.span,
                });
                sig_env.insert(name, sid);
                let pid = ctx.alloc_port_id();
                ports.push(Port {
                    id: pid,
                    name,
                    direction: dir,
                    ty,
                    signal: sid,
                    span: iface.span,
                });
            }
        }
    }
}

/// Elaborates a VHDL architecture declaration (signal, constant, etc.).
fn elaborate_vhdl_decl(
    decl: &vhdl_ast::Declaration,
    const_env: &ConstEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    ctx: &mut ElaborationContext<'_>,
) {
    match decl {
        vhdl_ast::Declaration::Signal(sd) => {
            let ty = types::resolve_vhdl_type(
                &sd.ty,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for &name in &sd.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name,
                    ty,
                    kind: SignalKind::Wire,
                    init: None,
                    clock_domain: None,
                    span: sd.span,
                });
                sig_env.insert(name, sid);
            }
        }
        vhdl_ast::Declaration::Constant(cd) => {
            let ty = types::resolve_vhdl_type(
                &cd.ty,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for &name in &cd.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name,
                    ty,
                    kind: SignalKind::Const,
                    init: None,
                    clock_domain: None,
                    span: cd.span,
                });
                sig_env.insert(name, sid);
            }
        }
        vhdl_ast::Declaration::Variable(vd) => {
            let ty = types::resolve_vhdl_type(
                &vd.ty,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for &name in &vd.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name,
                    ty,
                    kind: SignalKind::Reg,
                    init: None,
                    clock_domain: None,
                    span: vd.span,
                });
                sig_env.insert(name, sid);
            }
        }
        _ => {
            // Other declarations (type, component, etc.) not handled in Phase 0
        }
    }
}

/// Elaborates a VHDL concurrent statement.
#[allow(clippy::too_many_arguments)]
fn elaborate_vhdl_concurrent(
    stmt: &vhdl_ast::ConcurrentStatement,
    _const_env: &ConstEnv,
    _signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    processes: &mut Arena<ProcessId, Process>,
    assignments: &mut Vec<Assignment>,
    ctx: &mut ElaborationContext<'_>,
) {
    match stmt {
        vhdl_ast::ConcurrentStatement::Process(ps) => {
            // Detect sequential processes by scanning for rising_edge/falling_edge
            let (kind, sensitivity) =
                detect_vhdl_process_kind(&ps.sensitivity, &ps.stmts, sig_env, ctx.interner);
            let ir_stmts: Vec<_> = ps
                .stmts
                .iter()
                .map(|s| lower_vhdl_stmt(s, sig_env, ctx.source_db, ctx.interner, ctx.sink))
                .collect();
            let body = if ir_stmts.len() == 1 {
                ir_stmts.into_iter().next().unwrap()
            } else {
                aion_ir::stmt::Statement::Block {
                    stmts: ir_stmts,
                    span: ps.span,
                }
            };
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: ps.label,
                kind,
                body,
                sensitivity,
                span: ps.span,
            });
        }
        vhdl_ast::ConcurrentStatement::SignalAssignment(sa) => {
            let target = lower_vhdl_to_signal_ref(&sa.target, sig_env, ctx.interner, ctx.sink);
            let value = if let Some(wf) = sa.waveforms.first() {
                lower_vhdl_expr(&wf.value, sig_env, ctx.source_db, ctx.interner, ctx.sink)
            } else {
                aion_ir::expr::Expr::Literal(aion_common::LogicVec::all_zero(1))
            };
            assignments.push(Assignment {
                target,
                value,
                span: sa.span,
            });
        }
        vhdl_ast::ConcurrentStatement::ComponentInstantiation(ci) => {
            elaborate_vhdl_component(ci, sig_env, cells, ctx);
        }
        vhdl_ast::ConcurrentStatement::ForGenerate(_)
        | vhdl_ast::ConcurrentStatement::IfGenerate(_) => {
            // Generate statements: not fully elaborated in Phase 0
        }
        vhdl_ast::ConcurrentStatement::Assert(_) => {}
        vhdl_ast::ConcurrentStatement::Error(_) => {}
    }
}

/// Maps VHDL sensitivity list to IR Sensitivity.
fn map_vhdl_sensitivity(sens: &vhdl_ast::SensitivityList, sig_env: &SignalEnv) -> Sensitivity {
    match sens {
        vhdl_ast::SensitivityList::All => Sensitivity::All,
        vhdl_ast::SensitivityList::None => Sensitivity::All,
        vhdl_ast::SensitivityList::List(names) => {
            let sigs: Vec<_> = names
                .iter()
                .filter_map(|sn| {
                    let name = sn.parts.last()?;
                    sig_env.get(name).copied()
                })
                .collect();
            if sigs.is_empty() {
                Sensitivity::All
            } else {
                Sensitivity::SignalList(sigs)
            }
        }
    }
}

/// Determines the process kind and sensitivity for a VHDL process.
///
/// Scans the process body statements for `rising_edge` / `falling_edge` calls.
/// When found, returns `ProcessKind::Sequential` with an `EdgeList` sensitivity,
/// matching the approach used for SystemVerilog `always_ff` blocks.
fn detect_vhdl_process_kind(
    sens_list: &vhdl_ast::SensitivityList,
    stmts: &[vhdl_ast::SequentialStatement],
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
) -> (ProcessKind, Sensitivity) {
    // Scan statements for rising_edge/falling_edge calls
    let mut edges = Vec::new();
    for stmt in stmts {
        collect_edge_calls(stmt, sig_env, interner, &mut edges);
    }

    if !edges.is_empty() {
        // Sequential process — build EdgeList from detected edges
        // Also add any other sensitivity list signals as async reset candidates
        if let vhdl_ast::SensitivityList::List(names) = sens_list {
            for sn in names {
                if let Some(&sig_name) = sn.parts.last() {
                    if let Some(&sid) = sig_env.get(&sig_name) {
                        // Add signals not already in the edge list as async resets (negedge)
                        let already_listed = edges.iter().any(|e| e.signal == sid);
                        if !already_listed {
                            edges.push(EdgeSensitivity {
                                signal: sid,
                                edge: Edge::Both,
                            });
                        }
                    }
                }
            }
        }
        (ProcessKind::Sequential, Sensitivity::EdgeList(edges))
    } else {
        // No edge calls found — use original sensitivity mapping
        let sensitivity = map_vhdl_sensitivity(sens_list, sig_env);
        (ProcessKind::Combinational, sensitivity)
    }
}

/// Recursively scans VHDL sequential statements for `rising_edge` / `falling_edge` calls.
fn collect_edge_calls(
    stmt: &vhdl_ast::SequentialStatement,
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
    edges: &mut Vec<EdgeSensitivity>,
) {
    match stmt {
        vhdl_ast::SequentialStatement::If(if_stmt) => {
            collect_edge_calls_expr(&if_stmt.condition, sig_env, interner, edges);
            for s in &if_stmt.then_stmts {
                collect_edge_calls(s, sig_env, interner, edges);
            }
            for elsif in &if_stmt.elsif_branches {
                collect_edge_calls_expr(&elsif.condition, sig_env, interner, edges);
                for s in &elsif.stmts {
                    collect_edge_calls(s, sig_env, interner, edges);
                }
            }
            for s in &if_stmt.else_stmts {
                collect_edge_calls(s, sig_env, interner, edges);
            }
        }
        vhdl_ast::SequentialStatement::Case(case_stmt) => {
            for alt in &case_stmt.alternatives {
                for s in &alt.stmts {
                    collect_edge_calls(s, sig_env, interner, edges);
                }
            }
        }
        _ => {}
    }
}

/// Checks a VHDL expression for `rising_edge(sig)` or `falling_edge(sig)` calls.
fn collect_edge_calls_expr(
    expr: &vhdl_ast::Expr,
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
    edges: &mut Vec<EdgeSensitivity>,
) {
    match expr {
        vhdl_ast::Expr::Name(name) => {
            let primary_text = interner.resolve(name.primary).to_lowercase();
            if (primary_text == "rising_edge" || primary_text == "falling_edge")
                && !name.parts.is_empty()
            {
                if let vhdl_ast::NameSuffix::Index(args, _) = &name.parts[0] {
                    if let Some(vhdl_ast::Expr::Name(arg_name)) = args.first() {
                        if let Some(&sid) = sig_env.get(&arg_name.primary) {
                            let edge = if primary_text == "rising_edge" {
                                Edge::Posedge
                            } else {
                                Edge::Negedge
                            };
                            let already = edges.iter().any(|e| e.signal == sid);
                            if !already {
                                edges.push(EdgeSensitivity { signal: sid, edge });
                            }
                        }
                    }
                }
            }
        }
        vhdl_ast::Expr::FunctionCall { name, .. } => {
            // Also handle the case where the parser uses FunctionCall variant
            collect_edge_calls_expr(name, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Binary { left, right, .. } => {
            collect_edge_calls_expr(left, sig_env, interner, edges);
            collect_edge_calls_expr(right, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Unary { operand, .. } => {
            collect_edge_calls_expr(operand, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Paren { inner, .. } => {
            collect_edge_calls_expr(inner, sig_env, interner, edges);
        }
        _ => {}
    }
}

/// Elaborates a VHDL component instantiation.
fn elaborate_vhdl_component(
    ci: &vhdl_ast::ComponentInstantiation,
    sig_env: &SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    ctx: &mut ElaborationContext<'_>,
) {
    let module_name = match &ci.unit {
        vhdl_ast::InstantiatedUnit::Component(sn) => {
            if let Some(&name) = sn.parts.last() {
                name
            } else {
                return;
            }
        }
        vhdl_ast::InstantiatedUnit::Entity(sn, _) => {
            if let Some(&name) = sn.parts.last() {
                name
            } else {
                return;
            }
        }
    };

    // Build generic overrides
    let generic_overrides: Vec<(Ident, ConstValue)> = ci
        .generic_map
        .as_ref()
        .map(|gm| {
            gm.elements
                .iter()
                .filter_map(|elem| {
                    let formal_name = extract_vhdl_formal(&elem.formal)?;
                    let val = const_eval::eval_vhdl_expr(
                        &elem.actual,
                        ctx.source_db,
                        ctx.interner,
                        &Default::default(),
                        ctx.sink,
                    )?;
                    Some((formal_name, val))
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(mid) = ctx.check_cache(module_name, &generic_overrides) {
        let connections = build_vhdl_port_connections(ci, sig_env, mid, ctx);
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: ci.label,
            kind: CellKind::Instance {
                module: mid,
                params: generic_overrides,
            },
            connections,
            span: ci.span,
        });
        return;
    }

    if !ctx.push_elab_stack(module_name, ci.span) {
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: ci.label,
            kind: CellKind::BlackBox { port_names: vec![] },
            connections: vec![],
            span: ci.span,
        });
        return;
    }

    let mid = match ctx.registry.lookup(module_name) {
        Some(ModuleEntry::Vhdl {
            entity,
            architecture,
        }) => {
            let mid = elaborate_vhdl_entity(entity, architecture, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        Some(ModuleEntry::Verilog(sub_decl)) => {
            let mid = crate::verilog::elaborate_verilog_module(sub_decl, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        Some(ModuleEntry::Sv(sub_decl)) => {
            let mid = crate::sv::elaborate_sv_module(sub_decl, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        None => {
            ctx.sink.emit(errors::error_unknown_module(
                ctx.interner.resolve(module_name),
                ci.span,
            ));
            ctx.pop_elab_stack();
            cells.alloc(Cell {
                id: CellId::from_raw(0),
                name: ci.label,
                kind: CellKind::BlackBox { port_names: vec![] },
                connections: vec![],
                span: ci.span,
            });
            return;
        }
    };
    ctx.pop_elab_stack();

    let connections = build_vhdl_port_connections(ci, sig_env, mid, ctx);
    cells.alloc(Cell {
        id: CellId::from_raw(0),
        name: ci.label,
        kind: CellKind::Instance {
            module: mid,
            params: generic_overrides,
        },
        connections,
        span: ci.span,
    });
}

/// Extracts a formal name from a VHDL association formal expression.
fn extract_vhdl_formal(formal: &Option<vhdl_ast::Expr>) -> Option<Ident> {
    match formal {
        Some(vhdl_ast::Expr::Name(name)) => Some(name.primary),
        _ => None,
    }
}

/// Builds IR connections from VHDL port map, looking up actual port directions
/// from the target module.
fn build_vhdl_port_connections(
    ci: &vhdl_ast::ComponentInstantiation,
    sig_env: &SignalEnv,
    target_module: ModuleId,
    ctx: &ElaborationContext<'_>,
) -> Vec<Connection> {
    ci.port_map
        .as_ref()
        .map(|pm| {
            pm.elements
                .iter()
                .filter_map(|elem| {
                    let formal_name = extract_vhdl_formal(&elem.formal)?;
                    let signal =
                        lower_vhdl_to_signal_ref(&elem.actual, sig_env, ctx.interner, ctx.sink);
                    let direction = lookup_port_direction(target_module, formal_name, ctx);
                    Some(Connection {
                        port_name: formal_name,
                        direction,
                        signal,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Looks up the direction of a port in the target module by name.
///
/// Returns `PortDirection::Input` as fallback if the port is not found.
fn lookup_port_direction(
    target: ModuleId,
    port_name: Ident,
    ctx: &ElaborationContext<'_>,
) -> PortDirection {
    let module = ctx.design.modules.get(target);
    module
        .ports
        .iter()
        .find(|p| p.name == port_name)
        .map(|p| p.direction)
        .unwrap_or(PortDirection::Input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::{SourceDb, Span};

    use crate::registry::ModuleRegistry;

    fn setup() -> (Interner, SourceDb, DiagnosticSink) {
        (Interner::new(), SourceDb::new(), DiagnosticSink::new())
    }

    #[test]
    fn elaborate_empty_vhdl_entity() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("empty");
        let arch_name = interner.get_or_intern("rtl");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: None,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 0);
    }

    #[test]
    fn elaborate_vhdl_entity_with_ports() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("counter");
        let arch_name = interner.get_or_intern("rtl");
        let clk = interner.get_or_intern("clk");
        let std_logic = interner.get_or_intern("std_logic");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: Some(vhdl_ast::PortClause {
                decls: vec![vhdl_ast::InterfaceDecl {
                    names: vec![clk],
                    mode: Some(PortMode::In),
                    ty: vhdl_ast::TypeIndication {
                        type_mark: vhdl_ast::SelectedName {
                            parts: vec![std_logic],
                            span: Span::DUMMY,
                        },
                        constraint: None,
                        span: Span::DUMMY,
                    },
                    default: None,
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }),
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 1);
        assert_eq!(ctx.design.modules[mid].signals.len(), 1);
    }

    #[test]
    fn elaborate_vhdl_arch_with_signal() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("test");
        let arch_name = interner.get_or_intern("rtl");
        let sig = interner.get_or_intern("internal_sig");
        let std_logic = interner.get_or_intern("std_logic");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: None,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![vhdl_ast::Declaration::Signal(vhdl_ast::SignalDecl {
                names: vec![sig],
                ty: vhdl_ast::TypeIndication {
                    type_mark: vhdl_ast::SelectedName {
                        parts: vec![std_logic],
                        span: Span::DUMMY,
                    },
                    constraint: None,
                    span: Span::DUMMY,
                },
                default: None,
                span: Span::DUMMY,
            })],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].signals.len(), 1);
    }

    #[test]
    fn detect_sequential_process_with_rising_edge() {
        let (interner, _source_db, _sink) = setup();
        let clk = interner.get_or_intern("clk");
        let rising_edge = interner.get_or_intern("rising_edge");

        let mut sig_env = SignalEnv::new();
        let clk_sid = SignalId::from_raw(0);
        sig_env.insert(clk, clk_sid);

        let sensitivity = vhdl_ast::SensitivityList::List(vec![vhdl_ast::SelectedName {
            parts: vec![clk],
            span: Span::DUMMY,
        }]);

        // Build a process body with: if rising_edge(clk) then ... end if;
        let stmts = vec![vhdl_ast::SequentialStatement::If(vhdl_ast::IfStatement {
            label: None,
            condition: vhdl_ast::Expr::Name(vhdl_ast::Name {
                primary: rising_edge,
                parts: vec![vhdl_ast::NameSuffix::Index(
                    vec![vhdl_ast::Expr::Name(vhdl_ast::Name {
                        primary: clk,
                        parts: vec![],
                        span: Span::DUMMY,
                    })],
                    Span::DUMMY,
                )],
                span: Span::DUMMY,
            }),
            then_stmts: vec![],
            elsif_branches: vec![],
            else_stmts: vec![],
            span: Span::DUMMY,
        })];

        let (kind, sens) = detect_vhdl_process_kind(&sensitivity, &stmts, &sig_env, &interner);
        assert!(
            matches!(kind, ProcessKind::Sequential),
            "process with rising_edge should be Sequential"
        );
        assert!(
            matches!(sens, Sensitivity::EdgeList(ref edges) if !edges.is_empty()),
            "should have EdgeList sensitivity"
        );
        if let Sensitivity::EdgeList(edges) = sens {
            assert_eq!(edges[0].signal, clk_sid);
            assert!(matches!(edges[0].edge, Edge::Posedge));
        }
    }

    #[test]
    fn detect_combinational_process_without_edges() {
        let (interner, _source_db, _sink) = setup();
        let a = interner.get_or_intern("a");

        let mut sig_env = SignalEnv::new();
        sig_env.insert(a, SignalId::from_raw(0));

        let sensitivity = vhdl_ast::SensitivityList::List(vec![vhdl_ast::SelectedName {
            parts: vec![a],
            span: Span::DUMMY,
        }]);

        // Empty body — no rising_edge/falling_edge calls
        let stmts = vec![vhdl_ast::SequentialStatement::Null { span: Span::DUMMY }];

        let (kind, _sens) = detect_vhdl_process_kind(&sensitivity, &stmts, &sig_env, &interner);
        assert!(
            matches!(kind, ProcessKind::Combinational),
            "process without edge calls should be Combinational"
        );
    }
}
