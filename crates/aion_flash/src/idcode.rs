//! Known JTAG IDCODE values for supported FPGA devices.
//!
//! Used to cross-check that the device detected on the JTAG chain matches
//! the part number configured in `aion.toml` before programming. The table
//! currently covers the Intel Cyclone IV E parts modeled in `aion_arch`;
//! extend it as more device families gain flash support.

/// A known FPGA device identified by its JTAG IDCODE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownDevice {
    /// The 32-bit JTAG IDCODE for this die.
    pub idcode: u32,
    /// Part-number prefix that selects this die (e.g. `EP4CE22` matches
    /// the full ordering code `EP4CE22F17C6`).
    pub part_prefix: &'static str,
    /// Aion architecture family name (as used in `aion.toml`).
    pub family: &'static str,
}

/// Table of known devices, keyed by part-number prefix.
///
/// Note: EP4CE6 and EP4CE10 share a die and therefore an IDCODE.
pub const KNOWN_DEVICES: &[KnownDevice] = &[
    KnownDevice {
        idcode: 0x020F10DD,
        part_prefix: "EP4CE6",
        family: "cyclone4",
    },
    KnownDevice {
        idcode: 0x020F10DD,
        part_prefix: "EP4CE10",
        family: "cyclone4",
    },
    KnownDevice {
        idcode: 0x020F20DD,
        part_prefix: "EP4CE15",
        family: "cyclone4",
    },
    KnownDevice {
        idcode: 0x020F30DD,
        part_prefix: "EP4CE22",
        family: "cyclone4",
    },
];

/// Looks up a known device by its JTAG IDCODE.
///
/// Returns the first table entry with a matching IDCODE, or `None` if the
/// IDCODE is not in the table. For dies shared between parts (e.g.
/// EP4CE6/EP4CE10) the returned entry is the smallest part.
pub fn lookup_idcode(idcode: u32) -> Option<&'static KnownDevice> {
    KNOWN_DEVICES.iter().find(|dev| dev.idcode == idcode)
}

/// Finds the expected device entry for a full part number (ordering code).
///
/// Matches case-insensitively on the part-number prefix and returns the
/// longest matching entry, so `EP4CE22F17C6` resolves to `EP4CE22` even if
/// shorter prefixes were also present in the table. Returns `None` for
/// unknown parts.
pub fn idcode_for_part(part: &str) -> Option<&'static KnownDevice> {
    let upper = part.to_ascii_uppercase();
    KNOWN_DEVICES
        .iter()
        .filter(|dev| upper.starts_with(dev.part_prefix))
        .max_by_key(|dev| dev.part_prefix.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_idcode_known() {
        let dev = lookup_idcode(0x020F30DD).unwrap();
        assert_eq!(dev.part_prefix, "EP4CE22");
        assert_eq!(dev.family, "cyclone4");
    }

    #[test]
    fn lookup_idcode_unknown() {
        assert!(lookup_idcode(0xDEADBEEF).is_none());
    }

    #[test]
    fn idcode_for_part_full_ordering_code() {
        // The DE0-Nano's part.
        let dev = idcode_for_part("EP4CE22F17C6").unwrap();
        assert_eq!(dev.idcode, 0x020F30DD);
    }

    #[test]
    fn idcode_for_part_case_insensitive() {
        let dev = idcode_for_part("ep4ce22f17c6").unwrap();
        assert_eq!(dev.idcode, 0x020F30DD);
    }

    #[test]
    fn idcode_for_part_unknown() {
        assert!(idcode_for_part("XC7A35T").is_none());
    }

    #[test]
    fn idcode_for_part_prefers_longest_prefix() {
        // EP4CE15 must not be shadowed by any shorter prefix.
        let dev = idcode_for_part("EP4CE15F17C8").unwrap();
        assert_eq!(dev.part_prefix, "EP4CE15");
        assert_eq!(dev.idcode, 0x020F20DD);
    }

    #[test]
    fn shared_die_parts_share_idcode() {
        let small = idcode_for_part("EP4CE6E22C8").unwrap();
        let larger = idcode_for_part("EP4CE10E22C8").unwrap();
        assert_eq!(small.idcode, larger.idcode);
    }
}
