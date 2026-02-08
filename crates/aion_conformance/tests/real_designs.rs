//! Real-world HDL design conformance tests.
//!
//! These tests exercise the full parse → elaborate → lint pipeline on
//! realistic, production-style HDL designs that engineers actually write.
//! Each design represents a common FPGA building block (UART, SPI, FIFO,
//! CPU, DSP filter, etc.) and verifies that the toolchain handles them
//! without errors.

use aion_conformance::{full_pipeline_sv, full_pipeline_verilog, full_pipeline_vhdl};

// ============================================================================
// Category 5: Hierarchy, Stress, and Edge Cases
// ============================================================================

#[test]
fn verilog_five_level_hierarchy() {
    let src = r#"
module leaf (
    input  a,
    output b
);
    assign b = ~a;
endmodule

module level1 (
    input  x,
    output y
);
    leaf u0 (.a(x), .b(y));
endmodule

module level2 (
    input  x,
    output y
);
    level1 u0 (.x(x), .y(y));
endmodule

module level3 (
    input  x,
    output y
);
    level2 u0 (.x(x), .y(y));
endmodule

module top (
    input  in_sig,
    output out_sig
);
    level3 u0 (.x(in_sig), .y(out_sig));
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    assert_eq!(result.design.module_count(), 5);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 2);
    assert!(!top.cells.is_empty(), "should have instantiation");
}

#[test]
fn verilog_many_signals() {
    // A module with 32 inputs, 32 outputs, and pipeline registers
    let src = r#"
module wide_datapath (
    input  clk,
    input  [31:0] a0, a1, a2, a3, a4, a5, a6, a7,
    input  [31:0] a8, a9, a10, a11, a12, a13, a14, a15,
    input  [31:0] b0, b1, b2, b3, b4, b5, b6, b7,
    input  [31:0] b8, b9, b10, b11, b12, b13, b14, b15,
    output reg [31:0] s0, s1, s2, s3, s4, s5, s6, s7,
    output reg [31:0] s8, s9, s10, s11, s12, s13, s14, s15
);
    always @(posedge clk) begin
        s0  <= a0  + b0;
        s1  <= a1  + b1;
        s2  <= a2  + b2;
        s3  <= a3  + b3;
        s4  <= a4  + b4;
        s5  <= a5  + b5;
        s6  <= a6  + b6;
        s7  <= a7  + b7;
        s8  <= a8  + b8;
        s9  <= a9  + b9;
        s10 <= a10 + b10;
        s11 <= a11 + b11;
        s12 <= a12 + b12;
        s13 <= a13 + b13;
        s14 <= a14 + b14;
        s15 <= a15 + b15;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "wide_datapath");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    // clk + 16 a-inputs + 16 b-inputs + 16 s-outputs = 49
    assert_eq!(top.ports.len(), 49);
    assert_eq!(top.processes.len(), 1);
}

#[test]
fn verilog_many_instantiations() {
    let src = r#"
module inverter (
    input  a,
    output b
);
    assign b = ~a;
endmodule

module top (
    input  [7:0] din,
    output [7:0] dout
);
    inverter u0 (.a(din[0]), .b(dout[0]));
    inverter u1 (.a(din[1]), .b(dout[1]));
    inverter u2 (.a(din[2]), .b(dout[2]));
    inverter u3 (.a(din[3]), .b(dout[3]));
    inverter u4 (.a(din[4]), .b(dout[4]));
    inverter u5 (.a(din[5]), .b(dout[5]));
    inverter u6 (.a(din[6]), .b(dout[6]));
    inverter u7 (.a(din[7]), .b(dout[7]));
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 2);
    assert_eq!(top.cells.len(), 8);
}

#[test]
fn sv_crossbar_2x2() {
    let src = r#"
module crossbar_2x2 (
    input  logic [7:0] in0,
    input  logic [7:0] in1,
    input  logic       sel,
    output logic [7:0] out0,
    output logic [7:0] out1
);
    always_comb begin
        if (sel) begin
            out0 = in1;
            out1 = in0;
        end else begin
            out0 = in0;
            out1 = in1;
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "crossbar_2x2");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
}

#[test]
fn sv_multi_always_module() {
    let src = r#"
module multi_always (
    input  logic       clk,
    input  logic       rst,
    input  logic [7:0] din,
    output logic [7:0] pipe1,
    output logic [7:0] pipe2,
    output logic [7:0] sum,
    output logic       valid
);
    always_ff @(posedge clk) begin
        if (rst)
            pipe1 <= 8'h00;
        else
            pipe1 <= din;
    end

    always_ff @(posedge clk) begin
        if (rst)
            pipe2 <= 8'h00;
        else
            pipe2 <= pipe1;
    end

    always_comb begin
        sum = pipe1 + pipe2;
    end

    always_comb begin
        valid = (sum != 8'h00) ? 1'b1 : 1'b0;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "multi_always");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
    assert!(top.processes.len() >= 4, "should have 4 always blocks");
}

#[test]
fn sv_empty_module() {
    let src = "module empty_mod; endmodule";
    let result = full_pipeline_sv(src, "empty_mod");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 0);
    assert_eq!(top.processes.len(), 0);
}

#[test]
fn sv_deep_generate() {
    // Nested generate-for creating a reduction tree
    let src = r#"
module reduce_or (
    input  logic [7:0] din,
    output logic       result
);
    wire [7:0] stage0;
    wire [3:0] stage1;
    wire [1:0] stage2;

    assign stage0 = din;

    genvar i;
    generate
        for (i = 0; i < 4; i = i + 1) begin : gen_s1
            assign stage1[i] = stage0[2*i] | stage0[2*i+1];
        end
    endgenerate

    genvar j;
    generate
        for (j = 0; j < 2; j = j + 1) begin : gen_s2
            assign stage2[j] = stage1[2*j] | stage1[2*j+1];
        end
    endgenerate

    assign result = stage2[0] | stage2[1];
endmodule
"#;
    let result = full_pipeline_sv(src, "reduce_or");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 2);
}

#[test]
fn vhdl_four_entity_hierarchy() {
    let src = r#"
entity leaf is
    port (
        a : in  std_logic;
        b : out std_logic
    );
end entity leaf;

architecture rtl of leaf is
begin
    b <= not a;
end architecture rtl;

entity mid1 is
    port (
        x : in  std_logic;
        y : out std_logic
    );
end entity mid1;

architecture rtl of mid1 is
    component leaf is
        port (
            a : in  std_logic;
            b : out std_logic
        );
    end component;
begin
    u0: leaf port map (a => x, b => y);
end architecture rtl;

entity mid2 is
    port (
        x : in  std_logic;
        y : out std_logic
    );
end entity mid2;

architecture rtl of mid2 is
    component mid1 is
        port (
            x : in  std_logic;
            y : out std_logic
        );
    end component;
begin
    u0: mid1 port map (x => x, y => y);
end architecture rtl;

entity top is
    port (
        inp  : in  std_logic;
        outp : out std_logic
    );
end entity top;

architecture rtl of top is
    component mid2 is
        port (
            x : in  std_logic;
            y : out std_logic
        );
    end component;
begin
    u0: mid2 port map (x => inp, y => outp);
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "top");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    assert_eq!(result.design.module_count(), 4);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 2);
}

