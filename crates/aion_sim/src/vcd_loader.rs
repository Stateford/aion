//! VCD file loader for reading previously recorded waveform data.
//!
//! Parses IEEE 1364 Value Change Dump (VCD) files produced by `VcdRecorder`
//! or other simulators, returning a [`LoadedWaveform`] with signal definitions
//! and value-change histories that can be displayed in the TUI viewer.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use aion_common::{Logic, LogicVec};
use thiserror::Error;

use crate::time::{FS_PER_MS, FS_PER_NS, FS_PER_PS, FS_PER_US};

/// Errors that can occur while loading a VCD file.
#[derive(Debug, Error)]
pub enum VcdLoadError {
    /// An I/O error occurred while reading.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A parse error at a specific line number.
    #[error("parse error at line {line}: {message}")]
    ParseError {
        /// The 1-based line number where the error occurred.
        line: usize,
        /// Description of the error.
        message: String,
    },
    /// The VCD file has a structural format error.
    #[error("format error: {0}")]
    FormatError(String),
}

/// Timescale information from the VCD header.
///
/// Records the number of femtoseconds per VCD time unit, used to convert
/// VCD timestamps (which are in timescale units) to absolute femtoseconds.
#[derive(Clone, Debug)]
pub struct VcdTimescale {
    /// Femtoseconds per VCD time unit.
    pub fs_per_unit: u64,
}

impl Default for VcdTimescale {
    fn default() -> Self {
        Self { fs_per_unit: 1 } // default: 1 fs
    }
}

/// Metadata for a signal found in the VCD file.
#[derive(Clone, Debug)]
pub struct VcdSignalDef {
    /// The VCD identifier code (e.g., "!", "\"", "!\"").
    pub id_code: String,
    /// The hierarchical signal name (dotted path from scope stack).
    pub name: String,
    /// Bit width of the signal.
    pub width: u32,
    /// The VCD variable type (e.g., "wire", "reg").
    pub var_type: String,
}

/// A fully loaded waveform from a VCD file.
///
/// Contains the timescale, signal definitions, and per-signal value-change
/// histories ready for display in the TUI viewer.
#[derive(Clone, Debug)]
pub struct LoadedWaveform {
    /// The timescale from the VCD header.
    pub timescale: VcdTimescale,
    /// Signal definitions in order of registration.
    pub signals: Vec<VcdSignalDef>,
    /// Per-signal value-change histories, parallel to `signals`.
    /// Each entry is a list of `(time_fs, value)` pairs sorted by time.
    pub histories: Vec<Vec<(u64, LogicVec)>>,
}

