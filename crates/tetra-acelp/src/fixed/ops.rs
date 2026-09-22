//! The TETRA/ETSI fixed-point "basic operators".
//!
//! Each operator reproduces the exact numerical result and saturation behaviour
//! of the fixed-point basic operators defined by the standard. Names use
//! idiomatic Rust `snake_case`; the
//! 32-bit operators keep the conventional `l_` prefix (for "long") so the
//! algorithm code reads close to the DSP pseudo-code in the specification.

use super::overflow;
use super::{MAX_16, MAX_32, MIN_16, MIN_32, Word16, Word32};

/// Saturate a 32-bit value to the 16-bit range, updating the overflow flag.
///
/// Sets the overflow flag when clamping occurs and clears it otherwise, as
/// defined for the fixed-point basic operators.
#[inline]
pub fn sature(value: Word32) -> Word16 {
    if value > MAX_16 as Word32 {
        overflow::set(true);
        MAX_16
    } else if value < MIN_16 as Word32 {
        overflow::set(true);
        MIN_16
    } else {
        overflow::set(false);
        value as Word16
    }
}

/// Absolute value; `abs_s(MIN_16) == MAX_16`.
#[inline]
pub fn abs_s(var1: Word16) -> Word16 {
    if var1 == MIN_16 { MAX_16 } else { var1.abs() }
}

/// Saturating 16-bit addition.
#[inline]
pub fn add(var1: Word16, var2: Word16) -> Word16 {
    sature(var1 as Word32 + var2 as Word32)
}

/// Saturating 16-bit subtraction.
#[inline]
pub fn sub(var1: Word16, var2: Word16) -> Word16 {
    sature(var1 as Word32 - var2 as Word32)
}

/// Saturating 16-bit negation; `negate(MIN_16) == MAX_16`.
#[inline]
pub fn negate(var1: Word16) -> Word16 {
    if var1 == MIN_16 { MAX_16 } else { -var1 }
}

/// Extract the 16 most-significant bits of a 32-bit value.
#[inline]
pub fn extract_h(l_var1: Word32) -> Word16 {
    (l_var1 >> 16) as Word16
}

/// Extract the 16 least-significant bits of a 32-bit value.
#[inline]
pub fn extract_l(l_var1: Word32) -> Word16 {
    l_var1 as Word16
}

/// Round the low 16 bits into the high 16 bits with saturation, then extract
/// the high half: `round(x) == extract_h(l_add(x, 0x8000))`.
#[inline]
pub fn round_(l_var1: Word32) -> Word16 {
    extract_h(l_add(l_var1, 0x0000_8000))
}

/// 16×16 → 32 multiply with a left shift by one (fractional multiply).
///
/// `l_mult(MIN_16, MIN_16) == MAX_32`.
#[inline]
pub fn l_mult(var1: Word16, var2: Word16) -> Word32 {
    let product = (var1 as Word32) * (var2 as Word32);
    if product != 0x4000_0000 {
        product * 2
    } else {
        overflow::set(true);
        MAX_32
    }
}

/// 16×16 → 32 multiply with no shift.
#[inline]
pub fn l_mult0(var1: Word16, var2: Word16) -> Word32 {
    (var1 as Word32) * (var2 as Word32)
}

/// Fractional 16×16 multiply returning a 16-bit result: `shr(var1*var2, 15)`.
#[inline]
pub fn mult(var1: Word16, var2: Word16) -> Word16 {
    let product = (var1 as Word32) * (var2 as Word32);
    let mut product = (product & 0xffff_8000u32 as i32) >> 15;
    if product & 0x0001_0000 != 0 {
        product |= 0xffff_0000u32 as i32;
    }
    sature(product)
}

/// Fractional 16×16 multiply with rounding: `shr(var1*var2 + 0x4000, 15)`.
#[inline]
pub fn mult_r(var1: Word16, var2: Word16) -> Word16 {
    let mut product = (var1 as Word32) * (var2 as Word32) + 0x0000_4000;
    product = (product & 0xffff_8000u32 as i32) >> 15;
    if product & 0x0001_0000 != 0 {
        product |= 0xffff_0000u32 as i32;
    }
    sature(product)
}

/// Saturating 32-bit addition.
#[inline]
pub fn l_add(l_var1: Word32, l_var2: Word32) -> Word32 {
    let sum = l_var1.wrapping_add(l_var2);
    // Overflow is only possible when both operands share the same sign and the
    // result's sign differs from that of the operands.
    if (l_var1 ^ l_var2) & MIN_32 == 0 && (sum ^ l_var1) & MIN_32 != 0 {
        overflow::set(true);
        if l_var1 < 0 { MIN_32 } else { MAX_32 }
    } else {
        sum
    }
}