#[test]
fn vhdl_many_concurrent_assigns() {
    let src = r#"
entity assign_heavy is
    port (
        a : in  std_logic_vector(15 downto 0);
        b : in  std_logic_vector(15 downto 0);
        s : out std_logic_vector(15 downto 0)
    );
end entity assign_heavy;

architecture rtl of assign_heavy is
begin
    s(0)  <= a(0)  xor b(0);
    s(1)  <= a(1)  xor b(1);
    s(2)  <= a(2)  xor b(2);
    s(3)  <= a(3)  xor b(3);
    s(4)  <= a(4)  xor b(4);
    s(5)  <= a(5)  xor b(5);
    s(6)  <= a(6)  xor b(6);
    s(7)  <= a(7)  xor b(7);
    s(8)  <= a(8)  and b(8);
    s(9)  <= a(9)  and b(9);
    s(10) <= a(10) and b(10);
    s(11) <= a(11) and b(11);
    s(12) <= a(12) or  b(12);
    s(13) <= a(13) or  b(13);
    s(14) <= a(14) or  b(14);
    s(15) <= a(15) or  b(15);
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "assign_heavy");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
    assert!(
        top.assignments.len() >= 16,
        "should have 16 concurrent assignments, got {}",
        top.assignments.len()
    );
}

#[test]
fn vhdl_generic_chain() {
    let src = r#"
entity adder is
    generic (
        WIDTH : integer := 8
    );
    port (
        a : in  std_logic_vector(WIDTH-1 downto 0);
        b : in  std_logic_vector(WIDTH-1 downto 0);
        s : out std_logic_vector(WIDTH-1 downto 0)
    );
end entity adder;

architecture rtl of adder is
begin
    s <= a;
end architecture rtl;

entity wrapper is
    port (
        x : in  std_logic_vector(7 downto 0);
        y : in  std_logic_vector(7 downto 0);
        z : out std_logic_vector(7 downto 0)
    );
end entity wrapper;

architecture rtl of wrapper is
    component adder is
        generic (
            WIDTH : integer := 8
        );
        port (
            a : in  std_logic_vector(WIDTH-1 downto 0);
            b : in  std_logic_vector(WIDTH-1 downto 0);
            s : out std_logic_vector(WIDTH-1 downto 0)
        );
    end component;
begin
    u0: adder generic map (WIDTH => 8) port map (a => x, b => y, s => z);
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "wrapper");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    assert_eq!(result.design.module_count(), 2);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
}

// ============================================================================
// Category 1: Communication Interfaces
// ============================================================================

