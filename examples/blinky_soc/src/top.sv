// Blinky SoC top module: wires a clock divider, counter, and LED controller.
// The clock divider produces a slow tick, which enables the counter.
// The counter value drives the LED outputs through the LED controller.
module blinky_top (
    input  logic       clk,
    input  logic       rst_n,
    output logic [7:0] leds
);
    logic       tick;
    logic [7:0] count;

    clk_divider u_clk_div (
        .clk   (clk),
        .rst_n (rst_n),
        .tick  (tick)
    );

    counter u_counter (
        .clk   (clk),
        .rst_n (rst_n),
        .en    (tick),
        .count (count)
    );

    led_ctrl u_led_ctrl (
        .count (count),
        .leds  (leds)
    );
endmodule
