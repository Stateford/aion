//! Diagnostic codes and helper functions for elaboration errors and warnings.
//!
//! Error codes `E200`--`E211` cover elaboration failures (unknown modules,
//! duplicate signals, type mismatches, etc.). Warning codes `W200`--`W201`
//! cover non-fatal issues (width mismatches, unconnected ports).

use aion_diagnostics::{Category, Diagnostic, DiagnosticCode};
use aion_source::Span;

/// Unknown module referenced in instantiation.
pub const E200: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 200,
};

/// Port count or name mismatch in instantiation.
pub const E201: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 201,
};

/// Duplicate module name across source files.
pub const E202: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 202,
};

/// Duplicate signal name within a module.
pub const E203: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 203,
};

/// Reference to an unknown signal.
pub const E204: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 204,
};

/// Type mismatch in assignment or connection.
pub const E205: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 205,
};

/// Top-level module not found in any source file.
pub const E206: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 206,
};

/// Circular instantiation detected.
pub const E207: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 207,
};

/// Unknown port name in instantiation connection.
pub const E208: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 208,
};

/// Parameter constant-expression evaluation failure.
pub const E209: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 209,
};

/// Unsupported construct (e.g., complex typedef in Phase 0).
pub const E210: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 210,
};

/// No architecture found for a VHDL entity.
pub const E211: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 211,
};

/// Width mismatch in assignment or connection.
pub const W200: DiagnosticCode = DiagnosticCode {
    category: Category::Warning,
    number: 200,
};

/// Unconnected port in instantiation.
pub const W201: DiagnosticCode = DiagnosticCode {
    category: Category::Warning,
    number: 201,
};

/// Creates a diagnostic for an unknown module in an instantiation.
pub fn error_unknown_module(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E200, format!("unknown module `{name}`"), span)
        .with_help("check that the module is defined in the source files")
}

/// Creates a diagnostic for a missing top-level module.
pub fn error_top_not_found(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E206, format!("top-level module `{name}` not found"), span)
        .with_help("set `project.top` in aion.toml to the name of an existing module")
}

/// Creates a diagnostic for a duplicate module name.
pub fn error_duplicate_module(name: &str, span: Span, prev_span: Span) -> Diagnostic {
    Diagnostic::error(E202, format!("duplicate module `{name}`"), span).with_label(
        aion_diagnostics::Label::secondary(prev_span, "previously defined here"),
    )
}

/// Creates a diagnostic for a duplicate signal name within a module.
pub fn error_duplicate_signal(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E203, format!("duplicate signal `{name}`"), span)
}

/// Creates a diagnostic for an unknown signal reference.
pub fn error_unknown_signal(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E204, format!("unknown signal `{name}`"), span)
}

/// Creates a diagnostic for a circular instantiation.
pub fn error_circular_instantiation(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        E207,
        format!("circular instantiation of module `{name}`"),
        span,
    )
    .with_note("the module directly or indirectly instantiates itself")
}

/// Creates a diagnostic when a parameter cannot be constant-evaluated.
pub fn error_param_not_const(msg: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E209, format!("cannot evaluate parameter: {msg}"), span)
}

/// Creates a diagnostic for an unsupported construct.
pub fn error_unsupported(what: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E210, format!("unsupported construct: {what}"), span)
        .with_note("this will be supported in a future release")
}

/// Creates a diagnostic when no architecture is found for a VHDL entity.
pub fn error_no_architecture(entity_name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        E211,
        format!("no architecture found for entity `{entity_name}`"),
        span,
    )
}

/// Creates a diagnostic for a port mismatch in instantiation.
pub fn error_port_mismatch(msg: &str, span: Span) -> Diagnostic {
    Diagnostic::error(E201, msg.to_string(), span)
}

/// Creates a diagnostic for an unknown port in an instantiation.
pub fn error_unknown_port(port_name: &str, module_name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        E208,
        format!("unknown port `{port_name}` on module `{module_name}`"),
        span,
    )
}

/// Creates a warning for a width mismatch.
pub fn warn_width_mismatch(msg: &str, span: Span) -> Diagnostic {
    Diagnostic::warning(W200, msg.to_string(), span)
}

/// Creates a warning for an unconnected port.
pub fn warn_unconnected_port(port_name: &str, instance_name: &str, span: Span) -> Diagnostic {
    Diagnostic::warning(
        W201,
        format!("port `{port_name}` is unconnected on instance `{instance_name}`"),
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_formats() {
        assert_eq!(format!("{E200}"), "E200");
        assert_eq!(format!("{E206}"), "E206");
        assert_eq!(format!("{W200}"), "W200");
        assert_eq!(format!("{W201}"), "W201");
    }

    #[test]
    fn unknown_module_diagnostic() {
        let d = error_unknown_module("counter", Span::DUMMY);
        assert_eq!(d.code, E200);
        assert!(d.message.contains("counter"));
    }

    #[test]
    fn top_not_found_diagnostic() {
        let d = error_top_not_found("top", Span::DUMMY);
        assert_eq!(d.code, E206);
        assert!(d.message.contains("top"));
    }

    #[test]
    fn duplicate_module_diagnostic() {
        let d = error_duplicate_module("counter", Span::DUMMY, Span::DUMMY);
        assert_eq!(d.code, E202);
        assert_eq!(d.labels.len(), 1);
    }

    #[test]
    fn duplicate_signal_diagnostic() {
        let d = error_duplicate_signal("clk", Span::DUMMY);
        assert_eq!(d.code, E203);
    }

    #[test]
    fn unknown_signal_diagnostic() {
        let d = error_unknown_signal("rst", Span::DUMMY);
        assert_eq!(d.code, E204);
    }

    #[test]
    fn circular_instantiation_diagnostic() {
        let d = error_circular_instantiation("top", Span::DUMMY);
        assert_eq!(d.code, E207);
        assert!(!d.notes.is_empty());
    }

    #[test]
    fn param_not_const_diagnostic() {
        let d = error_param_not_const("non-constant expression", Span::DUMMY);
        assert_eq!(d.code, E209);
    }

    #[test]
    fn unsupported_diagnostic() {
        let d = error_unsupported("complex typedef", Span::DUMMY);
        assert_eq!(d.code, E210);
    }

    #[test]
    fn no_architecture_diagnostic() {
        let d = error_no_architecture("counter", Span::DUMMY);
        assert_eq!(d.code, E211);
    }

    #[test]
    fn warning_diagnostics() {
        let d = warn_width_mismatch("8-bit to 4-bit", Span::DUMMY);
        assert_eq!(d.code, W200);

        let d = warn_unconnected_port("clk", "u1", Span::DUMMY);
        assert_eq!(d.code, W201);
        assert!(d.message.contains("clk"));
    }

    #[test]
    fn port_mismatch_diagnostic() {
        let d = error_port_mismatch("expected 3 ports, found 2", Span::DUMMY);
        assert_eq!(d.code, E201);
    }

    #[test]
    fn unknown_port_diagnostic() {
        let d = error_unknown_port("data", "counter", Span::DUMMY);
        assert_eq!(d.code, E208);
        assert!(d.message.contains("data"));
        assert!(d.message.contains("counter"));
    }
}
