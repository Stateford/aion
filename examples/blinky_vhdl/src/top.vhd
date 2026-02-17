-- Blinky top module: wires a clock divider, counter, and LED controller.
-- The clock divider produces a slow tick, which enables the counter.
-- The counter value drives the LED outputs through the LED controller.
library ieee;
use ieee.std_logic_1164.all;

entity blinky_top is
    port (
        clk   : in  std_logic;
        rst_n : in  std_logic;
        leds  : out std_logic_vector(7 downto 0)
    );
end entity blinky_top;

architecture rtl of blinky_top is
    signal tick  : std_logic;
    signal count : std_logic_vector(7 downto 0);

    component clk_divider is
        port (
            clk   : in  std_logic;
            rst_n : in  std_logic;
            tick  : out std_logic
        );
    end component;

    component counter is
        port (
            clk   : in  std_logic;
            rst_n : in  std_logic;
            en    : in  std_logic;
            count : out std_logic_vector(7 downto 0)
        );
    end component;

    component led_ctrl is
        port (
            count : in  std_logic_vector(7 downto 0);
            leds  : out std_logic_vector(7 downto 0)
        );
    end component;
begin
    u_clk_div : clk_divider
        port map (
            clk   => clk,
            rst_n => rst_n,
            tick  => tick
        );

    u_counter : counter
        port map (
            clk   => clk,
            rst_n => rst_n,
            en    => tick,
            count => count
        );

    u_led_ctrl : led_ctrl
        port map (
            count => count,
            leds  => leds
        );
end architecture rtl;