/// Loads a VCD waveform from a buffered reader.
///
/// Parses the VCD header (timescale, scopes, variables) and all value
/// changes, returning a [`LoadedWaveform`] with signal definitions and
/// histories indexed in femtoseconds.
///
/// # Errors
///
/// Returns [`VcdLoadError`] on I/O errors, parse errors, or missing
/// `$enddefinitions`.
pub fn load_vcd<R: BufRead>(reader: R) -> Result<LoadedWaveform, VcdLoadError> {
    let mut timescale = VcdTimescale::default();
    let mut signals: Vec<VcdSignalDef> = Vec::new();
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut scope_stack: Vec<String> = Vec::new();
    let mut in_definitions = true;
    let mut saw_enddefinitions = false;
    let mut histories: Vec<Vec<(u64, LogicVec)>> = Vec::new();
    let mut current_time_fs: u64 = 0;
    let mut line_num: usize = 0;

    // Buffer for multi-line keyword parsing
    let mut pending_keyword: Option<String> = None;
    let mut pending_body = String::new();

    for line_result in reader.lines() {
        let line = line_result?;
        line_num += 1;
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Handle multi-line keywords (collect until $end)
        if let Some(ref kw) = pending_keyword.clone() {
            if trimmed.contains("$end") {
                // Extract content before $end
                if let Some(pos) = trimmed.find("$end") {
                    pending_body.push(' ');
                    pending_body.push_str(trimmed[..pos].trim());
                }
                process_keyword(
                    kw,
                    pending_body.trim(),
                    &mut timescale,
                    &mut signals,
                    &mut id_to_idx,
                    &mut histories,
                    &mut scope_stack,
                    line_num,
                )?;
                pending_keyword = None;
                pending_body.clear();
            } else {
                pending_body.push(' ');
                pending_body.push_str(trimmed);
            }
            continue;
        }

        if in_definitions {
            if trimmed.starts_with("$enddefinitions") {
                saw_enddefinitions = true;
                in_definitions = false;
                continue;
            }

            // Check for keywords that may span multiple lines
            if let Some(kw) = extract_keyword(trimmed) {
                if trimmed.contains("$end") && kw != "enddefinitions" {
                    // Single-line keyword
                    let body = extract_keyword_body(trimmed);
                    process_keyword(
                        &kw,
                        &body,
                        &mut timescale,
                        &mut signals,
                        &mut id_to_idx,
                        &mut histories,
                        &mut scope_stack,
                        line_num,
                    )?;
                } else if kw == "scope" || kw == "upscope" || kw == "var" || kw == "timescale" {
                    // Multi-line — start collecting
                    let body = extract_keyword_body(trimmed);
                    pending_keyword = Some(kw);
                    pending_body = body;
                }
                // Skip $comment, $date, $version — they end with $end
                else {
                    pending_keyword = Some(kw);
                    pending_body.clear();
                }
            }
            continue;
        }

        // Value change phase
        if trimmed.starts_with("$dumpvars") || trimmed.starts_with("$end") {
            continue;
        }

        if let Some(time_str) = trimmed.strip_prefix('#') {
            // Timestamp
            match time_str.parse::<u64>() {
                Ok(t) => current_time_fs = t * timescale.fs_per_unit,
                Err(_) => {
                    return Err(VcdLoadError::ParseError {
                        line: line_num,
                        message: format!("invalid timestamp: {trimmed}"),
                    });
                }
            }
            continue;
        }

        // Value change line
        parse_value_change(
            trimmed,
            current_time_fs,
            &id_to_idx,
            &signals,
            &mut histories,
            line_num,
        )?;
    }

    if !saw_enddefinitions && !signals.is_empty() {
        return Err(VcdLoadError::FormatError(
            "missing $enddefinitions".to_string(),
        ));
    }

    Ok(LoadedWaveform {
        timescale,
        signals,
        histories,
    })
}

/// Loads a VCD file from a filesystem path.
///
/// Opens the file, wraps it in a `BufReader`, and calls [`load_vcd`].
///
/// # Errors
///
/// Returns [`VcdLoadError`] on I/O or parse errors.
pub fn load_vcd_file(path: &Path) -> Result<LoadedWaveform, VcdLoadError> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    load_vcd(reader)
}

/// Extracts a VCD keyword name from a line starting with `$`.
fn extract_keyword(line: &str) -> Option<String> {
    if !line.starts_with('$') {
        return None;
    }
    let rest = &line[1..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '$')
        .unwrap_or(rest.len());
    let kw = &rest[..end];
    if kw.is_empty() {
        None
    } else {
        Some(kw.to_lowercase())
    }
}

/// Extracts the body text between the keyword and `$end` on a single line.
fn extract_keyword_body(line: &str) -> String {
    // Find first whitespace after $keyword
    let after_dollar = if let Some(pos) = line.find(|c: char| c.is_whitespace()) {
        &line[pos..]
    } else {
        return String::new();
    };
    // Remove trailing $end
    let body = if let Some(pos) = after_dollar.find("$end") {
        &after_dollar[..pos]
    } else {
        after_dollar
    };
    body.trim().to_string()
}

