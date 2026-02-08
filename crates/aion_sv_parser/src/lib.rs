//! Hand-rolled recursive descent parser for SystemVerilog-2017.
//!
//! This crate provides a SystemVerilog-2017 parser targeting the synthesizable
//! subset, with error recovery and source spans on all AST nodes. The main
//! entry point is [`parse_file`], which takes a source file and returns an
//! [`SvSourceFile`].
//!
//! # Architecture
//!
//! - **Lexer** ([`lexer`]): Converts source text to tokens, handling
//!   case-sensitive keywords, SV operators, sized/based literals, and comments.
//! - **Parser** ([`parser`]): Recursive descent parser with Pratt expression
//!   parsing and error recovery via poison nodes.
//! - **AST** ([`ast`]): All AST node types with spans and serde support.
//!
//! # Scope
//!
//! Targets the synthesizable subset of IEEE 1800-2017:
//! - Data types: `logic`, `bit`, `byte`, `int`, `longint`, `enum`, `struct packed`, `typedef`
//! - Always blocks: `always_comb`, `always_ff`, `always_latch`
//! - Packages: `package ... endpackage`, `import pkg::*`
//! - Interfaces: `interface ... endinterface`, `modport`
//! - Enhanced operators: `inside`, `==?`/`!=?`, `++`/`--`, `+=`/`-=`, `::`
//! - `unique`/`priority` case/if, `for (int i = 0; ...)`, `return`/`break`/`continue`

#![warn(missing_docs)]

/// AST node types for the SystemVerilog-2017 parser.
pub mod ast;
mod decl;
mod expr;
/// Lexical analyzer for SystemVerilog-2017 source text.
pub mod lexer;
/// Recursive descent parser for SystemVerilog-2017 with error recovery.
pub mod parser;
mod stmt;
/// Token types for the SystemVerilog-2017 lexer.
pub mod token;

pub use ast::SvSourceFile;
pub use token::{SvToken, Token};

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_source::{FileId, SourceDb};

