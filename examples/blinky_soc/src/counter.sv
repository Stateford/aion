// 8-bit counter with synchronous enable.
// Increments on each clock edge when enable is asserted.
module counter (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [7:0] count
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            count <= 8'h00;
        else if (en)
            count <= count + 8'h01;
    end
endmodule
