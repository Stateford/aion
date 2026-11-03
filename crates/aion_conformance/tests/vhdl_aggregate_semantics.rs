//! End-to-end tests for VHDL array aggregates with named and `others`
//! choices (IEEE 1076-2008 9.3.3.3).
//!
//! An aggregate such as `(3 => '1', others => '0')` takes its index range
//! from its context: the target of the assignment, or the declared subtype
//! of the object whose default it is. Each choice picks element positions in
//! that range (by index, honouring `to`/`downto`) and `others` fills the
//! rest. Where the elaborator cannot determine that range, or the aggregate
//! is not supported, it must report an error rather than build hardware
//! with a guessed bit layout.

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::{Diagnostic, DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::SimKernel;
use aion_source::SourceDb;

/// Parses and elaborates the VHDL `src` with top `tb`, returning the design
/// (if elaboration produced one) and every error diagnostic.
fn elaborate(src: &str) -> (Option<aion_ir::Design>, Vec<Diagnostic>, Interner) {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let file_id = source_db.add_source("tb.vhd", src.to_string());
    let parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files: vec![],
        vhdl_files: vec![aion_vhdl_parser::parse_file(
            file_id, &source_db, &interner, &sink,
        )],
    };
    let config: ProjectConfig =
        toml::from_str("[project]\nname = \"sim_test\"\nversion = \"0.1.0\"\ntop = \"tb\"\n")
            .unwrap();
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink).ok();
    let errors = sink
        .diagnostics()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    (design, errors, interner)
}

/// Elaborates `src` (which must be error-free) and simulates it to
/// completion.
fn simulate(src: &str) -> SimKernel {
    let (design, errors, interner) = elaborate(src);
    assert!(errors.is_empty(), "errors: {errors:?}");
    let design = design.expect("elaboration should not return an internal error");
    let mut kernel = SimKernel::new(&design, &interner).expect("kernel construction");
    kernel.run_to_completion().expect("simulation");
    kernel
}

/// Returns the final value of `top.<name>` as an integer (`None` if X/Z).
fn value(kernel: &SimKernel, name: &str) -> Option<u64> {
    let full = format!("top.{name}");
    let id = kernel
        .find_signal(&full)
        .unwrap_or_else(|| panic!("signal {full} not found"));
    kernel.signal_value(id).to_u64()
}

/// Wraps architecture declarations and a process body into a testbench.
fn tb(decls: &str, body: &str) -> String {
    format!(
        r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal one : std_logic := '1';
{decls}
begin
  process (one)
  begin
{body}
  end process;
end architecture;
"#
    )
}

#[test]
fn named_choice_with_others_maps_the_index_into_a_downto_target() {
    let src = tb(
        "  signal y : std_logic_vector(7 downto 0);\n  signal z : std_logic_vector(7 downto 0);",
        "    y <= (3 => '1', others => '0');\n    z <= (7 => '0', 0 => '0', others => '1');",
    );
    let k = simulate(&src);
    assert_eq!(value(&k, "y"), Some(0x08));
    assert_eq!(value(&k, "z"), Some(0x7E));
}

#[test]
fn others_fills_every_bit_of_the_target() {
    let src = tb(
        "  signal a : std_logic_vector(7 downto 0);\n  signal b : std_logic_vector(7 downto 0);\n  signal c : std_logic_vector(7 downto 0);",
        "    a <= (others => '1');\n    b <= (others => '0');\n    c <= (others => one);",
    );
    let k = simulate(&src);
    assert_eq!(value(&k, "a"), Some(0xFF), "used to be a 1-bit '1' = 0x01");
    assert_eq!(value(&k, "b"), Some(0x00));
    assert_eq!(
        value(&k, "c"),
        Some(0xFF),
        "a non-constant element is replicated"
    );
}

#[test]
fn ascending_target_puts_its_left_index_in_the_msb() {
    // For `0 to 7` the leftmost element (index 0) is the most significant
    // bit of the value, as for a string literal assigned to it.
    let src = tb(
        "  signal a : std_logic_vector(0 to 7);\n  signal p : std_logic_vector(0 to 3);",
        "    a <= (0 => '1', others => '0');\n    p <= (0 => '1', 1 => '0', 2 => '1', 3 => '1');",
    );
    let k = simulate(&src);
    assert_eq!(value(&k, "a"), Some(0x80));
    assert_eq!(value(&k, "p"), Some(0b1011));
}

