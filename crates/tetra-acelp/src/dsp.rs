//! Shared signal-processing primitives used by both the encoder and decoder.
//!
//! These implement the signal-processing steps of EN 300 395-2 clause 4.2
//! (LP analysis, LSP conversions, synthesis/analysis filtering), written from
//! the algorithms in the specification and built on the verified fixed-point
//! operators.

use crate::fixed::dsp_ops::*;
use crate::fixed::ext::*;
use crate::fixed::ops::*;
use crate::fixed::{Word16, Word32, overflow};
use crate::tables::{GRID, INTER32_1_3, INTER32_M1_3, LAG_H, LAG_L, WINDOW};

/// LP predictor order.
pub const P: usize = 10;
/// Half the LP order (number of LSP pairs).
pub const NC: usize = P / 2;
/// Analysis window length.
pub const L_WINDOW: usize = 256;
/// Number of grid points used when searching for LSP roots.
pub const GRID_POINTS: usize = 60;
/// Length of the impulse response used by [`lp_impulse_energy`].
const LLG: usize = 60;

/// Post-processing of synthesised speech: multiply by two with saturation.
pub fn postprocess(signal: &mut [Word16]) {
    for s in signal.iter_mut() {
        *s = add(*s, *s);
    }
}

/// Compute the spectral-expansion factors `fac[i] = gamma^(i+1)` (Q15).
pub fn weight_factors(gamma: Word16, fac: &mut [Word16; P]) {
    fac[0] = gamma;
    for i in 1..P {
        fac[i] = round_(l_mult(fac[i - 1], gamma));
    }
}

/// Apply spectral expansion to `a`: `a_exp[i] = a[i] * fac[i-1]`.
pub fn weight_lp(a: &[Word16], fac: &[Word16], a_exp: &mut [Word16]) {
    a_exp[0] = a[0];
    for i in 1..=P {
        a_exp[i] = round_(l_mult(a[i], fac[i - 1]));
    }
}

/// Compute autocorrelations `r[0..=P]` of the windowed signal, in DPF
/// (`r_h`/`r_l`) with a shared normalisation.
pub fn autocorrelation(x: &[Word16], r_h: &mut [Word16], r_l: &mut [Word16]) {
    let mut y = [0i16; L_WINDOW];
    for i in 0..L_WINDOW {
        y[i] = mult_r(x[i], WINDOW[i]);
    }

    // r[0], rescaling the windowed signal if the accumulation overflows.
    let mut sum;
    loop {
        overflow::reset();
        sum = 1;
        for i in 0..L_WINDOW {
            sum = l_mac0(sum, y[i], y[i]);
        }
        if !overflow::occurred() {
            break;
        }
        for i in 0..L_WINDOW {
            y[i] = shr(y[i], 2);
        }
    }

    let norm = norm_l(sum);
    sum = l_shl(sum, norm);
    sum = l_shr(sum, 1); // store r[0] as a packed high/low pair
    let (h, l) = dpf_split(sum);
    r_h[0] = h;
    r_l[0] = l;

    for i in 1..=P {
        let mut s = 0;
        for j in 0..(L_WINDOW - i) {
            s = l_mac0(s, y[j], y[j + i]);
        }
        s = l_shr(s, 1);
        s = l_shl(s, norm);
        let (h, l) = dpf_split(s);
        r_h[i] = h;
        r_l[i] = l;
    }
}

/// Apply the 60 Hz lag window to the autocorrelations (in place, DPF).
pub fn apply_lag_window(r_h: &mut [Word16], r_l: &mut [Word16]) {
    for i in 1..=P {
        let x = mul_dpf(r_h[i], r_l[i], LAG_H[i - 1], LAG_L[i - 1]);
        let (h, l) = dpf_split(x);
        r_h[i] = h;
        r_l[i] = l;
    }
}

