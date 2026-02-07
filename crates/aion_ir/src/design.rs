//! Top-level design container.
//!
//! A [`Design`] holds all modules, the type database, and the source map.
//! It is the primary output of the elaboration stage and input to synthesis.

use crate::arena::Arena;
use crate::ids::ModuleId;
use crate::module::Module;
use crate::source_map::SourceMap;
use crate::types::TypeDb;
use serde::{Deserialize, Serialize};

/// A complete hardware design after elaboration.
///
/// This is the top-level AionIR structure containing all modules in the
/// design hierarchy, the shared type database, and source location mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Design {
    /// All modules in the design, keyed by [`ModuleId`].
    pub modules: Arena<ModuleId, Module>,
    /// The top-level module.
    pub top: ModuleId,
    /// Global type definitions shared across all modules.
    pub types: TypeDb,
    /// Source mapping from IR entities to original source spans.
    pub source_map: SourceMap,
}

impl Design {
    /// Returns a reference to the top-level module.
    pub fn top_module(&self) -> &Module {
        &self.modules[self.top]
    }

    /// Returns the number of modules in the design.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::module::Module;
    use aion_common::{ContentHash, Ident};
    use aion_source::Span;

    fn make_design() -> Design {
        let mut modules = Arena::new();
        let top_id = modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(1),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"top"),
        });
        Design {
            modules,
            top: top_id,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn design_construction() {
        let design = make_design();
        assert_eq!(design.module_count(), 1);
    }

    #[test]
    fn top_module_access() {
        let design = make_design();
        let top = design.top_module();
        assert_eq!(top.id.as_raw(), 0);
    }

    #[test]
    fn design_with_multiple_modules() {
        let mut design = make_design();
        design.modules.alloc(Module {
            id: ModuleId::from_raw(1),
            name: Ident::from_raw(2),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"sub"),
        });
        assert_eq!(design.module_count(), 2);
        // Top module is still accessible
        assert_eq!(design.top_module().name, Ident::from_raw(1));
    }

    #[test]
    fn design_serde_roundtrip() {
        let design = make_design();
        let json = serde_json::to_string(&design).unwrap();
        let restored: Design = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.module_count(), 1);
        assert_eq!(restored.top, design.top);
    }
}
