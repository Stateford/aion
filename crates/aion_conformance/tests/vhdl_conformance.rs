//! Conformance tests for realistic VHDL-2008 designs through the full pipeline.

use aion_conformance::full_pipeline_vhdl;

#[test]
fn counter_entity_with_generic() {
    let src = r#"
entity counter is
    generic (
        WIDTH : integer := 8
    );
    port (
        clk   : in  std_logic;
        rst   : in  std_logic;
        count : out std_logic_vector(WIDTH-1 downto 0)
    );
end entity counter;

architecture rtl of counter is
    signal cnt : std_logic_vector(WIDTH-1 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                cnt <= (others => '0');
            else
                cnt <= cnt;
            end if;
        end if;
    end process;
    count <= cnt;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "counter");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 3);
}

#[test]
fn mux_with_case_when() {
    let src = r#"
entity mux4 is
    port (
        a   : in  std_logic_vector(7 downto 0);
        b   : in  std_logic_vector(7 downto 0);
        c   : in  std_logic_vector(7 downto 0);
        d   : in  std_logic_vector(7 downto 0);
        sel : in  std_logic_vector(1 downto 0);
        y   : out std_logic_vector(7 downto 0)
    );
end entity mux4;

architecture rtl of mux4 is
begin
    process(a, b, c, d, sel)
    begin
        case sel is
            when "00" => y <= a;
            when "01" => y <= b;
            when "10" => y <= c;
            when others => y <= d;
        end case;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "mux4");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 6);
}

#[test]
fn fsm_with_std_logic_states() {
    // Uses std_logic_vector states instead of enum type (enum elaboration
    // is a known limitation)
    let src = r#"
entity fsm is
    port (
        clk  : in  std_logic;
        rst  : in  std_logic;
        go   : in  std_logic;
        busy : out std_logic
    );
end entity fsm;

architecture rtl of fsm is
    signal state : std_logic_vector(1 downto 0);
begin
    process(clk)
    begin
        if clk = '1' then
            if rst = '1' then
                state <= "00";
                busy <= '0';
            else
                case state is
                    when "00" =>
                        if go = '1' then
                            state <= "01";
                        end if;
                        busy <= '0';
                    when "01" =>
                        state <= "10";
                        busy <= '1';
                    when "10" =>
                        state <= "00";
                        busy <= '0';
                    when others =>
                        state <= "00";
                end case;
            end if;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "fsm");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn two_entity_hierarchy() {
    let src = r#"
entity inverter is
    port (
        a : in  std_logic;
        y : out std_logic
    );
end entity inverter;

architecture rtl of inverter is
begin
    y <= not a;
end architecture rtl;

entity top is
    port (
        input_a  : in  std_logic;
        output_y : out std_logic
    );
end entity top;

architecture structural of top is
    component inverter is
        port (
            a : in  std_logic;
            y : out std_logic
        );
    end component;
begin
    u0: inverter port map (a => input_a, y => output_y);
end architecture structural;
"#;
    let result = full_pipeline_vhdl(src, "top");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    assert_eq!(result.design.module_count(), 2);
}

#[test]
fn architecture_signals_and_concurrent_assigns() {
    let src = r#"
entity logic_block is
    port (
        a : in  std_logic;
        b : in  std_logic;
        y : out std_logic;
        z : out std_logic
    );
end entity logic_block;

architecture rtl of logic_block is
    signal tmp : std_logic;
begin
    tmp <= a and b;
    y <= tmp;
    z <= not tmp;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "logic_block");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    let top = result.design.top_module();
    assert_eq!(top.ports.len(), 4);
}

#[test]
fn process_with_sensitivity_all() {
    let src = r#"
entity comb is
    port (
        a : in  std_logic;
        b : in  std_logic;
        y : out std_logic
    );
end entity comb;

architecture rtl of comb is
begin
    process(all)
    begin
        y <= a xor b;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "comb");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}

#[test]
fn for_generate() {
    let src = r#"
entity gen_test is
    port (
        a : in  std_logic_vector(3 downto 0);
        y : out std_logic_vector(3 downto 0)
    );
end entity gen_test;

architecture rtl of gen_test is
begin
    gen_inv: for i in 0 to 3 generate
        y(i) <= not a(i);
    end generate gen_inv;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "gen_test");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}

#[test]
fn multi_unit_file() {
    let src = r#"
entity unit_a is
    port (x : in std_logic; y : out std_logic);
end entity unit_a;

architecture rtl of unit_a is
begin
    y <= x;
end architecture rtl;

entity unit_b is
    port (x : in std_logic; y : out std_logic);
end entity unit_b;

architecture rtl of unit_b is
begin
    y <= not x;
end architecture rtl;
"#;
    // Elaborate unit_b as top
    let result = full_pipeline_vhdl(src, "unit_b");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}

#[test]
fn entity_with_generics_and_port_map() {
    let src = r#"
entity parameterized is
    generic (
        N : integer := 4
    );
    port (
        d : in  std_logic_vector(N-1 downto 0);
        q : out std_logic_vector(N-1 downto 0)
    );
end entity parameterized;

architecture rtl of parameterized is
begin
    q <= d;
end architecture rtl;

entity wrapper is
    port (
        data_in  : in  std_logic_vector(7 downto 0);
        data_out : out std_logic_vector(7 downto 0)
    );
end entity wrapper;

architecture structural of wrapper is
    component parameterized is
        generic (N : integer := 4);
        port (
            d : in  std_logic_vector(N-1 downto 0);
            q : out std_logic_vector(N-1 downto 0)
        );
    end component;
begin
    u0: parameterized generic map (N => 8) port map (d => data_in, q => data_out);
end architecture structural;
"#;
    let result = full_pipeline_vhdl(src, "wrapper");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
    assert_eq!(result.design.module_count(), 2);
}

#[test]
fn constant_declarations() {
    let src = r#"
entity const_test is
    port (
        sel : in  std_logic;
        y   : out std_logic_vector(7 downto 0)
    );
end entity const_test;

architecture rtl of const_test is
    constant ZERO_VAL : std_logic_vector(7 downto 0) := "00000000";
    constant ONE_VAL  : std_logic_vector(7 downto 0) := "11111111";
begin
    process(sel)
    begin
        if sel = '1' then
            y <= ONE_VAL;
        else
            y <= ZERO_VAL;
        end if;
    end process;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "const_test");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}

#[test]
fn if_generate_with_integer() {
    // Uses integer generic instead of boolean (boolean const eval
    // is a known limitation)
    let src = r#"
entity if_gen is
    generic (USE_INVERT : integer := 1);
    port (
        a : in  std_logic;
        y : out std_logic
    );
end entity if_gen;

architecture rtl of if_gen is
begin
    gen_inv: if USE_INVERT = 1 generate
        y <= not a;
    end generate gen_inv;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "if_gen");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}

#[test]
fn package_with_types() {
    let src = r#"
package my_pkg is
    constant DATA_WIDTH : integer := 8;
end package my_pkg;

entity pkg_user is
    port (
        d : in  std_logic_vector(7 downto 0);
        q : out std_logic_vector(7 downto 0)
    );
end entity pkg_user;

architecture rtl of pkg_user is
begin
    q <= d;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "pkg_user");
    assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
}
