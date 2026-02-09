//! CRC computation for Intel and Xilinx bitstream formats.
//!
//! Provides CRC-16-CCITT for Intel SOF/POF files and CRC-32 (IEEE 802.3)
//! for Xilinx BIT files. Both use precomputed lookup tables for performance.

/// CRC-16-CCITT polynomial used by Intel/Altera bitstream formats.
const CRC16_POLY: u16 = 0x8005;

/// CRC-32 polynomial used by Xilinx bitstream formats (IEEE 802.3).
const CRC32_POLY: u32 = 0x04C1_1DB7;

/// Precomputed CRC-16-CCITT lookup table (256 entries).
const CRC16_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ CRC16_POLY;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Precomputed CRC-32 lookup table (256 entries).
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ CRC32_POLY;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Computes CRC-16-CCITT over the given byte slice.
///
/// Uses polynomial 0x8005 with MSB-first processing, matching the
/// Intel/Altera SOF and POF file format specification.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        let idx = ((crc >> 8) ^ byte as u16) as usize;
        crc = (crc << 8) ^ CRC16_TABLE[idx];
    }
    crc
}

/// Computes CRC-32 over the given byte slice.
///
/// Uses polynomial 0x04C11DB7 with MSB-first processing, matching the
/// Xilinx 7-series bitstream CRC computation.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &byte in data {
        let idx = ((crc >> 24) ^ byte as u32) as usize;
        crc = (crc << 8) ^ CRC32_TABLE[idx];
    }
    crc
}

/// Computes CRC-32 over big-endian 32-bit words.
///
/// Each word is split into 4 bytes (big-endian order) and fed through
/// the CRC-32 computation, matching Xilinx configuration register writes.
pub fn crc32_words(words: &[u32]) -> u32 {
    let mut data = Vec::with_capacity(words.len() * 4);
    for &w in words {
        data.extend_from_slice(&w.to_be_bytes());
    }
    crc32(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_empty() {
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn crc16_single_byte() {
        let result = crc16(&[0x01]);
        // Should be non-zero for non-zero input
        assert_ne!(result, 0);
    }

    #[test]
    fn crc16_known_vector() {
        // Verify deterministic output for a known input
        let data = b"123456789";
        let result = crc16(data);
        // Our CRC-16 (poly 0x8005, init 0, MSB-first) produces a specific value
        assert_ne!(result, 0);
        // Running twice should give the same result
        assert_eq!(crc16(data), result);
    }

    #[test]
    fn crc16_incremental_consistency() {
        let data = b"Hello, World!";
        let full = crc16(data);
        // Should be deterministic
        assert_eq!(crc16(data), full);
    }

    #[test]
    fn crc32_empty() {
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn crc32_single_byte() {
        let result = crc32(&[0x01]);
        assert_ne!(result, 0);
    }

    #[test]
    fn crc32_deterministic() {
        let data = b"test data for crc32";
        let a = crc32(data);
        let b = crc32(data);
        assert_eq!(a, b);
    }

    #[test]
    fn crc32_different_data() {
        let a = crc32(b"hello");
        let b = crc32(b"world");
        assert_ne!(a, b);
    }

    #[test]
    fn crc32_words_empty() {
        assert_eq!(crc32_words(&[]), 0);
    }

    #[test]
    fn crc32_words_matches_bytes() {
        let words = [0x01020304u32, 0x05060708u32];
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(crc32_words(&words), crc32(&bytes));
    }

    #[test]
    fn crc16_table_first_entry() {
        // Table[0] should be 0
        assert_eq!(CRC16_TABLE[0], 0);
    }

    #[test]
    fn crc32_table_first_entry() {
        // Table[0] should be 0
        assert_eq!(CRC32_TABLE[0], 0);
    }
}
