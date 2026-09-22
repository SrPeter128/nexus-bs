//! Extended-precision fixed-point operations.
//!
//! Some parts of the codec need more than 16 bits of precision but not the full
//! 32 bits. They use a "double precision format" (DPF) that splits a 31-bit
//! value into a 16-bit high part and a 16-bit low part carrying the sign:
//! `value = hi << 15 + lo`, with `0xC000_0000 <= value <= 0x3FFF_FFFF`.

use super::dsp_ops::{add_shifted, load_shifted, sub_shifted};
use super::ops::*;
use super::{Word16, Word32};

/// Compose a 32-bit value from a DPF (high, low) pair: `hi << 15 + lo`.
#[inline]
pub fn dpf_compose(hi: Word16, lo: Word16) -> Word32 {
    add_shifted(load_shifted(lo, 0), hi, 15)
}

/// Split a 31-bit value (with `b30 == b31`) into a DPF (high, low) pair.
#[inline]
pub fn dpf_split(l_32: Word32) -> (Word16, Word16) {
    let hi = extract_h(l_shl(l_32, 1));
    let lo = extract_l(sub_shifted(l_32, hi, 15));
    (hi, lo)
}

/// Multiply a 16-bit integer by a DPF value, dividing the result by 2**16:
/// `hi1*lo2 + (lo1*lo2) >> 15`.
#[inline]
pub fn mul_word_dpf(hi1: Word16, lo1: Word16, lo2: Word16) -> Word32 {
    let p1 = extract_h(l_mult0(lo1, lo2));
    let l_32 = l_mult0(hi1, lo2);
    add_shifted(l_32, p1, 1)
}

/// Multiply two DPF values, dividing the result by 2**32.
#[inline]
pub fn mul_dpf(hi1: Word16, lo1: Word16, hi2: Word16, lo2: Word16) -> Word32 {
    let p1 = extract_h(l_mult0(hi1, lo2));
    let p2 = extract_h(l_mult0(lo1, hi2));
    let mut l_32 = l_mult0(hi1, hi2);
    l_32 = add_shifted(l_32, p1, 1);
    add_shifted(l_32, p2, 1)
}

/// Fractional division `l_num / l_denom` in Q30.
///
/// `l_num` and `l_denom` must be positive with `l_num <= l_denom`, and
/// `denom_hi` (the high DPF part of `l_denom`) must be normalised.
#[inline]
pub fn div_dpf(l_num: Word32, denom_hi: Word16, denom_lo: Word16) -> Word32 {
    // Start from the reciprocal of the high part alone (Q15).
    let approx = div_s(0x3fff, denom_hi);

    // One Newton step doubles the accuracy of the reciprocal.
    let mut t0 = mul_word_dpf(denom_hi, denom_lo, approx); // Q29
    t0 = l_sub(0x4000_0000, t0); // Q29
    let (hi, lo) = dpf_split(t0);
    t0 = mul_word_dpf(hi, lo, approx); // reciprocal of l_denom in Q28

    // Multiply the numerator by that reciprocal.
    let (hi, lo) = dpf_split(t0);
    let (n_hi, n_lo) = dpf_split(l_num);
    t0 = mul_dpf(n_hi, n_lo, hi, lo);

    l_shl(t0, 2) // Q28 -> Q30
}
