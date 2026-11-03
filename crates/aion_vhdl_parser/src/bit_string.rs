//! VHDL-2008 bit-string literal expansion (IEEE 1076-2008 §15.8).
//!
//! A bit-string literal has the form `[length] base_specifier "bit_value"`,
//! for example `X"FF"`, `8UX"F"`, `12SB"101"`, or `16D"255"`. This module
//! turns the literal's source text into its *expanded bit value*: the string
//! of extended digits (one character per element, leftmost = most
//! significant) that the literal denotes after base expansion and after
//! padding or truncation to the optional explicit length.
//!
//! The lexer uses [`expand_bit_string`] to validate literals (reporting
//! precise spans on error) and the elaborator uses it to compute values, so
//! both agree on the semantics.

/// Largest explicit or expanded length (in elements) accepted for a
/// bit-string literal. Larger literals are rejected rather than allocated.
pub const MAX_BIT_STRING_LEN: usize = 1 << 20;

/// An error found while expanding a bit-string literal.
///
/// Offsets are byte offsets relative to the start of the literal text passed
/// to [`expand_bit_string`], so callers can map them onto source spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitStringError {
    /// Human-readable description of the problem.
    pub message: String,
    /// Byte offset (within the literal text) where the offending part starts.
    pub start: usize,
    /// Byte offset (within the literal text) one past the offending part.
    pub end: usize,
}

impl BitStringError {
    fn new(message: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            message: message.into(),
            start,
            end,
        }
    }
}

/// The numeric base named by a bit-string literal's base specifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    /// `B`, `UB`, `SB`: one element per extended digit.
    Binary,
    /// `O`, `UO`, `SO`: three elements per extended digit.
    Octal,
    /// `X`, `UX`, `SX`: four elements per extended digit.
    Hex,
    /// `D`: the bit value is a decimal integer converted to binary.
    Decimal,
}

impl Base {
    fn radix(self) -> u32 {
        match self {
            Base::Binary => 2,
            Base::Octal => 8,
            Base::Hex => 16,
            Base::Decimal => 10,
        }
    }

    fn bits_per_digit(self) -> usize {
        match self {
            Base::Binary => 1,
            Base::Octal => 3,
            Base::Hex => 4,
            Base::Decimal => 0,
        }
    }
}

/// Expands the source text of a VHDL bit-string literal into its expanded
/// bit value, applying the explicit length (if any).
///
/// The returned string holds one character per element, leftmost (most
/// significant) first. Digits of the base are replaced by their binary
/// representation; other graphic characters (VHDL-2008 metavalues such as
/// `Z`, `X`, `-`) are replicated once per bit of the base digit width. With
/// an explicit length, the value is padded on the left with `'0'` (`B`, `O`,
/// `X`, `UB`, `UO`, `UX`, `D`) or with its leftmost element (`SB`, `SO`,
/// `SX`), or truncated on the left, which is an error unless every dropped
/// element is `'0'` (unsigned) or equal to the leftmost remaining element
/// (signed).
///
/// Returns an error for a malformed literal: a missing or invalid base
/// specifier or quotes, a digit not valid for the base, a misplaced
/// underline, a decimal value that does not fit, or a truncation that would
/// drop significant elements.
pub fn expand_bit_string(text: &str) -> Result<String, BitStringError> {
    let bytes = text.as_bytes();

    // Optional decimal length: digit { [_] digit }.
    let mut pos = 0;
    while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'_') {
        pos += 1;
    }
    let length = if pos > 0 {
        let len_text = &text[..pos];
        check_underlines(len_text, 0, "length")?;
        let digits: String = len_text.chars().filter(|c| *c != '_').collect();
        match digits.parse::<usize>() {
            Ok(n) if n <= MAX_BIT_STRING_LEN => Some(n),
            _ => {
                return Err(BitStringError::new(
                    format!(
                        "bit string length {digits} exceeds the supported maximum of \
                         {MAX_BIT_STRING_LEN}"
                    ),
                    0,
                    pos,
                ))
            }
        }
    } else {
        None
    };

    // Base specifier: [U|S](B|O|X) | D.
    let spec_start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_alphabetic() {
        pos += 1;
    }
    let spec = text[spec_start..pos].to_ascii_uppercase();
    let (base, signed) = match spec.as_str() {
        "B" | "UB" => (Base::Binary, false),
        "O" | "UO" => (Base::Octal, false),
        "X" | "UX" => (Base::Hex, false),
        "SB" => (Base::Binary, true),
        "SO" => (Base::Octal, true),
        "SX" => (Base::Hex, true),
        "D" => (Base::Decimal, false),
        _ => {
            return Err(BitStringError::new(
                format!(
                    "invalid bit string base specifier `{}`",
                    &text[spec_start..pos]
                ),
                spec_start,
                pos,
            ))
        }
    };

    // Quoted bit value.
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return Err(BitStringError::new(
            "expected '\"' after bit string base specifier",
            spec_start,
            pos,
        ));
    }
    let value_start = pos + 1;
    let Some(rel_close) = text[value_start..].find('"') else {
        return Err(BitStringError::new(
            "unterminated bit string literal",
            0,
            text.len(),
        ));
    };
    let value_end = value_start + rel_close;
    if value_end + 1 != text.len() {
        return Err(BitStringError::new(
            "unexpected characters after bit string literal",
            value_end + 1,
            text.len(),
        ));
    }
    let value = &text[value_start..value_end];
    check_underlines(value, value_start, "bit value")?;

    let expanded = if base == Base::Decimal {
        expand_decimal(value, value_start, length)?
    } else {
        expand_based(value, value_start, base)?
    };
    if expanded.chars().count() > MAX_BIT_STRING_LEN {
        return Err(BitStringError::new(
            format!(
                "bit string literal expands to more than the supported maximum of \
                 {MAX_BIT_STRING_LEN} elements"
            ),
            0,
            text.len(),
        ));
    }

    match length {
        None => Ok(expanded),
        Some(len) => resize(expanded, len, signed, text.len()),
    }
}