#[test]
fn verilog_uart_transmitter() {
    let src = r#"
module uart_tx (
    input        clk,
    input        rst_n,
    input        tx_start,
    input  [7:0] tx_data,
    output reg   tx_out,
    output reg   tx_busy
);
    reg [2:0] state;
    reg [15:0] clk_cnt;
    reg [2:0]  bit_idx;
    reg [7:0]  shift_reg;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state     <= 3'b000;
            tx_out    <= 1'b1;
            tx_busy   <= 1'b0;
            clk_cnt   <= 16'h0000;
            bit_idx   <= 3'b000;
            shift_reg <= 8'h00;
        end else begin
            case (state)
                3'b000: begin
                    tx_out  <= 1'b1;
                    tx_busy <= 1'b0;
                    if (tx_start) begin
                        shift_reg <= tx_data;
                        state     <= 3'b001;
                        tx_busy   <= 1'b1;
                    end
                end
                3'b001: begin
                    tx_out <= 1'b0;
                    if (clk_cnt < 16'd867) begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end else begin
                        clk_cnt <= 16'h0000;
                        state   <= 3'b010;
                    end
                end
                3'b010: begin
                    tx_out <= shift_reg[0];
                    if (clk_cnt < 16'd867) begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end else begin
                        clk_cnt   <= 16'h0000;
                        shift_reg <= {1'b0, shift_reg[7:1]};
                        if (bit_idx < 3'b111) begin
                            bit_idx <= bit_idx + 3'b001;
                        end else begin
                            bit_idx <= 3'b000;
                            state   <= 3'b011;
                        end
                    end
                end
                3'b011: begin
                    tx_out <= 1'b1;
                    if (clk_cnt < 16'd867) begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end else begin
                        clk_cnt <= 16'h0000;
                        state   <= 3'b000;
                    end
                end
                default: state <= 3'b000;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "uart_tx");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
    assert_eq!(top.processes.len(), 1);
}

#[test]
fn verilog_spi_master() {
    let src = r#"
module spi_master (
    input        clk,
    input        rst_n,
    input        start,
    input  [7:0] mosi_data,
    output reg [7:0] miso_data,
    output reg   sclk,
    output reg   mosi,
    input        miso,
    output reg   cs_n,
    output reg   done
);
    reg [3:0]  clk_cnt;
    reg [2:0]  bit_cnt;
    reg [1:0]  state;
    reg [7:0]  shift_out;
    reg [7:0]  shift_in;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state     <= 2'b00;
            sclk      <= 1'b0;
            mosi      <= 1'b0;
            cs_n      <= 1'b1;
            done      <= 1'b0;
            clk_cnt   <= 4'h0;
            bit_cnt   <= 3'b000;
            shift_out <= 8'h00;
            shift_in  <= 8'h00;
            miso_data <= 8'h00;
        end else begin
            case (state)
                2'b00: begin
                    cs_n <= 1'b1;
                    done <= 1'b0;
                    sclk <= 1'b0;
                    if (start) begin
                        shift_out <= mosi_data;
                        cs_n      <= 1'b0;
                        state     <= 2'b01;
                    end
                end
                2'b01: begin
                    mosi <= shift_out[7];
                    if (clk_cnt < 4'd3) begin
                        clk_cnt <= clk_cnt + 4'h1;
                    end else begin
                        clk_cnt <= 4'h0;
                        sclk    <= 1'b1;
                        state   <= 2'b10;
                    end
                end
                2'b10: begin
                    if (clk_cnt < 4'd3) begin
                        clk_cnt <= clk_cnt + 4'h1;
                    end else begin
                        clk_cnt  <= 4'h0;
                        sclk     <= 1'b0;
                        shift_in <= {shift_in[6:0], miso};
                        shift_out <= {shift_out[6:0], 1'b0};
                        if (bit_cnt < 3'b111) begin
                            bit_cnt <= bit_cnt + 3'b001;
                            state   <= 2'b01;
                        end else begin
                            bit_cnt   <= 3'b000;
                            miso_data <= {shift_in[6:0], miso};
                            done      <= 1'b1;
                            state     <= 2'b00;
                        end
                    end
                end
                default: state <= 2'b00;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "spi_master");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 10);
}

#[test]
fn verilog_uart_rx_oversampling() {
    let src = r#"
module uart_rx (
    input        clk,
    input        rst_n,
    input        rx_in,
    output reg [7:0] rx_data,
    output reg   rx_valid,
    output reg   rx_error
);
    reg [2:0]  state;
    reg [15:0] clk_cnt;
    reg [2:0]  bit_idx;
    reg [7:0]  shift_reg;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state     <= 3'b000;
            clk_cnt   <= 16'h0000;
            bit_idx   <= 3'b000;
            shift_reg <= 8'h00;
            rx_data   <= 8'h00;
            rx_valid  <= 1'b0;
            rx_error  <= 1'b0;
        end else begin
            rx_valid <= 1'b0;
            case (state)
                3'b000: begin
                    if (rx_in == 1'b0) begin
                        state <= 3'b001;
                        clk_cnt <= 16'h0000;
                    end
                end
                3'b001: begin
                    if (clk_cnt == 16'd433) begin
                        if (rx_in == 1'b0) begin
                            clk_cnt <= 16'h0000;
                            state   <= 3'b010;
                        end else begin
                            state <= 3'b000;
                        end
                    end else begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end
                end
                3'b010: begin
                    if (clk_cnt < 16'd867) begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end else begin
                        clk_cnt <= 16'h0000;
                        shift_reg <= {rx_in, shift_reg[7:1]};
                        if (bit_idx < 3'b111) begin
                            bit_idx <= bit_idx + 3'b001;
                        end else begin
                            bit_idx <= 3'b000;
                            state   <= 3'b011;
                        end
                    end
                end
                3'b011: begin
                    if (clk_cnt < 16'd867) begin
                        clk_cnt <= clk_cnt + 16'h0001;
                    end else begin
                        clk_cnt <= 16'h0000;
                        if (rx_in == 1'b1) begin
                            rx_data  <= shift_reg;
                            rx_valid <= 1'b1;
                        end else begin
                            rx_error <= 1'b1;
                        end
                        state <= 3'b000;
                    end
                end
                default: state <= 3'b000;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "uart_rx");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
    assert_eq!(top.processes.len(), 1);
}

#[test]
fn verilog_pwm_controller() {
    let src = r#"
module pwm #(parameter WIDTH = 8) (
    input                  clk,
    input                  rst_n,
    input      [WIDTH-1:0] duty,
    output reg             pwm_out,
    output reg             pwm_n
);
    reg [WIDTH-1:0] counter;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            counter <= 0;
            pwm_out <= 1'b0;
            pwm_n   <= 1'b1;
        end else begin
            counter <= counter + 1;
            if (counter < duty) begin
                pwm_out <= 1'b1;
                pwm_n   <= 1'b0;
            end else begin
                pwm_out <= 1'b0;
                pwm_n   <= 1'b1;
            end
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "pwm");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
    assert!(!top.params.is_empty());
}

#[test]
fn sv_axi_lite_slave() {
    let src = r#"
module axi_lite_slave (
    input  logic        aclk,
    input  logic        aresetn,
    input  logic [31:0] awaddr,
    input  logic        awvalid,
    output logic        awready,
    input  logic [31:0] wdata,
    input  logic [3:0]  wstrb,
    input  logic        wvalid,
    output logic        wready,
    output logic [1:0]  bresp,
    output logic        bvalid,
    input  logic        bready,
    input  logic [31:0] araddr,
    input  logic        arvalid,
    output logic        arready,
    output logic [31:0] rdata,
    output logic [1:0]  rresp,
    output logic        rvalid,
    input  logic        rready
);
    logic [31:0] reg0;
    logic [31:0] reg1;
    logic [31:0] reg2;
    logic [31:0] reg3;
    logic        aw_en;

    always_ff @(posedge aclk) begin
        if (!aresetn) begin
            awready <= 1'b0;
            aw_en   <= 1'b1;
            wready  <= 1'b0;
            bvalid  <= 1'b0;
            bresp   <= 2'b00;
        end else begin
            if (~awready && awvalid && wvalid && aw_en) begin
                awready <= 1'b1;
                aw_en   <= 1'b0;
            end else begin
                if (bready && bvalid) begin
                    aw_en <= 1'b1;
                end
                awready <= 1'b0;
            end

            if (~wready && wvalid && awvalid && aw_en) begin
                wready <= 1'b1;
            end else begin
                wready <= 1'b0;
            end

            if (awready && awvalid && wready && wvalid && ~bvalid) begin
                bvalid <= 1'b1;
                bresp  <= 2'b00;
            end else if (bready && bvalid) begin
                bvalid <= 1'b0;
            end
        end
    end

    always_ff @(posedge aclk) begin
        if (!aresetn) begin
            reg0 <= 32'h00000000;
            reg1 <= 32'h00000000;
            reg2 <= 32'h00000000;
            reg3 <= 32'h00000000;
        end else if (awready && awvalid && wready && wvalid) begin
            case (awaddr[3:2])
                2'b00: reg0 <= wdata;
                2'b01: reg1 <= wdata;
                2'b10: reg2 <= wdata;
                2'b11: reg3 <= wdata;
                default: reg0 <= reg0;
            endcase
        end
    end

    always_comb begin
        rvalid  = 1'b0;
        arready = 1'b0;
        rdata   = 32'h00000000;
        rresp   = 2'b00;
        if (arvalid) begin
            arready = 1'b1;
            rvalid  = 1'b1;
            case (araddr[3:2])
                2'b00: rdata = reg0;
                2'b01: rdata = reg1;
                2'b10: rdata = reg2;
                2'b11: rdata = reg3;
                default: rdata = 32'h00000000;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "axi_lite_slave");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 19);
    assert!(
        top.processes.len() >= 3,
        "should have 2 ff + 1 comb, got {}",
        top.processes.len()
    );
}

#[test]
fn sv_interrupt_controller() {
    let src = r#"
module irq_ctrl (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] irq_in,
    input  logic [7:0] irq_mask,
    output logic [2:0] irq_id,
    output logic       irq_valid
);
    logic [7:0] irq_pending;
    logic [7:0] irq_prev;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            irq_pending <= 8'h00;
            irq_prev    <= 8'h00;
        end else begin
            irq_prev <= irq_in;
            irq_pending <= (irq_pending | (irq_in & ~irq_prev)) & irq_mask;
        end
    end

    always_comb begin
        irq_valid = 1'b0;
        irq_id    = 3'b000;
        if (irq_pending[0])      begin irq_id = 3'b000; irq_valid = 1'b1; end
        else if (irq_pending[1]) begin irq_id = 3'b001; irq_valid = 1'b1; end
        else if (irq_pending[2]) begin irq_id = 3'b010; irq_valid = 1'b1; end
        else if (irq_pending[3]) begin irq_id = 3'b011; irq_valid = 1'b1; end
        else if (irq_pending[4]) begin irq_id = 3'b100; irq_valid = 1'b1; end
        else if (irq_pending[5]) begin irq_id = 3'b101; irq_valid = 1'b1; end
        else if (irq_pending[6]) begin irq_id = 3'b110; irq_valid = 1'b1; end
        else if (irq_pending[7]) begin irq_id = 3'b111; irq_valid = 1'b1; end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "irq_ctrl");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
    assert!(top.processes.len() >= 2);
}

#[test]
fn sv_wishbone_master() {
    let src = r#"
module wb_master (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        req,
    input  logic        we,
    input  logic [31:0] addr,
    input  logic [31:0] wdata,
    output logic [31:0] rdata,
    output logic        ack,
    output logic        wb_cyc,
    output logic        wb_stb,
    output logic        wb_we,
    output logic [31:0] wb_adr,
    output logic [31:0] wb_dat_o,
    input  logic [31:0] wb_dat_i,
    input  logic        wb_ack
);
    logic [1:0] state;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            state    <= 2'b00;
            wb_cyc   <= 1'b0;
            wb_stb   <= 1'b0;
            wb_we    <= 1'b0;
            wb_adr   <= 32'h00000000;
            wb_dat_o <= 32'h00000000;
            rdata    <= 32'h00000000;
            ack      <= 1'b0;
        end else begin
            case (state)
                2'b00: begin
                    ack <= 1'b0;
                    if (req) begin
                        wb_cyc   <= 1'b1;
                        wb_stb   <= 1'b1;
                        wb_we    <= we;
                        wb_adr   <= addr;
                        wb_dat_o <= wdata;
                        state    <= 2'b01;
                    end
                end
                2'b01: begin
                    if (wb_ack) begin
                        rdata  <= wb_dat_i;
                        ack    <= 1'b1;
                        wb_cyc <= 1'b0;
                        wb_stb <= 1'b0;
                        state  <= 2'b00;
                    end
                end
                default: begin
                    state  <= 2'b00;
                    wb_cyc <= 1'b0;
                    wb_stb <= 1'b0;
                end
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "wb_master");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 15);
    assert_eq!(top.processes.len(), 1);
}

#[test]
fn vhdl_uart_receiver() {
    let src = r#"
entity uart_rx is
    generic (
        CLKS_PER_BIT : integer := 868
    );
    port (
        clk      : in  std_logic;
        rst      : in  std_logic;
        rx_in    : in  std_logic;
        rx_data  : out std_logic_vector(7 downto 0);
        rx_valid : out std_logic;
        rx_error : out std_logic
    );
end entity uart_rx;

architecture rtl of uart_rx is
    signal state   : std_logic_vector(2 downto 0);
    signal bit_idx : std_logic_vector(2 downto 0);
    signal clk_cnt : std_logic_vector(15 downto 0);
    signal shift   : std_logic_vector(7 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                state    <= "000";
                bit_idx  <= "000";
                clk_cnt  <= "0000000000000000";
                shift    <= "00000000";
                rx_data  <= "00000000";
                rx_valid <= '0';
                rx_error <= '0';
            else
                rx_valid <= '0';
                case state is
                    when "000" =>
                        if rx_in = '0' then
                            state   <= "001";
                            clk_cnt <= "0000000000000000";
                        end if;
                    when "010" =>
                        shift   <= rx_in & shift(7 downto 1);
                        clk_cnt <= "0000000000000000";
                        if bit_idx = "111" then
                            bit_idx <= "000";
                            state   <= "011";
                        else
                            bit_idx <= bit_idx;
                        end if;
                    when "011" =>
                        if rx_in = '1' then
                            rx_data  <= shift;
                            rx_valid <= '1';
                        else
                            rx_error <= '1';
                        end if;
                        state <= "000";
                    when others =>
                        state <= "000";
                end case;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "uart_rx");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
    assert!(!top.processes.is_empty());
}

#[test]
fn vhdl_spi_slave() {
    let src = r#"
entity spi_slave is
    port (
        clk       : in  std_logic;
        rst       : in  std_logic;
        sclk      : in  std_logic;
        mosi      : in  std_logic;
        miso      : out std_logic;
        cs_n      : in  std_logic;
        byte_done : out std_logic
    );
end entity spi_slave;

architecture rtl of spi_slave is
    signal shift_in  : std_logic_vector(7 downto 0);
    signal shift_out : std_logic_vector(7 downto 0);
    signal bit_cnt   : std_logic_vector(2 downto 0);
    signal sclk_prev : std_logic;
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                shift_in  <= "00000000";
                shift_out <= "00000000";
                bit_cnt   <= "000";
                sclk_prev <= '0';
                byte_done <= '0';
                miso      <= '0';
            else
                byte_done <= '0';
                sclk_prev <= sclk;
                if cs_n = '0' then
                    if sclk = '1' then
                        if sclk_prev = '0' then
                            shift_in <= shift_in(6 downto 0) & mosi;
                            bit_cnt  <= bit_cnt;
                        end if;
                    end if;
                    miso <= shift_out(7);
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "spi_slave");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
}

#[test]
fn vhdl_bus_arbiter() {
    let src = r#"
entity arbiter is
    port (
        clk   : in  std_logic;
        rst   : in  std_logic;
        req   : in  std_logic_vector(3 downto 0);
        grant : out std_logic_vector(3 downto 0)
    );
end entity arbiter;

architecture rtl of arbiter is
    signal last_grant : std_logic_vector(1 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                grant      <= "0000";
                last_grant <= "00";
            else
                grant <= "0000";
                if req(0) = '1' then
                    grant(0)   <= '1';
                    last_grant <= "00";
                else
                    if req(1) = '1' then
                        grant(1)   <= '1';
                        last_grant <= "01";
                    else
                        if req(2) = '1' then
                            grant(2)   <= '1';
                            last_grant <= "10";
                        else
                            if req(3) = '1' then
                                grant(3)   <= '1';
                                last_grant <= "11";
                            end if;
                        end if;
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "arbiter");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn verilog_i2s_transmitter() {
    let src = r#"
module i2s_tx (
    input         clk,
    input         rst_n,
    input  [15:0] left_data,
    input  [15:0] right_data,
    output reg    bclk,
    output reg    lrclk,
    output reg    sdata
);
    reg [4:0]  bit_cnt;
    reg [15:0] shift_reg;
    reg [7:0]  bclk_div;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            bclk      <= 1'b0;
            lrclk     <= 1'b0;
            sdata     <= 1'b0;
            bit_cnt   <= 5'b00000;
            shift_reg <= 16'h0000;
            bclk_div  <= 8'h00;
        end else begin
            bclk_div <= bclk_div + 8'h01;
            if (bclk_div == 8'h03) begin
                bclk_div <= 8'h00;
                bclk     <= ~bclk;
                if (bclk == 1'b1) begin
                    sdata <= shift_reg[15];
                    shift_reg <= {shift_reg[14:0], 1'b0};
                    bit_cnt <= bit_cnt + 5'b00001;
                    if (bit_cnt == 5'b01111) begin
                        lrclk <= 1'b1;
                        shift_reg <= right_data;
                    end else if (bit_cnt == 5'b11111) begin
                        lrclk <= 1'b0;
                        shift_reg <= left_data;
                    end
                end
            end
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "i2s_tx");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
}

// ============================================================================
// Category 4: Control Logic and Peripherals
// ============================================================================

#[test]
fn verilog_debouncer() {
    let src = r#"
module debounce (
    input      clk,
    input      rst_n,
    input      btn_in,
    output reg btn_out
);
    reg [3:0] shift;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            shift   <= 4'b0000;
            btn_out <= 1'b0;
        end else begin
            shift <= {shift[2:0], btn_in};
            if (shift == 4'b1111)
                btn_out <= 1'b1;
            else if (shift == 4'b0000)
                btn_out <= 1'b0;
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "debounce");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn verilog_watchdog_timer() {
    let src = r#"
module watchdog (
    input      clk,
    input      rst_n,
    input      kick,
    output reg timeout_irq
);
    reg [15:0] counter;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            counter     <= 16'h0000;
            timeout_irq <= 1'b0;
        end else begin
            if (kick) begin
                counter     <= 16'h0000;
                timeout_irq <= 1'b0;
            end else if (counter < 16'd1000) begin
                counter <= counter + 16'h0001;
            end else begin
                timeout_irq <= 1'b1;
            end
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "watchdog");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn verilog_edge_detector() {
    let src = r#"
module edge_detect (
    input      clk,
    input      rst_n,
    input      sig,
    output reg rise,
    output reg fall,
    output reg any_edge
);
    reg sig_d;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            sig_d    <= 1'b0;
            rise     <= 1'b0;
            fall     <= 1'b0;
            any_edge <= 1'b0;
        end else begin
            sig_d    <= sig;
            rise     <= sig & ~sig_d;
            fall     <= ~sig & sig_d;
            any_edge <= sig ^ sig_d;
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "edge_detect");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
}

#[test]
fn sv_pwm_dead_time() {
    let src = r#"
module pwm_dt #(parameter WIDTH = 8, parameter DEAD = 4) (
    input  logic               clk,
    input  logic               rst_n,
    input  logic [WIDTH-1:0]   duty,
    output logic               pwm_h,
    output logic               pwm_l
);
    logic [WIDTH-1:0] counter;
    logic             raw_pwm;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            counter <= 0;
        end else begin
            counter <= counter + 1;
        end
    end

    always_comb begin
        raw_pwm = (counter < duty) ? 1'b1 : 1'b0;
    end

    always_comb begin
        pwm_h = raw_pwm;
        pwm_l = ~raw_pwm;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "pwm_dt");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
    assert!(top.processes.len() >= 3);
}

#[test]
fn sv_clock_divider() {
    let src = r#"
module clk_div (
    input  logic clk,
    input  logic rst_n,
    output logic clk_out
);
    logic [15:0] counter;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            counter <= 16'h0000;
            clk_out <= 1'b0;
        end else if (counter >= 16'd4) begin
            counter <= 16'h0000;
            clk_out <= ~clk_out;
        end else begin
            counter <= counter + 16'h0001;
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "clk_div");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
}

#[test]
fn sv_gpio_controller() {
    let src = r#"
module gpio_ctrl (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [7:0]  gpio_in,
    output logic [7:0]  gpio_out,
    input  logic [7:0]  dir_reg,
    input  logic [7:0]  out_reg,
    output logic [7:0]  in_sync
);
    logic [7:0] sync1;
    logic [7:0] sync2;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            sync1   <= 8'h00;
            sync2   <= 8'h00;
            in_sync <= 8'h00;
        end else begin
            sync1   <= gpio_in;
            sync2   <= sync1;
            in_sync <= sync2;
        end
    end

    always_comb begin
        gpio_out = out_reg & dir_reg;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "gpio_ctrl");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
}

#[test]
fn sv_phase_accumulator() {
    let src = r#"
module nco (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [31:0] freq_word,
    output logic        msb_out
);
    logic [31:0] accumulator;

    always_ff @(posedge clk) begin
        if (!rst_n)
            accumulator <= 32'h00000000;
        else
            accumulator <= accumulator + freq_word;
    end

    always_comb begin
        msb_out = accumulator[31];
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "nco");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn vhdl_timer_prescaler() {
    let src = r#"
entity timer is
    generic (
        PRESCALE : integer := 100
    );
    port (
        clk     : in  std_logic;
        rst     : in  std_logic;
        enable  : in  std_logic;
        compare : in  std_logic_vector(15 downto 0);
        irq     : out std_logic
    );
end entity timer;

architecture rtl of timer is
    signal prescale_cnt : std_logic_vector(15 downto 0);
    signal timer_cnt    : std_logic_vector(15 downto 0);
    signal tick         : std_logic;
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                prescale_cnt <= "0000000000000000";
                timer_cnt    <= "0000000000000000";
                tick         <= '0';
                irq          <= '0';
            else
                tick <= '0';
                irq  <= '0';
                if enable = '1' then
                    prescale_cnt <= prescale_cnt;
                    tick         <= '1';
                    timer_cnt    <= timer_cnt;
                    if timer_cnt = compare then
                        irq       <= '1';
                        timer_cnt <= "0000000000000000";
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "timer");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
}

#[test]
fn vhdl_pulse_width_measurer() {
    let src = r#"
entity pulse_meas is
    port (
        clk      : in  std_logic;
        rst      : in  std_logic;
        pulse_in : in  std_logic;
        width    : out std_logic_vector(15 downto 0);
        valid    : out std_logic
    );
end entity pulse_meas;

architecture rtl of pulse_meas is
    signal counter  : std_logic_vector(15 downto 0);
    signal prev     : std_logic;
    signal counting : std_logic;
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                counter  <= "0000000000000000";
                prev     <= '0';
                counting <= '0';
                width    <= "0000000000000000";
                valid    <= '0';
            else
                prev  <= pulse_in;
                valid <= '0';
                if pulse_in = '1' then
                    if prev = '0' then
                        counter  <= "0000000000000001";
                        counting <= '1';
                    else
                        if counting = '1' then
                            counter <= counter;
                        end if;
                    end if;
                else
                    if counting = '1' then
                        width    <= counter;
                        valid    <= '1';
                        counting <= '0';
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "pulse_meas");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
}

#[test]
fn vhdl_priority_encoder() {
    let src = r#"
entity priority_enc is
    port (
        req   : in  std_logic_vector(7 downto 0);
        grant : out std_logic_vector(2 downto 0);
        valid : out std_logic
    );
end entity priority_enc;

architecture rtl of priority_enc is
begin
    process(req)
    begin
        valid <= '1';
        if req(7) = '1' then
            grant <= "111";
        else
            if req(6) = '1' then
                grant <= "110";
            else
                if req(5) = '1' then
                    grant <= "101";
                else
                    if req(4) = '1' then
                        grant <= "100";
                    else
                        if req(3) = '1' then
                            grant <= "011";
                        else
                            if req(2) = '1' then
                                grant <= "010";
                            else
                                if req(1) = '1' then
                                    grant <= "001";
                                else
                                    if req(0) = '1' then
                                        grant <= "000";
                                    else
                                        grant <= "000";
                                        valid <= '0';
                                    end if;
                                end if;
                            end if;
                        end if;
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "priority_enc");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
}

// ============================================================================
// Category 2: Compute / DSP
// ============================================================================

#[test]
fn verilog_simple_risc_cpu() {
    let src = r#"
module cpu (
    input         clk,
    input         rst_n,
    input  [7:0]  instr,
    input  [7:0]  data_in,
    output reg [7:0] data_out,
    output reg [7:0] addr,
    output reg       mem_we
);
    reg [7:0] acc;
    reg [7:0] pc;
    reg [2:0] opcode;
    reg [1:0] state;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            acc      <= 8'h00;
            pc       <= 8'h00;
            state    <= 2'b00;
            data_out <= 8'h00;
            addr     <= 8'h00;
            mem_we   <= 1'b0;
            opcode   <= 3'b000;
        end else begin
            case (state)
                2'b00: begin
                    addr   <= pc;
                    mem_we <= 1'b0;
                    state  <= 2'b01;
                end
                2'b01: begin
                    opcode <= instr[7:5];
                    addr   <= {3'b000, instr[4:0]};
                    state  <= 2'b10;
                end
                2'b10: begin
                    case (opcode)
                        3'b000: acc      <= data_in;
                        3'b001: acc      <= acc + data_in;
                        3'b010: acc      <= acc - data_in;
                        3'b011: acc      <= acc & data_in;
                        3'b100: acc      <= acc | data_in;
                        3'b101: acc      <= acc ^ data_in;
                        3'b110: begin
                            data_out <= acc;
                            mem_we   <= 1'b1;
                        end
                        default: acc <= acc;
                    endcase
                    pc    <= pc + 8'h01;
                    state <= 2'b00;
                end
                default: state <= 2'b00;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "cpu");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
    assert_eq!(top.processes.len(), 1);
}

#[test]
fn verilog_barrel_shifter() {
    let src = r#"
module barrel_shift (
    input  [15:0]          data_in,
    input  [3:0]           shift_amt,
    input                  shift_dir,
    output reg [15:0]      data_out
);
    reg [15:0] s0, s1, s2, s3;

    always @(*) begin
        s0 = shift_dir ? {1'b0, data_in[15:1]} : {data_in[14:0], 1'b0};
        s1 = s0;
        s2 = s1;
        s3 = s2;
        if (shift_amt[0]) data_out = s0;
        else if (shift_amt[1]) data_out = s1;
        else if (shift_amt[2]) data_out = s2;
        else if (shift_amt[3]) data_out = s3;
        else data_out = data_in;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "barrel_shift");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn verilog_multiply_accumulate() {
    let src = r#"
module mac (
    input         clk,
    input         rst_n,
    input         clear,
    input  [7:0]  a,
    input  [7:0]  b,
    output reg [19:0] acc
);
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            acc <= 20'h00000;
        else if (clear)
            acc <= 20'h00000;
        else
            acc <= acc + a * b;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "mac");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
}

#[test]
fn sv_pipelined_multiplier() {
    let src = r#"
module pipe_mult (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [15:0] a,
    input  logic [15:0] b,
    input  logic        valid_in,
    output logic [31:0] product,
    output logic        valid_out
);
    logic [15:0] a_s1, b_s1;
    logic [31:0] partial_s2;
    logic [31:0] result_s3;
    logic        v1, v2, v3;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            a_s1       <= 16'h0000;
            b_s1       <= 16'h0000;
            partial_s2 <= 32'h00000000;
            result_s3  <= 32'h00000000;
            v1 <= 1'b0;
            v2 <= 1'b0;
            v3 <= 1'b0;
        end else begin
            a_s1       <= a;
            b_s1       <= b;
            v1         <= valid_in;

            partial_s2 <= a_s1 * b_s1;
            v2         <= v1;

            result_s3  <= partial_s2;
            v3         <= v2;
        end
    end

    always_comb begin
        product   = result_s3;
        valid_out = v3;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "pipe_mult");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 7);
    assert!(top.processes.len() >= 2);
}

#[test]
fn sv_leading_zero_counter() {
    let src = r#"
module clz (
    input  logic [31:0] data,
    output logic [5:0]  count,
    output logic        valid
);
    always_comb begin
        valid = 1'b1;
        if      (data[31]) count = 6'd0;
        else if (data[30]) count = 6'd1;
        else if (data[29]) count = 6'd2;
        else if (data[28]) count = 6'd3;
        else if (data[27]) count = 6'd4;
        else if (data[26]) count = 6'd5;
        else if (data[25]) count = 6'd6;
        else if (data[24]) count = 6'd7;
        else if (data[23]) count = 6'd8;
        else if (data[22]) count = 6'd9;
        else if (data[21]) count = 6'd10;
        else if (data[20]) count = 6'd11;
        else if (data[19]) count = 6'd12;
        else if (data[18]) count = 6'd13;
        else if (data[17]) count = 6'd14;
        else if (data[16]) count = 6'd15;
        else if (data[15]) count = 6'd16;
        else if (data[14]) count = 6'd17;
        else if (data[13]) count = 6'd18;
        else if (data[12]) count = 6'd19;
        else if (data[11]) count = 6'd20;
        else if (data[10]) count = 6'd21;
        else if (data[9])  count = 6'd22;
        else if (data[8])  count = 6'd23;
        else if (data[7])  count = 6'd24;
        else if (data[6])  count = 6'd25;
        else if (data[5])  count = 6'd26;
        else if (data[4])  count = 6'd27;
        else if (data[3])  count = 6'd28;
        else if (data[2])  count = 6'd29;
        else if (data[1])  count = 6'd30;
        else if (data[0])  count = 6'd31;
        else begin
            count = 6'd32;
            valid = 1'b0;
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "clz");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
}

#[test]
fn sv_crc32_calculator() {
    let src = r#"
module crc32 (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [7:0]  data_in,
    input  logic        data_valid,
    output logic [31:0] crc_out
);
    logic [31:0] crc;
    logic [31:0] crc_next;

    always_comb begin
        crc_next = crc;
        if (data_valid) begin
            crc_next = crc ^ {24'h000000, data_in};
            crc_next = crc_next ^ (crc_next << 1);
        end
    end

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            crc <= 32'hFFFFFFFF;
        end else begin
            crc <= crc_next;
        end
    end

    always_comb begin
        crc_out = ~crc;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "crc32");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
    assert!(top.processes.len() >= 3);
}

#[test]
fn sv_fixed_point_adder() {
    let src = r#"
module fp_add (
    input  logic [15:0] a,
    input  logic [15:0] b,
    output logic [15:0] sum,
    output logic        overflow
);
    logic [16:0] extended_sum;

    always_comb begin
        extended_sum = {1'b0, a} + {1'b0, b};
        sum      = extended_sum[15:0];
        overflow = extended_sum[16];
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "fp_add");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn vhdl_fir_filter_4tap() {
    let src = r#"
entity fir_4tap is
    port (
        clk    : in  std_logic;
        rst    : in  std_logic;
        din    : in  std_logic_vector(7 downto 0);
        dout   : out std_logic_vector(15 downto 0)
    );
end entity fir_4tap;

architecture rtl of fir_4tap is
    signal d0 : std_logic_vector(7 downto 0);
    signal d1 : std_logic_vector(7 downto 0);
    signal d2 : std_logic_vector(7 downto 0);
    signal d3 : std_logic_vector(7 downto 0);
    signal c0 : std_logic_vector(7 downto 0);
    signal c1 : std_logic_vector(7 downto 0);
    signal c2 : std_logic_vector(7 downto 0);
    signal c3 : std_logic_vector(7 downto 0);
begin
    c0 <= "00000001";
    c1 <= "00000010";
    c2 <= "00000010";
    c3 <= "00000001";

    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                d0   <= "00000000";
                d1   <= "00000000";
                d2   <= "00000000";
                d3   <= "00000000";
                dout <= "0000000000000000";
            else
                d0 <= din;
                d1 <= d0;
                d2 <= d1;
                d3 <= d2;
                dout <= dout;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "fir_4tap");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
    assert!(!top.processes.is_empty());
}

#[test]
fn vhdl_unsigned_divider() {
    let src = r#"
entity divider is
    port (
        clk       : in  std_logic;
        rst       : in  std_logic;
        start     : in  std_logic;
        dividend  : in  std_logic_vector(7 downto 0);
        divisor   : in  std_logic_vector(7 downto 0);
        quotient  : out std_logic_vector(7 downto 0);
        remainder : out std_logic_vector(7 downto 0);
        done      : out std_logic
    );
end entity divider;

architecture rtl of divider is
    signal acc     : std_logic_vector(7 downto 0);
    signal dvd     : std_logic_vector(7 downto 0);
    signal cnt     : std_logic_vector(3 downto 0);
    signal running : std_logic;
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                acc       <= "00000000";
                dvd       <= "00000000";
                cnt       <= "0000";
                running   <= '0';
                quotient  <= "00000000";
                remainder <= "00000000";
                done      <= '0';
            else
                done <= '0';
                if start = '1' then
                    if running = '0' then
                        dvd     <= dividend;
                        acc     <= "00000000";
                        cnt     <= "1000";
                        running <= '1';
                    end if;
                end if;
                if running = '1' then
                    if cnt = "0000" then
                        quotient  <= dvd;
                        remainder <= acc;
                        done      <= '1';
                        running   <= '0';
                    else
                        cnt <= cnt;
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "divider");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 8);
}

#[test]
fn vhdl_saturating_accumulator() {
    let src = r#"
entity sat_acc is
    generic (
        WIDTH : integer := 16
    );
    port (
        clk   : in  std_logic;
        rst   : in  std_logic;
        din   : in  std_logic_vector(WIDTH-1 downto 0);
        acc   : out std_logic_vector(WIDTH-1 downto 0);
        oflow : out std_logic
    );
end entity sat_acc;

architecture rtl of sat_acc is
    signal accum : std_logic_vector(WIDTH-1 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                accum <= (others => '0');
                oflow <= '0';
            else
                accum <= accum;
                oflow <= '0';
            end if;
        end if;
    end process;
    acc <= accum;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "sat_acc");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
}

// ============================================================================
// Category 3: Memory and Storage
// ============================================================================

#[test]
fn verilog_async_fifo() {
    // Dual-clock FIFO with gray-code pointers
    let src = r#"
module async_fifo #(parameter DEPTH = 16, parameter WIDTH = 8) (
    input                  wclk,
    input                  rclk,
    input                  rst_n,
    input                  wr_en,
    input  [WIDTH-1:0]     wdata,
    input                  rd_en,
    output reg [WIDTH-1:0] rdata,
    output reg             full,
    output reg             empty
);
    reg [WIDTH-1:0] mem [0:DEPTH-1];
    reg [4:0] wptr, rptr;
    reg [4:0] wptr_gray, rptr_gray;

    always @(posedge wclk or negedge rst_n) begin
        if (!rst_n) begin
            wptr      <= 5'b00000;
            wptr_gray <= 5'b00000;
            full      <= 1'b0;
        end else begin
            if (wr_en && !full) begin
                mem[wptr[3:0]] <= wdata;
                wptr <= wptr + 5'b00001;
            end
            wptr_gray <= wptr ^ (wptr >> 1);
            full <= (wptr_gray == {~rptr_gray[4:3], rptr_gray[2:0]}) ? 1'b1 : 1'b0;
        end
    end

    always @(posedge rclk or negedge rst_n) begin
        if (!rst_n) begin
            rptr      <= 5'b00000;
            rptr_gray <= 5'b00000;
            rdata     <= 0;
            empty     <= 1'b1;
        end else begin
            if (rd_en && !empty) begin
                rdata <= mem[rptr[3:0]];
                rptr  <= rptr + 5'b00001;
            end
            rptr_gray <= rptr ^ (rptr >> 1);
            empty <= (rptr_gray == wptr_gray) ? 1'b1 : 1'b0;
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "async_fifo");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 9);
    assert!(
        top.processes.len() >= 2,
        "should have write and read processes"
    );
}

#[test]
fn verilog_dual_port_ram() {
    let src = r#"
module dual_port_ram #(parameter ADDR_W = 4, parameter DATA_W = 8) (
    input                  clk,
    input                  we_a,
    input  [ADDR_W-1:0]   addr_a,
    input  [DATA_W-1:0]   wdata_a,
    output reg [DATA_W-1:0] rdata_a,
    input                  we_b,
    input  [ADDR_W-1:0]   addr_b,
    input  [DATA_W-1:0]   wdata_b,
    output reg [DATA_W-1:0] rdata_b
);
    reg [DATA_W-1:0] mem [0:(1<<ADDR_W)-1];

    always @(posedge clk) begin
        if (we_a)
            mem[addr_a] <= wdata_a;
        rdata_a <= mem[addr_a];
    end

    always @(posedge clk) begin
        if (we_b)
            mem[addr_b] <= wdata_b;
        rdata_b <= mem[addr_b];
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "dual_port_ram");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 9);
    assert!(top.processes.len() >= 2);
}

