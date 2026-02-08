//! FST (Fast Signal Trace) waveform recorder.
//!
//! Implements the FST binary waveform format used by GTKWave. FST files are
//! significantly more compact than VCD due to block-level compression (ZLib
//! for value change data and geometry, GZip for hierarchy).
//!
//! The format consists of four block types:
//! - Header (type 0): 329-byte payload with metadata
//! - Value Change Data (type 1): compressed signal value changes
//! - Geometry (type 3): per-signal bit widths
//! - Hierarchy (type 4): scope/signal tree
//!
//! # Usage
//!
//! ```ignore
//! use aion_sim::fst::FstRecorder;
//! use std::io::Cursor;
//!
//! let mut buf = Cursor::new(Vec::new());
//! let mut rec = FstRecorder::new(&mut buf);
//! rec.begin_scope("top").unwrap();
//! rec.register_signal(id, "clk", 1).unwrap();
//! rec.end_scope().unwrap();
//! rec.record_change(0, id, &value).unwrap();
//! rec.finalize().unwrap();
//! ```

use std::io::{Seek, Write};

use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;

use aion_common::{Logic, LogicVec};

use crate::error::SimError;
use crate::value::SimSignalId;
use crate::waveform::WaveformRecorder;

/// FST block type identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum FstBlockType {
    /// Header block containing simulation metadata.
    Header = 0,
    /// Value change data block with compressed signal changes.
    VcData = 1,
    /// Geometry block with per-signal bit widths.
    Geometry = 3,
    /// Hierarchy block with scope/signal tree (GZip compressed).
    Hierarchy = 4,
}

/// FST hierarchy tag bytes per the spec.
const FST_ST_VCD_SCOPE: u8 = 0xFE;
/// Upscope tag in hierarchy data.
const FST_ST_VCD_UPSCOPE: u8 = 0xFF;

/// FST variable type: VCD wire.
const FST_VT_VCD_WIRE: u8 = 0x05;
/// FST variable type: VCD reg.
const FST_VT_VCD_REG: u8 = 0x04;

/// FST scope type: VCD module.
const FST_ST_VCD_MODULE: u8 = 0x03;

/// An entry in the FST hierarchy tree.
#[derive(Clone, Debug)]
enum FstHierEntry {
    /// Opens a new scope (module/block).
    Scope {
        /// Scope name.
        name: String,
    },
    /// Closes the current scope.
    Upscope,
    /// Declares a variable (signal).
    Var {
        /// Signal index (0-based).
        index: u32,
        /// Signal name.
        name: String,
        /// Bit width.
        width: u32,
    },
}

/// A buffered value change event.
#[derive(Clone, Debug)]
struct FstValueChange {
    /// Simulation time in femtoseconds.
    time_fs: u64,
    /// Index of the signal in the registration order.
    signal_index: u32,
    /// New value of the signal.
    value: LogicVec,
}

/// FST (Fast Signal Trace) waveform recorder.
///
/// Buffers all hierarchy and value change data in memory, then writes the
/// complete FST binary format on [`finalize`](WaveformRecorder::finalize).
/// This approach produces correct FST files since the header block requires
/// knowledge of the total signal count and time range.
pub struct FstRecorder<W: Write + Seek> {
    writer: W,
    /// Mapping from SimSignalId to sequential index.
    signal_map: Vec<(SimSignalId, u32)>,
    /// Next sequential signal index.
    next_index: u32,
    /// Buffered hierarchy entries.
    hierarchy: Vec<FstHierEntry>,
    /// Buffered value changes.
    changes: Vec<FstValueChange>,
    /// Per-signal bit widths (indexed by signal index).
    widths: Vec<u32>,
    /// Start time in femtoseconds.
    start_time: u64,
    /// End time in femtoseconds.
    end_time: u64,
    /// Whether any changes have been recorded.
    has_changes: bool,
}

