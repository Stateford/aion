//! Module registry for scanning parsed ASTs and mapping module names to declarations.
//!
//! The [`ModuleRegistry`] scans all parsed source files and builds lookup tables
//! for Verilog modules, SystemVerilog modules, and VHDL entity/architecture pairs.
//! Duplicate module names are detected and reported.

use std::collections::HashMap;

use aion_common::{Ident, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_source::Span;

use crate::errors;

/// A VHDL entity paired with its architectures.
pub struct VhdlEntityEntry<'a> {
    /// The entity declaration.
    pub entity: &'a aion_vhdl_parser::ast::EntityDecl,
    /// All architectures that implement this entity.
    pub architectures: Vec<&'a aion_vhdl_parser::ast::ArchitectureDecl>,
}

/// A unified reference to a module declaration from any supported language.
pub enum ModuleEntry<'a> {
    /// A Verilog-2005 module.
    Verilog(&'a aion_verilog_parser::ast::ModuleDecl),
    /// A SystemVerilog module.
    Sv(&'a aion_sv_parser::ast::SvModuleDecl),
    /// A VHDL entity with its selected architecture.
    Vhdl {
        /// The entity declaration.
        entity: &'a aion_vhdl_parser::ast::EntityDecl,
        /// The architecture body (last declared).
        architecture: &'a aion_vhdl_parser::ast::ArchitectureDecl,
    },
}

/// Registry of all module/entity declarations across all parsed source files.
///
/// Provides O(1) lookup by interned name and detects duplicate definitions.
pub struct ModuleRegistry<'a> {
    /// Verilog modules by name.
    verilog: HashMap<Ident, &'a aion_verilog_parser::ast::ModuleDecl>,
    /// SystemVerilog modules by name.
    sv: HashMap<Ident, &'a aion_sv_parser::ast::SvModuleDecl>,
    /// VHDL entities by name, with associated architectures.
    vhdl: HashMap<Ident, VhdlEntityEntry<'a>>,
    /// Span of first occurrence for duplicate detection.
    first_span: HashMap<Ident, Span>,
}

impl<'a> ModuleRegistry<'a> {
    /// Builds a module registry from parsed design files.
    ///
    /// Scans all Verilog, SystemVerilog, and VHDL files to extract module/entity
    /// declarations. Emits `E202` diagnostics for duplicate module names.
    pub fn from_parsed_design(
        verilog_files: &'a [aion_verilog_parser::ast::VerilogSourceFile],
        sv_files: &'a [aion_sv_parser::ast::SvSourceFile],
        vhdl_files: &'a [aion_vhdl_parser::ast::VhdlDesignFile],
        interner: &Interner,
        sink: &DiagnosticSink,
    ) -> Self {
        let mut reg = Self {
            verilog: HashMap::new(),
            sv: HashMap::new(),
            vhdl: HashMap::new(),
            first_span: HashMap::new(),
        };

        // Scan Verilog files
        for file in verilog_files {
            for item in &file.items {
                if let aion_verilog_parser::ast::VerilogItem::Module(decl) = item {
                    reg.register_verilog(decl, interner, sink);
                }
            }
        }

        // Scan SV files
        for file in sv_files {
            for item in &file.items {
                if let aion_sv_parser::ast::SvItem::Module(decl) = item {
                    reg.register_sv(decl, interner, sink);
                }
            }
        }

        // Scan VHDL files — first entities, then architectures
        for file in vhdl_files {
            for unit in &file.units {
                if let aion_vhdl_parser::ast::DesignUnit::ContextUnit {
                    unit: aion_vhdl_parser::ast::DesignUnitKind::Entity(entity),
                    ..
                } = unit
                {
                    reg.register_vhdl_entity(entity, interner, sink);
                }
            }
        }
        for file in vhdl_files {
            for unit in &file.units {
                if let aion_vhdl_parser::ast::DesignUnit::ContextUnit {
                    unit: aion_vhdl_parser::ast::DesignUnitKind::Architecture(arch),
                    ..
                } = unit
                {
                    reg.register_vhdl_architecture(arch, interner);
                }
            }
        }

        reg
    }

    /// Registers a Verilog module, emitting a duplicate diagnostic if needed.
    fn register_verilog(
        &mut self,
        decl: &'a aion_verilog_parser::ast::ModuleDecl,
        interner: &Interner,
        sink: &DiagnosticSink,
    ) {
        let name = decl.name;
        if let Some(&prev_span) = self.first_span.get(&name) {
            sink.emit(errors::error_duplicate_module(
                interner.resolve(name),
                decl.span,
                prev_span,
            ));
        } else {
            self.verilog.insert(name, decl);
            self.first_span.insert(name, decl.span);
        }
    }

    /// Registers a SystemVerilog module, emitting a duplicate diagnostic if needed.
    fn register_sv(
        &mut self,
        decl: &'a aion_sv_parser::ast::SvModuleDecl,
        interner: &Interner,
        sink: &DiagnosticSink,
    ) {
        let name = decl.name;
        if let Some(&prev_span) = self.first_span.get(&name) {
            sink.emit(errors::error_duplicate_module(
                interner.resolve(name),
                decl.span,
                prev_span,
            ));
        } else {
            self.sv.insert(name, decl);
            self.first_span.insert(name, decl.span);
        }
    }

    /// Registers a VHDL entity.
    fn register_vhdl_entity(
        &mut self,
        entity: &'a aion_vhdl_parser::ast::EntityDecl,
        interner: &Interner,
        sink: &DiagnosticSink,
    ) {
        let name = entity.name;
        if let Some(&prev_span) = self.first_span.get(&name) {
            sink.emit(errors::error_duplicate_module(
                interner.resolve(name),
                entity.span,
                prev_span,
            ));
        } else {
            self.vhdl.insert(
                name,
                VhdlEntityEntry {
                    entity,
                    architectures: Vec::new(),
                },
            );
            self.first_span.insert(name, entity.span);
        }
    }

