-- LED controller: maps counter value bits to LED outputs.
-- Each LED is driven by one bit of the 8-bit count value.
library ieee;
use ieee.std_logic_1164.all;

entity led_ctrl is
    port (
        count : in  std_logic_vector(7 downto 0);
        leds  : out std_logic_vector(7 downto 0)
    );
end entity led_ctrl;

architecture rtl of led_ctrl is
begin
    leds <= count;
end architecture rtl;