/// Processes a completed VCD keyword with its body text.
#[allow(clippy::too_many_arguments)]
fn process_keyword(
    keyword: &str,
    body: &str,
    timescale: &mut VcdTimescale,
    signals: &mut Vec<VcdSignalDef>,
    id_to_idx: &mut HashMap<String, usize>,
    histories: &mut Vec<Vec<(u64, LogicVec)>>,
    scope_stack: &mut Vec<String>,
    line_num: usize,
) -> Result<(), VcdLoadError> {
    match keyword {
        "timescale" => {
            timescale.fs_per_unit = parse_timescale(body, line_num)?;
        }
        "scope" => {
            // Format: "module <name>" or "begin <name>" etc.
            let parts: Vec<&str> = body.split_whitespace().collect();
            if parts.len() >= 2 {
                scope_stack.push(parts[1].to_string());
            } else if parts.len() == 1 {
                scope_stack.push(parts[0].to_string());
            }
        }
        "upscope" => {
            scope_stack.pop();
        }
        "var" => {
            // Format: "<type> <width> <id_code> <name>"
            let parts: Vec<&str> = body.split_whitespace().collect();
            if parts.len() < 4 {
                return Err(VcdLoadError::ParseError {
                    line: line_num,
                    message: format!("invalid $var: {body}"),
                });
            }
            let var_type = parts[0].to_string();
            let width: u32 = parts[1].parse().map_err(|_| VcdLoadError::ParseError {
                line: line_num,
                message: format!("invalid width in $var: {}", parts[1]),
            })?;
            let id_code = parts[2].to_string();
            let var_name = parts[3].to_string();

            // Build hierarchical name
            let name = if scope_stack.is_empty() {
                var_name
            } else {
                format!("{}.{}", scope_stack.join("."), var_name)
            };

            let idx = signals.len();
            signals.push(VcdSignalDef {
                id_code: id_code.clone(),
                name,
                width,
                var_type,
            });
            id_to_idx.insert(id_code, idx);
            histories.push(Vec::new());
        }
        _ => {
            // Ignore $comment, $date, $version, etc.
        }
    }
    Ok(())
}

/// Parses a VCD timescale string like "1ns", "10ps", "100fs" into femtoseconds.
fn parse_timescale(body: &str, line_num: usize) -> Result<u64, VcdLoadError> {
    let s = body.trim();
    if s.is_empty() {
        return Ok(1); // default 1fs
    }

    // Find boundary between digits and unit
    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_str, unit_str) = s.split_at(digit_end);

    let num: u64 = if num_str.is_empty() {
        1
    } else {
        num_str.parse().map_err(|_| VcdLoadError::ParseError {
            line: line_num,
            message: format!("invalid timescale number: {num_str}"),
        })?
    };

    let unit = unit_str.trim().to_lowercase();
    let fs_per = match unit.as_str() {
        "fs" => 1,
        "ps" => FS_PER_PS,
        "ns" => FS_PER_NS,
        "us" => FS_PER_US,
        "ms" => FS_PER_MS,
        "s" => FS_PER_MS * 1000,
        "" => 1, // bare number = femtoseconds
        _ => {
            return Err(VcdLoadError::ParseError {
                line: line_num,
                message: format!("unknown timescale unit: {unit}"),
            });
        }
    };

    Ok(num * fs_per)
}

/// Parses a single value-change line and appends to histories.
fn parse_value_change(
    line: &str,
    time_fs: u64,
    id_to_idx: &HashMap<String, usize>,
    signals: &[VcdSignalDef],
    histories: &mut [Vec<(u64, LogicVec)>],
    line_num: usize,
) -> Result<(), VcdLoadError> {
    if line.is_empty() {
        return Ok(());
    }

    let first = line.as_bytes()[0];

    if first == b'b' || first == b'B' {
        // Multi-bit: "b<bits> <id_code>"
        let rest = &line[1..];
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(VcdLoadError::ParseError {
                line: line_num,
                message: format!("invalid binary value change: {line}"),
            });
        }
        let bits_str = parts[0];
        let id_code = parts[1];

        if let Some(&idx) = id_to_idx.get(id_code) {
            let width = signals[idx].width;
            let value = parse_binary_value(bits_str, width);
            histories[idx].push((time_fs, value));
        }
    } else if first == b'0'
        || first == b'1'
        || first == b'x'
        || first == b'X'
        || first == b'z'
        || first == b'Z'
    {
        // Single-bit: "<value><id_code>"
        let value_char = line.chars().next().unwrap();
        let id_code = &line[1..];

        if let Some(&idx) = id_to_idx.get(id_code) {
            let mut v = LogicVec::new(1);
            v.set(
                0,
                match value_char {
                    '0' => Logic::Zero,
                    '1' => Logic::One,
                    'x' | 'X' => Logic::X,
                    'z' | 'Z' => Logic::Z,
                    _ => Logic::X,
                },
            );
            histories[idx].push((time_fs, v));
        }
    }
    // Skip other lines (e.g., $dumpoff, $dumpon, real values)

    Ok(())
}

