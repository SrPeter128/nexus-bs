//! Differential bit-exactness check for the shared DSP primitives.
//!
//! Replays the exact deterministic LP-analysis / filtering chain implemented by
//! the C reference DSP oracle (`dsp_oracle.c`) and folds every routine's output
//! into the same checksum. A match proves the Rust `dsp` module reproduces the
//! reference bit-for-bit across 20 000 valid frames.

use tetra_acelp::dsp::*;

const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;
const EXPECTED_CHECKSUM: u64 = 9130172986994531003;

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        self.0
    }
    fn sig(&mut self) -> i16 {
        ((self.next() % 16001) as i32 - 8000) as i16
    }
}

struct Checksum(u64);
impl Checksum {
    fn m16(&mut self, v: i16) {
        self.0 = self.0.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
    }
    fn mv(&mut self, a: &[i16]) {
        for &v in a {
            self.m16(v);
        }
    }
    fn m32(&mut self, v: i32) {
        self.0 = self.0.wrapping_mul(FNV_PRIME).wrapping_add(v as u32 as u64);
    }
}

#[test]
fn dsp_chain_matches_reference() {
    let mut rng = Lcg(0xC0FFEE11);
    let mut ck = Checksum(FNV_OFFSET);

    let mut old_lsp: [i16; 10] = [
        30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
    ];
    let mut old_a: [i16; 11] = [4096, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut mem = [0i16; 10];

    for _ in 0..20_000 {
        let mut x = [0i16; 256];
        for xi in x.iter_mut() {
            *xi = rng.sig();
        }

        let mut rh = [0i16; 11];
        let mut rl = [0i16; 11];
        autocorrelation(&x, &mut rh, &mut rl);
        ck.mv(&rh);
        ck.mv(&rl);
        apply_lag_window(&mut rh, &mut rl);
        ck.mv(&rh);
        ck.mv(&rl);

        let mut a = [0i16; 11];
        levinson_durbin(&rh, &rl, &mut a, &mut old_a);
        ck.mv(&a);

        let mut lsp = [0i16; 10];
        lp_to_lsp(&a, &mut lsp, &old_lsp);
        ck.mv(&lsp);

        let mut a2 = [0i16; 11];
        lsp_to_lp(&lsp, &mut a2);
        ck.mv(&a2);

        let mut a44 = [0i16; 44];
        interpolate_lp(&old_lsp, &lsp, &mut a44);
        ck.mv(&a44);

        let mut fac = [0i16; 10];
        weight_factors(27853, &mut fac);
        ck.mv(&fac);

        let mut aexp = [0i16; 11];
        weight_lp(&a, &fac, &mut aexp);
        ck.mv(&aexp);

        let mut xbuf = [0i16; 70];
        xbuf.copy_from_slice(&x[..70]);
        let mut res = [0i16; 60];
        lp_residual(&a, &xbuf, &mut res, 60);
        ck.mv(&res);

        let mut syn = [0i16; 60];
        synthesis_filter(&a, &res, &mut syn, 60, &mut mem, true);
        ck.mv(&syn);

        let mut cv = [0i16; 60];
        let mut bf = [0i16; 60];
        {
            let mut h60 = [0i16; 60];
            h60[0] = 4096;
            let mut memz = [0i16; 10];
            let mut h60o = h60;
            synthesis_filter(&a, &h60, &mut h60o, 60, &mut memz, false);
            h60 = h60o;
            convolution(&res, &h60, &mut cv, 60);
            ck.mv(&cv);
            backward_filter(&res, &h60, &mut bf, 60);
            ck.mv(&bf);
        }

        let g = lp_impulse_energy(&a);
        ck.m32(g);

        old_lsp.copy_from_slice(&lsp);
    }

    assert_eq!(ck.0, EXPECTED_CHECKSUM, "DSP chain diverged from reference");
}
