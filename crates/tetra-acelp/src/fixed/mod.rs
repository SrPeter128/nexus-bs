//! Fixed-point arithmetic used throughout the TETRA speech codec.
//!
//! The ETSI TETRA codec (EN 300 395-2) is specified as a bit-exact fixed-point
//! algorithm built on a small set of saturating 16-/32-bit integer operators
//! (the same family of "basic operators" shared by the ITU-T/ETSI speech
//! codecs). This module provides a clean-room Rust implementation of those
//! operators, reproducing their exact numerical and saturation semantics so
//! that the higher layers can be bit-exact with the standard.
//!
//! Two integer widths are used:
//! * [`Word16`] — 16-bit signed (`i16`)
//! * [`Word32`] — 32-bit signed (`i32`)
//!
//! Saturating operators clamp to the 16- or 32-bit range. A few algorithms in
//! the codec (autocorrelation scaling, gain computation, pitch decimation)
//! detect arithmetic overflow to decide whether to rescale and recompute; to
//! support that, saturating operators record whether saturation occurred in a
//! thread-local [`overflow`] flag, mirroring the global saturation flag of the
//! fixed-point basic operators.

pub mod dsp_ops;
pub mod ext;
pub mod math;
pub mod ops;

pub use ops::*;

/// 16-bit signed word.
pub type Word16 = i16;
/// 32-bit signed word.
pub type Word32 = i32;

/// Largest 16-bit value, `0x7FFF`.
pub const MAX_16: Word16 = i16::MAX;
/// Smallest 16-bit value, `0x8000`.
pub const MIN_16: Word16 = i16::MIN;
/// Largest 32-bit value, `0x7FFF_FFFF`.
pub const MAX_32: Word32 = i32::MAX;
/// Smallest 32-bit value, `0x8000_0000`.
pub const MIN_32: Word32 = i32::MIN;

use std::cell::Cell;

thread_local! {
    static OVERFLOW: Cell<bool> = const { Cell::new(false) };
}

/// Overflow flag: the global saturation flag of the fixed-point basic operators.
///
/// Saturating operators set it to `true` when they saturate and (in the case
/// of the 16-bit `sature`-based operators) clear it to `false` when they do
/// not. Algorithms that need overflow-driven rescaling call [`overflow::reset`]
/// before an accumulation and [`overflow::occurred`] afterwards.
pub mod overflow {
    use super::OVERFLOW;

    /// Clear the overflow flag.
    #[inline]
    pub fn reset() {
        OVERFLOW.with(|o| o.set(false));
    }

    /// Read the overflow flag.
    #[inline]
    pub fn occurred() -> bool {
        OVERFLOW.with(|o| o.get())
    }

    #[inline]
    pub(crate) fn set(value: bool) {
        OVERFLOW.with(|o| o.set(value));
    }
}