    /// Associates an architecture with its entity.
    fn register_vhdl_architecture(
        &mut self,
        arch: &'a aion_vhdl_parser::ast::ArchitectureDecl,
        _interner: &Interner,
    ) {
        // Look up entity by entity_name
        let entity_name = arch.entity_name;
        if let Some(entry) = self.vhdl.get_mut(&entity_name) {
            entry.architectures.push(arch);
        }
        // If entity not found, we silently ignore — the entity might not be parsed yet
        // or might be from a library. This will be caught during elaboration.
    }

    /// Looks up a module by name across all languages.
    ///
    /// Returns the first match found, searching Verilog, then SV, then VHDL.
    /// For VHDL, selects the last declared architecture (VHDL convention).
    pub fn lookup(&self, name: Ident) -> Option<ModuleEntry<'a>> {
        if let Some(decl) = self.verilog.get(&name) {
            return Some(ModuleEntry::Verilog(decl));
        }
        if let Some(decl) = self.sv.get(&name) {
            return Some(ModuleEntry::Sv(decl));
        }
        if let Some(entry) = self.vhdl.get(&name) {
            if let Some(arch) = entry.architectures.last() {
                return Some(ModuleEntry::Vhdl {
                    entity: entry.entity,
                    architecture: arch,
                });
            }
        }
        None
    }

    /// Returns the source span of a module's declaration, if found.
    pub fn span_of(&self, name: Ident) -> Option<Span> {
        self.first_span.get(&name).copied()
    }

    /// Returns `true` if the VHDL entity exists but has no architectures.
    pub fn vhdl_has_no_arch(&self, name: Ident) -> bool {
        self.vhdl
            .get(&name)
            .is_some_and(|e| e.architectures.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::Span;

    #[test]
    fn empty_registry() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let unknown = interner.get_or_intern("unknown");
        assert!(reg.lookup(unknown).is_none());
        assert!(reg.span_of(unknown).is_none());
    }

    #[test]
    fn register_verilog_module() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let name = interner.get_or_intern("counter");
        let file = aion_verilog_parser::ast::VerilogSourceFile {
            items: vec![aion_verilog_parser::ast::VerilogItem::Module(
                aion_verilog_parser::ast::ModuleDecl {
                    name,
                    port_style: aion_verilog_parser::ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![],
                    span: Span::DUMMY,
                },
            )],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        assert!(reg.lookup(name).is_some());
        assert!(matches!(reg.lookup(name), Some(ModuleEntry::Verilog(_))));
    }

    #[test]
    fn register_sv_module() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let name = interner.get_or_intern("top");
        let file = aion_sv_parser::ast::SvSourceFile {
            items: vec![aion_sv_parser::ast::SvItem::Module(
                aion_sv_parser::ast::SvModuleDecl {
                    name,
                    port_style: aion_sv_parser::ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![],
                    end_label: None,
                    span: Span::DUMMY,
                },
            )],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &files, &[], &interner, &sink);
        assert!(matches!(reg.lookup(name), Some(ModuleEntry::Sv(_))));
    }

    #[test]
    fn duplicate_module_emits_diagnostic() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let name = interner.get_or_intern("dup");
        let file = aion_verilog_parser::ast::VerilogSourceFile {
            items: vec![
                aion_verilog_parser::ast::VerilogItem::Module(
                    aion_verilog_parser::ast::ModuleDecl {
                        name,
                        port_style: aion_verilog_parser::ast::PortStyle::Empty,
                        params: vec![],
                        ports: vec![],
                        port_names: vec![],
                        items: vec![],
                        span: Span::DUMMY,
                    },
                ),
                aion_verilog_parser::ast::VerilogItem::Module(
                    aion_verilog_parser::ast::ModuleDecl {
                        name,
                        port_style: aion_verilog_parser::ast::PortStyle::Empty,
                        params: vec![],
                        ports: vec![],
                        port_names: vec![],
                        items: vec![],
                        span: Span::DUMMY,
                    },
                ),
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let _reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        assert!(sink.has_errors());
        assert_eq!(sink.error_count(), 1);
    }

    #[test]
    fn lookup_miss_returns_none() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let name = interner.get_or_intern("counter");
        let file = aion_verilog_parser::ast::VerilogSourceFile {
            items: vec![aion_verilog_parser::ast::VerilogItem::Module(
                aion_verilog_parser::ast::ModuleDecl {
                    name,
                    port_style: aion_verilog_parser::ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![],
                    span: Span::DUMMY,
                },
            )],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let other = interner.get_or_intern("other");
        assert!(reg.lookup(other).is_none());
    }

    #[test]
    fn vhdl_entity_without_arch() {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let name = interner.get_or_intern("counter");
        let file = aion_vhdl_parser::ast::VhdlDesignFile {
            units: vec![aion_vhdl_parser::ast::DesignUnit::ContextUnit {
                context: vec![],
                unit: aion_vhdl_parser::ast::DesignUnitKind::Entity(
                    aion_vhdl_parser::ast::EntityDecl {
                        name,
                        generics: None,
                        ports: None,
                        decls: vec![],
                        stmts: vec![],
                        span: Span::DUMMY,
                    },
                ),
                span: Span::DUMMY,
            }],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        assert!(reg.lookup(name).is_none()); // No architecture
        assert!(reg.vhdl_has_no_arch(name));
    }
}
