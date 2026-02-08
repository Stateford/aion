//! `aion init` — project scaffolding command.
//!
//! Creates a new Aion project directory with standard layout: `src/`, `tests/`,
//! `constraints/`, `ip/`, an `aion.toml` config file, and a template top module
//! with testbench.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::HdlLanguage;

/// Runs the `aion init` command.
///
/// If `name` is `Some`, creates a new subdirectory with that name.
/// Otherwise initializes in the current working directory.
/// Returns exit code 0 on success.
pub fn run(
    name: Option<String>,
    lang: HdlLanguage,
    target: Option<String>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let project_dir = match &name {
        Some(n) => {
            let dir = PathBuf::from(n);
            if dir.exists() {
                return Err(format!("directory '{}' already exists", n).into());
            }
            fs::create_dir_all(&dir)?;
            dir
        }
        None => std::env::current_dir()?,
    };

    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my_project");

    eprintln!("  Creating new Aion project `{project_name}`");

    create_directories(&project_dir)?;

    let top_ext = top_extension(lang);
    let tb_ext = testbench_extension(lang);

    write_aion_toml(&project_dir, project_name, target.as_deref(), top_ext)?;
    write_top_file(&project_dir, lang, top_ext)?;
    write_testbench_file(&project_dir, lang, tb_ext)?;

    eprintln!("     Created {}", project_dir.join("aion.toml").display());
    eprintln!(
        "     Created {}",
        project_dir
            .join("src")
            .join(format!("top.{top_ext}"))
            .display()
    );
    eprintln!(
        "     Created {}",
        project_dir
            .join("tests")
            .join(format!("top_tb.{tb_ext}"))
            .display()
    );

    Ok(0)
}

/// Creates the standard project directories.
fn create_directories(root: &Path) -> io::Result<()> {
    for dir in &["src", "tests", "constraints", "ip"] {
        fs::create_dir_all(root.join(dir))?;
    }
    Ok(())
}

/// Returns the file extension for the top module file.
fn top_extension(lang: HdlLanguage) -> &'static str {
    match lang {
        HdlLanguage::Vhdl => "vhd",
        HdlLanguage::Verilog => "v",
        HdlLanguage::SystemVerilog => "sv",
    }
}

/// Returns the file extension for the testbench file.
fn testbench_extension(lang: HdlLanguage) -> &'static str {
    match lang {
        HdlLanguage::Vhdl => "vhd",
        HdlLanguage::Verilog => "v",
        HdlLanguage::SystemVerilog => "sv",
    }
}

/// Writes the `aion.toml` configuration file.
fn write_aion_toml(root: &Path, name: &str, target: Option<&str>, ext: &str) -> io::Result<()> {
    let mut content = format!(
        r#"[project]
name = "{name}"
version = "0.1.0"
top = "top"

[lint]
deny = []
allow = []
"#
    );

    if let Some(device) = target {
        content.push_str(&format!(
            r#"
[targets.default]
device = "{device}"
family = "unknown"
"#
        ));
    }

    // Add a comment about the source directory
    content.push_str(&format!("\n# Source files are in src/top.{ext}\n"));

    fs::write(root.join("aion.toml"), content)
}

/// Writes a template top module file.
fn write_top_file(root: &Path, lang: HdlLanguage, ext: &str) -> io::Result<()> {
    let content = match lang {
        HdlLanguage::SystemVerilog => r#"module top (
    input  logic clk,
    input  logic rst_n,
    output logic led
);

    // TODO: Add your design here
    assign led = 1'b0;

endmodule
"#
        .to_string(),
        HdlLanguage::Verilog => r#"module top (
    input  clk,
    input  rst_n,
    output led
);

    // TODO: Add your design here
    assign led = 1'b0;

endmodule
"#
        .to_string(),
        HdlLanguage::Vhdl => r#"library ieee;
use ieee.std_logic_1164.all;

entity top is
    port (
        clk   : in  std_logic;
        rst_n : in  std_logic;
        led   : out std_logic
    );
end entity top;

architecture rtl of top is
begin
    -- TODO: Add your design here
    led <= '0';
end architecture rtl;
"#
        .to_string(),
    };
    fs::write(root.join("src").join(format!("top.{ext}")), content)
}