/// Levinson-Durbin recursion in double precision.
///
/// Produces the LP coefficients `a[0..=P]` in Q12 from the autocorrelations.
/// `old_a` holds the previous stable filter, reused if this frame is unstable.
pub fn levinson_durbin(
    rh: &[Word16],
    rl: &[Word16],
    a: &mut [Word16],
    old_a: &mut [Word16; P + 1],
) {
    let mut ah = [0i16; P + 1];
    let mut al = [0i16; P + 1];
    let mut anh = [0i16; P + 1];
    let mut anl = [0i16; P + 1];

    // First reflection coefficient, giving a[1] from -r[1]/r[0].
    let t1 = dpf_compose(rh[1], rl[1]);
    let t2 = l_abs(t1);
    let mut t0 = div_dpf(t2, rh[0], rl[0]);
    if t1 > 0 {
        t0 = l_negate(t0);
    }
    let (mut kh, mut kl) = dpf_split(t0);
    t0 = l_shr(t0, 4);
    let (h, l) = dpf_split(t0);
    ah[1] = h;
    al[1] = l;

    // Alpha = R[0] * (1 - K^2)
    t0 = mul_dpf(kh, kl, kh, kl);
    t0 = l_abs(t0);
    t0 = l_sub(0x3fff_ffff, t0);
    let (hi, lo) = dpf_split(t0);
    t0 = mul_dpf(rh[0], rl[0], hi, lo);

    // Normalise Alpha.
    let (mut t0n, exp0) = normalize_capped(t0, 12);
    t0n = l_shr(t0n, 1);
    let (mut alpha_hi, mut alpha_lo) = dpf_split(t0n);
    let mut alpha_exp = exp0 - 1;

    for i in 2..=P {
        // t0 = sum(R[j] * A[i-j], j=1..i-1) + R[i]
        let mut t0 = 0;
        for j in 1..i {
            t0 = l_add(t0, mul_dpf(rh[j], rl[j], ah[i - j], al[i - j]));
        }
        t0 = l_shl(t0, 4);
        let t1 = dpf_compose(rh[i], rl[i]);
        t0 = l_add(t0, t1);

        // Reflection coefficient for this order: negate t0 divided by alpha.
        let t1 = l_abs(t0);
        let mut t2 = div_dpf(t1, alpha_hi, alpha_lo);
        if t0 > 0 {
            t2 = l_negate(t2);
        }
        t2 = l_shl(t2, alpha_exp);
        let (kh_i, kl_i) = dpf_split(t2);
        kh = kh_i;
        kl = kl_i;

        // Unstable filter: keep the previous A(z).
        if abs_s(kh) > 32750 {
            a[..=P].copy_from_slice(&old_a[..=P]);
            return;
        }

        for j in 1..i {
            let mut t0 = mul_dpf(kh, kl, ah[i - j], al[i - j]);
            t0 = add_shifted(t0, ah[j], 15);
            t0 = add_shifted(t0, al[j], 0);
            let (h, l) = dpf_split(t0);
            anh[j] = h;
            anl[j] = l;
        }
        t2 = l_shr(t2, 4);
        let (h, l) = dpf_split(t2);
        anh[i] = h;
        anl[i] = l;

        // Alpha = Alpha * (1 - K^2)
        let mut t0 = mul_dpf(kh, kl, kh, kl);
        t0 = l_abs(t0);
        t0 = l_sub(0x3fff_ffff, t0);
        let (hi, lo) = dpf_split(t0);
        t0 = mul_dpf(alpha_hi, alpha_lo, hi, lo);

        let (t0n, exp) = normalize_capped(t0, 12);
        let t0n = l_shr(t0n, 1);
        let (h, l) = dpf_split(t0n);
        alpha_hi = h;
        alpha_lo = l;
        alpha_exp += exp - 1;

        for j in 1..=i {
            ah[j] = anh[j];
            al[j] = anl[j];
        }
    }

    // Truncate A[i] from Q26 to Q12 with rounding.
    a[0] = 4096;
    for i in 1..=P {
        let mut t0 = dpf_compose(ah[i], al[i]);
        t0 = add_shifted(t0, 1, 13);
        let v = store_high(t0, 2);
        old_a[i] = v;
        a[i] = v;
    }
}

