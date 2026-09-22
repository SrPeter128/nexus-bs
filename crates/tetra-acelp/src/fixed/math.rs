//! Table-based fixed-point math: `inv_sqrt`, `log2`, `pow2`.
//!
//! Each is a piecewise-linear interpolation over a small lookup table. The
//! `pow2` and `inv_sqrt` tables are exact evaluations of published formulas
//! (`round(2^(i/32) * 16384)` and `round(32768 / sqrt(1 + i/16))`) and so are
//! kept inline. The `log2` table is not reproducible from its formula and is
//! obtained with the other generated data tables (see [`crate::tables`]).

use super::ops::*;
use super::{Word16, Word32};
use crate::tables::TAB_LOG2;

/// Interpolation table for [`inv_sqrt`]: `round(32768 / sqrt(1 + i/16))`.
const TAB_INV_SQRT: [Word16; 49] = [
    32767, 31790, 30894, 30070, 29309, 28602, 27945, 27330, 26755, 26214, 25705, 25225, 24770,
    24339, 23930, 23541, 23170, 22817, 22479, 22155, 21845, 21548, 21263, 20988, 20724, 20470,
    20225, 19988, 19760, 19539, 19326, 19119, 18919, 18725, 18536, 18354, 18176, 18004, 17837,
    17674, 17515, 17361, 17211, 17064, 16921, 16782, 16646, 16514, 16384,
];

/// Interpolation table for [`pow2`]: `round(2^(i/32) * 16384)`.
const TAB_POW2: [Word16; 33] = [
    16384, 16743, 17109, 17484, 17867, 18258, 18658, 19066, 19484, 19911, 20347, 20792, 21247,
    21713, 22188, 22674, 23170, 23678, 24196, 24726, 25268, 25821, 26386, 26964, 27554, 28158,
    28774, 29405, 30048, 30706, 31379, 32066, 32767,
];

/// Compute `1 / sqrt(l_x)` for `l_x >= 0`; result is in Q30.
pub fn inv_sqrt(l_x: Word32) -> Word32 {
    if l_x <= 0 {
        return 0x3fff_ffff;
    }

    let mut exp = norm_l(l_x);
    let mut l_x = l_shl(l_x, exp); // normalised

    exp = sub(30, exp);
    if exp & 1 == 0 {
        l_x = l_shr(l_x, 1);
    }
    exp = shr(exp, 1);
    exp = add(exp, 1);

    l_x = l_shr(l_x, 9);
    let i = extract_h(l_x); // b25..b31
    l_x = l_shr(l_x, 1);
    let a = extract_l(l_x) & 0x7fff; // b10..b24

    let i = sub(i, 16) as usize;

    let mut l_y = l_deposit_h(TAB_INV_SQRT[i]);
    let tmp = sub(TAB_INV_SQRT[i], TAB_INV_SQRT[i + 1]);
    l_y = l_msu(l_y, tmp, a);

    l_shr(l_y, exp)
}

/// Compute `log2(l_x)` for `l_x >= 0`, returning the integer exponent (0..=30)
/// and the Q15 fractional part.
pub fn log2(l_x: Word32) -> (Word16, Word16) {
    if l_x <= 0 {
        return (0, 0);
    }

    let exp = norm_l(l_x);
    let mut l_x = l_shl(l_x, exp);
    let exponent = sub(30, exp);

    l_x = l_shr(l_x, 9);
    let i = extract_h(l_x);
    l_x = l_shr(l_x, 1);
    let a = extract_l(l_x) & 0x7fff;

    let i = sub(i, 32) as usize;

    let mut l_y = l_deposit_h(TAB_LOG2[i]);
    let tmp = sub(TAB_LOG2[i], TAB_LOG2[i + 1]);
    l_y = l_msu(l_y, tmp, a);

    (exponent, extract_h(l_y))
}

/// Compute `2^(exponent.fraction)`; `fraction` is Q15, `exponent` in 0..=30.
pub fn pow2(exponent: Word16, fraction: Word16) -> Word32 {
    let mut l_x = l_deposit_l(fraction);
    l_x = l_shl(l_x, 6);
    let i = extract_h(l_x) as usize;
    l_x = l_shr(l_x, 1);
    let a = extract_l(l_x) & 0x7fff;

    let mut l_x = l_deposit_h(TAB_POW2[i]);
    let tmp = sub(TAB_POW2[i], TAB_POW2[i + 1]);
    l_x = l_msu(l_x, tmp, a);

    let exp = sub(30, exponent);
    l_shr_r(l_x, exp)
}
