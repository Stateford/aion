//! End-to-end checks for aggregate choice lists, named association in calls,
//! the `<>` box, VHDL-2008 array constraints and resolution indications
//! (GitHub issue #19, findings VHDL-11 and VHDL-12).
//!
//! Each case is a complete design file parsed through the public
//! [`aion_vhdl_parser::parse_file`] entry point. Legal sources must parse
//! without diagnostics; illegal ones must report the expected error and
//! still produce the design units around it.

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_source::SourceDb;
use aion_vhdl_parser::ast::*;
use aion_vhdl_parser::parse_file;

/// Parses `src`, returning the AST and the diagnostic messages.
fn parse(src: &str) -> (VhdlDesignFile, Vec<String>) {
    let mut db = SourceDb::new();
    let file = db.add_source("issue19.vhd", src.to_string());
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let ast = parse_file(file, &db, &interner, &sink);
    let msgs = sink.take_all().into_iter().map(|d| d.message).collect();
    (ast, msgs)
}

/// Parses `src`, asserting that no diagnostics are reported.
fn parse_ok(src: &str) -> VhdlDesignFile {
    let (ast, msgs) = parse(src);
    assert!(msgs.is_empty(), "unexpected diagnostics: {msgs:?}");
    ast
}

/// Wraps sequential statements in an entity/architecture/process.
fn in_process(decls: &str, stmts: &str) -> String {
    format!(
        "library ieee;
        use ieee.std_logic_1164.all;
        use ieee.numeric_std.all;
        entity e is end entity;
        architecture rtl of e is
        begin
            process
                {decls}
            begin
                {stmts}
                wait;
            end process;
        end architecture;"
    )
}

/// Returns the statements of the single process in the architecture.
fn process_stmts(ast: &VhdlDesignFile) -> &[SequentialStatement] {
    for unit in &ast.units {
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = unit
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                return &p.stmts;
            }
        }
    }
    panic!("no process found");
}

/// Returns the value assigned by a variable assignment statement.
fn assigned_value(stmt: &SequentialStatement) -> &Expr {
    match stmt {
        SequentialStatement::VariableAssignment { value, .. } => value,
        other => panic!("expected variable assignment, got {other:?}"),
    }
}

/// Returns the declarations of the single package in the file.
fn package_decls(ast: &VhdlDesignFile) -> &[Declaration] {
    for unit in &ast.units {
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = unit
        {
            return &p.decls;
        }
    }
    panic!("no package found");
}

// ---- VHDL-11: aggregates -------------------------------------------------

#[test]
fn aggregate_with_bar_choices() {
    let ast = parse_ok(&in_process(
        "variable v : std_logic_vector(7 downto 0);",
        "v := (1 | 3 => '1', others => '0');",
    ));
    let Expr::Aggregate { elements, .. } = assigned_value(&process_stmts(&ast)[0]) else {
        panic!("expected aggregate");
    };
    assert_eq!(elements.len(), 2);
    assert!(matches!(
        elements[0].choices[..],
        [Choice::Expr(_), Choice::Expr(_)]
    ));
    assert!(matches!(elements[1].choices[..], [Choice::Others(_)]));
}

#[test]
fn aggregate_with_range_choice() {
    let ast = parse_ok(&in_process(
        "variable v : std_logic_vector(7 downto 0);",
        "v := (7 downto 4 => '1', others => '0');",
    ));
    let Expr::Aggregate { elements, .. } = assigned_value(&process_stmts(&ast)[0]) else {
        panic!("expected aggregate");
    };
    let [Choice::Range(rc)] = &elements[0].choices[..] else {
        panic!("expected range choice, got {:?}", elements[0].choices);
    };
    assert_eq!(rc.direction, RangeDirection::Downto);
}

#[test]
fn aggregate_mixing_ranges_and_bars() {
    parse_ok(&in_process(
        "variable v : std_logic_vector(15 downto 0);",
        "v := (0 to 3 | 8 | 12 to 13 => '1', others => '0');
         v := ('1', '0', others => '1');",
    ));
}

#[test]
fn aggregate_others_must_be_last_and_alone() {
    let (ast, msgs) = parse(&in_process(
        "variable v : std_logic_vector(7 downto 0);",
        "v := (others => '0', 1 => '1');
         v := (others | 1 => '0');",
    ));
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(msgs[0].contains("last element"), "{msgs:?}");
    assert!(msgs[1].contains("cannot be combined"), "{msgs:?}");
    // Both statements are still in the tree.
    assert_eq!(process_stmts(&ast).len(), 3);
}

#[test]
fn aggregate_positional_after_named_is_rejected() {
    let (_, msgs) = parse(&in_process(
        "variable v : std_logic_vector(1 downto 0);",
        "v := (1 => '1', '0');",
    ));
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("positional element"), "{msgs:?}");
}

