//! Fixed-width two-state integer arithmetic used by the expression evaluator.
//!
//! [`Bits`] is an unsigned integer of an exact bit width (any width, including
//! widths above 64 bits), stored as little-endian 64-bit limbs. Every
//! operation wraps modulo `2^width`, which is exactly Verilog's arithmetic on
//! a vector of that width. Signed operations interpret the same bits as two's
//! complement.
//!
//! The evaluator converts a [`LogicVec`] with [`Bits::from_logic`] only when
//! every bit is `0` or `1`; any `X`/`Z` operand makes an arithmetic result
//! all-`X` before this module is reached, following IEEE 1364-2005 5.1.5.

use std::cmp::Ordering;

use aion_common::{Logic, LogicVec};

/// A two-state unsigned integer of exactly `width` bits.
///
/// Bits above `width` in the top limb are always zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bits {
    /// The number of bits.
    width: u32,
    /// Little-endian 64-bit limbs; `width.div_ceil(64)` of them.
    limbs: Vec<u64>,
}

/// Returns the number of 64-bit limbs needed for `width` bits.
fn limb_count(width: u32) -> usize {
    width.div_ceil(64) as usize
}

impl Bits {
    /// Returns the value zero at `width` bits.
    pub(crate) fn zero(width: u32) -> Self {
        Self {
            width,
            limbs: vec![0; limb_count(width)],
        }
    }

    /// Returns `value` truncated to `width` bits.
    pub(crate) fn from_u64(value: u64, width: u32) -> Self {
        let mut b = Self::zero(width);
        if let Some(first) = b.limbs.first_mut() {
            *first = value;
        }
        b.mask_top();
        b
    }

    /// Converts a fully known `LogicVec` (only `0`/`1` bits) to `Bits`.
    ///
    /// Returns `None` if any bit is `X` or `Z`.
    pub(crate) fn from_logic(lv: &LogicVec) -> Option<Self> {
        let mut b = Self::zero(lv.width());
        for i in 0..lv.width() {
            match lv.get(i) {
                Logic::Zero => {}
                Logic::One => b.set_bit(i),
                Logic::X | Logic::Z => return None,
            }
        }
        Some(b)
    }

    /// Converts the value back to a `LogicVec` of the same width.
    pub(crate) fn to_logic(&self) -> LogicVec {
        let mut lv = LogicVec::new(self.width);
        for i in 0..self.width {
            if self.bit(i) {
                lv.set(i, Logic::One);
            }
        }
        lv
    }