#[test]
fn range_and_alternative_choices_select_positions() {
    let src = tb(
        "  signal r : std_logic_vector(7 downto 0);\n  signal m : std_logic_vector(7 downto 0);\n  signal q : std_logic_vector(7 downto 0);",
        "    r <= (7 downto 4 => '1', others => '0');\n    m <= (1 | 3 => '1', others => '0');\n    q <= (0 to 1 => '1', 6 | 7 => '1', others => '0');",
    );
    let k = simulate(&src);
    assert_eq!(value(&k, "r"), Some(0xF0));
    assert_eq!(value(&k, "m"), Some(0x0A));
    assert_eq!(value(&k, "q"), Some(0xC3));
}

#[test]
fn positional_elements_then_others() {
    let src = tb(
        "  signal y : std_logic_vector(7 downto 0);",
        "    y <= ('1', '0', others => '1');",
    );
    let k = simulate(&src);
    assert_eq!(value(&k, "y"), Some(0xBF));
}

#[test]
fn generic_index_choice_is_constant() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
  generic (BIT_POS : integer := 5);
end entity;

architecture sim of tb is
  signal one : std_logic := '1';
  signal y : std_logic_vector(7 downto 0);
begin
  process (one)
  begin
    y <= (BIT_POS => '1', others => '0');
  end process;
end architecture;
"#;
    let k = simulate(src);
    assert_eq!(value(&k, "y"), Some(0x20));
}

#[test]
fn variable_assignments_and_conditional_values_use_their_target() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal one : std_logic := '1';
  signal rst : std_logic := '1';
  signal y : std_logic_vector(7 downto 0);
  signal w : std_logic_vector(7 downto 0);
  signal c : std_logic_vector(3 downto 0);
begin
  c <= (others => '1') when rst = '1' else (others => '0');
  process (one)
    variable v : std_logic_vector(7 downto 0);
  begin
    v := (others => '1');
    y <= v;
    v := (2 => '1', others => '0');
    w <= v;
  end process;
end architecture;
"#;
    let k = simulate(src);
    assert_eq!(value(&k, "y"), Some(0xFF));
    assert_eq!(value(&k, "w"), Some(0x04));
    assert_eq!(value(&k, "c"), Some(0xF));
}

#[test]
fn named_aggregate_defaults_of_signals_constants_and_variables() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  constant K : std_logic_vector(7 downto 0) := (7 => '1', 0 => '1', others => '0');
  constant ONES : std_logic_vector(7 downto 0) := (others => '1');
  signal s : std_logic_vector(7 downto 0) := (3 downto 0 => '1', others => '0');
  signal one : std_logic := '1';
  signal y : std_logic_vector(7 downto 0);
begin
  process (one)
    variable v : std_logic_vector(0 to 3) := (0 => '1', others => '0');
  begin
    y <= K;
  end process;
end architecture;
"#;
    let k = simulate(src);
    assert_eq!(value(&k, "k"), Some(0x81));
    assert_eq!(value(&k, "ones"), Some(0xFF));
    assert_eq!(value(&k, "s"), Some(0x0F));
    assert_eq!(value(&k, "_process0.v"), Some(0x8));
    assert_eq!(value(&k, "y"), Some(0x81));
}

/// Returns the messages of every error for `src`, asserting there is at
/// least one with code `code`.
fn errors_with_code(src: &str, code: &str) -> Vec<String> {
    let (_, errors, _) = elaborate(src);
    assert!(
        errors.iter().any(|d| d.code.to_string() == code),
        "expected a {code} error, got: {errors:?}"
    );
    errors.into_iter().map(|d| d.message).collect()
}

#[test]
fn aggregate_without_a_known_index_range_is_an_error() {
    // An operand of an operator gives the aggregate no index range.
    let src = tb(
        "  signal y : std_logic_vector(7 downto 0);",
        "    y <= not (3 => '1', others => '0');",
    );
    let msgs = errors_with_code(&src, "E210");
    assert!(
        msgs[0].contains("index range cannot be determined"),
        "{msgs:?}"
    );
}