#[test]
fn verilog_sync_fifo() {
    let src = r#"
module sync_fifo (
    input                  clk,
    input                  rst_n,
    input                  wr_en,
    input  [7:0]           wdata,
    input                  rd_en,
    output reg [7:0]       rdata,
    output reg             full,
    output reg             empty,
    output reg [3:0]       count
);
    reg [7:0] mem [0:7];
    reg [3:0] wptr, rptr;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            wptr  <= 4'h0;
            rptr  <= 4'h0;
            count <= 4'h0;
            full  <= 1'b0;
            empty <= 1'b1;
            rdata <= 8'h00;
        end else begin
            if (wr_en && !full) begin
                mem[wptr] <= wdata;
                wptr <= wptr + 4'h1;
            end
            if (rd_en && !empty) begin
                rdata <= mem[rptr];
                rptr  <= rptr + 4'h1;
            end
            if (wr_en && !rd_en && !full)
                count <= count + 4'h1;
            else if (rd_en && !wr_en && !empty)
                count <= count - 4'h1;
            full  <= (count == 4'd7 && wr_en && !rd_en) ? 1'b1 : 1'b0;
            empty <= (count == 4'h1 && rd_en && !wr_en) ? 1'b1 : 1'b0;
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "sync_fifo");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 9);
}