/// Parses a binary value string (MSB-first) into a [`LogicVec`].
///
/// Left-extends short values with '0' (or 'x'/'z' if MSB is x/z).
fn parse_binary_value(bits: &str, width: u32) -> LogicVec {
    let mut v = LogicVec::new(width);
    let bit_chars: Vec<char> = bits.chars().collect();
    let bit_count = bit_chars.len();

    // Determine fill value from MSB
    let fill = if bit_count > 0 {
        match bit_chars[0] {
            'x' | 'X' => Logic::X,
            'z' | 'Z' => Logic::Z,
            _ => Logic::Zero,
        }
    } else {
        Logic::Zero
    };

    // Fill positions not covered by the binary string
    for i in bit_count as u32..width {
        v.set(i, fill);
    }

    // Set bits from the string (MSB-first)
    for (i, &ch) in bit_chars.iter().enumerate() {
        let bit_idx = bit_count - 1 - i; // Convert MSB-first to bit index
        if (bit_idx as u32) < width {
            v.set(
                bit_idx as u32,
                match ch {
                    '0' => Logic::Zero,
                    '1' => Logic::One,
                    'x' | 'X' => Logic::X,
                    'z' | 'Z' => Logic::Z,
                    _ => Logic::X,
                },
            );
        }
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::SimSignalId;
    use crate::waveform::VcdRecorder;
    use crate::waveform::WaveformRecorder;
    use std::io::Cursor;

    fn minimal_vcd() -> &'static str {
        "\
$date
  Simulation date
$end
$version
  Aion HDL Simulator
$end
$timescale
  1fs
$end
$scope module top $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
$dumpvars
#0
0!
#1000
1!
#2000
0!
"
    }

    #[test]
    fn load_minimal_vcd() {
        let reader = Cursor::new(minimal_vcd());
        let waveform = load_vcd(reader).unwrap();

        assert_eq!(waveform.signals.len(), 1);
        assert_eq!(waveform.signals[0].name, "top.clk");
        assert_eq!(waveform.signals[0].width, 1);
        assert_eq!(waveform.signals[0].id_code, "!");

        assert_eq!(waveform.histories[0].len(), 3);
        assert_eq!(waveform.histories[0][0].0, 0); // time
        assert_eq!(waveform.histories[0][1].0, 1000);
        assert_eq!(waveform.histories[0][2].0, 2000);
    }

    #[test]
    fn timescale_1ns() {
        let vcd = "\
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
#0
0!
#10
1!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.timescale.fs_per_unit, FS_PER_NS);
        // #10 * 1ns = 10_000_000 fs
        assert_eq!(waveform.histories[0][1].0, 10 * FS_PER_NS);
    }

    #[test]
    fn timescale_10ps() {
        let vcd = "\
$timescale 10ps $end
$scope module top $end
$var wire 1 ! s $end
$upscope $end
$enddefinitions $end
#0
0!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.timescale.fs_per_unit, 10 * FS_PER_PS);
    }

    #[test]
    fn timescale_100us() {
        let vcd = "\
$timescale 100us $end
$scope module top $end
$var wire 1 ! s $end
$upscope $end
$enddefinitions $end
#0
0!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.timescale.fs_per_unit, 100 * FS_PER_US);
    }

    #[test]
    fn multi_signal() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 1 \" data $end
