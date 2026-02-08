//! Shared pipeline helpers for CLI commands.
//!
//! Contains common utilities used by `lint`, `sim`, and `test` commands:
//! source file discovery, language detection, project root resolution,
//! duration parsing, and the parse-all-files step.

use std::path::{Path, PathBuf};

use aion_common::Interner;
use aion_diagnostics::{DiagnosticRenderer, DiagnosticSink, TerminalRenderer};
use aion_elaborate::ParsedDesign;
use aion_sim::time::{FS_PER_MS, FS_PER_NS, FS_PER_PS, FS_PER_US};
use aion_source::SourceDb;

use crate::GlobalArgs;

/// Femtoseconds per second (1e15).
const FS_PER_S: u64 = FS_PER_MS * 1_000;

/// HDL language detected from a file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLanguage {
    /// Verilog-2005 (`.v`).
    Verilog,
    /// SystemVerilog-2017 (`.sv`).
    SystemVerilog,
    /// VHDL-2008 (`.vhd`, `.vhdl`).
    Vhdl,
}

/// Walks up from `start` looking for the nearest directory containing `aion.toml`.
///
/// Returns the directory containing `aion.toml`, or an error if none is found.
pub fn find_project_root(start: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut current = start.to_path_buf();
    loop {
        if current.join("aion.toml").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(format!(
                "could not find aion.toml in {} or any parent directory",
                start.display()
            )
            .into());
        }
    }
}

/// Resolves the project root directory from global CLI args.
///
/// If `--config` is specified, uses that path (file → parent dir, dir → itself).
/// Otherwise walks up from the current directory looking for `aion.toml`.
pub fn resolve_project_root(global: &GlobalArgs) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(ref config_path) = global.config {
        let p = PathBuf::from(config_path);
        if p.is_file() {
            Ok(p.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")))
        } else {
            Ok(p)
        }
    } else {
        find_project_root(&std::env::current_dir()?)
    }
}

/// Discovers HDL source files in the given directory (recursive).
///
/// Returns a list of `(path, language)` pairs for files with recognized
/// HDL extensions (`.v`, `.sv`, `.vhd`, `.vhdl`), sorted by path.
pub fn discover_source_files(
    dir: &Path,
) -> Result<Vec<(PathBuf, SourceLanguage)>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    walk_dir(dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Recursively walks a directory collecting HDL source files.
fn walk_dir(
    dir: &Path,
    files: &mut Vec<(PathBuf, SourceLanguage)>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, files)?;
        } else if let Some(lang) = detect_language(&path) {
            files.push((path, lang));
        }
    }
    Ok(())
}

/// Detects the HDL language from a file's extension.
///
/// Returns `None` for unrecognized extensions.
pub fn detect_language(path: &Path) -> Option<SourceLanguage> {
    match path.extension()?.to_str()? {
        "v" => Some(SourceLanguage::Verilog),
        "sv" => Some(SourceLanguage::SystemVerilog),
        "vhd" | "vhdl" => Some(SourceLanguage::Vhdl),
        _ => None,
    }
}

/// Parses a human-readable duration string into femtoseconds.
///
/// Supports units: `fs`, `ps`, `ns`, `us`, `ms`, `s`.
/// Examples: `"100ns"`, `"1us"`, `"10ms"`, `"500ps"`, `"0fs"`.
pub fn parse_duration(s: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".into());
    }

    // Find where digits end and unit begins
    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());

    if digit_end == 0 {
        return Err(format!("invalid duration: no numeric value in '{s}'").into());
    }

    let number: u64 = s[..digit_end]
        .parse()
        .map_err(|_| format!("invalid number in duration '{s}'"))?;

    let unit = s[digit_end..].trim();

    let multiplier = match unit {
        "fs" => 1,
        "ps" => FS_PER_PS,
        "ns" => FS_PER_NS,
        "us" => FS_PER_US,
        "ms" => FS_PER_MS,
        "s" => FS_PER_S,
        "" => {
            return Err(
                format!("missing unit in duration '{s}' (use fs, ps, ns, us, ms, or s)").into(),
            )
        }
        _ => {
            return Err(
                format!("unknown duration unit '{unit}' (use fs, ps, ns, us, ms, or s)").into(),
            )
        }
    };

    Ok(number * multiplier)
}