#[test]
fn sv_cache_controller() {
    let src = r#"
module cache_ctrl #(parameter LINES = 16, parameter LINE_W = 32) (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [31:0] addr,
    input  logic        req,
    input  logic        we,
    input  logic [31:0] wdata,
    output logic [31:0] rdata,
    output logic        hit,
    output logic        miss,
    output logic        busy
);
    logic [31:0] tag_array   [0:LINES-1];
    logic        valid_array [0:LINES-1];
    logic [31:0] data_array  [0:LINES-1];
    logic [3:0]  index;
    logic [1:0]  state;

    always_comb begin
        index = addr[5:2];
    end

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            state <= 2'b00;
            hit   <= 1'b0;
            miss  <= 1'b0;
            busy  <= 1'b0;
            rdata <= 32'h00000000;
        end else begin
            hit  <= 1'b0;
            miss <= 1'b0;
            case (state)
                2'b00: begin
                    busy <= 1'b0;
                    if (req) begin
                        if (valid_array[index] && tag_array[index] == addr[31:6]) begin
                            hit <= 1'b1;
                            if (we)
                                data_array[index] <= wdata;
                            else
                                rdata <= data_array[index];
                        end else begin
                            miss  <= 1'b1;
                            busy  <= 1'b1;
                            state <= 2'b01;
                        end
                    end
                end
                2'b01: begin
                    tag_array[index]   <= addr[31:6];
                    valid_array[index] <= 1'b1;
                    if (we)
                        data_array[index] <= wdata;
                    state <= 2'b00;
                end
                default: state <= 2'b00;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "cache_ctrl");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 10);
}