/// Evaluate the Chebyshev polynomial series `C(x)` in Q14.
pub fn cheby_eval(x: Word16, f: &[Word16], n: usize) -> Word16 {
    let mut b2_h = 512; // 1.0 in Q24 DPF
    let mut b2_l = 0;

    let mut t0 = load_shifted(x, 10);
    t0 = add_shifted(t0, f[1], 13);
    let (mut b1_h, mut b1_l) = dpf_split(t0);

    for i in 2..n {
        let mut t0 = mul_word_dpf(b1_h, b1_l, x);
        t0 = l_shl(t0, 1);
        t0 = sub_shifted(t0, b2_l, 0);
        t0 = sub_shifted(t0, b2_h, 15);
        t0 = add_shifted(t0, f[i], 13);

        let (b0_h, b0_l) = dpf_split(t0);
        b2_l = b1_l;
        b2_h = b1_h;
        b1_l = b0_l;
        b1_h = b0_h;
    }
    let mut t0 = mul_word_dpf(b1_h, b1_l, x);
    t0 = sub_shifted(t0, b2_l, 0);
    t0 = sub_shifted(t0, b2_h, 15);
    t0 = add_shifted(t0, f[n], 12);

    t0 = l_shl(t0, 6);
    extract_h(t0)
}

/// Convert LP coefficients to LSPs in the cosine domain (Q15).
///
/// `old_lsp` is used unchanged if fewer than `P` roots are found.
pub fn lp_to_lsp(a: &[Word16], lsp: &mut [Word16], old_lsp: &[Word16]) {
    let mut f1 = [0i16; NC + 1];
    let mut f2 = [0i16; NC + 1];

    f1[0] = 2048; // 1.0 in Q11
    f2[0] = 2048;
    for i in 0..NC {
        let mut t0 = load_shifted(a[i + 1], 15);
        t0 = add_shifted(t0, a[P - i], 15);
        t0 = sub_high16(t0, f1[i]);
        f1[i + 1] = extract_h(t0);

        let mut t0 = load_shifted(a[i + 1], 15);
        t0 = sub_shifted(t0, a[P - i], 15);
        t0 = add_high16(t0, f2[i]);
        f2[i + 1] = extract_h(t0);
    }

    let mut nf = 0;
    let mut ip = 0;
    let mut use_f1 = true;

    let mut xlow = GRID[0];
    let mut ylow = cheby_eval(xlow, &f1, NC);

    let mut j = 0;
    while nf < P && j < GRID_POINTS {
        j += 1;
        let mut xhigh = xlow;
        let mut yhigh = ylow;
        xlow = GRID[j];
        ylow = cheby_eval(xlow, if use_f1 { &f1 } else { &f2 }, NC);

        if l_mult0(ylow, yhigh) <= 0 {
            for _ in 0..4 {
                let mut t0 = load_shifted(xlow, 15);
                t0 = add_shifted(t0, xhigh, 15);
                let xmid = extract_h(t0);

                let ymid = cheby_eval(xmid, if use_f1 { &f1 } else { &f2 }, NC);

                if l_mult0(ylow, ymid) <= 0 {
                    yhigh = ymid;
                    xhigh = xmid;
                } else {
                    ylow = ymid;
                    xlow = xmid;
                }
            }

            // Locate the root inside the bracket by interpolating between its ends.
            let x = sub(xhigh, xlow);
            let y = sub(yhigh, ylow);

            let xint = if y == 0 {
                xlow
            } else {
                let sign = y;
                let mut y = abs_s(y);
                let exp = norm_s(y);
                y = shl(y, exp);
                y = div_s(16383, y);
                let mut t0 = l_mult0(x, y);
                t0 = l_shr(t0, sub(19, exp));
                let mut y = extract_l(t0);
                if sign < 0 {
                    y = negate(y);
                }
                let mut t0 = load_shifted(xlow, 10);
                t0 = l_msu0(t0, ylow, y);
                store_high(t0, 6)
            };

            lsp[nf] = xint;
            xlow = xint;
            nf += 1;

            if ip == 0 {
                ip = 1;
                use_f1 = false;
            } else {
                ip = 0;
                use_f1 = true;
            }
            ylow = cheby_eval(xlow, if use_f1 { &f1 } else { &f2 }, NC);
        }
    }

    if nf < P {
        lsp[..P].copy_from_slice(&old_lsp[..P]);
    }
}