#[test]
fn aggregate_range_choice_without_arrow_is_rejected() {
    let (_, msgs) = parse(&in_process(
        "variable v : std_logic_vector(1 downto 0);",
        "v := (1 downto 0);",
    ));
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("`=>` after aggregate choices"), "{msgs:?}");
}

// ---- VHDL-11: named association in calls --------------------------------

#[test]
fn named_association_in_function_calls() {
    let ast = parse_ok(&in_process(
        "variable x : unsigned(7 downto 0); variable v : unsigned(15 downto 0);",
        "x := to_unsigned(arg => 3, size => 8);
         v := resize(arg => x, new_size => 16);
         v := resize(x, new_size => 16);",
    ));
    let stmts = process_stmts(&ast);
    let Expr::Name(name) = assigned_value(&stmts[0]) else {
        panic!("expected a name");
    };
    let [NameSuffix::Call(list, _)] = &name.parts[..] else {
        panic!("expected a call suffix, got {:?}", name.parts);
    };
    assert_eq!(list.elements.len(), 2);
    assert!(list.elements.iter().all(|e| e.formal.is_some()));

    let Expr::Name(name) = assigned_value(&stmts[2]) else {
        panic!("expected a name");
    };
    let [NameSuffix::Call(list, _)] = &name.parts[..] else {
        panic!("expected a call suffix, got {:?}", name.parts);
    };
    assert!(list.elements[0].formal.is_none());
    assert!(list.elements[1].formal.is_some());
}

#[test]
fn named_association_with_open_actual_and_procedure_call() {
    let ast = parse_ok(&in_process(
        "variable x : integer;",
        "x := f(a => open, b => 2);
         write(l, value => x);",
    ));
    let stmts = process_stmts(&ast);
    let Expr::Name(name) = assigned_value(&stmts[0]) else {
        panic!("expected a name");
    };
    let [NameSuffix::Call(list, _)] = &name.parts[..] else {
        panic!("expected a call suffix");
    };
    assert!(matches!(list.elements[0].actual, Expr::Open { .. }));
    assert!(matches!(
        stmts[1],
        SequentialStatement::ProcedureCall { .. }
    ));
}

#[test]
fn positional_only_calls_stay_index_suffixes() {
    let ast = parse_ok(&in_process(
        "variable x : unsigned(7 downto 0);",
        "x := to_unsigned(3, 8);",
    ));
    let Expr::Name(name) = assigned_value(&process_stmts(&ast)[0]) else {
        panic!("expected a name");
    };
    assert!(matches!(name.parts[..], [NameSuffix::Index(ref a, _)] if a.len() == 2));
}

#[test]
fn positional_after_named_association_is_rejected() {
    let (_, msgs) = parse(&in_process(
        "variable x : unsigned(7 downto 0);",
        "x := to_unsigned(arg => 3, 8);",
    ));
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("positional association"), "{msgs:?}");
}

// ---- VHDL-12: box and unconstrained arrays ------------------------------

#[test]
fn unconstrained_array_type_definitions() {
    let ast = parse_ok(
        "package p is
            type m is array (natural range <>) of bit;
            type grid_t is array (natural range <>, integer range <>) of std_logic;
            type mem_t is array (natural range <>) of std_logic_vector;
            subtype mem4_t is mem_t(0 to 3)(7 downto 0);
            subtype memx_t is mem_t(open)(15 downto 0);
            subtype rsl is resolved std_ulogic;
            subtype rslv is (resolved) std_ulogic_vector;
        end package;",
    );
    let decls = package_decls(&ast);
    let Declaration::Type(TypeDecl {
        def: TypeDef::Array { indices, .. },
        ..
    }) = &decls[0]
    else {
        panic!("expected array type");
    };
    assert!(matches!(indices[..], [DiscreteRange::Unconstrained { .. }]));
    let Declaration::Subtype(st) = &decls[3] else {
        panic!("expected subtype");
    };
    assert!(matches!(st.ty.constraint, Some(Constraint::Array { .. })));
}

#[test]
fn box_outside_array_type_definition_is_rejected() {
    let (ast, msgs) = parse(
        "package p is
            signal s : std_logic_vector(natural range <>);
            type ok_t is array (natural range <>) of bit;
        end package;",
    );
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(
        msgs[0].contains("only allowed in an array type"),
        "{msgs:?}"
    );
    assert_eq!(package_decls(&ast).len(), 2);
}

#[test]
fn mixed_constrained_and_unconstrained_indices_are_rejected() {
    let (_, msgs) = parse(
        "package p is
            type bad_t is array (natural range <>, 0 to 3) of bit;
        end package;",
    );
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("cannot mix"), "{msgs:?}");
}