/// Writes a template testbench file.
fn write_testbench_file(root: &Path, lang: HdlLanguage, ext: &str) -> io::Result<()> {
    let content = match lang {
        HdlLanguage::SystemVerilog => r#"module top_tb;

    logic clk;
    logic rst_n;
    logic led;

    top uut (
        .clk   (clk),
        .rst_n (rst_n),
        .led   (led)
    );

    initial begin
        clk = 0;
        forever #5 clk = ~clk;
    end

    initial begin
        rst_n = 0;
        #20 rst_n = 1;
        #100 $finish;
    end

endmodule
"#
        .to_string(),
        HdlLanguage::Verilog => r#"module top_tb;

    reg clk;
    reg rst_n;
    wire led;

    top uut (
        .clk   (clk),
        .rst_n (rst_n),
        .led   (led)
    );

    initial begin
        clk = 0;
        forever #5 clk = ~clk;
    end

    initial begin
        rst_n = 0;
        #20 rst_n = 1;
        #100 $finish;
    end

endmodule
"#
        .to_string(),
        HdlLanguage::Vhdl => r#"library ieee;
use ieee.std_logic_1164.all;

entity top_tb is
end entity top_tb;

architecture sim of top_tb is
    signal clk   : std_logic := '0';
    signal rst_n : std_logic := '0';
    signal led   : std_logic;
begin

    uut : entity work.top
        port map (
            clk   => clk,
            rst_n => rst_n,
            led   => led
        );

    clk <= not clk after 5 ns;

    process
    begin
        rst_n <= '0';
        wait for 20 ns;
        rst_n <= '1';
        wait for 100 ns;
        assert false report "Simulation finished" severity failure;
    end process;

end architecture sim;
"#
        .to_string(),
    };
    fs::write(root.join("tests").join(format!("top_tb.{ext}")), content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_creates_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("test_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        assert!(project_dir.join("aion.toml").exists());
        assert!(project_dir.join("src").is_dir());
        assert!(project_dir.join("tests").is_dir());
        assert!(project_dir.join("constraints").is_dir());
        assert!(project_dir.join("ip").is_dir());
        assert!(project_dir.join("src").join("top.sv").exists());
        assert!(project_dir.join("tests").join("top_tb.sv").exists());
    }

    #[test]
    fn init_vhdl_generates_correct_files() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("vhdl_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::Vhdl,
            None,
        )
        .unwrap();

        assert!(project_dir.join("src").join("top.vhd").exists());
        assert!(project_dir.join("tests").join("top_tb.vhd").exists());

        let top = fs::read_to_string(project_dir.join("src").join("top.vhd")).unwrap();
        assert!(top.contains("entity top is"));
        assert!(top.contains("architecture rtl of top is"));
    }

    #[test]
    fn init_verilog_generates_correct_files() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("verilog_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::Verilog,
            None,
        )
        .unwrap();

        assert!(project_dir.join("src").join("top.v").exists());
        assert!(project_dir.join("tests").join("top_tb.v").exists());

        let top = fs::read_to_string(project_dir.join("src").join("top.v")).unwrap();
        assert!(top.contains("module top"));
        assert!(!top.contains("logic")); // Verilog, not SV
    }

    #[test]
    fn init_systemverilog_generates_correct_files() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("sv_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        assert!(project_dir.join("src").join("top.sv").exists());
        assert!(project_dir.join("tests").join("top_tb.sv").exists());

        let top = fs::read_to_string(project_dir.join("src").join("top.sv")).unwrap();
        assert!(top.contains("module top"));
        assert!(top.contains("logic")); // SV uses logic
    }

    #[test]
    fn init_generates_valid_toml() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("toml_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        let toml_str = fs::read_to_string(project_dir.join("aion.toml")).unwrap();
        let config = aion_config::load_config_from_str(&toml_str);
        assert!(
            config.is_ok(),
            "generated aion.toml should be valid: {config:?}"
        );
        let config = config.unwrap();
        assert_eq!(config.project.name, "toml_proj");
        assert_eq!(config.project.version, "0.1.0");
        assert_eq!(config.project.top, "top");
    }

    #[test]
    fn init_with_target_adds_target_section() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("target_proj");
        run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::SystemVerilog,
            Some("xc7a35t".to_string()),
        )
        .unwrap();

        let toml_str = fs::read_to_string(project_dir.join("aion.toml")).unwrap();
        assert!(toml_str.contains("xc7a35t"));
        assert!(toml_str.contains("[targets.default]"));
    }

    #[test]
    fn init_existing_dir_error() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("exists");
        fs::create_dir_all(&project_dir).unwrap();

        let result = run(
            Some(project_dir.to_str().unwrap().to_string()),
            HdlLanguage::SystemVerilog,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn init_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        // We need to set current dir temporarily. Use create_directories directly.
        create_directories(tmp.path()).unwrap();
        assert!(tmp.path().join("src").is_dir());
        assert!(tmp.path().join("tests").is_dir());
        assert!(tmp.path().join("constraints").is_dir());
        assert!(tmp.path().join("ip").is_dir());
    }

    #[test]
    fn top_extension_mapping() {
        assert_eq!(top_extension(HdlLanguage::Vhdl), "vhd");
        assert_eq!(top_extension(HdlLanguage::Verilog), "v");
        assert_eq!(top_extension(HdlLanguage::SystemVerilog), "sv");
    }

    #[test]
    fn testbench_extension_mapping() {
        assert_eq!(testbench_extension(HdlLanguage::Vhdl), "vhd");
        assert_eq!(testbench_extension(HdlLanguage::Verilog), "v");
        assert_eq!(testbench_extension(HdlLanguage::SystemVerilog), "sv");
    }
}