#[test]
fn sv_register_bank() {
    let src = r#"
module reg_bank #(parameter DEPTH = 32, parameter WIDTH = 32) (
    input  logic                clk,
    input  logic                we,
    input  logic [4:0]          waddr,
    input  logic [WIDTH-1:0]    wdata,
    input  logic [4:0]          raddr1,
    input  logic [4:0]          raddr2,
    output logic [WIDTH-1:0]    rdata1,
    output logic [WIDTH-1:0]    rdata2
);
    logic [WIDTH-1:0] regs [0:DEPTH-1];

    always_ff @(posedge clk) begin
        if (we)
            regs[waddr] <= wdata;
    end

    always_comb begin
        rdata1 = regs[raddr1];
        rdata2 = regs[raddr2];
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "reg_bank");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 8);
}

#[test]
fn sv_cam() {
    let src = r#"
module cam #(parameter ENTRIES = 8, parameter WIDTH = 8) (
    input  logic              clk,
    input  logic              rst_n,
    input  logic              we,
    input  logic [2:0]        waddr,
    input  logic [WIDTH-1:0]  wdata,
    input  logic [WIDTH-1:0]  search_key,
    output logic [ENTRIES-1:0] match_vec,
    output logic              found
);
    logic [WIDTH-1:0] mem   [0:ENTRIES-1];
    logic [ENTRIES-1:0] valid;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            valid <= 8'h00;
        end else if (we) begin
            mem[waddr]   <= wdata;
            valid[waddr] <= 1'b1;
        end
    end

    always_comb begin
        match_vec = 8'h00;
        found     = 1'b0;
        if (valid[0] && mem[0] == search_key) begin match_vec[0] = 1'b1; found = 1'b1; end
        if (valid[1] && mem[1] == search_key) begin match_vec[1] = 1'b1; found = 1'b1; end
        if (valid[2] && mem[2] == search_key) begin match_vec[2] = 1'b1; found = 1'b1; end
        if (valid[3] && mem[3] == search_key) begin match_vec[3] = 1'b1; found = 1'b1; end
        if (valid[4] && mem[4] == search_key) begin match_vec[4] = 1'b1; found = 1'b1; end
        if (valid[5] && mem[5] == search_key) begin match_vec[5] = 1'b1; found = 1'b1; end
        if (valid[6] && mem[6] == search_key) begin match_vec[6] = 1'b1; found = 1'b1; end
        if (valid[7] && mem[7] == search_key) begin match_vec[7] = 1'b1; found = 1'b1; end
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "cam");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 8);
}