/// Saturating 32-bit subtraction.
#[inline]
pub fn l_sub(l_var1: Word32, l_var2: Word32) -> Word32 {
    let diff = l_var1.wrapping_sub(l_var2);
    // Overflow is only possible when the operands have different signs and the
    // result's sign differs from that of the first operand.
    if (l_var1 ^ l_var2) & MIN_32 != 0 && (diff ^ l_var1) & MIN_32 != 0 {
        overflow::set(true);
        if l_var1 < 0 { MIN_32 } else { MAX_32 }
    } else {
        diff
    }
}

/// Multiply-accumulate: `l_add(l_var3, l_mult(var1, var2))`.
#[inline]
pub fn l_mac(l_var3: Word32, var1: Word16, var2: Word16) -> Word32 {
    l_add(l_var3, l_mult(var1, var2))
}

/// Multiply-accumulate, no shift: `l_add(l_var3, l_mult0(var1, var2))`.
#[inline]
pub fn l_mac0(l_var3: Word32, var1: Word16, var2: Word16) -> Word32 {
    l_add(l_var3, l_mult0(var1, var2))
}

/// Multiply-subtract: `l_sub(l_var3, l_mult(var1, var2))`.
#[inline]
pub fn l_msu(l_var3: Word32, var1: Word16, var2: Word16) -> Word32 {
    l_sub(l_var3, l_mult(var1, var2))
}

/// Multiply-subtract, no shift: `l_sub(l_var3, l_mult0(var1, var2))`.
#[inline]
pub fn l_msu0(l_var3: Word32, var1: Word16, var2: Word16) -> Word32 {
    l_sub(l_var3, l_mult0(var1, var2))
}

/// Deposit a 16-bit value into the high half of a 32-bit word.
#[inline]
pub fn l_deposit_h(var1: Word16) -> Word32 {
    (var1 as Word32) << 16
}

/// Deposit a 16-bit value into the low half of a 32-bit word (sign extended).
#[inline]
pub fn l_deposit_l(var1: Word16) -> Word32 {
    var1 as Word32
}

/// Saturating 32-bit negation; `l_negate(MIN_32) == MAX_32`.
#[inline]
pub fn l_negate(l_var1: Word32) -> Word32 {
    if l_var1 == MIN_32 { MAX_32 } else { -l_var1 }
}

/// 32-bit absolute value; `l_abs(MIN_32) == MAX_32`.
#[inline]
pub fn l_abs(l_var1: Word32) -> Word32 {
    if l_var1 == MIN_32 {
        MAX_32
    } else {
        l_var1.abs()
    }
}

/// Arithmetic left shift of a 16-bit value with saturation.
///
/// A negative shift count shifts right (see [`shr`]).
#[inline]
pub fn shl(var1: Word16, var2: Word16) -> Word16 {
    if var2 < 0 {
        return shr(var1, var2.wrapping_neg());
    }
    if var2 > 15 {
        return if var1 != 0 {
            overflow::set(true);
            if var1 > 0 { MAX_16 } else { MIN_16 }
        } else {
            0
        };
    }
    let result = (var1 as Word32) * (1 << var2);
    if result != result as Word16 as Word32 {
        overflow::set(true);
        if var1 > 0 { MAX_16 } else { MIN_16 }
    } else {
        result as Word16
    }
}

/// Arithmetic right shift of a 16-bit value with sign extension.
///
/// A negative shift count shifts left (see [`shl`]).
#[inline]
pub fn shr(var1: Word16, var2: Word16) -> Word16 {
    if var2 < 0 {
        return shl(var1, var2.wrapping_neg());
    }
    if var2 >= 15 {
        return if var1 < 0 { -1 } else { 0 };
    }
    var1 >> var2
}

/// Arithmetic left shift of a 32-bit value with saturation.
///
/// A negative shift count shifts right (see [`l_shr`]).
#[inline]
pub fn l_shl(l_var1: Word32, var2: Word16) -> Word32 {
    if var2 <= 0 {
        return l_shr(l_var1, var2.wrapping_neg());
    }
    let mut value = l_var1;
    for _ in 0..var2 {
        if value > 0x3fff_ffff {
            overflow::set(true);
            return MAX_32;
        }
        if value < -0x4000_0000 {
            overflow::set(true);
            return MIN_32;
        }
        value *= 2;
    }
    value
}