impl<W: Write + Seek> FstRecorder<W> {
    /// Creates a new FST recorder writing to the given output.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            signal_map: Vec::new(),
            next_index: 0,
            hierarchy: Vec::new(),
            changes: Vec::new(),
            widths: Vec::new(),
            start_time: 0,
            end_time: 0,
            has_changes: false,
        }
    }

    /// Looks up the sequential index for a SimSignalId.
    fn find_index(&self, id: SimSignalId) -> Option<u32> {
        self.signal_map
            .iter()
            .find(|(sid, _)| *sid == id)
            .map(|(_, idx)| *idx)
    }

    /// Writes the FST header block (type 0).
    ///
    /// The header is a fixed 329-byte payload containing simulation metadata:
    /// start/end time, endianness marker, signal count, timescale, writer
    /// string, date string, file type, and time zero.
    fn write_header_block(&mut self) -> Result<(), SimError> {
        let mut payload = vec![0u8; 329];

        // Offset 0: start_time (u64 BE)
        payload[0..8].copy_from_slice(&self.start_time.to_be_bytes());
        // Offset 8: end_time (u64 BE)
        payload[8..16].copy_from_slice(&self.end_time.to_be_bytes());
        // Offset 16: real endianness test (f64, native byte order)
        payload[16..24].copy_from_slice(&std::f64::consts::E.to_ne_bytes());
        // Offset 24: writer memory use (u64 BE)
        payload[24..32].copy_from_slice(&0u64.to_be_bytes());
        // Offset 32: num_scopes (u64 BE)
        let scope_count = self
            .hierarchy
            .iter()
            .filter(|h| matches!(h, FstHierEntry::Scope { .. }))
            .count() as u64;
        payload[32..40].copy_from_slice(&scope_count.to_be_bytes());
        // Offset 40: num_hierarchy_vars (u64 BE)
        payload[40..48].copy_from_slice(&(self.next_index as u64).to_be_bytes());
        // Offset 48: num_vars (u64 BE) — same as hierarchy_vars (no aliases)
        payload[48..56].copy_from_slice(&(self.next_index as u64).to_be_bytes());
        // Offset 56: num_vc_blocks (u64 BE)
        let vc_count: u64 = if self.has_changes { 1 } else { 0 };
        payload[56..64].copy_from_slice(&vc_count.to_be_bytes());
        // Offset 64: timescale exponent (i8): -15 = femtoseconds
        payload[64] = (-15_i8) as u8;
        // Offset 65: writer string (128 bytes, null-padded)
        let writer_str = b"Aion HDL Simulator";
        let copy_len = writer_str.len().min(127);
        payload[65..65 + copy_len].copy_from_slice(&writer_str[..copy_len]);
        // Offset 193: date string (26 bytes, null-padded)
        let date_str = b"2024-01-01 00:00:00\n";
        let date_copy = date_str.len().min(25);
        payload[193..193 + date_copy].copy_from_slice(&date_str[..date_copy]);
        // Offsets 219..312: reserved (already zero)
        // Offset 312: file type (u8): 0 = Verilog
        payload[312] = 0;
        // Offset 313: time_zero (i64 BE)
        payload[313..321].copy_from_slice(&0i64.to_be_bytes());
        // Offsets 321..329: padding (already zero)

        write_block(&mut self.writer, FstBlockType::Header, &payload)?;
        Ok(())
    }

    /// Builds the initial value string (bits array) for all signals.
    ///
    /// Returns ASCII bytes: one byte per bit of each signal, in signal order.
    /// Initial values come from the first recorded change for each signal,
    /// or '0' if no change recorded.
    fn build_bits_array(&self) -> Vec<u8> {
        let num_vars = self.next_index as usize;
        let mut initial_values: Vec<Option<&LogicVec>> = vec![None; num_vars];

        // Find first change per signal (the changes at start_time)
        for change in &self.changes {
            let idx = change.signal_index as usize;
            if change.time_fs == self.start_time {
                initial_values[idx] = Some(&change.value);
            }
        }

        let mut bits = Vec::new();
        for (i, init_val) in initial_values.iter().enumerate() {
            let width = self.widths[i];
            if let Some(val) = init_val {
                // Write MSB first
                for bit_idx in (0..width).rev() {
                    bits.push(match val.get(bit_idx) {
                        Logic::Zero => b'0',
                        Logic::One => b'1',
                        Logic::X => b'x',
                        Logic::Z => b'z',
                    });
                }
            } else {
                // Default to 'x' for each bit
                bits.extend(std::iter::repeat_n(b'x', width as usize));
            }
        }
        bits
    }

    /// Builds the waves table for value changes (excluding initial values).
    ///
    /// Returns per-signal encoded change data, concatenated, plus a position
    /// array giving byte offsets into the waves data for each signal.
    fn build_waves_and_positions(&self, unique_times: &[u64]) -> (Vec<u8>, Vec<u64>) {
        let num_vars = self.next_index as usize;
        let time_index: std::collections::HashMap<u64, u64> = unique_times
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, i as u64))
            .collect();

        // Group changes by signal, excluding initial time
        let mut per_signal: Vec<Vec<(u64, &LogicVec)>> = vec![Vec::new(); num_vars];
        for change in &self.changes {
            if change.time_fs == self.start_time {
                continue; // Already in bits array
            }
            let idx = change.signal_index as usize;
            if let Some(&time_idx) = time_index.get(&change.time_fs) {
                per_signal[idx].push((time_idx, &change.value));
            }
        }

        let mut waves_data = Vec::new();
        let mut positions = vec![0u64; num_vars];

        for (sig_idx, changes) in per_signal.iter().enumerate() {
            if changes.is_empty() {
                positions[sig_idx] = 0; // No changes
                continue;
            }
            // Position is 1-based byte offset into waves_data
            positions[sig_idx] = waves_data.len() as u64 + 1;

            let width = self.widths[sig_idx];

            // Encode changes for this signal
            let mut sig_data = Vec::new();
            let mut prev_time_idx: u64 = 0;
            for &(time_idx, value) in changes {
                let time_delta = time_idx - prev_time_idx;
                prev_time_idx = time_idx;

                if width == 1 {
                    // 1-bit encoding: (time_delta << 2 | (value << 1)) for 0/1
                    let bit = value.get(0);
                    match bit {
                        Logic::Zero => {
                            write_varint(&mut sig_data, time_delta << 2);
                        }
                        Logic::One => {
                            write_varint(&mut sig_data, (time_delta << 2) | 2);
                        }
                        Logic::X => {
                            // Extended encoding: (time_delta << 4 | (code << 1)) | 1
                            write_varint(&mut sig_data, (time_delta << 4) | 1);
                        }
                        Logic::Z => {
                            write_varint(&mut sig_data, (time_delta << 4) | 3);
                        }
                    }
                } else {
                    // Multi-bit: (time_delta << 1 | 1) for binary
                    write_varint(&mut sig_data, (time_delta << 1) | 1);
                    // Write value as ASCII bits (MSB first)
                    for bit_idx in (0..width).rev() {
                        sig_data.push(match value.get(bit_idx) {
                            Logic::Zero => b'0',
                            Logic::One => b'1',
                            Logic::X => b'x',
                            Logic::Z => b'z',
                        });
                    }
                }
            }

            // Write per-signal entry: varint(uncompressed_length) + data
            // For simplicity, we write uncompressed (length=0 means uncompressed)
            write_varint(&mut waves_data, 0);
            waves_data.extend_from_slice(&sig_data);
        }

        (waves_data, positions)
    }

    /// Encodes position table using simple varint encoding.
    fn encode_position_table(&self, positions: &[u64]) -> Vec<u8> {
        let mut buf = Vec::new();
        for &pos in positions {
            write_varint(&mut buf, pos);
        }
        buf
    }

    /// Builds the time table: varint-encoded deltas between unique times.
    fn build_time_table(&self, unique_times: &[u64]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut prev = 0u64;
        for &t in unique_times {
            let delta = t - prev;
            write_varint(&mut buf, delta);
            prev = t;
        }
        buf
    }

    /// Writes the value change data block (type 1).
    ///
    /// Contains four sub-sections: bits (initial values), waves (value changes),
    /// position table (per-signal offsets into waves), and time table (time deltas).
    fn write_vc_data_block(&mut self) -> Result<(), SimError> {
        if !self.has_changes {
            return Ok(());
        }

        // Collect unique sorted times
        let mut unique_times: Vec<u64> = self.changes.iter().map(|c| c.time_fs).collect();
        unique_times.sort_unstable();
        unique_times.dedup();

        // Build sub-sections
        let bits_raw = self.build_bits_array();
        let bits_compressed = compress_zlib(&bits_raw)?;

        let (waves_raw, positions) = self.build_waves_and_positions(&unique_times);
        // Compress waves
        let waves_compressed = if waves_raw.is_empty() {
            Vec::new()
        } else {
            compress_zlib(&waves_raw)?
        };

        let position_raw = self.encode_position_table(&positions);

        let time_raw = self.build_time_table(&unique_times);
        let time_compressed = compress_zlib(&time_raw)?;

        // Assemble VcData payload
        let mut payload = Vec::new();

        // start_time (u64 BE)
        write_u64_be(&mut payload, self.start_time);
        // end_time (u64 BE)
        write_u64_be(&mut payload, self.end_time);
        // memory_required (u64 BE) — 0
        write_u64_be(&mut payload, 0);

        // bits section
        write_varint(&mut payload, bits_raw.len() as u64); // bits_uncompressed_length
        write_varint(&mut payload, bits_compressed.len() as u64); // bits_compressed_length
        write_varint(&mut payload, self.next_index as u64); // bits_count (num vars)
        payload.extend_from_slice(&bits_compressed); // bits_data

        // waves section
        write_varint(&mut payload, self.next_index as u64); // waves_count
        payload.push(0x5A); // waves_packtype: 'Z' = ZLib
        payload.extend_from_slice(&waves_compressed); // waves_data

        // position section
        payload.extend_from_slice(&position_raw);
        write_u64_be(&mut payload, position_raw.len() as u64); // position_length

        // time section
        payload.extend_from_slice(&time_compressed);
        write_u64_be(&mut payload, time_raw.len() as u64); // time_uncompressed_length
        write_u64_be(&mut payload, time_compressed.len() as u64); // time_compressed_length
        write_u64_be(&mut payload, unique_times.len() as u64); // time_count

        write_block(&mut self.writer, FstBlockType::VcData, &payload)?;
        Ok(())
    }

    /// Writes the geometry block (type 3).
    ///
    /// Contains per-signal bit widths as varints, preceded by uncompressed
    /// length and count headers.
    fn write_geometry_block(&mut self) -> Result<(), SimError> {
        let mut raw = Vec::new();

        // Write per-signal widths as varints
        for &w in &self.widths {
            write_varint(&mut raw, w as u64);
        }

        let compressed = compress_zlib(&raw)?;

        // Build payload: uncompressed_length(u64 BE) + count(u64 BE) + compressed_data
        let mut payload = Vec::new();
        write_u64_be(&mut payload, raw.len() as u64); // uncompressed_length
        write_u64_be(&mut payload, self.widths.len() as u64); // count
        payload.extend_from_slice(&compressed);

        write_block(&mut self.writer, FstBlockType::Geometry, &payload)?;
        Ok(())
    }

    /// Writes the hierarchy block (type 4, GZip compressed).
    ///
    /// Contains tagged entries: scope (`0xFE`), upscope (`0xFF`), and
    /// variable declarations (type byte `0x00`-`0x2A`).
    fn write_hierarchy_block(&mut self) -> Result<(), SimError> {
        let mut raw = Vec::new();

        for entry in &self.hierarchy {
            match entry {
                FstHierEntry::Scope { name } => {
                    // Tag: FST_ST_VCD_SCOPE (0xFE)
                    raw.push(FST_ST_VCD_SCOPE);
                    // Scope type: VCD_MODULE
                    raw.push(FST_ST_VCD_MODULE);
                    // Scope name (null-terminated)
                    raw.extend_from_slice(name.as_bytes());
                    raw.push(0);
                    // Component name (null-terminated, empty)
                    raw.push(0);
                }
                FstHierEntry::Upscope => {
                    // Tag: FST_ST_VCD_UPSCOPE (0xFF)
                    raw.push(FST_ST_VCD_UPSCOPE);
                }
                FstHierEntry::Var { index, name, width } => {
                    // Var type byte (acts as tag): VCD_WIRE
                    raw.push(if *width == 1 {
                        FST_VT_VCD_WIRE
                    } else {
                        FST_VT_VCD_REG
                    });
                    // Direction: 0 = implicit
                    raw.push(0);
                    // Variable name (null-terminated)
                    raw.extend_from_slice(name.as_bytes());
                    raw.push(0);
                    // Width as varint
                    write_varint(&mut raw, *width as u64);
                    // Alias: 0 = new variable (auto-increment ID)
                    write_varint(&mut raw, 0);
                    // The actual variable handle is (*index + 1) but with alias=0,
                    // the reader auto-assigns sequential IDs starting from 0.
                    let _ = index; // suppress unused warning; index determines order
                }
            }
        }

        let compressed = compress_gzip(&raw)?;

        // Payload: uncompressed_length(u64 BE) + compressed data
        let mut payload = Vec::new();
        write_u64_be(&mut payload, raw.len() as u64);
        payload.extend_from_slice(&compressed);

        write_block(&mut self.writer, FstBlockType::Hierarchy, &payload)?;
        Ok(())
    }
}