/// Convert LSPs (cosine domain, Q15) back to LP coefficients (Q12).
pub fn lsp_to_lp(lsp: &[Word16], a: &mut [Word16]) {
    // F1(z) / F2(z) (Q24) built from a set of LSPs.
    let poly = |lsp: &[Word16], f: &mut [Word32; NC + 1]| {
        f[0] = load_shifted(4096, 12); // 1.0 in Q24
        f[1] = 0;
        f[1] = sub_shifted(f[1], lsp[0], 10); // -2 * lsp[0]

        let mut lp = 2usize;
        for i in 2..=NC {
            f[i] = f[i - 2];
            let mut cur = i;
            for _ in 1..i {
                let (hi, lo) = dpf_split(f[cur - 1]);
                let mut t0 = mul_word_dpf(hi, lo, lsp[lp]);
                t0 = l_shl(t0, 1);
                f[cur] = l_add(f[cur], f[cur - 2]);
                f[cur] = l_sub(f[cur], t0);
                cur -= 1;
            }
            f[1] = sub_shifted(f[1], lsp[lp], 10);
            lp += 2;
        }
    };

    let mut f1 = [0i32; NC + 1];
    let mut f2 = [0i32; NC + 1];
    poly(&lsp[0..], &mut f1);
    poly(&lsp[1..], &mut f2);

    for i in (1..=NC).rev() {
        f1[i] = l_add(f1[i], f1[i - 1]);
        f2[i] = l_sub(f2[i], f2[i - 1]);
    }

    a[0] = 4096;
    let mut j = P;
    for i in 1..=NC {
        let t0 = l_add(f1[i], f2[i]);
        a[i] = extract_l(l_shr_r(t0, 13));
        let t0 = l_sub(f1[i], f2[i]);
        a[j] = extract_l(l_shr_r(t0, 13));
        j -= 1;
    }
}

/// Interpolate LSPs across the four subframes and convert to LP filters.
///
/// `a` receives four `P+1`-length coefficient sets (44 values).
pub fn interpolate_lp(lsp_old: &[Word16], lsp_new: &[Word16], a: &mut [Word16]) {
    let mut fac_new: Word16 = 8192; // 1/4 Q15
    let mut fac_old: Word16 = 24576; // 3/4 Q15

    let mut j = 0;
    while j < 33 {
        let mut lsp = [0i16; P];
        for i in 0..P {
            let mut t0 = l_mult(lsp_old[i], fac_old);
            t0 = l_mac(t0, lsp_new[i], fac_new);
            lsp[i] = extract_h(t0);
        }
        lsp_to_lp(&lsp, &mut a[j..]);
        fac_old = sub(fac_old, 8192);
        fac_new = add(fac_new, 8192);
        j += 11;
    }
    lsp_to_lp(lsp_new, &mut a[33..]);
}

/// Compute the LP residual by filtering `x` through A(z). `x` must carry `P`
/// samples of history before index 0.
pub fn lp_residual(a: &[Word16], x: &[Word16], y: &mut [Word16], lg: usize) {
    // `x[P]` is sample 0; history is x[0..P].
    for i in 0..lg {
        let mut s = load_shifted(x[P + i], 12);
        for j in 1..=P {
            s = l_mac0(s, a[j], x[P + i - j]);
        }
        s = add_shifted(s, 1, 11);
        s = l_shl(s, 4);
        y[i] = extract_h(s);
    }
}