/// Checks that underlines in `s` only separate characters: no leading,
/// trailing, or doubled underline. `offset` maps positions back into the
/// full literal text.
fn check_underlines(s: &str, offset: usize, what: &str) -> Result<(), BitStringError> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'_' {
            continue;
        }
        let misplaced = i == 0 || i + 1 == bytes.len() || bytes[i + 1] == b'_';
        if misplaced {
            return Err(BitStringError::new(
                format!("underline in bit string {what} must appear between two characters"),
                offset + i,
                offset + i + 1,
            ));
        }
    }
    Ok(())
}

/// Returns the numeric value of `c` if it is a digit or a hexadecimal
/// letter (`0`-`9`, `A`-`F`, case-insensitive).
fn digit_value(c: char) -> Option<u32> {
    c.to_digit(16)
}

/// Expands a binary, octal, or hexadecimal bit value.
fn expand_based(value: &str, offset: usize, base: Base) -> Result<String, BitStringError> {
    let width = base.bits_per_digit();
    let radix = base.radix();
    let mut out = String::with_capacity(value.len() * width);
    for (i, c) in value.char_indices() {
        if c == '_' {
            continue;
        }
        match digit_value(c) {
            Some(v) if v < radix => {
                for bit in (0..width).rev() {
                    out.push(if (v >> bit) & 1 == 1 { '1' } else { '0' });
                }
            }
            Some(_) => {
                return Err(BitStringError::new(
                    format!("digit '{c}' is not valid in a base-{radix} bit string literal"),
                    offset + i,
                    offset + i + c.len_utf8(),
                ))
            }
            None if c.is_control() => {
                return Err(BitStringError::new(
                    format!(
                        "character U+{:04X} is not a graphic character and cannot appear in a \
                         bit string literal",
                        c as u32
                    ),
                    offset + i,
                    offset + i + c.len_utf8(),
                ))
            }
            None => {
                // VHDL-2008: any other graphic character is replicated once
                // per bit of the base digit width.
                for _ in 0..width {
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

/// Expands a decimal (`D`) bit value into its minimal binary representation,
/// or into exactly `length` bits when an explicit length is given.
fn expand_decimal(
    value: &str,
    offset: usize,
    length: Option<usize>,
) -> Result<String, BitStringError> {
    let mut digits: Vec<u8> = Vec::with_capacity(value.len());
    for (i, c) in value.char_indices() {
        if c == '_' {
            continue;
        }
        match c.to_digit(10) {
            Some(d) => digits.push(d as u8),
            None => {
                return Err(BitStringError::new(
                    format!(
                        "character '{c}' is not a decimal digit; `D` bit strings allow only 0-9"
                    ),
                    offset + i,
                    offset + i + c.len_utf8(),
                ))
            }
        }
    }
    if digits.is_empty() {
        return Err(BitStringError::new(
            "decimal bit string literal requires at least one digit",
            offset.saturating_sub(1),
            offset + value.len() + 1,
        ));
    }

    // Repeated halving of the decimal digit string yields bits LSB first.
    let mut bits_lsb: Vec<char> = Vec::new();
    while digits.iter().any(|&d| d != 0) {
        let mut rem = 0u8;
        for d in digits.iter_mut() {
            let cur = rem * 10 + *d;
            *d = cur / 2;
            rem = cur % 2;
        }
        bits_lsb.push(if rem == 1 { '1' } else { '0' });
        if bits_lsb.len() > MAX_BIT_STRING_LEN {
            break;
        }
    }
    if bits_lsb.is_empty() {
        // The value zero needs a single bit.
        bits_lsb.push('0');
    }
    if let Some(len) = length {
        let needed = bits_lsb
            .iter()
            .rposition(|&b| b == '1')
            .map_or(0, |p| p + 1);
        if needed > len {
            return Err(BitStringError::new(
                format!(
                    "decimal value {} needs {needed} bits but the bit string length is {len}",
                    value.replace('_', "")
                ),
                offset,
                offset + value.len(),
            ));
        }
    }
    Ok(bits_lsb.iter().rev().collect())
}

/// Pads or truncates `expanded` on the left to exactly `len` elements,
/// following the LRM rules for signed and unsigned base specifiers.
fn resize(
    expanded: String,
    len: usize,
    signed: bool,
    text_len: usize,
) -> Result<String, BitStringError> {
    let chars: Vec<char> = expanded.chars().collect();
    let n = chars.len();
    if len >= n {
        let pad = if signed {
            match chars.first() {
                Some(&c) => c,
                None => {
                    return Err(BitStringError::new(
                        "a signed bit string literal with an explicit length cannot have an \
                         empty bit value",
                        0,
                        text_len,
                    ))
                }
            }
        } else {
            '0'
        };
        let mut out = String::with_capacity(len);
        for _ in 0..len - n {
            out.push(pad);
        }
        out.extend(chars);
        return Ok(out);
    }

    let (dropped, kept) = chars.split_at(n - len);
    let ok = if signed {
        match kept.first() {
            Some(&lead) => dropped.iter().all(|&c| c == lead),
            None => false,
        }
    } else {
        dropped.iter().all(|&c| c == '0')
    };
    if !ok {
        let rule = if signed {
            "every dropped element must equal the leftmost remaining element"
        } else {
            "every dropped element must be '0'"
        };
        return Err(BitStringError::new(
            format!(
                "bit string literal has {n} elements and cannot be truncated to length {len}: \
                 {rule}"
            ),
            0,
            text_len,
        ));
    }
    Ok(kept.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> String {
        expand_bit_string(text).unwrap_or_else(|e| panic!("{text}: {e:?}"))
    }

    fn err(text: &str) -> BitStringError {
        expand_bit_string(text).expect_err(text)
    }

    #[test]
    fn unsized_bases() {
        assert_eq!(ok("X\"F0\""), "11110000");
        assert_eq!(ok("x\"a\""), "1010");
        assert_eq!(ok("B\"1010\""), "1010");
        assert_eq!(ok("O\"7\""), "111");
        assert_eq!(ok("UX\"1\""), "0001");
        assert_eq!(ok("SX\"8\""), "1000");
        assert_eq!(ok("ub\"01\""), "01");
        assert_eq!(ok("so\"4\""), "100");
        assert_eq!(ok("B\"1_0_1\""), "101");
        assert_eq!(ok("X\"\""), "");
    }

    #[test]
    fn metavalues_are_replicated() {
        assert_eq!(ok("X\"Z\""), "ZZZZ");
        assert_eq!(ok("O\"-1\""), "---001");
        assert_eq!(ok("B\"1X0U\""), "1X0U");
        assert_eq!(ok("X\"1W\""), "0001WWWW");
    }

    #[test]
    fn sized_unsigned_pads_with_zero() {
        assert_eq!(ok("8UX\"F\""), "00001111");
        assert_eq!(ok("8X\"F\""), "00001111");
        assert_eq!(ok("6B\"11\""), "000011");
        assert_eq!(ok("1_2UO\"7\""), "000000000111");
    }

    #[test]
    fn sized_signed_pads_with_leftmost() {
        assert_eq!(ok("8SX\"F\""), "11111111");
        assert_eq!(ok("8SX\"7\""), "00000111");
        assert_eq!(ok("6SB\"10\""), "111110");
        assert_eq!(ok("8SX\"Z\""), "ZZZZZZZZ");
    }

    #[test]
    fn sized_truncation_rules() {
        // Unsigned: dropped elements are zero, so truncation is fine.
        assert_eq!(ok("3X\"5\""), "101");
        assert_eq!(ok("0X\"0\""), "");
        // Signed: dropped elements equal the new leftmost one.
        assert_eq!(ok("3SX\"F\""), "111");
        assert_eq!(ok("3SX\"3\""), "011");
        // Dropping a '1' in an unsigned literal is an error.
        let e = err("3UX\"F\"");
        assert!(e.message.contains("cannot be truncated"), "{}", e.message);
        assert_eq!((e.start, e.end), (0, 6));
        // Signed: the dropped bits must all match the kept sign bit.
        assert!(err("2SX\"5\"").message.contains("leftmost remaining"));
        assert!(err("3SX\"7\"").message.contains("cannot be truncated"));
    }

    #[test]
    fn decimal_literals() {
        assert_eq!(ok("D\"0\""), "0");
        assert_eq!(ok("D\"5\""), "101");
        assert_eq!(ok("16D\"255\""), "0000000011111111");
        assert_eq!(ok("8d\"1_0\""), "00001010");
        assert_eq!(ok("8D\"255\""), "11111111");
        // 2^64 needs 65 bits and must not overflow.
        let big = ok("D\"18446744073709551616\"");
        assert_eq!(big.len(), 65);
        assert!(big.starts_with('1') && big[1..].chars().all(|c| c == '0'));
    }

    #[test]
    fn decimal_errors() {
        let e = err("8D\"256\"");
        assert!(e.message.contains("needs 9 bits"), "{}", e.message);
        assert_eq!((e.start, e.end), (3, 6));
        let e = err("D\"1Z\"");
        assert!(e.message.contains("not a decimal digit"));
        assert_eq!((e.start, e.end), (3, 4));
        assert!(err("D\"\"").message.contains("at least one digit"));
    }

    #[test]
    fn invalid_digits_for_base() {
        let e = err("B\"0120\"");
        assert!(e.message.contains("digit '2'"), "{}", e.message);
        assert_eq!((e.start, e.end), (4, 5));
        let e = err("O\"78\"");
        assert_eq!((e.start, e.end), (3, 4));
        assert!(err("B\"A\"").message.contains("base-2"));
        assert!(err("UO\"9\"").message.contains("base-8"));
    }

    #[test]
    fn underline_placement() {
        assert!(err("X\"_F\"").message.contains("underline"));
        assert!(err("X\"F_\"").message.contains("underline"));
        assert!(err("X\"F__F\"").message.contains("underline"));
        assert!(err("1__6X\"F\"").message.contains("length"));
    }

    #[test]
    fn malformed_shapes() {
        assert!(err("Q\"1\"").message.contains("base specifier"));
        assert!(err("8\"1\"").message.contains("base specifier"));
        assert!(err("X1").message.contains("expected '\"'"));
        assert!(err("X\"1").message.contains("unterminated"));
        assert!(err("X\"1\"x").message.contains("unexpected characters"));
        assert!(err("8SX\"\"").message.contains("empty bit value"));
        assert!(err("99999999999X\"0\"").message.contains("maximum"));
    }

    #[test]
    fn control_characters_rejected() {
        assert!(err("X\"\t\"").message.contains("not a graphic character"));
    }
}
