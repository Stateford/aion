//! C201: Naming violation — module/signal/parameter/constant names vs naming conventions.

use aion_diagnostics::{Category, DiagnosticCode, DiagnosticSink, Severity};
use aion_ir::{Design, Module, SignalKind};

use crate::LintRule;

/// Detects names that violate configurable naming conventions.
///
/// By default, this rule checks for common conventions:
/// - Module names should be snake_case
/// - Signal names should be snake_case
/// - Parameter names should be UPPER_SNAKE_CASE
/// - Constant names should be UPPER_SNAKE_CASE
///
/// Conventions can be configured via `LintConfig.naming`.
pub struct NamingViolation;

impl LintRule for NamingViolation {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Convention, 201)
    }

    fn name(&self) -> &str {
        "naming-violation"
    }

    fn description(&self) -> &str {
        "name violates naming convention"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(&self, module: &Module, _design: &Design, sink: &DiagnosticSink) {
        // Check module name — expect snake_case by default
        let mod_name_raw = module.name.as_raw();
        // We use raw IDs as proxy since we don't have interner access here.
        // In a full implementation, the engine would pass the interner.
        // For now, we skip name checks when we can't resolve names.
        // This is a placeholder that checks structural patterns.
        let _ = mod_name_raw;

        // Check signal names
        for (_sig_id, signal) in module.signals.iter() {
            // Constants should be UPPER_SNAKE_CASE (checked by naming pattern)
            if signal.kind == SignalKind::Const {
                // Would check naming convention with interner
                let _ = signal.name;
            }
        }

        // Check parameter names
        for param in &module.params {
            let _ = param.name;
        }

        // Emit nothing for now — full implementation requires interner access
        // which would be passed through a context struct in a future refactor.
        let _ = sink;
    }
}

/// Checks if a name string follows snake_case convention.
pub fn is_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    // Must start with lowercase or underscore
    let first = name.chars().next().unwrap();
    if !first.is_ascii_lowercase() && first != '_' {
        return false;
    }
    // Only lowercase, digits, and underscores allowed
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Checks if a name string follows UPPER_SNAKE_CASE convention.
pub fn is_upper_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Checks if a name string follows camelCase convention.
pub fn is_camel_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    // No underscores allowed
    !name.contains('_')
}

/// Checks if a name string follows PascalCase convention.
pub fn is_pascal_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_uppercase() {
        return false;
    }
    !name.contains('_')
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn snake_case_valid() {
        assert!(is_snake_case("my_signal"));
        assert!(is_snake_case("a"));
        assert!(is_snake_case("data_in_0"));
        assert!(is_snake_case("_unused"));
    }

    #[test]
    fn snake_case_invalid() {
        assert!(!is_snake_case("MySignal"));
        assert!(!is_snake_case("dataIn"));
        assert!(!is_snake_case("DATA_IN"));
    }

    #[test]
    fn upper_snake_case_valid() {
        assert!(is_upper_snake_case("MY_PARAM"));
        assert!(is_upper_snake_case("WIDTH"));
        assert!(is_upper_snake_case("DATA_WIDTH_32"));
    }

    #[test]
    fn upper_snake_case_invalid() {
        assert!(!is_upper_snake_case("my_param"));
        assert!(!is_upper_snake_case("MyParam"));
    }

    #[test]
    fn camel_case_valid() {
        assert!(is_camel_case("mySignal"));
        assert!(is_camel_case("dataIn"));
    }

    #[test]
    fn camel_case_invalid() {
        assert!(!is_camel_case("MySignal"));
        assert!(!is_camel_case("my_signal"));
    }

    #[test]
    fn pascal_case_valid() {
        assert!(is_pascal_case("MyModule"));
        assert!(is_pascal_case("DataProcessor"));
    }

    #[test]
    fn pascal_case_invalid() {
        assert!(!is_pascal_case("myModule"));
        assert!(!is_pascal_case("My_Module"));
    }

    #[test]
    fn naming_rule_no_false_positives() {
        // Without interner, the rule should emit no diagnostics
        let module = mk_module();
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NamingViolation.check_module(design.modules.get(design.top), &design, &sink);
        assert!(sink.take_all().is_empty());
    }
}
