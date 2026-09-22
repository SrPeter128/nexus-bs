//! Differential bit-exactness check for the fixed-point basic operators.
//!
//! This replays the exact deterministic input sweep implemented by the C
//! reference operator oracle (`op_oracle.c`, built against the ETSI reference
//! `tetra_op.c`) and folds every operator result into the same FNV-style
//! checksum. The expected value below is the checksum printed by that oracle;
//! a match proves the Rust operators are bit-identical to the reference across
//! two million randomised cases plus all the exercised edge values.

use tetra_acelp::fixed::dsp_ops::*;
use tetra_acelp::fixed::ext::*;
use tetra_acelp::fixed::math::*;
use tetra_acelp::fixed::ops::*;

const EXPECTED_CHECKSUM: u64 = 3446111561188545445;
const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;

struct Lcg(u32);
impl Lcg {
    #[inline]
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        self.0
    }
    #[inline]
    fn r16(&mut self) -> i16 {
        (self.next() >> 8) as i16
    }
    #[inline]
    fn r32(&mut self) -> i32 {
        let hi = self.next();
        let lo = self.next();
        ((hi << 8) ^ lo) as i32
    }
}

struct Checksum(u64);
impl Checksum {
    #[inline]
    fn mix16(&mut self, v: i16) {
        self.0 = self.0.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
    }
    #[inline]
    fn mix32(&mut self, v: i32) {
        self.0 = self.0.wrapping_mul(FNV_PRIME).wrapping_add(v as u32 as u64);
    }
}

#[test]
fn basic_operators_match_reference() {
    let mut rng = Lcg(0x12345678);
    let mut ck = Checksum(FNV_OFFSET);

    for _ in 0..2_000_000 {
        let a = rng.r16();
        let b = rng.r16();
        let la = rng.r32();
        let lb = rng.r32();
        let sh = ((rng.next() % 63) as i32 - 31) as i16;
        let sh16 = (rng.next() % 16) as i16;
        let sh32 = (rng.next() % 32) as i16;

        ck.mix16(abs_s(a));
        ck.mix16(add(a, b));
        ck.mix16(sub(a, b));
        ck.mix16(negate(a));
        ck.mix16(mult(a, b));
        ck.mix16(mult_r(a, b));
        ck.mix16(extract_h(la));
        ck.mix16(extract_l(la));
        ck.mix16(round_(la));
        ck.mix16(shl(a, sh));
        ck.mix16(shr(a, sh));
        ck.mix16(norm_s(a));
        ck.mix16(norm_l(la));

        ck.mix32(l_mult(a, b));
        ck.mix32(l_mult0(a, b));
        ck.mix32(l_mac(la, a, b));
        ck.mix32(l_mac0(la, a, b));
        ck.mix32(l_msu(la, a, b));
        ck.mix32(l_msu0(la, a, b));
        ck.mix32(l_add(la, lb));
        ck.mix32(l_sub(la, lb));
        ck.mix32(l_negate(la));
        ck.mix32(l_abs(la));
        ck.mix32(l_deposit_h(a));
        ck.mix32(l_deposit_l(a));
        ck.mix32(l_shl(la, sh));
        ck.mix32(l_shr(la, sh));
        ck.mix32(l_shr_r(la, sh));
        ck.mix32(l_shl(la, sh16));
        ck.mix32(l_shr(la, sh32));

        let x = abs_s(a) >> 1;
        let y = abs_s(b) | 1;
        let (x, y) = if x > y { (y, x) } else { (x, y) };
        ck.mix16(div_s(x, y));
    }

    // Second sweep: extended-precision, DSP helpers, and table-based math.
    for _ in 0..500_000 {
        let a = rng.r16();
        let b = rng.r16();
        let la = rng.r32();
        let lb = rng.r32();
        let sh = (rng.next() % 16) as i16;
        let v2 = (rng.next() % 8) as i16;
        let ms = (rng.next() % 16) as i16;
        let e = (rng.next() % 31) as i16;
        let fr = rng.r16() & 0x7fff;
        let dh = (0x4001 + (rng.next() % 0x3ffe)) as i16;
        let dl = (rng.next() % 0x8000) as i16;
        let k = (1 + (rng.next() % 14)) as i16;
        let lxpos = la & 0x7fff_ffff;
        let den = dpf_compose(dh, dl);
        let num = l_shr(den, k);

        ck.mix32(add_shifted(la, a, sh));
        ck.mix32(add_high16(la, a));
        ck.mix32(sub_shifted(la, a, sh));
        ck.mix32(sub_high16(la, a));
        ck.mix32(load_shifted(a, sh));
        ck.mix32(load_high16(a));
        ck.mix16(store_high(la, v2));

        let (nv, ns) = normalize_capped(la, ms);
        ck.mix32(nv);
        ck.mix16(ns);
        ck.mix32(dpf_compose(a, b));
        let (hi, lo) = dpf_split(lxpos);
        ck.mix16(hi);
        ck.mix16(lo);
        ck.mix32(mul_word_dpf(a, b, sub(b, a)));
        ck.mix32(mul_dpf(a, b, extract_h(lb), extract_l(lb)));
        ck.mix32(inv_sqrt(lxpos));
        let (ex, frc) = log2(lxpos);
        ck.mix16(ex);
        ck.mix16(frc);
        ck.mix32(pow2(e, fr));
        ck.mix32(div_dpf(num, dh, dl));
    }

    assert_eq!(
        ck.0, EXPECTED_CHECKSUM,
        "operator sweep diverged from reference"
    );
}