    /// Returns bit `i` (`false` for bits at or above the width).
    pub(crate) fn bit(&self, i: u32) -> bool {
        if i >= self.width {
            return false;
        }
        (self.limbs[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    /// Sets bit `i` to one. `i` must be below the width.
    fn set_bit(&mut self, i: u32) {
        self.limbs[(i / 64) as usize] |= 1 << (i % 64);
    }

    /// Clears the bits of the top limb that lie above the width.
    fn mask_top(&mut self) {
        let rem = self.width % 64;
        if rem != 0 {
            if let Some(top) = self.limbs.last_mut() {
                *top &= (1u64 << rem) - 1;
            }
        }
    }

    /// Returns `true` if the value is zero.
    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.iter().all(|&l| l == 0)
    }

    /// Returns the most significant bit (the sign bit when read as signed).
    pub(crate) fn msb(&self) -> bool {
        self.width > 0 && self.bit(self.width - 1)
    }

    /// Returns `true` if the value is one.
    fn is_one(&self) -> bool {
        self.limbs.first() == Some(&1) && self.limbs[1..].iter().all(|&l| l == 0)
    }

    /// Returns `true` if every bit is one (the value `-1` when signed).
    fn is_all_ones(&self) -> bool {
        self.width > 0 && *self == Self::zero(self.width).sub(&Self::from_u64(1, self.width))
    }

    /// Returns the value as a `u64`, or `None` if it does not fit.
    pub(crate) fn as_u64(&self) -> Option<u64> {
        if self.limbs.iter().skip(1).any(|&l| l != 0) {
            return None;
        }
        Some(self.limbs.first().copied().unwrap_or(0))
    }

    /// Returns `self + other` modulo `2^width`. Both must have the same width.
    pub(crate) fn add(&self, other: &Self) -> Self {
        debug_assert_eq!(self.width, other.width);
        let mut out = Self::zero(self.width);
        let mut carry = false;
        for i in 0..out.limbs.len() {
            let (s1, c1) = self.limbs[i].overflowing_add(other.limbs[i]);
            let (s2, c2) = s1.overflowing_add(u64::from(carry));
            out.limbs[i] = s2;
            carry = c1 || c2;
        }
        out.mask_top();
        out
    }

    /// Returns `self - other` modulo `2^width`. Both must have the same width.
    pub(crate) fn sub(&self, other: &Self) -> Self {
        debug_assert_eq!(self.width, other.width);
        let mut out = Self::zero(self.width);
        let mut borrow = false;
        for i in 0..out.limbs.len() {
            let (d1, b1) = self.limbs[i].overflowing_sub(other.limbs[i]);
            let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
            out.limbs[i] = d2;
            borrow = b1 || b2;
        }
        out.mask_top();
        out
    }

    /// Returns `-self` modulo `2^width` (two's complement negation).
    pub(crate) fn neg(&self) -> Self {
        Self::zero(self.width).sub(self)
    }

    /// Returns `self * other` modulo `2^width`. Both must have the same width.
    pub(crate) fn mul(&self, other: &Self) -> Self {
        debug_assert_eq!(self.width, other.width);
        let n = self.limbs.len();
        let mut out = Self::zero(self.width);
        for i in 0..n {
            if self.limbs[i] == 0 {
                continue;
            }
            let mut carry: u128 = 0;
            for j in 0..(n - i) {
                let t = u128::from(self.limbs[i]) * u128::from(other.limbs[j])
                    + u128::from(out.limbs[i + j])
                    + carry;
                out.limbs[i + j] = t as u64;
                carry = t >> 64;
            }
        }
        out.mask_top();
        out
    }

    /// Compares two values of the same width as unsigned integers.
    pub(crate) fn ucmp(&self, other: &Self) -> Ordering {
        debug_assert_eq!(self.width, other.width);
        for i in (0..self.limbs.len()).rev() {
            match self.limbs[i].cmp(&other.limbs[i]) {
                Ordering::Equal => {}
                ord => return ord,
            }
        }
        Ordering::Equal
    }

    /// Compares two values of the same width as two's complement integers.
    pub(crate) fn scmp(&self, other: &Self) -> Ordering {
        match (self.msb(), other.msb()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => self.ucmp(other),
        }
    }

    /// Returns `self << amount`, filling with zeros; zero when
    /// `amount >= width`.
    pub(crate) fn shl(&self, amount: u64) -> Self {
        let mut out = Self::zero(self.width);
        if amount >= u64::from(self.width) {
            return out;
        }
        let limb_shift = (amount / 64) as usize;
        let bit_shift = (amount % 64) as u32;
        for i in (limb_shift..out.limbs.len()).rev() {
            let src = i - limb_shift;
            let mut v = self.limbs[src] << bit_shift;
            if bit_shift != 0 && src > 0 {
                v |= self.limbs[src - 1] >> (64 - bit_shift);
            }
            out.limbs[i] = v;
        }
        out.mask_top();
        out
    }

    /// Unsigned division with remainder. Both must have the same width.
    ///
    /// Returns `None` when `divisor` is zero.
    pub(crate) fn udivrem(&self, divisor: &Self) -> Option<(Self, Self)> {
        debug_assert_eq!(self.width, divisor.width);
        if divisor.is_zero() {
            return None;
        }
        if self.width <= 64 {
            let (a, b) = (self.as_u64()?, divisor.as_u64()?);
            return Some((
                Self::from_u64(a / b, self.width),
                Self::from_u64(a % b, self.width),
            ));
        }
        // Restoring long division, one bit at a time from the MSB.
        let mut quotient = Self::zero(self.width);
        let mut rem = Self::zero(self.width);
        for i in (0..self.width).rev() {
            rem = rem.shl(1);
            if self.bit(i) {
                rem.limbs[0] |= 1;
            }
            if rem.ucmp(divisor) != Ordering::Less {
                rem = rem.sub(divisor);
                quotient.set_bit(i);
            }
        }
        Some((quotient, rem))
    }

    /// Signed (two's complement) division with remainder, truncating toward
    /// zero; the remainder takes the sign of the dividend (IEEE 1364-2005
    /// 5.1.5).
    ///
    /// Returns `None` when `divisor` is zero.
    pub(crate) fn sdivrem(&self, divisor: &Self) -> Option<(Self, Self)> {
        let (neg_a, neg_b) = (self.msb(), divisor.msb());
        let a = if neg_a { self.neg() } else { self.clone() };
        let b = if neg_b {
            divisor.neg()
        } else {
            divisor.clone()
        };
        let (q, r) = a.udivrem(&b)?;
        let q = if neg_a != neg_b { q.neg() } else { q };
        let r = if neg_a { r.neg() } else { r };
        Some((q, r))
    }

    /// Returns `self ** exp` modulo `2^width`, by square-and-multiply with
    /// wrapping multiplication, for a non-negative exponent of any width.
    pub(crate) fn pow(&self, exp: &Self) -> Self {
        let mut result = Self::from_u64(1, self.width);
        let mut base = self.clone();
        let top = (0..exp.width).rev().find(|&i| exp.bit(i));
        let Some(top) = top else {
            return result; // x ** 0 == 1 (including 0 ** 0)
        };
        for i in 0..=top {
            if exp.bit(i) {
                result = result.mul(&base);
            }
            if i < top {
                base = base.mul(&base);
                if base.is_zero() {
                    // Every further factor is zero; the result is zero if a
                    // later exponent bit is set.
                    if (i + 1..=top).any(|j| exp.bit(j)) {
                        return Self::zero(self.width);
                    }
                    break;
                }
            }
        }
        result
    }

    /// Returns `self ** exp` for an exponent known to be negative, following
    /// IEEE 1364-2005 Table 5-6: `0 ** neg` is `None` (all-X), `1 ** neg`
    /// is `1`, `-1 ** neg` (signed base only) is `1` for an even exponent and
    /// `-1` for an odd one, and any other base gives `0`.
    pub(crate) fn pow_negative_exp(&self, exp: &Self, signed_base: bool) -> Option<Self> {
        if self.is_zero() {
            None
        } else if self.is_one() {
            Some(Self::from_u64(1, self.width))
        } else if signed_base && self.is_all_ones() {
            if exp.bit(0) {
                Some(self.clone())
            } else {
                Some(Self::from_u64(1, self.width))
            }
        } else {
            Some(Self::zero(self.width))
        }
    }

    /// Formats the value as an unsigned decimal number.
    pub(crate) fn to_decimal(&self) -> String {
        const CHUNK: u64 = 10_000_000_000_000_000_000; // 10^19
        let mut limbs = self.limbs.clone();
        let mut chunks = Vec::new();
        loop {
            // Divide the limb vector by 10^19 in place.
            let mut rem: u128 = 0;
            for l in limbs.iter_mut().rev() {
                let cur = (rem << 64) | u128::from(*l);
                *l = (cur / u128::from(CHUNK)) as u64;
                rem = cur % u128::from(CHUNK);
            }
            chunks.push(rem as u64);
            if limbs.iter().all(|&l| l == 0) {
                break;
            }
        }
        let mut s = chunks.pop().unwrap_or(0).to_string();
        for c in chunks.iter().rev() {
            s.push_str(&format!("{c:019}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(v: u64, w: u32) -> Bits {
        Bits::from_u64(v, w)
    }

    /// Builds a `w`-bit value from a `u128`.
    fn b128(v: u128, w: u32) -> Bits {
        let lo = Bits::from_u64(v as u64, w);
        let hi = Bits::from_u64((v >> 64) as u64, w).shl(64);
        lo.add(&hi)
    }

    #[test]
    fn from_u64_masks_to_width() {
        assert_eq!(b(0x1FF, 8).as_u64(), Some(0xFF));
        assert_eq!(Bits::zero(0).as_u64(), Some(0));
    }

    #[test]
    fn logic_roundtrip_and_xz_rejected() {
        let lv = LogicVec::from_u64(0b1011, 4);
        assert_eq!(Bits::from_logic(&lv).unwrap().to_logic(), lv);
        let x = LogicVec::from_binary_str("1x01").unwrap();
        assert!(Bits::from_logic(&x).is_none());
    }

    #[test]
    fn add_carries_across_limbs_and_wraps() {
        let a = b128(u128::from(u64::MAX), 72);
        let r = a.add(&b(1, 72));
        assert_eq!(r, b128(1u128 << 64, 72));
        // 72-bit all-ones + 1 wraps to 0.
        let ones = Bits::zero(72).sub(&b(1, 72));
        assert!(ones.add(&b(1, 72)).is_zero());
    }

    #[test]
    fn sub_borrows_and_neg_wraps() {
        assert_eq!(b(0, 8).sub(&b(1, 8)).as_u64(), Some(0xFF));
        assert_eq!(b(5, 8).neg().as_u64(), Some(0xFB));
        let r = b128(1u128 << 64, 100).sub(&b(1, 100));
        assert_eq!(r, b128(u128::from(u64::MAX), 100));
    }

    #[test]
    fn mul_wide_and_truncating() {
        let a = b128(1u128 << 40, 128);
        let r = a.mul(&a);
        assert_eq!(r, b128(1u128 << 80, 128));
        // 8-bit wrap: 16 * 16 = 256 -> 0.
        assert!(b(16, 8).mul(&b(16, 8)).is_zero());
    }

    #[test]
    fn compare_unsigned_and_signed() {
        assert_eq!(b(0x80, 8).ucmp(&b(1, 8)), Ordering::Greater);
        assert_eq!(b(0x80, 8).scmp(&b(1, 8)), Ordering::Less);
        assert_eq!(b(0xFF, 8).scmp(&b(0xFE, 8)), Ordering::Greater);
        let hi = b128(1u128 << 70, 72);
        assert_eq!(hi.ucmp(&b(5, 72)), Ordering::Greater);
    }

    #[test]
    fn shifts_across_limbs_and_out_of_range() {
        let one = b(1, 100);
        assert_eq!(one.shl(70), b128(1u128 << 70, 100));
        assert_eq!(b128(1u128 << 3, 100).shl(67), b128(1u128 << 70, 100));
        assert!(one.shl(100).is_zero());
        assert!(one.shl(u64::MAX).is_zero());
        assert_eq!(b(0b0011, 4).shl(2).as_u64(), Some(0b1100));
        assert_eq!(b(0b0011, 4).shl(3).as_u64(), Some(0b1000));
    }

    #[test]
    fn udivrem_small_wide_and_by_zero() {
        let (q, r) = b(17, 8).udivrem(&b(5, 8)).unwrap();
        assert_eq!((q.as_u64(), r.as_u64()), (Some(3), Some(2)));
        let a = b128((1u128 << 80) + 7, 96);
        let (q, r) = a.udivrem(&b128(1u128 << 40, 96)).unwrap();
        assert_eq!(q, b128(1u128 << 40, 96));
        assert_eq!(r.as_u64(), Some(7));
        assert!(b(3, 8).udivrem(&b(0, 8)).is_none());
        assert!(b128(1u128 << 70, 72).udivrem(&Bits::zero(72)).is_none());
    }

    #[test]
    fn sdivrem_truncates_toward_zero() {
        // -7 / 2 = -3 rem -1 (8-bit).
        let (q, r) = b(0xF9, 8).sdivrem(&b(2, 8)).unwrap();
        assert_eq!((q.as_u64(), r.as_u64()), (Some(0xFD), Some(0xFF)));
        // 7 / -2 = -3 rem 1.
        let (q, r) = b(7, 8).sdivrem(&b(0xFE, 8)).unwrap();
        assert_eq!((q.as_u64(), r.as_u64()), (Some(0xFD), Some(1)));
        assert!(b(7, 8).sdivrem(&b(0, 8)).is_none());
    }

    #[test]
    fn pow_wraps_without_overflow() {
        // 2 ** 200 mod 2^32 == 0; 3 ** 5 == 243.
        assert!(b(2, 32).pow(&b(200, 32)).is_zero());
        assert_eq!(b(3, 32).pow(&b(5, 32)).as_u64(), Some(243));
        assert_eq!(b(0, 8).pow(&b(0, 8)).as_u64(), Some(1));
        // 3 ** (2^64 - 1) mod 2^8 with a 64-bit exponent: no truncation.
        let e = b(u64::MAX, 64);
        // 3 has multiplicative order 64 mod 256, and 2^64-1 ≡ 63 (mod 64).
        let r = b(3, 8).pow(&e);
        let r63 = b(3, 8).pow(&b(63, 8));
        assert_eq!(r, r63);
        // Wide exponent with high bits set.
        let wide_e = b128(1u128 << 100, 128);
        assert!(b(2, 16).pow(&wide_e).is_zero());
        assert_eq!(b(1, 16).pow(&wide_e).as_u64(), Some(1));
    }

    #[test]
    fn pow_negative_exponent_table() {
        let e = b(0xFF, 8); // -1
        assert!(b(0, 8).pow_negative_exp(&e, true).is_none());
        assert_eq!(
            b(1, 8).pow_negative_exp(&e, true).unwrap().as_u64(),
            Some(1)
        );
        assert_eq!(
            b(0xFF, 8).pow_negative_exp(&e, true).unwrap().as_u64(),
            Some(0xFF)
        );
        assert_eq!(
            b(0xFF, 8)
                .pow_negative_exp(&b(0xFE, 8), true)
                .unwrap()
                .as_u64(),
            Some(1)
        );
        assert_eq!(
            b(5, 8).pow_negative_exp(&e, true).unwrap().as_u64(),
            Some(0)
        );
    }

    #[test]
    fn decimal_formatting() {
        assert_eq!(b(0, 8).to_decimal(), "0");
        assert_eq!(b(1234, 16).to_decimal(), "1234");
        assert_eq!(
            b128(u128::MAX, 128).to_decimal(),
            "340282366920938463463374607431768211455"
        );
        assert_eq!(b128(1u128 << 64, 72).to_decimal(), "18446744073709551616");
        assert_eq!(Bits::zero(0).to_decimal(), "0");
    }
}