#[test]
fn vhdl_single_port_bram() {
    let src = r#"
entity bram is
    generic (
        ADDR_W : integer := 4;
        DATA_W : integer := 8
    );
    port (
        clk   : in  std_logic;
        we    : in  std_logic;
        addr  : in  std_logic_vector(ADDR_W-1 downto 0);
        wdata : in  std_logic_vector(DATA_W-1 downto 0);
        rdata : out std_logic_vector(DATA_W-1 downto 0)
    );
end entity bram;

architecture rtl of bram is
begin
    process(clk)
    begin
        if clk = '1' then
            if we = '1' then
                rdata <= wdata;
            else
                rdata <= rdata;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "bram");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 5);
}

#[test]
fn vhdl_lifo_stack() {
    let src = r#"
entity lifo is
    generic (
        DEPTH : integer := 8;
        WIDTH : integer := 8
    );
    port (
        clk       : in  std_logic;
        rst       : in  std_logic;
        push      : in  std_logic;
        pop       : in  std_logic;
        din       : in  std_logic_vector(WIDTH-1 downto 0);
        dout      : out std_logic_vector(WIDTH-1 downto 0);
        full      : out std_logic;
        empty     : out std_logic
    );
end entity lifo;

architecture rtl of lifo is
    signal sp : std_logic_vector(3 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                sp    <= "0000";
                dout  <= (others => '0');
                full  <= '0';
                empty <= '1';
            else
                if push = '1' then
                    sp <= sp;
                    empty <= '0';
                end if;
                if pop = '1' then
                    sp <= sp;
                    if sp = "0001" then
                        empty <= '1';
                    end if;
                end if;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "lifo");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 8);
}
