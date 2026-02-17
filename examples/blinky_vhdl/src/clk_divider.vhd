-- Clock divider: divides input clock by 8.
-- Produces a single-cycle tick pulse every 8 input clocks.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity clk_divider is
    port (
        clk   : in  std_logic;
        rst_n : in  std_logic;
        tick  : out std_logic
    );
end entity clk_divider;

architecture rtl of clk_divider is
    signal cnt : unsigned(2 downto 0);
begin
    process (clk, rst_n)
    begin
        if rst_n = '0' then
            cnt  <= (others => '0');
            tick <= '0';
        elsif rising_edge(clk) then
            if cnt = "111" then
                cnt  <= (others => '0');
                tick <= '1';
            else
                cnt  <= cnt + 1;
                tick <= '0';
            end if;
        end if;
    end process;
end architecture rtl;