impl<W: Write + Seek> WaveformRecorder for FstRecorder<W> {
    fn register_signal(&mut self, id: SimSignalId, name: &str, width: u32) -> Result<(), SimError> {
        let index = self.next_index;
        self.next_index += 1;
        self.signal_map.push((id, index));
        self.widths.push(width);
        self.hierarchy.push(FstHierEntry::Var {
            index,
            name: name.to_string(),
            width,
        });
        Ok(())
    }

    fn begin_scope(&mut self, name: &str) -> Result<(), SimError> {
        self.hierarchy.push(FstHierEntry::Scope {
            name: name.to_string(),
        });
        Ok(())
    }

    fn end_scope(&mut self) -> Result<(), SimError> {
        self.hierarchy.push(FstHierEntry::Upscope);
        Ok(())
    }

    fn record_change(
        &mut self,
        time_fs: u64,
        id: SimSignalId,
        value: &LogicVec,
    ) -> Result<(), SimError> {
        let index = self
            .find_index(id)
            .ok_or_else(|| SimError::InvalidSignalRef {
                reason: format!("unregistered FST signal {}", id.as_raw()),
            })?;

        if !self.has_changes {
            self.start_time = time_fs;
            self.has_changes = true;
        }
        self.end_time = time_fs;

        self.changes.push(FstValueChange {
            time_fs,
            signal_index: index,
            value: value.clone(),
        });
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), SimError> {
        self.write_header_block()?;
        self.write_vc_data_block()?;
        self.write_geometry_block()?;
        self.write_hierarchy_block()?;
        self.writer.flush()?;
        Ok(())
    }
}

