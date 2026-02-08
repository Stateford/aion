//! Mutable elaboration state for recursive module elaboration.
//!
//! [`ElaborationContext`] holds the `Design` under construction, the module
//! registry, a cache of already-elaborated modules (keyed by name + parameter
//! values), and the current elaboration stack for cycle detection.

use std::collections::HashMap;

use aion_common::{ContentHash, Ident, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_ir::arena::Arena;
use aion_ir::ids::{ModuleId, PortId};
use aion_ir::source_map::SourceMap;
use aion_ir::types::TypeDb;
use aion_ir::{ConstValue, Design};
use aion_source::SourceDb;

use crate::errors;
use crate::registry::ModuleRegistry;

/// Cache key: module name + sorted parameter bindings, hashed together.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    /// The module name.
    name: Ident,
    /// Hash of the parameter bindings (sorted by name for determinism).
    param_hash: ContentHash,
}

/// Mutable state carried through recursive elaboration.
///
/// Owns the `Design` under construction and provides helpers for caching,
/// cycle detection, and ID allocation.
pub struct ElaborationContext<'a> {
    /// The design being built.
    pub design: Design,
    /// The module registry providing name-to-declaration lookup.
    pub registry: &'a ModuleRegistry<'a>,
    /// The string interner shared with the parsers.
    pub interner: &'a Interner,
    /// The source database for snippet access.
    pub source_db: &'a SourceDb,
    /// The diagnostic sink for error reporting.
    pub sink: &'a DiagnosticSink,
    /// Cache of elaborated modules by (name, param_hash) → ModuleId.
    cache: HashMap<CacheKey, ModuleId>,
    /// Stack of module names currently being elaborated (for cycle detection).
    elab_stack: Vec<Ident>,
    /// Next port ID counter (global across the design).
    next_port_id: u32,
}

impl<'a> ElaborationContext<'a> {
    /// Creates a new elaboration context.
    pub fn new(
        registry: &'a ModuleRegistry<'a>,
        interner: &'a Interner,
        source_db: &'a SourceDb,
        sink: &'a DiagnosticSink,
    ) -> Self {
        Self {
            design: Design {
                modules: Arena::new(),
                top: ModuleId::from_raw(0),
                types: TypeDb::new(),
                source_map: SourceMap::new(),
            },
            registry,
            interner,
            source_db,
            sink,
            cache: HashMap::new(),
            elab_stack: Vec::new(),
            next_port_id: 0,
        }
    }

    /// Returns a mutable reference to the shared type database.
    pub fn types(&mut self) -> &mut TypeDb {
        &mut self.design.types
    }

    /// Allocates a globally unique [`PortId`].
    pub fn alloc_port_id(&mut self) -> PortId {
        let id = PortId::from_raw(self.next_port_id);
        self.next_port_id += 1;
        id
    }

    /// Checks the cache for a previously elaborated module with the given
    /// name and parameter bindings.
    pub fn check_cache(&self, name: Ident, params: &[(Ident, ConstValue)]) -> Option<ModuleId> {
        let key = CacheKey {
            name,
            param_hash: hash_params(params),
        };
        self.cache.get(&key).copied()
    }

    /// Inserts an elaborated module into the cache.
    pub fn insert_cache(
        &mut self,
        name: Ident,
        params: &[(Ident, ConstValue)],
        module_id: ModuleId,
    ) {
        let key = CacheKey {
            name,
            param_hash: hash_params(params),
        };
        self.cache.insert(key, module_id);
    }

    /// Pushes a module name onto the elaboration stack.
    ///
    /// Returns `false` if the module is already on the stack (cycle detected),
    /// emitting an `E207` diagnostic.
    pub fn push_elab_stack(&mut self, name: Ident, span: aion_source::Span) -> bool {
        if self.elab_stack.contains(&name) {
            self.sink.emit(errors::error_circular_instantiation(
                self.interner.resolve(name),
                span,
            ));
            return false;
        }
        self.elab_stack.push(name);
        true
    }

    /// Pops the most recent module name from the elaboration stack.
    pub fn pop_elab_stack(&mut self) {
        self.elab_stack.pop();
    }
}

