// Clock divider: divides input clock by a fixed ratio.
// Produces a single-cycle tick pulse every 8 input clocks.
module clk_divider (
    input  logic        clk,
    input  logic        rst_n,
    output logic        tick
);
    logic [2:0] cnt;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cnt  <= 3'b000;
            tick <= 1'b0;
        end else begin
            if (cnt == 3'b111) begin
                cnt  <= 3'b000;
                tick <= 1'b1;
            end else begin
                cnt  <= cnt + 3'b001;
                tick <= 1'b0;
            end
        end
    end
endmodule
