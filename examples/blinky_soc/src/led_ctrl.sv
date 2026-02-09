// LED controller: maps counter value bits to LED outputs.
// Each LED is driven by one bit of the 8-bit count value.
module led_ctrl (
    input  logic [7:0] count,
    output logic [7:0] leds
);
    always_comb begin
        leds[0] = count[0];
        leds[1] = count[1];
        leds[2] = count[2];
        leds[3] = count[3];
        leds[4] = count[4];
        leds[5] = count[5];
        leds[6] = count[6];
        leds[7] = count[7];
    end
endmodule