$upscope $end
$enddefinitions $end
#0
0!
1\"
#100
1!
0\"
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.signals.len(), 2);
        assert_eq!(waveform.signals[0].name, "top.clk");
        assert_eq!(waveform.signals[1].name, "top.data");
        assert_eq!(waveform.histories[0].len(), 2);
        assert_eq!(waveform.histories[1].len(), 2);
    }

    #[test]
    fn binary_values() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 4 ! data $end
$upscope $end
$enddefinitions $end
#0
b0000 !
#100
b1010 !
#200
b1111 !
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.histories[0].len(), 3);
        assert_eq!(waveform.histories[0][0].1.to_u64(), Some(0b0000));
        assert_eq!(waveform.histories[0][1].1.to_u64(), Some(0b1010));
        assert_eq!(waveform.histories[0][2].1.to_u64(), Some(0b1111));
    }

    #[test]
    fn xz_values() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 1 ! sig $end
$upscope $end
$enddefinitions $end
#0
x!
#100
z!
#200
1!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.histories[0][0].1.get(0), Logic::X);
        assert_eq!(waveform.histories[0][1].1.get(0), Logic::Z);
        assert_eq!(waveform.histories[0][2].1.get(0), Logic::One);
    }

    #[test]
    fn hierarchical_scopes() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$scope module cpu $end
$var wire 1 ! clk $end
$upscope $end
$scope module mem $end
$var wire 8 \" data $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
b00000000 \"
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.signals[0].name, "top.cpu.clk");
        assert_eq!(waveform.signals[1].name, "top.mem.data");
        assert_eq!(waveform.signals[1].width, 8);
    }

    #[test]
    fn dumpvars_section() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
$dumpvars
0!
$end
#100
1!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.histories[0].len(), 2);
    }

    #[test]
    fn empty_vcd() {
        let vcd = "\
$timescale 1fs $end
$enddefinitions $end
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert!(waveform.signals.is_empty());
        assert!(waveform.histories.is_empty());
    }

    #[test]
    fn missing_enddefinitions_with_signals() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 1 ! clk $end
$upscope $end
";
        let result = load_vcd(Cursor::new(vcd));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("missing $enddefinitions"));
    }

    #[test]
    fn multichar_id_codes() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 1 !! sig0 $end
