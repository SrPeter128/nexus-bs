//! Additional fixed-point helpers used across the codec (beyond the core
//! basic operators): shifted add/subtract, normalisation, bit packing.
//!
//! These are built directly on the core operators in [`super::ops`] and so are
//! bit-exact by construction.

use super::ops::*;
use super::{Word16, Word32};

/// `POW2[k] == -(1 << k)`, used to fold a shift into a no-shift multiply.
const NEG_POW2: [Word16; 16] = [
    -1, -2, -4, -8, -16, -32, -64, -128, -256, -512, -1024, -2048, -4096, -8192, -16384, -32768,
];

/// Add `var1 << shift` (0..=15) to `l_var2` with saturation.
#[inline]
pub fn add_shifted(l_var2: Word32, var1: Word16, shift: Word16) -> Word32 {
    l_msu0(l_var2, var1, NEG_POW2[shift as usize])
}

/// Add `var1 << 16` to `l_var2` with saturation.
#[inline]
pub fn add_high16(l_var2: Word32, var1: Word16) -> Word32 {
    l_msu(l_var2, var1, -32768)
}

/// Subtract `var1 << shift` (0..=15) from `l_var2` with saturation.
#[inline]
pub fn sub_shifted(l_var2: Word32, var1: Word16, shift: Word16) -> Word32 {
    l_mac0(l_var2, var1, NEG_POW2[shift as usize])
}

/// Subtract `var1 << 16` from `l_var2` with saturation.
#[inline]
pub fn sub_high16(l_var2: Word32, var1: Word16) -> Word32 {
    l_mac(l_var2, var1, -32768)
}

/// Load `var1 << shift` (0..=15) into a 32-bit word (sign extended).
#[inline]
pub fn load_shifted(var1: Word16, shift: Word16) -> Word32 {
    l_msu0(0, var1, NEG_POW2[shift as usize])
}

/// Load `var1 << 16` into a 32-bit word.
#[inline]
pub fn load_high16(var1: Word16) -> Word32 {
    l_msu(0, var1, -32768)
}

/// Store the high part of `l_var1` shifted left by `var2` (0..=7).
#[inline]
pub fn store_high(l_var1: Word32, var2: Word16) -> Word16 {
    const SHR: [Word16; 8] = [16, 15, 14, 13, 12, 11, 10, 9];
    extract_l(l_shr(l_var1, SHR[var2 as usize]))
}

/// Normalise `l_var3` by at most `max_shift` (0..=15) left shifts.
///
/// Returns the normalised value together with the number of shifts applied.
#[inline]
pub fn normalize_capped(l_var3: Word32, max_shift: Word16) -> (Word32, Word16) {
    let mut shift = norm_l(l_var3);
    if sub(shift, max_shift) > 0 {
        shift = max_shift;
    }
    (l_shl(l_var3, shift), shift)
}

/// Read `no_of_bits` bits (MSB first) from a bit slice and return the integer.
#[inline]
pub fn bits_to_int(no_of_bits: usize, bitstream: &[Word16]) -> Word16 {
    let mut value: Word16 = 0;
    for &bit in &bitstream[..no_of_bits] {
        value = shl(value, 1);
        if bit == 1 {
            value += 1;
        }
    }
    value
}

/// Write `no_of_bits` bits of `value` (MSB first) into a bit slice.
#[inline]
pub fn int_to_bits(value: Word16, no_of_bits: usize, bitstream: &mut [Word16]) {
    let mut value = value;
    for slot in bitstream[..no_of_bits].iter_mut().rev() {
        *slot = value & 1;
        value = shr(value, 1);
    }
}
