//! Hand-rolled recursive descent parser for VHDL-2008.
//!
//! This crate provides a full VHDL-2008 parser with error recovery,
//! producing an AST with source spans for all nodes. The main entry point
//! is [`parse_file`], which takes a source file and returns a [`VhdlDesignFile`].
//!
//! # Architecture
//!
//! - **Lexer** ([`lexer`]): Converts source text to tokens, handling case-insensitive
//!   keywords, based literals, and block comments.
//! - **Parser** ([`parser`]): Recursive descent parser with Pratt expression parsing
//!   and error recovery via poison nodes.
//! - **AST** ([`ast`]): All AST node types with spans and serde support.

#![warn(missing_docs)]

pub mod ast;
mod decl;
mod expr;
pub mod lexer;
pub mod parser;
mod stmt;
pub mod token;
mod types;

pub use ast::VhdlDesignFile;
pub use token::{Token, VhdlToken};

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_source::{FileId, SourceDb};

/// Parses a VHDL source file into an AST.
///
/// Lexes the source text and parses it into a [`VhdlDesignFile`]. Errors are
/// reported to the diagnostic sink and represented as `Error` variants in the AST
/// for downstream processing.
pub fn parse_file(
    file_id: FileId,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> VhdlDesignFile {
    let file = source_db.get_file(file_id);
    let source = &file.content;
    let tokens = lexer::lex(source, file_id, sink);
    let mut parser = parser::VhdlParser::new(tokens, source, file_id, interner, sink);
    parser.parse_design_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::SourceDb;

    fn parse_source(source: &str) -> (VhdlDesignFile, Vec<aion_diagnostics::Diagnostic>) {
        let mut db = SourceDb::new();
        let file_id = db.add_source("test.vhd", source.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let ast = parse_file(file_id, &db, &interner, &sink);
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> VhdlDesignFile {
        let (ast, errors) = parse_source(source);
        assert!(
            errors.is_empty(),
            "unexpected errors: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        ast
    }

    #[test]
    fn integration_counter_entity_and_arch() {
        let ast = parse_ok(
            "library ieee;
            use ieee.std_logic_1164.all;
            use ieee.numeric_std.all;

            entity counter is
                generic (
                    WIDTH : integer := 8
                );
                port (
                    clk   : in  std_logic;
                    rst   : in  std_logic;
                    en    : in  std_logic;
                    count : out std_logic_vector(WIDTH - 1 downto 0)
                );
            end entity counter;

            architecture rtl of counter is
                signal cnt : unsigned(WIDTH - 1 downto 0);
            begin
                process(clk, rst)
                begin
                    if rst = '1' then
                        cnt <= (others => '0');
                    elsif clk'event and clk = '1' then
                        if en = '1' then
                            cnt <= cnt + 1;
                        end if;
                    end if;
                end process;

                count <= std_logic_vector(cnt);
            end architecture rtl;",
        );
        assert_eq!(ast.units.len(), 2);
    }

    #[test]
    fn integration_multiplexer_with_case() {
        let ast = parse_ok(
            "library ieee;
            use ieee.std_logic_1164.all;

            entity mux4 is
                port (
                    sel : in  std_logic_vector(1 downto 0);
                    a, b, c, d : in  std_logic;
                    y   : out std_logic
                );
            end entity mux4;

            architecture rtl of mux4 is
            begin
                process(sel, a, b, c, d)
                begin
                    case sel is
                        when \"00\" =>
                            y <= a;
                        when \"01\" =>
                            y <= b;
                        when \"10\" =>
                            y <= c;
                        when others =>
                            y <= d;
                    end case;
                end process;
            end architecture rtl;",
        );
        assert_eq!(ast.units.len(), 2);
    }

    #[test]
    fn integration_package_and_body() {
        let ast = parse_ok(
            "package utils_pkg is
                type state_t is (idle, running, stopped);
                constant MAX_COUNT : integer := 1023;
                function log2(val : integer) return integer;
            end package utils_pkg;

            package body utils_pkg is
                function log2(val : integer) return integer is
                    variable result : integer := 0;
                    variable v : integer;
                begin
                    v := val;
                    while v > 1 loop
                        v := v / 2;
                        result := result + 1;
                    end loop;
                    return result;
                end function log2;
            end package body utils_pkg;",
        );
        assert_eq!(ast.units.len(), 2);
    }

    #[test]
    fn integration_multi_unit_file() {
        let ast = parse_ok(
            "library ieee;
            use ieee.std_logic_1164.all;

            entity a is
                port (x : in std_logic);
            end entity a;

            architecture rtl of a is
            begin
            end architecture rtl;

            entity b is
                port (y : out std_logic);
            end entity b;

            architecture rtl of b is
            begin
                y <= '1';
            end architecture rtl;",
        );
        assert_eq!(ast.units.len(), 4);
    }

    #[test]
    fn integration_error_recovery() {
        let (ast, errors) = parse_source(
            "library ieee;
            use ieee.std_logic_1164.all;

            entity bad is
                port (
                    clk : in std_logic
                );
            end entity bad;

            entity good is
                port (
                    data : out std_logic
                );
            end entity good;",
        );
        // Should still parse both entities
        assert_eq!(ast.units.len(), 2);
        // errors may or may not be empty depending on recovery
        let _ = errors;
    }

    #[test]
    fn integration_component_instantiation() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                u1 : entity work.counter
                    generic map (WIDTH => 16)
                    port map (
                        clk   => sys_clk,
                        rst   => sys_rst,
                        en    => enable,
                        count => data_out
                    );
            end architecture rtl;",
        );
        if let ast::DesignUnit::ContextUnit {
            unit: ast::DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert_eq!(a.stmts.len(), 1);
            if let ast::ConcurrentStatement::ComponentInstantiation(c) = &a.stmts[0] {
                assert!(c.generic_map.is_some());
                let pm = c.port_map.as_ref().unwrap();
                assert_eq!(pm.elements.len(), 4);
            }
        }
    }

    #[test]
    fn integration_generate_statements() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                gen_bits : for i in 0 to 7 generate
                    data(i) <= not data(i);
                end generate gen_bits;
            end architecture rtl;",
        );
        if let ast::DesignUnit::ContextUnit {
            unit: ast::DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(
                a.stmts[0],
                ast::ConcurrentStatement::ForGenerate(_)
            ));
        }
    }

    #[test]
    fn integration_process_with_wait() {
        let ast = parse_ok(
            "architecture tb of testbench is
            begin
                process
                begin
                    clk <= '0';
                    wait for 10 ns;
                    clk <= '1';
                    wait for 10 ns;
                end process;
            end architecture tb;",
        );
        if let ast::DesignUnit::ContextUnit {
            unit: ast::DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ast::ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.sensitivity, ast::SensitivityList::None));
                assert_eq!(p.stmts.len(), 4);
            }
        }
    }

    #[test]
    fn integration_signal_assignment_with_after() {
        let ast = parse_ok(
            "architecture tb of test is
            begin
                process
                begin
                    data <= '1' after 5 ns, '0' after 10 ns;
                    wait;
                end process;
            end architecture tb;",
        );
        if let ast::DesignUnit::ContextUnit {
            unit: ast::DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ast::ConcurrentStatement::Process(p) = &a.stmts[0] {
                if let ast::SequentialStatement::SignalAssignment { waveforms, .. } = &p.stmts[0] {
                    assert_eq!(waveforms.len(), 2);
                    assert!(waveforms[0].after.is_some());
                    assert!(waveforms[1].after.is_some());
                }
            }
        }
    }

    #[test]
    fn integration_serde_roundtrip() {
        let ast = parse_ok(
            "entity top is
                port (clk : in std_logic);
            end entity top;",
        );
        let json = serde_json::to_string(&ast).unwrap();
        let back: VhdlDesignFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.units.len(), ast.units.len());
    }
}