/// Synthesis filter 1/A(z). `mem` holds the `P` filter-memory samples and is
/// updated when `update` is set.
pub fn synthesis_filter(
    a: &[Word16],
    x: &[Word16],
    y: &mut [Word16],
    lg: usize,
    mem: &mut [Word16],
    update: bool,
) {
    let mut tmp = [0i16; 80];
    tmp[..P].copy_from_slice(&mem[..P]);

    for i in 0..lg {
        let mut s = load_shifted(x[i], 12);
        for j in 1..=P {
            s = l_msu0(s, a[j], tmp[P + i - j]);
        }
        s = add_shifted(s, 1, 11);
        tmp[P + i] = extract_h(l_shl(s, 4));
    }

    for i in 0..lg {
        y[i] = tmp[i + P];
    }

    if update {
        for i in 0..P {
            mem[i] = y[lg - P + i];
        }
    }
}

/// Convolve `x` with impulse response `h` (Q12) over `L` samples.
pub fn convolution(x: &[Word16], h: &[Word16], y: &mut [Word16], l: usize) {
    for n in 0..l {
        let mut s = 0;
        for i in 0..=n {
            s = l_mac0(s, x[i], h[n - i]);
        }
        y[n] = store_high(s, 4);
    }
}

/// Backward filtering of `x` by impulse response `h`, with block scaling.
pub fn backward_filter(x: &[Word16], h: &[Word16], y: &mut [Word16], l: usize) {
    let mut y32 = [0i32; 60];
    let mut max = 0;

    for i in 0..l {
        let mut s = 0;
        for j in i..l {
            s = l_mac0(s, x[j], h[j - i]);
        }
        y32[i] = s;
        let s = l_abs(s);
        if l_sub(s, max) > 0 {
            max = s;
        }
    }

    let mut j = norm_l(max);
    if sub(j, 16) > 0 {
        j = 16;
    }
    j = sub(18, j);

    for i in 0..l {
        y[i] = extract_l(l_shr(y32[i], j));
    }
}

/// Energy of the impulse response of 1/A(z) over 60 points (Q20).
pub fn lp_impulse_energy(a: &[Word16]) -> Word32 {
    let mut h = [0i16; LLG];
    h[0] = 1024; // 1.0 in Q10
    let mut mem = [0i16; P];
    let mut hh = h;
    synthesis_filter(a, &h, &mut hh, LLG, &mut mem, false);
    h = hh;

    let mut ener = 0;
    for i in 0..LLG {
        ener = l_mac0(ener, h[i], h[i]);
    }
    ener
}

/// 32-tap fractional interpolation at -1/3; `x[c]` is the reference sample.
pub fn interpolate_down_32(x: &[Word16], c: usize) -> Word16 {
    let mut s = 0;
    for i in 0..32 {
        s = l_mac0(s, x[c + i - 15], INTER32_M1_3[i]);
    }
    s = l_add(s, s);
    round_(s)
}

/// 32-tap fractional interpolation at +1/3; `x[c]` is the reference sample.
pub fn interpolate_up_32(x: &[Word16], c: usize) -> Word16 {
    let mut s = 0;
    for i in 0..32 {
        s = l_mac0(s, x[c + i - 16], INTER32_1_3[i]);
    }
    s = l_add(s, s);
    round_(s)
}

/// Long-term prediction with fractional interpolation, in place on `exc[eb..]`.
///
/// `exc` must provide `T0 + 16` samples of history before index `eb`.
pub fn long_term_predict(exc: &mut [Word16], eb: usize, t0: Word16, frac: Word16, l_subfr: usize) {
    let t0 = t0 as usize;
    if frac == 0 {
        for i in 0..l_subfr {
            exc[eb + i] = exc[eb + i - t0];
        }
    } else if frac == 1 {
        for i in 0..l_subfr {
            exc[eb + i] = interpolate_up_32(exc, eb + i - t0);
        }
    } else if frac == -1 {
        for i in 0..l_subfr {
            exc[eb + i] = interpolate_down_32(exc, eb + i - t0);
        }
    }
}