/// Arithmetic right shift of a 32-bit value with sign extension.
///
/// A negative shift count shifts left (see [`l_shl`]).
#[inline]
pub fn l_shr(l_var1: Word32, var2: Word16) -> Word32 {
    if var2 < 0 {
        return l_shl(l_var1, var2.wrapping_neg());
    }
    if var2 >= 31 {
        return if l_var1 < 0 { -1 } else { 0 };
    }
    l_var1 >> var2
}

/// Arithmetic right shift of a 32-bit value with rounding.
#[inline]
pub fn l_shr_r(l_var1: Word32, var2: Word16) -> Word32 {
    if var2 > 31 {
        return 0;
    }
    let mut result = l_shr(l_var1, var2);
    if var2 > 0 && l_var1 & (1 << (var2 - 1)) != 0 {
        result = result.wrapping_add(1);
    }
    result
}

/// Number of left shifts needed to normalise a 16-bit value.
#[inline]
pub fn norm_s(var1: Word16) -> Word16 {
    if var1 == 0 {
        0
    } else if var1 == -1 {
        15
    } else {
        let mut value = if var1 < 0 { !var1 } else { var1 };
        let mut count = 0;
        while value < 0x4000 {
            value <<= 1;
            count += 1;
        }
        count
    }
}

/// Number of left shifts needed to normalise a 32-bit value.
#[inline]
pub fn norm_l(l_var1: Word32) -> Word16 {
    if l_var1 == 0 {
        0
    } else if l_var1 == -1 {
        31
    } else {
        let mut value = if l_var1 < 0 { !l_var1 } else { l_var1 };
        let mut count = 0;
        while value < 0x4000_0000 {
            value <<= 1;
            count += 1;
        }
        count
    }
}

/// Fractional integer division `var1 / var2` in Q15.
///
/// Requires `0 <= var1 <= var2` and `var2 > 0`.
#[inline]
pub fn div_s(var1: Word16, var2: Word16) -> Word16 {
    debug_assert!(
        var1 >= 0 && var2 >= 0 && var1 <= var2 && var2 != 0,
        "div_s requires 0 <= var1 <= var2 and var2 != 0"
    );
    if var1 == 0 {
        0
    } else if var1 == var2 {
        MAX_16
    } else {
        let mut numerator = l_deposit_l(var1);
        let denominator = l_deposit_l(var2);
        let mut quotient: Word16 = 0;
        for _ in 0..15 {
            quotient <<= 1;
            numerator <<= 1;
            if numerator >= denominator {
                numerator = l_sub(numerator, denominator);
                quotient = add(quotient, 1);
            }
        }
        quotient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturating_add_sub() {
        assert_eq!(add(MAX_16, 1), MAX_16);
        assert_eq!(add(MIN_16, -1), MIN_16);
        assert_eq!(sub(MIN_16, 1), MIN_16);
        assert_eq!(sub(MAX_16, -1), MAX_16);
        assert_eq!(add(100, 200), 300);
    }

    #[test]
    fn multiply_edges() {
        assert_eq!(l_mult(MIN_16, MIN_16), MAX_32);
        assert_eq!(mult(MIN_16, MIN_16), MAX_16);
        assert_eq!(mult_r(MIN_16, MIN_16), MAX_16);
        assert_eq!(l_mult(0x4000, 0x4000), 0x2000_0000);
    }

    #[test]
    fn abs_and_negate_edges() {
        assert_eq!(abs_s(MIN_16), MAX_16);
        assert_eq!(negate(MIN_16), MAX_16);
        assert_eq!(l_abs(MIN_32), MAX_32);
        assert_eq!(l_negate(MIN_32), MAX_32);
    }

    #[test]
    fn shifts() {
        assert_eq!(shl(0x4000, 1), MAX_16);
        assert_eq!(shr(-4, 1), -2);
        assert_eq!(shr(-3, 1), -2);
        assert_eq!(l_shl(0x4000_0000, 1), MAX_32);
        assert_eq!(l_shr(-1, 5), -1);
        assert_eq!(l_shr_r(3, 1), 2);
    }

    #[test]
    fn norms() {
        assert_eq!(norm_s(0x4000), 0);
        assert_eq!(norm_s(0x2000), 1);
        assert_eq!(norm_l(0x4000_0000), 0);
        assert_eq!(norm_l(0x2000_0000), 1);
        assert_eq!(norm_l(-1), 31);
    }

    #[test]
    fn division() {
        assert_eq!(div_s(0, 100), 0);
        assert_eq!(div_s(100, 100), MAX_16);
        assert_eq!(div_s(0x2000, 0x4000), 0x4000);
    }
}