$var wire 1 !\" sig1 $end
$upscope $end
$enddefinitions $end
#0
0!!
1!\"
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.signals.len(), 2);
        assert_eq!(waveform.signals[0].id_code, "!!");
        assert_eq!(waveform.signals[1].id_code, "!\"");
        assert_eq!(waveform.histories[0][0].1.get(0), Logic::Zero);
        assert_eq!(waveform.histories[1][0].1.get(0), Logic::One);
    }

    #[test]
    fn large_timestamps() {
        let vcd = "\
$timescale 1ns $end
$scope module top $end
$var wire 1 ! sig $end
$upscope $end
$enddefinitions $end
#0
0!
#1000000
1!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.histories[0][1].0, 1_000_000 * FS_PER_NS);
    }

    #[test]
    fn binary_value_extension() {
        // Short binary value should be zero-extended on the left
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 8 ! data $end
$upscope $end
$enddefinitions $end
#0
b101 !
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        // b101 in 8 bits = 00000101 = 5
        assert_eq!(waveform.histories[0][0].1.to_u64(), Some(5));
        assert_eq!(waveform.histories[0][0].1.width(), 8);
    }

    #[test]
    fn binary_xz_extension() {
        // Binary value with x MSB should be x-extended
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var wire 4 ! data $end
$upscope $end
$enddefinitions $end
#0
bx1 !
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        // bx1 in 4 bits: bit3=x, bit2=x, bit1=x, bit0=1
        assert_eq!(waveform.histories[0][0].1.get(0), Logic::One);
        assert_eq!(waveform.histories[0][0].1.get(1), Logic::X);
        assert_eq!(waveform.histories[0][0].1.get(2), Logic::X);
        assert_eq!(waveform.histories[0][0].1.get(3), Logic::X);
    }

    #[test]
    fn roundtrip_write_then_load() {
        // Write a VCD with VcdRecorder, then load it back
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut rec = VcdRecorder::new(&mut buf);
            rec.begin_scope("top").unwrap();
            rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
                .unwrap();
            rec.register_signal(SimSignalId::from_raw(1), "data", 4)
                .unwrap();
            rec.end_scope().unwrap();
            rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
                .unwrap();
            rec.record_change(0, SimSignalId::from_raw(1), &LogicVec::from_u64(0, 4))
                .unwrap();
            rec.record_change(
                5_000_000,
                SimSignalId::from_raw(0),
                &LogicVec::from_bool(true),
            )
            .unwrap();
            rec.record_change(
                5_000_000,
                SimSignalId::from_raw(1),
                &LogicVec::from_u64(0b1010, 4),
            )
            .unwrap();
            rec.record_change(
                10_000_000,
                SimSignalId::from_raw(0),
                &LogicVec::from_bool(false),
            )
            .unwrap();
            rec.finalize().unwrap();
        }

        let loaded = load_vcd(Cursor::new(&buf)).unwrap();

        // Verify signals
        assert_eq!(loaded.signals.len(), 2);
        assert_eq!(loaded.signals[0].name, "top.clk");
        assert_eq!(loaded.signals[0].width, 1);
        assert_eq!(loaded.signals[1].name, "top.data");
        assert_eq!(loaded.signals[1].width, 4);

        // Verify clk history
        assert_eq!(loaded.histories[0].len(), 3);
        assert_eq!(loaded.histories[0][0].1.get(0), Logic::Zero);
        assert_eq!(loaded.histories[0][1].1.get(0), Logic::One);
        assert_eq!(loaded.histories[0][2].1.get(0), Logic::Zero);

        // Verify data history
        assert_eq!(loaded.histories[1].len(), 2);
        assert_eq!(loaded.histories[1][0].1.to_u64(), Some(0));
        assert_eq!(loaded.histories[1][1].1.to_u64(), Some(0b1010));
    }

    #[test]
    fn load_vcd_file_not_found() {
        let result = load_vcd_file(Path::new("/nonexistent/file.vcd"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VcdLoadError::Io(_)));
    }

    #[test]
    fn parse_timescale_variants() {
        assert_eq!(parse_timescale("1fs", 1).unwrap(), 1);
        assert_eq!(parse_timescale("1ps", 1).unwrap(), FS_PER_PS);
        assert_eq!(parse_timescale("1ns", 1).unwrap(), FS_PER_NS);
        assert_eq!(parse_timescale("10ns", 1).unwrap(), 10 * FS_PER_NS);
        assert_eq!(parse_timescale("100ps", 1).unwrap(), 100 * FS_PER_PS);
        assert_eq!(parse_timescale("1us", 1).unwrap(), FS_PER_US);
        assert_eq!(parse_timescale("1ms", 1).unwrap(), FS_PER_MS);
        assert_eq!(parse_timescale("1s", 1).unwrap(), FS_PER_MS * 1000);
    }

    #[test]
    fn parse_binary_value_basic() {
        let v = parse_binary_value("1010", 4);
        assert_eq!(v.to_u64(), Some(0b1010));
        assert_eq!(v.width(), 4);
    }

    #[test]
    fn comment_and_version_skipped() {
        let vcd = "\
$comment
  This is a comment
  with multiple lines
$end
$version
  Some Simulator v1.0
$end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! sig $end
$upscope $end
$enddefinitions $end
#0
0!
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.signals.len(), 1);
        assert_eq!(waveform.signals[0].name, "top.sig");
    }

    #[test]
    fn var_type_preserved() {
        let vcd = "\
$timescale 1fs $end
$scope module top $end
$var reg 8 ! count $end
$upscope $end
$enddefinitions $end
";
        let waveform = load_vcd(Cursor::new(vcd)).unwrap();
        assert_eq!(waveform.signals[0].var_type, "reg");
    }
}