/// Parses all source files into a `ParsedDesign`, loading them into the source DB.
///
/// Each file is loaded into `source_db`, lexed/parsed with the appropriate parser,
/// and the resulting AST is collected into the returned `ParsedDesign`.
pub fn parse_all_files(
    source_files: &[(PathBuf, SourceLanguage)],
    source_db: &mut SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> Result<ParsedDesign, Box<dyn std::error::Error>> {
    let mut verilog_files = Vec::new();
    let mut sv_files = Vec::new();
    let mut vhdl_files = Vec::new();

    for (path, lang) in source_files {
        let file_id = source_db.load_file(path)?;

        match lang {
            SourceLanguage::Verilog => {
                let ast = aion_verilog_parser::parse_file(file_id, source_db, interner, sink);
                verilog_files.push(ast);
            }
            SourceLanguage::SystemVerilog => {
                let ast = aion_sv_parser::parse_file(file_id, source_db, interner, sink);
                sv_files.push(ast);
            }
            SourceLanguage::Vhdl => {
                let ast = aion_vhdl_parser::parse_file(file_id, source_db, interner, sink);
                vhdl_files.push(ast);
            }
        }
    }

    Ok(ParsedDesign {
        verilog_files,
        sv_files,
        vhdl_files,
    })
}

/// Renders all diagnostics from a sink to stderr using the terminal renderer.
///
/// Returns the number of diagnostics rendered.
pub fn render_diagnostics(sink: &DiagnosticSink, source_db: &SourceDb, color: bool) -> usize {
    let diagnostics = sink.diagnostics();
    let renderer = TerminalRenderer::new(color, 80);
    for diag in &diagnostics {
        eprintln!("{}", renderer.render(diag, source_db));
    }
    diagnostics.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // -- find_project_root tests --

    #[test]
    fn find_project_root_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("aion.toml"),
            "[project]\nname=\"t\"\nversion=\"0.1.0\"\ntop=\"top\"",
        )
        .unwrap();
        let root = find_project_root(tmp.path()).unwrap();
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn find_project_root_in_parent() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("aion.toml"),
            "[project]\nname=\"t\"\nversion=\"0.1.0\"\ntop=\"top\"",
        )
        .unwrap();
        let sub = tmp.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        let root = find_project_root(&sub).unwrap();
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn find_project_root_not_found() {
        let tmp = TempDir::new().unwrap();
        let result = find_project_root(tmp.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("could not find aion.toml"));
    }

    // -- detect_language tests --

    #[test]
    fn detect_language_verilog() {
        assert_eq!(
            detect_language(Path::new("foo.v")),
            Some(SourceLanguage::Verilog)
        );
    }

    #[test]
    fn detect_language_systemverilog() {
        assert_eq!(
            detect_language(Path::new("foo.sv")),
            Some(SourceLanguage::SystemVerilog)
        );
    }

    #[test]
    fn detect_language_vhdl() {
        assert_eq!(
            detect_language(Path::new("foo.vhd")),
            Some(SourceLanguage::Vhdl)
        );
        assert_eq!(
            detect_language(Path::new("foo.vhdl")),
            Some(SourceLanguage::Vhdl)
        );
    }

    #[test]
    fn detect_language_unknown() {
        assert_eq!(detect_language(Path::new("foo.rs")), None);
        assert_eq!(detect_language(Path::new("foo.txt")), None);
        assert_eq!(detect_language(Path::new("foo")), None);
    }

    // -- discover_source_files tests --

    #[test]
    fn discover_files_finds_hdl() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("top.sv"), "module top; endmodule").unwrap();
        fs::write(src.join("sub.v"), "module sub; endmodule").unwrap();
        fs::write(src.join("readme.txt"), "not hdl").unwrap();

        let files = discover_source_files(&src).unwrap();
        assert_eq!(files.len(), 2);
        let langs: Vec<_> = files.iter().map(|(_, l)| *l).collect();
        assert!(langs.contains(&SourceLanguage::Verilog));
        assert!(langs.contains(&SourceLanguage::SystemVerilog));
    }

    #[test]
    fn discover_files_recursive() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path();
        let sub = src.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.join("top.sv"), "module top; endmodule").unwrap();
        fs::write(sub.join("child.vhd"), "entity child is end;").unwrap();

        let files = discover_source_files(src).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn discover_files_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let files = discover_source_files(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    // -- parse_duration tests --

    #[test]
    fn parse_duration_nanoseconds() {
        assert_eq!(parse_duration("100ns").unwrap(), 100 * FS_PER_NS);
    }

    #[test]
    fn parse_duration_microseconds() {
        assert_eq!(parse_duration("5us").unwrap(), 5 * FS_PER_US);
    }

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(parse_duration("10ms").unwrap(), 10 * FS_PER_MS);
    }

    #[test]
    fn parse_duration_picoseconds() {
        assert_eq!(parse_duration("250ps").unwrap(), 250 * FS_PER_PS);
    }

    #[test]
    fn parse_duration_femtoseconds() {
        assert_eq!(parse_duration("42fs").unwrap(), 42);
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("1s").unwrap(), FS_PER_S);
    }

    #[test]
    fn parse_duration_zero() {
        assert_eq!(parse_duration("0ns").unwrap(), 0);
    }

    #[test]
    fn parse_duration_invalid_unit() {
        let err = parse_duration("100xyz").unwrap_err();
        assert!(err.to_string().contains("unknown duration unit"));
    }

    #[test]
    fn parse_duration_no_number() {
        let err = parse_duration("ns").unwrap_err();
        assert!(err.to_string().contains("no numeric value"));
    }

    #[test]
    fn parse_duration_empty() {
        let err = parse_duration("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn parse_duration_missing_unit() {
        let err = parse_duration("100").unwrap_err();
        assert!(err.to_string().contains("missing unit"));
    }

    #[test]
    fn parse_duration_with_whitespace() {
        assert_eq!(parse_duration("  50ns  ").unwrap(), 50 * FS_PER_NS);
    }

    // -- resolve_project_root tests --

    #[test]
    fn resolve_project_root_from_config_file() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("aion.toml");
        fs::write(
            &config_path,
            "[project]\nname=\"t\"\nversion=\"0.1.0\"\ntop=\"top\"",
        )
        .unwrap();

        let global = GlobalArgs {
            quiet: false,
            verbose: false,
            color: false,
            config: Some(config_path.to_str().unwrap().to_string()),
        };
        let root = resolve_project_root(&global).unwrap();
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn resolve_project_root_from_config_dir() {
        let tmp = TempDir::new().unwrap();
        let global = GlobalArgs {
            quiet: false,
            verbose: false,
            color: false,
            config: Some(tmp.path().to_str().unwrap().to_string()),
        };
        let root = resolve_project_root(&global).unwrap();
        assert_eq!(root, tmp.path());
    }
}
