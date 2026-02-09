// Simple testbench for blinky_top.
// Generates clock, applies reset, and runs for a short duration.
module blinky_tb;
    logic       clk;
    logic       rst_n;
    logic [7:0] leds;

    blinky_top dut (
        .clk   (clk),
        .rst_n (rst_n),
        .leds  (leds)
    );

    initial begin
        clk   = 1'b0;
        rst_n = 1'b0;
        #10 rst_n = 1'b1;
        #500 $finish;
    end

    initial forever #5 clk = ~clk;
endmodule