/// Computes a deterministic hash over sorted parameter bindings.
fn hash_params(params: &[(Ident, ConstValue)]) -> ContentHash {
    use std::hash::Hash;

    let mut sorted: Vec<_> = params.iter().collect();
    sorted.sort_by_key(|(name, _)| name.as_raw());

    // Use a simple digest: hash via DefaultHasher then wrap in ContentHash
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (name, val) in &sorted {
        name.as_raw().hash(&mut hasher);
        // Hash the discriminant + value
        match val {
            ConstValue::Int(n) => {
                0u8.hash(&mut hasher);
                n.hash(&mut hasher);
            }
            ConstValue::Real(f) => {
                1u8.hash(&mut hasher);
                f.to_bits().hash(&mut hasher);
            }
            ConstValue::Logic(lv) => {
                2u8.hash(&mut hasher);
                lv.width().hash(&mut hasher);
            }
            ConstValue::String(s) => {
                3u8.hash(&mut hasher);
                s.hash(&mut hasher);
            }
            ConstValue::Bool(b) => {
                4u8.hash(&mut hasher);
                b.hash(&mut hasher);
            }
        }
    }
    let h = std::hash::Hasher::finish(&hasher);
    ContentHash::from_bytes(&h.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::{SourceDb, Span};

    fn make_ctx() -> (Interner, SourceDb, DiagnosticSink) {
        (Interner::new(), SourceDb::new(), DiagnosticSink::new())
    }

    #[test]
    fn context_construction() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        assert_eq!(ctx.design.modules.len(), 0);
    }

    #[test]
    fn alloc_port_id_increments() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let p0 = ctx.alloc_port_id();
        let p1 = ctx.alloc_port_id();
        let p2 = ctx.alloc_port_id();
        assert_eq!(p0.as_raw(), 0);
        assert_eq!(p1.as_raw(), 1);
        assert_eq!(p2.as_raw(), 2);
    }

    #[test]
    fn cache_miss_returns_none() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let name = interner.get_or_intern("counter");
        assert!(ctx.check_cache(name, &[]).is_none());
    }

    #[test]
    fn cache_hit_after_insert() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let name = interner.get_or_intern("counter");
        let mid = ModuleId::from_raw(42);
        ctx.insert_cache(name, &[], mid);
        assert_eq!(ctx.check_cache(name, &[]), Some(mid));
    }

    #[test]
    fn cache_different_params_different_entries() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let name = interner.get_or_intern("counter");
        let width = interner.get_or_intern("WIDTH");

        let m1 = ModuleId::from_raw(1);
        let m2 = ModuleId::from_raw(2);
        ctx.insert_cache(name, &[(width, ConstValue::Int(8))], m1);
        ctx.insert_cache(name, &[(width, ConstValue::Int(16))], m2);

        assert_eq!(
            ctx.check_cache(name, &[(width, ConstValue::Int(8))]),
            Some(m1)
        );
        assert_eq!(
            ctx.check_cache(name, &[(width, ConstValue::Int(16))]),
            Some(m2)
        );
    }

    #[test]
    fn elab_stack_push_pop() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let name = interner.get_or_intern("top");
        assert!(ctx.push_elab_stack(name, Span::DUMMY));
        ctx.pop_elab_stack();
    }

    #[test]
    fn elab_stack_cycle_detection() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let name = interner.get_or_intern("top");
        assert!(ctx.push_elab_stack(name, Span::DUMMY));
        // Pushing same name again = cycle
        assert!(!ctx.push_elab_stack(name, Span::DUMMY));
        assert!(sink.has_errors());
    }

    #[test]
    fn elab_stack_no_false_positive() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        assert!(ctx.push_elab_stack(a, Span::DUMMY));
        assert!(ctx.push_elab_stack(b, Span::DUMMY));
        ctx.pop_elab_stack(); // pop b
        ctx.pop_elab_stack(); // pop a
                              // Can push a again after popping
        assert!(ctx.push_elab_stack(a, Span::DUMMY));
        assert!(!sink.has_errors());
    }

    #[test]
    fn types_access() {
        let (interner, source_db, sink) = make_ctx();
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        use aion_ir::types::Type;
        let tid = ctx.types().intern(Type::Bit);
        assert_eq!(*ctx.types().get(tid), Type::Bit);
    }
}