#[test]
fn non_constant_choice_is_an_error() {
    let src = tb(
        "  signal i : integer := 2;\n  signal y : std_logic_vector(7 downto 0);",
        "    y <= (i => '1', others => '0');",
    );
    let msgs = errors_with_code(&src, "E210");
    assert!(msgs[0].contains("not a constant integer index"), "{msgs:?}");
}

#[test]
fn named_aggregate_without_others_into_a_downto_target_is_an_error() {
    // Without `others`, the aggregate's direction comes from the index
    // subtype (`natural`, ascending), not from the target, so the bit order
    // would not be the one the indices suggest: this is rejected.
    let src = tb(
        "  signal y : std_logic_vector(3 downto 0);",
        "    y <= (3 => '1', 2 => '0', 1 => '0', 0 => '0');",
    );
    let msgs = errors_with_code(&src, "E210");
    assert!(msgs[0].contains("without `others`"), "{msgs:?}");
}

#[test]
fn out_of_range_duplicate_and_missing_choices_are_errors() {
    let src = tb(
        "  signal a : std_logic_vector(3 downto 0);\n  signal b : std_logic_vector(3 downto 0);\n  signal c : std_logic_vector(0 to 3);",
        "    a <= (9 => '1', others => '0');\n    b <= (1 => '1', 1 => '0', others => '0');\n    c <= (0 => '1', 1 => '0');",
    );
    let (_, errors, _) = elaborate(&src);
    let found: Vec<_> = errors
        .iter()
        .map(|d| (d.code.to_string(), d.message.as_str()))
        .collect();
    assert_eq!(found.len(), 3, "errors: {errors:?}");
    assert!(found.iter().all(|(code, _)| code == "E205"), "{found:?}");
    assert!(found[0].1.contains("index 9 is outside"), "{found:?}");
    assert!(found[1].1.contains("more than once"), "{found:?}");
    assert!(found[2].1.contains("no element for index 2"), "{found:?}");
}

#[test]
fn named_default_that_cannot_be_laid_out_warns_and_is_not_guessed() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal s : std_logic_vector(3 downto 0) := (9 => '1', others => '0');
begin
end architecture;
"#;
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let file_id = source_db.add_source("tb.vhd", src.to_string());
    let parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files: vec![],
        vhdl_files: vec![aion_vhdl_parser::parse_file(
            file_id, &source_db, &interner, &sink,
        )],
    };
    let config: ProjectConfig =
        toml::from_str("[project]\nname = \"sim_test\"\nversion = \"0.1.0\"\ntop = \"tb\"\n")
            .unwrap();
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)
        .expect("elaboration should not return an internal error");
    assert!(sink
        .diagnostics()
        .iter()
        .any(|d| d.code.to_string() == "W204"));
    let module = design.modules.iter().next().unwrap().1;
    let s = module
        .signals
        .iter()
        .find(|(_, s)| interner.resolve(s.name) == "s")
        .unwrap()
        .1;
    assert!(s.init.is_none(), "the default must not be guessed");
}

#[test]
fn selected_case_and_conditional_assignments_to_ports_of_generic_width() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
  generic (W : integer := 6);
  port (q : out std_logic_vector(W - 1 downto 0);
        r : out std_logic_vector(W - 1 downto 0));
end entity;

architecture sim of tb is
  signal sel : std_logic := '1';
  signal d : std_logic_vector(3 downto 0);
begin
  with sel select q <= (others => '0') when '0', (W - 1 => '1', 0 => '1', others => '0') when others;
  process (sel)
  begin
    case sel is
      when '1' => r <= (1 => '1', others => '0');
      when others => r <= (others => '0');
    end case;
  end process;
  process
  begin
    d <= (0 => '1', others => '0') when sel = '1' else (others => '1');
    wait;
  end process;
end architecture;
"#;
    let k = simulate(src);
    assert_eq!(value(&k, "q"), Some(0b100001));
    assert_eq!(value(&k, "r"), Some(0b000010));
    assert_eq!(value(&k, "d"), Some(0b0001));
}
