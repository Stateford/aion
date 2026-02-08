//! Waveform recording for simulation output.
//!
//! The [`WaveformRecorder`] trait abstracts waveform output. [`VcdRecorder`]
//! implements the IEEE 1364 Value Change Dump (VCD) format, producing text
//! files that can be viewed in GTKWave, Surfer, or other waveform viewers.

use std::io::Write;

use aion_common::{Logic, LogicVec};

use crate::error::SimError;
use crate::value::SimSignalId;

/// Trait for recording simulation waveforms.
///
/// Implementations write signal changes to a particular format (VCD, FST, etc.).
pub trait WaveformRecorder {
    /// Registers a signal for recording and returns its recorder-internal ID code.
    fn register_signal(&mut self, id: SimSignalId, name: &str, width: u32) -> Result<(), SimError>;

    /// Opens a new scope (hierarchy level) in the waveform.
    fn begin_scope(&mut self, name: &str) -> Result<(), SimError>;

    /// Closes the current scope.
    fn end_scope(&mut self) -> Result<(), SimError>;

    /// Records a value change at the given time (in femtoseconds).
    fn record_change(
        &mut self,
        time_fs: u64,
        id: SimSignalId,
        value: &LogicVec,
    ) -> Result<(), SimError>;

    /// Finalizes the waveform output (flush, write trailer, etc.).
    fn finalize(&mut self) -> Result<(), SimError>;
}

/// VCD (Value Change Dump) format recorder following IEEE 1364.
///
/// Produces human-readable text output with timestamps and signal value changes.
/// Signal identifiers use printable ASCII characters starting from `!` (0x21).
pub struct VcdRecorder<W: Write> {
    writer: W,
    id_map: Vec<(SimSignalId, String, u32)>, // (signal_id, id_code, width)
    next_id: u32,
    header_written: bool,
    current_time: Option<u64>,
}