/// Writes a varint (unsigned LEB128) to a byte buffer.
fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Writes a u64 in big-endian format.
fn write_u64_be(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_be_bytes());
}

/// Compresses data using ZLib (deflate with zlib header).
fn compress_zlib(data: &[u8]) -> Result<Vec<u8>, SimError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// Compresses data using GZip.
fn compress_gzip(data: &[u8]) -> Result<Vec<u8>, SimError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// Writes an FST block: type byte + u64 section length + payload.
///
/// Per the spec, section length includes the 8-byte length field itself
/// but excludes the 1-byte type byte.
fn write_block<WR: Write>(
    writer: &mut WR,
    block_type: FstBlockType,
    payload: &[u8],
) -> Result<(), SimError> {
    // Block type (1 byte)
    writer.write_all(&[block_type as u8])?;
    // Section length: 8 bytes (length field itself) + payload length
    let section_length = 8u64 + payload.len() as u64;
    writer.write_all(&section_length.to_be_bytes())?;
    // Payload
    writer.write_all(payload)?;
    Ok(())
}

/// Formats a LogicVec as an FST value string (ASCII, MSB first).
#[cfg(test)]
fn format_fst_value(value: &LogicVec) -> String {
    let width = value.width();
    if width == 1 {
        match value.get(0) {
            Logic::Zero => "0".into(),
            Logic::One => "1".into(),
            Logic::X => "x".into(),
            Logic::Z => "z".into(),
        }
    } else {
        let mut s = String::with_capacity(width as usize);
        for i in (0..width).rev() {
            s.push(match value.get(i) {
                Logic::Zero => '0',
                Logic::One => '1',
                Logic::X => 'x',
                Logic::Z => 'z',
            });
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // -- Encoding helper tests --

    #[test]
    fn varint_single_byte() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 42);
        assert_eq!(buf, vec![42]);
    }

    #[test]
    fn varint_multi_byte() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 300);
        assert_eq!(buf, vec![0xAC, 0x02]);
    }

    #[test]
    fn varint_large_value() {
        let mut buf = Vec::new();
        write_varint(&mut buf, u64::MAX);
        assert_eq!(buf.len(), 10); // u64::MAX needs 10 bytes in LEB128
    }

    #[test]
    fn varint_zero() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0);
        assert_eq!(buf, vec![0]);
    }

    #[test]
    fn u64_be_encoding() {
        let mut buf = Vec::new();
        write_u64_be(&mut buf, 0x0102030405060708);
        assert_eq!(buf, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    }

    #[test]
    fn hierarchy_scope_encoding() {
        let entry = FstHierEntry::Scope {
            name: "top".to_string(),
        };
        match entry {
            FstHierEntry::Scope { ref name } => assert_eq!(name, "top"),
            _ => panic!("expected Scope"),
        }
    }

    #[test]
    fn hierarchy_upscope_encoding() {
        let entry = FstHierEntry::Upscope;
        assert!(matches!(entry, FstHierEntry::Upscope));
    }

    #[test]
    fn hierarchy_var_encoding() {
        let entry = FstHierEntry::Var {
            index: 0,
            name: "clk".to_string(),
            width: 1,
        };
        match entry {
            FstHierEntry::Var {
                index, name, width, ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(name, "clk");
                assert_eq!(width, 1);
            }
            _ => panic!("expected Var"),
        }
    }

    #[test]
    fn gzip_roundtrip() {
        let data = b"hello world hello world";
        let compressed = compress_gzip(data).unwrap();
        assert!(!compressed.is_empty());
        assert_ne!(&compressed[..], data.as_slice());

        // Decompress to verify
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(&decompressed, data);
    }

    #[test]
    fn zlib_roundtrip() {
        let data = b"test data for zlib compression";
        let compressed = compress_zlib(data).unwrap();
        assert!(!compressed.is_empty());

        use flate2::read::ZlibDecoder;
        use std::io::Read;
        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(&decompressed, data);
    }

    // -- FstRecorder unit tests --

    fn make_recorder() -> FstRecorder<Cursor<Vec<u8>>> {
        FstRecorder::new(Cursor::new(Vec::new()))
    }

    #[test]
    fn fst_recorder_new() {
        let rec = make_recorder();
        assert_eq!(rec.next_index, 0);
        assert!(rec.hierarchy.is_empty());
        assert!(rec.changes.is_empty());
        assert!(!rec.has_changes);
    }

    #[test]
    fn fst_register_signal() {
        let mut rec = make_recorder();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        assert_eq!(rec.next_index, 1);
        assert_eq!(rec.widths.len(), 1);
        assert_eq!(rec.widths[0], 1);
        assert_eq!(rec.signal_map.len(), 1);
    }

    #[test]
    fn fst_begin_end_scope() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.end_scope().unwrap();
        assert_eq!(rec.hierarchy.len(), 2);
        assert!(matches!(
            &rec.hierarchy[0],
            FstHierEntry::Scope { name } if name == "top"
        ));
        assert!(matches!(&rec.hierarchy[1], FstHierEntry::Upscope));
    }

    #[test]
    fn fst_record_change_buffering() {
        let mut rec = make_recorder();
        rec.register_signal(SimSignalId::from_raw(0), "sig", 1)
            .unwrap();
        rec.record_change(100, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        assert_eq!(rec.changes.len(), 1);
        assert!(rec.has_changes);
        assert_eq!(rec.start_time, 100);
        assert_eq!(rec.end_time, 100);
    }

    #[test]
    fn fst_time_tracking() {
        let mut rec = make_recorder();
        rec.register_signal(SimSignalId::from_raw(0), "sig", 1)
            .unwrap();
        rec.record_change(100, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(500, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        assert_eq!(rec.start_time, 100);
        assert_eq!(rec.end_time, 500);
    }

    #[test]
    fn fst_signal_map_lookup() {
        let mut rec = make_recorder();
        rec.register_signal(SimSignalId::from_raw(5), "a", 1)
            .unwrap();
        rec.register_signal(SimSignalId::from_raw(10), "b", 4)
            .unwrap();
        assert_eq!(rec.find_index(SimSignalId::from_raw(5)), Some(0));
        assert_eq!(rec.find_index(SimSignalId::from_raw(10)), Some(1));
        assert_eq!(rec.find_index(SimSignalId::from_raw(99)), None);
    }

    #[test]
    fn fst_complex_hierarchy() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.begin_scope("sub").unwrap();
        rec.register_signal(SimSignalId::from_raw(1), "data", 8)
            .unwrap();
        rec.end_scope().unwrap();
        rec.end_scope().unwrap();
        assert_eq!(rec.hierarchy.len(), 6); // scope, var, scope, var, upscope, upscope
        assert_eq!(rec.next_index, 2);
    }

    #[test]
    fn fst_unregistered_signal_error() {
        let mut rec = make_recorder();
        let result = rec.record_change(0, SimSignalId::from_raw(99), &LogicVec::from_bool(true));
        assert!(result.is_err());
    }

    // -- Block structure tests --

    /// Helper to parse block boundaries from raw FST data.
    fn parse_blocks(data: &[u8]) -> Vec<(u8, u64, usize)> {
        let mut blocks = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let block_type = data[pos];
            if pos + 9 > data.len() {
                break;
            }
            let section_len = u64::from_be_bytes(data[pos + 1..pos + 9].try_into().unwrap());
            if section_len < 8 {
                break;
            }
            blocks.push((block_type, section_len, pos));
            // Advance: 1 (type) + section_len (includes 8-byte length field + payload)
            pos += 1 + section_len as usize;
        }
        blocks
    }

    #[test]
    fn fst_block_section_length_excludes_type_byte() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // First block is header
        assert_eq!(data[0], FstBlockType::Header as u8);
        let section_len = u64::from_be_bytes(data[1..9].try_into().unwrap());
        // section_length = 8 + 329 = 337
        assert_eq!(section_len, 337);
        // Total header block bytes = 1 (type) + 337 = 338
        assert!(data.len() >= 338);
    }

    #[test]
    fn fst_header_endianness_is_native() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // Endianness field is at payload offset 16, which is file offset 9 + 16 = 25
        let endian_bytes: [u8; 8] = data[25..33].try_into().unwrap();
        let value = f64::from_ne_bytes(endian_bytes);
        assert!((value - std::f64::consts::E).abs() < 1e-10);
    }

    #[test]
    fn fst_header_timescale_is_fs() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // Timescale at payload offset 64, file offset 9 + 64 = 73
        let timescale = data[73] as i8;
        assert_eq!(timescale, -15);
    }

    #[test]
    fn fst_header_writer_string() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // Writer string at payload offset 65, file offset 9 + 65 = 74
        let writer = &data[74..74 + 18];
        assert_eq!(writer, b"Aion HDL Simulator");
    }

    #[test]
    fn fst_hierarchy_uses_correct_tags() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // Find hierarchy block
        let blocks = parse_blocks(&data);
        let hier_block = blocks
            .iter()
            .find(|(t, _, _)| *t == FstBlockType::Hierarchy as u8)
            .expect("hierarchy block missing");

        // Hierarchy payload starts after type(1) + length(8) + uncompressed_length(8)
        let payload_start = hier_block.2 + 9 + 8;
        let payload_end = hier_block.2 + 1 + hier_block.1 as usize;
        let compressed = &data[payload_start..payload_end];

        // Decompress
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(compressed);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();

        // First byte should be scope tag (0xFE)
        assert_eq!(decompressed[0], FST_ST_VCD_SCOPE);
        // Should contain upscope tag (0xFF) somewhere
        assert!(decompressed.contains(&FST_ST_VCD_UPSCOPE));
        // Should contain a var entry (type byte 0x05 for wire)
        assert!(decompressed.contains(&FST_VT_VCD_WIRE));
    }

    #[test]
    fn fst_geometry_has_headers() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.register_signal(SimSignalId::from_raw(1), "data", 8)
            .unwrap();
        rec.end_scope().unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        let blocks = parse_blocks(&data);
        let geom_block = blocks
            .iter()
            .find(|(t, _, _)| *t == FstBlockType::Geometry as u8)
            .expect("geometry block missing");

        // Geometry payload starts at type(1) + length(8)
        let payload_start = geom_block.2 + 9;

        // First 8 bytes of payload: uncompressed_length (u64 BE)
        let uncomp_len =
            u64::from_be_bytes(data[payload_start..payload_start + 8].try_into().unwrap());
        assert!(uncomp_len > 0);

        // Next 8 bytes: count (u64 BE)
        let count = u64::from_be_bytes(
            data[payload_start + 8..payload_start + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(count, 2); // 2 signals
    }

    #[test]
    fn fst_finalize_all_block_types_present() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(1000, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        let blocks = parse_blocks(&data);
        let types: Vec<u8> = blocks.iter().map(|(t, _, _)| *t).collect();

        assert!(types.contains(&(FstBlockType::Header as u8)));
        assert!(types.contains(&(FstBlockType::VcData as u8)));
        assert!(types.contains(&(FstBlockType::Geometry as u8)));
        assert!(types.contains(&(FstBlockType::Hierarchy as u8)));
    }

    #[test]
    fn fst_finalize_empty_recording() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();
        // Should have header + geometry + hierarchy
        assert!(!data.is_empty());
        assert_eq!(data[0], FstBlockType::Header as u8);
    }

    #[test]
    fn fst_finalize_header_written() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();
        // Header = 1 (type) + 337 (section_length) = 338 bytes minimum
        assert!(data.len() >= 338);
    }

    #[test]
    fn fst_finalize_single_signal() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "a", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();
        assert!(data.len() > 338);
    }

    #[test]
    fn fst_finalize_multiple_signals() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.register_signal(SimSignalId::from_raw(1), "data", 8)
            .unwrap();
        rec.register_signal(SimSignalId::from_raw(2), "addr", 4)
            .unwrap();
        rec.end_scope().unwrap();
        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(0, SimSignalId::from_raw(1), &LogicVec::from_u64(0xFF, 8))
            .unwrap();
        rec.record_change(1000, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        rec.record_change(1000, SimSignalId::from_raw(2), &LogicVec::from_u64(0xA, 4))
            .unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();
        assert!(!data.is_empty());
    }

    #[test]
    fn fst_vcdata_has_start_end_times() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "s", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.record_change(100, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(500, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        let blocks = parse_blocks(&data);
        let vc_block = blocks
            .iter()
            .find(|(t, _, _)| *t == FstBlockType::VcData as u8)
            .expect("VcData block missing");

        let payload_start = vc_block.2 + 9;
        let start_time =
            u64::from_be_bytes(data[payload_start..payload_start + 8].try_into().unwrap());
        let end_time = u64::from_be_bytes(
            data[payload_start + 8..payload_start + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(start_time, 100);
        assert_eq!(end_time, 500);
    }

    #[test]
    fn fst_blocks_parseable_sequentially() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();
        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(1000, SimSignalId::from_raw(0), &LogicVec::from_bool(true))
            .unwrap();
        rec.finalize().unwrap();
        let data = rec.writer.into_inner();

        // Parse all blocks — should cover the entire file with no leftover bytes
        let blocks = parse_blocks(&data);
        assert_eq!(blocks.len(), 4); // header, vcdata, geometry, hierarchy

        let last = blocks.last().unwrap();
        let total_consumed = last.2 + 1 + last.1 as usize;
        assert_eq!(total_consumed, data.len());
    }

    // -- Integration tests --

    #[test]
    fn fst_trait_object_compatibility() {
        let cursor = Cursor::new(Vec::new());
        let rec = FstRecorder::new(cursor);
        let _boxed: Box<dyn WaveformRecorder> = Box::new(rec);
    }

    #[test]
    fn format_fst_value_single_bit() {
        assert_eq!(format_fst_value(&LogicVec::from_bool(false)), "0");
        assert_eq!(format_fst_value(&LogicVec::from_bool(true)), "1");
    }

    #[test]
    fn format_fst_value_multi_bit() {
        let v = LogicVec::from_u64(0b1010, 4);
        assert_eq!(format_fst_value(&v), "1010");
    }

    #[test]
    fn format_fst_value_x_z() {
        let mut v = LogicVec::new(2);
        v.set(0, Logic::X);
        v.set(1, Logic::Z);
        assert_eq!(format_fst_value(&v), "zx");
    }
}