/// Parses a SystemVerilog source file into an AST.
///
/// Lexes the source text and parses it into an [`SvSourceFile`]. Errors are
/// reported to the diagnostic sink and represented as `Error` variants in the
/// AST for downstream processing.
pub fn parse_file(
    file_id: FileId,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> SvSourceFile {
    let file = source_db.get_file(file_id);
    let source = &file.content;
    let tokens = lexer::lex(source, file_id, sink);
    let mut parser = parser::SvParser::new(tokens, source, file_id, interner, sink);
    parser.parse_source_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::SourceDb;

    fn parse_source(source: &str) -> (SvSourceFile, Vec<aion_diagnostics::Diagnostic>) {
        let mut db = SourceDb::new();
        let file_id = db.add_source("test.sv", source.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let ast = parse_file(file_id, &db, &interner, &sink);
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> SvSourceFile {
        let (ast, errors) = parse_source(source);
        assert!(
            errors.is_empty(),
            "unexpected errors: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        ast
    }

    #[test]
    fn integration_counter() {
        let ast = parse_ok(
            "module counter #(parameter int WIDTH = 8)(
                input logic clk,
                input logic rst,
                input logic en,
                output logic [WIDTH-1:0] count
            );
                always_ff @(posedge clk or negedge rst) begin
                    if (!rst)
                        count <= 0;
                    else if (en)
                        count <= count + 1;
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.port_style, ast::PortStyle::Ansi);
            assert_eq!(m.params.len(), 1);
            assert_eq!(m.ports.len(), 4);
            assert_eq!(m.items.len(), 1);
        } else {
            panic!("expected module");
        }
    }

    #[test]
    fn integration_mux_always_comb() {
        let ast = parse_ok(
            "module mux4(
                input logic [7:0] a, b, c, d,
                input logic [1:0] sel,
                output logic [7:0] y
            );
                always_comb begin
                    case (sel)
                        2'b00: y = a;
                        2'b01: y = b;
                        2'b10: y = c;
                        default: y = d;
                    endcase
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 1);
        }
    }

    #[test]
    fn integration_fsm_with_enum() {
        let ast = parse_ok(
            "module fsm(
                input logic clk, rst,
                input logic start,
                output logic done
            );
                typedef enum logic [1:0] {IDLE, RUN, STOP} state_t;
                state_t state;

                always_ff @(posedge clk or negedge rst) begin
                    if (!rst) begin
                        state <= IDLE;
                        done <= 0;
                    end else begin
                        unique case (state)
                            IDLE: begin
                                done <= 0;
                                if (start) state <= RUN;
                            end
                            RUN: state <= STOP;
                            STOP: begin
                                done <= 1;
                                state <= IDLE;
                            end
                        endcase
                    end
                end
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            // typedef + state_t var(instantiation) + always_ff
            assert!(m.items.len() >= 2);
        }
    }

    #[test]
    fn integration_package_and_import() {
        let ast = parse_ok(
            "package my_pkg;
                parameter int WIDTH = 8;
                typedef logic [WIDTH-1:0] data_t;
                function int max(input int a, input int b);
                    return (a > b) ? a : b;
                endfunction
            endpackage

            module top;
                import my_pkg::*;
                logic [7:0] data;
            endmodule",
        );
        assert_eq!(ast.items.len(), 2);
        assert!(matches!(ast.items[0], ast::SvItem::Package(_)));
        assert!(matches!(ast.items[1], ast::SvItem::Module(_)));
        if let ast::SvItem::Package(ref p) = ast.items[0] {
            assert_eq!(p.items.len(), 3);
        }
        if let ast::SvItem::Module(ref m) = ast.items[1] {
            assert_eq!(m.items.len(), 2); // import + logic
        }
    }

    #[test]
    fn integration_interface_with_modport() {
        let ast = parse_ok(
            "interface axi_if;
                logic valid;
                logic ready;
                logic [31:0] data;

                modport master(output valid, output data, input ready);
                modport slave(input valid, input data, output ready);
            endinterface

            module producer(axi_if.master bus);
            endmodule",
        );
        assert_eq!(ast.items.len(), 2);
        assert!(matches!(ast.items[0], ast::SvItem::Interface(_)));
        if let ast::SvItem::Interface(ref iface) = ast.items[0] {
            assert!(iface.items.len() >= 5); // 3 logic + 2 modport
        }
    }

    #[test]
    fn integration_struct_packed() {
        let ast = parse_ok(
            "module top;
                typedef struct packed {
                    logic [7:0] data;
                    logic valid;
                    logic ready;
                } packet_t;

                packet_t pkt;
                always_comb begin
                    pkt.data = 8'hFF;
                    pkt.valid = 1;
                    pkt.ready = 0;
                end
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert!(m.items.len() >= 2); // typedef + instantiation + always_comb
        }
    }

    #[test]
    fn integration_for_loop_with_int() {
        let ast = parse_ok(
            "module shift_reg #(parameter int N = 8)(
                input logic clk,
                input logic din,
                output logic [N-1:0] data
            );
                always_ff @(posedge clk) begin
                    for (int i = N - 1; i > 0; i--)
                        data[i] <= data[i - 1];
                    data[0] <= din;
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 1);
        }
    }

    #[test]
    fn integration_always_latch() {
        let ast = parse_ok(
            "module latch(
                input logic en,
                input logic [7:0] d,
                output logic [7:0] q
            );
                always_latch
                    if (en) q <= d;
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 1);
            assert!(matches!(m.items[0], ast::ModuleItem::AlwaysLatch(_)));
        }
    }

    #[test]
    fn integration_end_labels() {
        let ast = parse_ok(
            "module top;
            endmodule : top

            interface bus_if;
            endinterface : bus_if

            package my_pkg;
            endpackage : my_pkg",
        );
        assert_eq!(ast.items.len(), 3);
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert!(m.end_label.is_some());
        }
        if let ast::SvItem::Interface(ref i) = ast.items[1] {
            assert!(i.end_label.is_some());
        }
        if let ast::SvItem::Package(ref p) = ast.items[2] {
            assert!(p.end_label.is_some());
        }
    }

    #[test]
    fn integration_compound_assignments() {
        let ast = parse_ok(
            "module arith;
                int a;
                always_comb begin
                    a = 0;
                    a += 5;
                    a -= 2;
                    a *= 3;
                end
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert!(m.items.len() >= 2);
        }
    }

    #[test]
    fn integration_import_named() {
        let ast = parse_ok(
            "package defs;
                parameter int WIDTH = 16;
            endpackage

            module user;
                import defs::WIDTH;
                logic [WIDTH-1:0] data;
            endmodule",
        );
        assert_eq!(ast.items.len(), 2);
        if let ast::SvItem::Module(ref m) = ast.items[1] {
            assert_eq!(m.items.len(), 2);
        }
    }

    #[test]
    fn integration_non_ansi_ports() {
        let ast = parse_ok(
            "module counter(clk, rst, count);
                input logic clk;
                input logic rst;
                output logic [7:0] count;

                always_ff @(posedge clk)
                    if (rst) count <= 0;
                    else count <= count + 1;
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.port_style, ast::PortStyle::NonAnsi);
            assert_eq!(m.port_names.len(), 3);
        }
    }

    #[test]
    fn integration_error_recovery() {
        let (ast, errors) = parse_source(
            "module bad;
                wire ; // missing name
            endmodule

            module good(input logic clk);
                logic [7:0] data;
            endmodule",
        );
        assert!(ast.items.len() >= 2);
        assert!(!errors.is_empty());
    }

    #[test]
    fn integration_serde_roundtrip() {
        let ast = parse_ok(
            "module top(input logic clk, output logic [7:0] data);
                assign data = 8'hFF;
            endmodule",
        );
        let json = serde_json::to_string(&ast).unwrap();
        let back: SvSourceFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items.len(), ast.items.len());
    }

    #[test]
    fn integration_generate_with_always_ff() {
        let ast = parse_ok(
            "module gen_test #(parameter int N = 4)(
                input logic clk,
                input logic [N-1:0] din,
                output logic [N-1:0] dout
            );
                genvar i;
                generate
                    for (i = 0; i < N; i = i + 1) begin : gen_ff
                        always_ff @(posedge clk)
                            dout[i] <= din[i];
                    end
                endgenerate
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 2); // genvar + generate
        }
    }

    #[test]
    fn integration_function_with_return() {
        let ast = parse_ok(
            "module top;
                function int abs(input int val);
                    return (val < 0) ? -val : val;
                endfunction : abs

                int result;
                always_comb begin
                    result = abs(-42);
                end
            endmodule",
        );
        if let ast::SvItem::Module(ref m) = ast.items[0] {
            assert!(m.items.len() >= 2);
        }
    }
}