impl<W: Write> VcdRecorder<W> {
    /// Creates a new VCD recorder writing to the given output.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            id_map: Vec::new(),
            next_id: 0,
            header_written: false,
            current_time: None,
        }
    }

    /// Writes the VCD header section.
    fn write_header(&mut self) -> Result<(), SimError> {
        writeln!(self.writer, "$date")?;
        writeln!(self.writer, "  Simulation date")?;
        writeln!(self.writer, "$end")?;
        writeln!(self.writer, "$version")?;
        writeln!(self.writer, "  Aion HDL Simulator")?;
        writeln!(self.writer, "$end")?;
        writeln!(self.writer, "$timescale")?;
        writeln!(self.writer, "  1fs")?;
        writeln!(self.writer, "$end")?;
        Ok(())
    }

    /// Generates a VCD identifier code from a sequential index.
    ///
    /// Uses printable ASCII characters starting from `!` (0x21).
    /// Multi-character codes are generated for indices >= 94.
    fn make_id_code(index: u32) -> String {
        let mut result = String::new();
        let mut idx = index;
        loop {
            let c = (b'!' + (idx % 94) as u8) as char;
            result.push(c);
            idx /= 94;
            if idx == 0 {
                break;
            }
            idx -= 1;
        }
        result
    }

    /// Formats a LogicVec as a VCD value string.
    fn format_value(value: &LogicVec, width: u32) -> String {
        if width == 1 {
            match value.get(0) {
                Logic::Zero => "0".into(),
                Logic::One => "1".into(),
                Logic::X => "x".into(),
                Logic::Z => "z".into(),
            }
        } else {
            let mut s = String::with_capacity(width as usize + 1);
            s.push('b');
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
}

impl<W: Write> WaveformRecorder for VcdRecorder<W> {
    fn register_signal(&mut self, id: SimSignalId, name: &str, width: u32) -> Result<(), SimError> {
        let id_code = Self::make_id_code(self.next_id);
        self.next_id += 1;

        writeln!(self.writer, "$var wire {width} {id_code} {name} $end")?;

        self.id_map.push((id, id_code, width));
        Ok(())
    }

    fn begin_scope(&mut self, name: &str) -> Result<(), SimError> {
        if !self.header_written {
            self.write_header()?;
            self.header_written = true;
        }
        writeln!(self.writer, "$scope module {name} $end")?;
        Ok(())
    }

    fn end_scope(&mut self) -> Result<(), SimError> {
        writeln!(self.writer, "$upscope $end")?;
        Ok(())
    }

    fn record_change(
        &mut self,
        time_fs: u64,
        id: SimSignalId,
        value: &LogicVec,
    ) -> Result<(), SimError> {
        // Write enddefinitions before first change
        if !self.header_written {
            self.write_header()?;
            self.header_written = true;
        }

        // Emit timestamp if changed
        if self.current_time != Some(time_fs) {
            if self.current_time.is_none() {
                writeln!(self.writer, "$enddefinitions $end")?;
                writeln!(self.writer, "$dumpvars")?;
            }
            if self.current_time.is_some() || time_fs > 0 {
                writeln!(self.writer, "#{time_fs}")?;
            } else {
                // time_fs == 0 and no previous time
                writeln!(self.writer, "#0")?;
            }
            self.current_time = Some(time_fs);
        }

        // Find signal info
        let (_, id_code, width) = self
            .id_map
            .iter()
            .find(|(sid, _, _)| *sid == id)
            .ok_or_else(|| SimError::InvalidSignalRef {
                reason: format!("unregistered VCD signal {}", id.as_raw()),
            })?;

        let val_str = Self::format_value(value, *width);
        if *width == 1 {
            writeln!(self.writer, "{val_str}{id_code}")?;
        } else {
            writeln!(self.writer, "{val_str} {id_code}")?;
        }
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), SimError> {
        if self.current_time.is_none() {
            // No changes recorded, still write header
            if !self.header_written {
                self.write_header()?;
                self.header_written = true;
            }
            writeln!(self.writer, "$enddefinitions $end")?;
        }
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_recorder() -> VcdRecorder<Vec<u8>> {
        VcdRecorder::new(Vec::new())
    }

    #[test]
    fn id_code_first() {
        assert_eq!(VcdRecorder::<Vec<u8>>::make_id_code(0), "!");
    }

    #[test]
    fn id_code_sequential() {
        assert_eq!(VcdRecorder::<Vec<u8>>::make_id_code(1), "\"");
        assert_eq!(VcdRecorder::<Vec<u8>>::make_id_code(93), "~");
    }

    #[test]
    fn id_code_multi_char() {
        // 94 wraps to two characters
        let code = VcdRecorder::<Vec<u8>>::make_id_code(94);
        assert_eq!(code.len(), 2);
    }

    #[test]
    fn register_signal_writes_var() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();

        let output = String::from_utf8(rec.writer.clone()).unwrap();
        assert!(output.contains("$var wire 1 ! clk $end"));
        assert!(output.contains("$scope module top $end"));
        assert!(output.contains("$upscope $end"));
    }

    #[test]
    fn record_single_bit_change() {
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

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("#0"));
        assert!(output.contains("0!"));
        assert!(output.contains("#1000"));
        assert!(output.contains("1!"));
    }

    #[test]
    fn record_multi_bit_change() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "data", 4)
            .unwrap();
        rec.end_scope().unwrap();

        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_u64(0b1010, 4))
            .unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("b1010 !"));
    }

    #[test]
    fn record_x_values() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "sig", 1)
            .unwrap();
        rec.end_scope().unwrap();

        let mut xval = LogicVec::new(1);
        xval.set(0, Logic::X);
        rec.record_change(0, SimSignalId::from_raw(0), &xval)
            .unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("x!"));
    }

    #[test]
    fn format_value_single_bit() {
        assert_eq!(
            VcdRecorder::<Vec<u8>>::format_value(&LogicVec::from_bool(false), 1),
            "0"
        );
        assert_eq!(
            VcdRecorder::<Vec<u8>>::format_value(&LogicVec::from_bool(true), 1),
            "1"
        );
    }

    #[test]
    fn format_value_multi_bit() {
        let v = LogicVec::from_u64(0b1010, 4);
        assert_eq!(VcdRecorder::<Vec<u8>>::format_value(&v, 4), "b1010");
    }

    #[test]
    fn finalize_empty_recorder() {
        let mut rec = make_recorder();
        rec.finalize().unwrap();
        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("$enddefinitions $end"));
    }

    #[test]
    fn vcd_header_contents() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.end_scope().unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("$date"));
        assert!(output.contains("$version"));
        assert!(output.contains("Aion HDL Simulator"));
        assert!(output.contains("$timescale"));
        assert!(output.contains("1fs"));
    }

    #[test]
    fn dumpvars_before_first_change() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "clk", 1)
            .unwrap();
        rec.end_scope().unwrap();

        rec.record_change(0, SimSignalId::from_raw(0), &LogicVec::from_bool(false))
            .unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("$dumpvars"));
        assert!(output.contains("$enddefinitions $end"));
    }

    #[test]
    fn multiple_signals_different_ids() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "a", 1)
            .unwrap();
        rec.register_signal(SimSignalId::from_raw(1), "b", 1)
            .unwrap();
        rec.end_scope().unwrap();

        let output = String::from_utf8(rec.writer.clone()).unwrap();
        assert!(output.contains("$var wire 1 ! a $end"));
        assert!(output.contains("$var wire 1 \" b $end"));
    }

    #[test]
    fn record_z_value() {
        let mut rec = make_recorder();
        rec.begin_scope("top").unwrap();
        rec.register_signal(SimSignalId::from_raw(0), "tri", 1)
            .unwrap();
        rec.end_scope().unwrap();

        let mut zval = LogicVec::new(1);
        zval.set(0, Logic::Z);
        rec.record_change(0, SimSignalId::from_raw(0), &zval)
            .unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(rec.writer).unwrap();
        assert!(output.contains("z!"));
    }
}
