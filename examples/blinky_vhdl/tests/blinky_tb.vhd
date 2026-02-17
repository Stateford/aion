-- Testbench for blinky_top.
-- Applies reset, then clocks the design and checks that LEDs toggle.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity blinky_tb is
end entity blinky_tb;

architecture sim of blinky_tb is
    signal clk   : std_logic := '0';
    signal rst_n : std_logic := '0';
    signal leds  : std_logic_vector(7 downto 0);

    constant CLK_PERIOD : time := 20 ns;
begin
    -- Clock generation
    clk <= not clk after CLK_PERIOD / 2;

    -- DUT instantiation
    dut : entity work.blinky_top
        port map (
            clk   => clk,
            rst_n => rst_n,
            leds  => leds
        );

    -- Stimulus
    process
    begin
        -- Hold reset for 2 clock cycles
        rst_n <= '0';
        wait for CLK_PERIOD * 2;
        rst_n <= '1';

        -- Wait for counter to increment (8 clocks per tick, need a few ticks)
        wait for CLK_PERIOD * 100;

        -- Verify LEDs are not all zero after enough ticks
        assert leds /= x"00"
            report "LEDs should have toggled after 100 clock cycles"
            severity failure;

        -- End simulation
        assert false
            report "Simulation complete"
            severity note;
        wait;
    end process;
end architecture sim;
