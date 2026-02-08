//! Hand-rolled recursive descent parser for Verilog-2005.
//!
//! This crate provides a full Verilog-2005 parser with error recovery,
//! producing an AST with source spans for all nodes. The main entry point
//! is [`parse_file`], which takes a source file and returns a [`VerilogSourceFile`].
//!
//! # Architecture
//!
//! - **Lexer** ([`lexer`]): Converts source text to tokens, handling case-sensitive
//!   keywords, sized/based literals, and line/block comments.
//! - **Parser** ([`parser`]): Recursive descent parser with Pratt expression parsing
//!   and error recovery via poison nodes.
//! - **AST** ([`ast`]): All AST node types with spans and serde support.

#![warn(missing_docs)]

/// AST node types for the Verilog-2005 parser.
pub mod ast;
mod decl;
mod expr;
/// Lexical analyzer for Verilog-2005 source text.
pub mod lexer;
/// Recursive descent parser for Verilog-2005 with error recovery.
pub mod parser;
mod stmt;
/// Token types for the Verilog-2005 lexer.
pub mod token;

pub use ast::VerilogSourceFile;
pub use token::{Token, VerilogToken};

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_source::{FileId, SourceDb};

/// Parses a Verilog source file into an AST.
///
/// Lexes the source text and parses it into a [`VerilogSourceFile`]. Errors are
/// reported to the diagnostic sink and represented as `Error` variants in the AST
/// for downstream processing.
pub fn parse_file(
    file_id: FileId,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> VerilogSourceFile {
    let file = source_db.get_file(file_id);
    let source = &file.content;
    let tokens = lexer::lex(source, file_id, sink);
    let mut parser = parser::VerilogParser::new(tokens, source, file_id, interner, sink);
    parser.parse_source_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::SourceDb;

    fn parse_source(source: &str) -> (VerilogSourceFile, Vec<aion_diagnostics::Diagnostic>) {
        let mut db = SourceDb::new();
        let file_id = db.add_source("test.v", source.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let ast = parse_file(file_id, &db, &interner, &sink);
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> VerilogSourceFile {
        let (ast, errors) = parse_source(source);
        assert!(
            errors.is_empty(),
            "unexpected errors: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        ast
    }

    #[test]
    fn integration_counter_module() {
        let ast = parse_ok(
            "module counter #(parameter WIDTH = 8)(
                input wire clk,
                input wire rst,
                input wire en,
                output reg [WIDTH-1:0] count
            );
                always @(posedge clk or negedge rst) begin
                    if (!rst)
                        count <= 0;
                    else if (en)
                        count <= count + 1;
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.port_style, ast::PortStyle::Ansi);
            assert_eq!(m.params.len(), 1);
            assert_eq!(m.ports.len(), 4);
            assert_eq!(m.items.len(), 1); // always block
        } else {
            panic!("expected module");
        }
    }

    #[test]
    fn integration_mux4() {
        let ast = parse_ok(
            "module mux4(
                input wire [7:0] a, b, c, d,
                input wire [1:0] sel,
                output reg [7:0] y
            );
                always @(*) begin
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
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 1); // always block
        }
    }

    #[test]
    fn integration_shift_register() {
        let ast = parse_ok(
            "module shift_reg #(parameter N = 8)(
                input wire clk,
                input wire din,
                output wire dout
            );
                reg [N-1:0] sr;

                always @(posedge clk)
                    sr <= {sr[N-2:0], din};

                assign dout = sr[N-1];
            endmodule",
        );
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 3); // reg, always, assign
        }
    }

    #[test]
    fn integration_alu() {
        let ast = parse_ok(
            "module alu(
                input wire [7:0] a, b,
                input wire [2:0] op,
                output reg [7:0] result,
                output reg zero
            );
                always @(*) begin
                    case (op)
                        3'b000: result = a + b;
                        3'b001: result = a - b;
                        3'b010: result = a & b;
                        3'b011: result = a | b;
                        3'b100: result = a ^ b;
                        3'b101: result = ~a;
                        3'b110: result = a << 1;
                        3'b111: result = a >> 1;
                        default: result = 0;
                    endcase
                    zero = (result == 0) ? 1 : 0;
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
    }

    #[test]
    fn integration_ram() {
        let ast = parse_ok(
            "module ram #(parameter DEPTH = 256, parameter WIDTH = 8)(
                input wire clk,
                input wire we,
                input wire [7:0] addr,
                input wire [WIDTH-1:0] din,
                output reg [WIDTH-1:0] dout
            );
                reg [WIDTH-1:0] mem [0:DEPTH-1];

                always @(posedge clk) begin
                    if (we)
                        mem[addr] <= din;
                    dout <= mem[addr];
                end
            endmodule",
        );
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 2); // reg with array dims, always block
        }
    }

    #[test]
    fn integration_fsm() {
        let ast = parse_ok(
            "module fsm(
                input wire clk, rst,
                input wire start,
                output reg done
            );
                localparam IDLE = 2'b00;
                localparam RUN  = 2'b01;
                localparam DONE = 2'b10;

                reg [1:0] state;

                always @(posedge clk or negedge rst) begin
                    if (!rst) begin
                        state <= IDLE;
                        done <= 0;
                    end else begin
                        case (state)
                            IDLE: begin
                                done <= 0;
                                if (start)
                                    state <= RUN;
                            end
                            RUN:
                                state <= DONE;
                            DONE: begin
                                done <= 1;
                                state <= IDLE;
                            end
                            default:
                                state <= IDLE;
                        endcase
                    end
                end
            endmodule",
        );
        assert_eq!(ast.items.len(), 1);
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            // 3 localparams + 1 reg + 1 always
            assert_eq!(m.items.len(), 5);
        }
    }

    #[test]
    fn integration_generate_for() {
        let ast = parse_ok(
            "module gen_test #(parameter N = 8)(
                input wire [N-1:0] a,
                output wire [N-1:0] b
            );
                genvar i;
                generate
                    for (i = 0; i < N; i = i + 1) begin : inv_gen
                        assign b[i] = ~a[i];
                    end
                endgenerate
            endmodule",
        );
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            assert_eq!(m.items.len(), 2); // genvar + generate
        }
    }

    #[test]
    fn integration_testbench() {
        let ast = parse_ok(
            "module tb;
                reg clk;
                reg [7:0] data;
                wire [7:0] result;

                initial begin
                    clk = 0;
                    data = 8'h00;
                    #100 $finish;
                end

                initial forever #5 clk = ~clk;

                initial begin
                    @(posedge clk) ;
                    data = 8'hFF;
                    @(posedge clk) ;
                    data = 8'hAA;
                    $display(\"data = %h\", data);
                end
            endmodule",
        );
        if let ast::VerilogItem::Module(ref m) = ast.items[0] {
            // 3 regs/wires + 3 initial blocks
            assert_eq!(m.items.len(), 6);
        }
    }

    #[test]
    fn integration_instantiation_chain() {
        let ast = parse_ok(
            "module sub(input wire a, output wire b);
                assign b = ~a;
            endmodule

            module top(input wire x, output wire y);
                wire mid;
                sub u1(.a(x), .b(mid));
                sub u2(.a(mid), .b(y));
            endmodule",
        );
        assert_eq!(ast.items.len(), 2);
        if let ast::VerilogItem::Module(ref m) = &ast.items[1] {
            // wire + 2 instantiations
            assert_eq!(m.items.len(), 3);
            assert!(matches!(m.items[1], ast::ModuleItem::Instantiation(_)));
        }
    }

    #[test]
    fn integration_multi_module() {
        let ast = parse_ok(
            "module a(input wire x, output wire y);
                assign y = x;
            endmodule

            module b(input wire x, output wire y);
                assign y = ~x;
            endmodule

            module c(input wire x, output wire y);
                wire mid;
                a u1(.x(x), .y(mid));
                b u2(.x(mid), .y(y));
            endmodule",
        );
        assert_eq!(ast.items.len(), 3);
    }

    #[test]
    fn integration_error_recovery() {
        let (ast, errors) = parse_source(
            "module bad;
                wire ; // missing name
            endmodule

            module good(input wire clk);
                reg [7:0] data;
            endmodule",
        );
        // Should still parse at least 2 modules
        assert!(ast.items.len() >= 2);
        assert!(!errors.is_empty());
    }

    #[test]
    fn integration_serde_roundtrip() {
        let ast = parse_ok(
            "module top(input wire clk, output wire [7:0] data);
                assign data = 8'hFF;
            endmodule",
        );
        let json = serde_json::to_string(&ast).unwrap();
        let back: VerilogSourceFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items.len(), ast.items.len());
    }
}
