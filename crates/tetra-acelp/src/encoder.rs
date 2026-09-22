//! TETRA speech source encoder (clause 4.2.2).
//!
//! Analysis-by-synthesis ACELP encoder. Each 30 ms frame is processed through
//! the standard encoder stages:
//!
//! 1. Input pre-processing (DC-offset removal and scaling).
//! 2. LP analysis: windowed autocorrelation, Levinson-Durbin, conversion to LSPs.
//! 3. LSP quantisation (split vector quantiser) and per-subframe interpolation.
//! 4. Open-loop then closed-loop (fractional) pitch analysis per subframe.
//! 5. Algebraic (innovative) codebook search.
//! 6. Gain quantisation in the energy domain.
//! 7. Filter-memory update and serial bit packing.
//!
//! Built on the verified [`crate::dsp`] primitives and fixed-point operators.

use crate::dsp::*;
use crate::fixed::dsp_ops::*;
use crate::fixed::ext::*;
use crate::fixed::math::*;
use crate::fixed::ops::*;
use crate::fixed::{MAX_32, Word16, Word32, overflow};
use crate::tables::{DICO1_CLSP, DICO2_CLSP, DICO3_CLSP, INTER8_1_3, INTER8_M1_3, T_QUA_ENER};

// Frame geometry.
const L_FRAME: usize = 240;
const L_NEXT: usize = 40;
const L_SUBFR: usize = 60;
const PP1: usize = P + 1;
const L_TOTAL: usize = L_FRAME + L_NEXT + P; // 290
const PIT_MIN: Word16 = 20;
const PIT_MAX: usize = 143;
const L_INTER: usize = 15;

// Bandwidth-expansion factors (Q15): 0.95, 0.60, 0.75, 0.85.
const GAMMA1: Word16 = 31130;
const GAMMA2: Word16 = 19661;
const GAMMA3: Word16 = 24576;
const GAMMA4: Word16 = 27853;

// Algebraic codebook constants.
const LCODE: usize = 60;
const Q11_GAIN_I0: Word16 = 2896;
const Q13_GAIN_I0: Word16 = 11585;
const Q14_GAIN_I0: Word16 = 23170;
const THRESHOLD1: Word16 = 19200;
const THRESHOLD2: Word16 = 19200;
const MAX_TIME: Word16 = 350;

// phi[][] correlation matrix constants.
const DIM_RR: i32 = 32;
const DIAG: i32 = 33;
const RR_LAST: i32 = 957;

const NB_QUA_ENER: usize = 64;

/// Default / reset LSP vector (cosine domain, Q15).
const LSP_INIT: [Word16; P] = [
    30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
];

/// Input pre-processing filter state (offset compensation + divide by two).
#[derive(Clone)]
struct PreProcess {
    y_hi: Word16,
    y_lo: Word16,
    x0: Word16,
}

impl PreProcess {
    fn new() -> Self {
        Self {
            y_hi: 0,
            y_lo: 0,
            x0: 0,
        }
    }

    fn process(&mut self, signal: &mut [Word16]) {
        for s in signal.iter_mut() {
            let x1 = self.x0;
            self.x0 = *s;
            let mut l = load_shifted(self.x0, 15);
            l = sub_shifted(l, x1, 15);
            l = l_mac(l, self.y_hi, 32735);
            l = add_shifted(l, mult(self.y_lo, 32735), 1);
            *s = extract_h(l);
            self.y_hi = extract_h(l);
            self.y_lo = extract_l(sub_shifted(l_shr(l, 1), self.y_hi, 15));
        }
    }
}

/// Split vector quantisation of the LSPs (split-3 VQ in the cosine domain).
///
/// Writes the quantised LSPs and the three codebook indices; keeps the previous
/// quantised set (`lsp_old_q`) if the new one is not strictly ordered.
fn quantize_lsp(
    lsp: &[Word16],
    lsp_q: &mut [Word16],
    indice: &mut [Word16],
    lsp_old_q: &mut [Word16; P],
) {
    // Sub-codebook 1: lsp[0..3].
    let mut min = MAX_32;
    let mut ind = 0;
    for i in 0..DICO1_CLSP.len() / 3 {
        let mut temp = sub(lsp[0], DICO1_CLSP[i * 3]);
        let mut dist = l_mult0(temp, temp);
        for j in 1..3 {
            temp = sub(lsp[j], DICO1_CLSP[i * 3 + j]);
            dist = l_mac0(dist, temp, temp);
        }
        if l_sub(dist, min) < 0 {
            min = dist;
            ind = i;
        }
    }
    indice[0] = ind as Word16;
    lsp_q[0..3].copy_from_slice(&DICO1_CLSP[ind * 3..ind * 3 + 3]);

    // Sub-codebook 2: lsp[3..6].
    min = MAX_32;
    ind = 0;
    for i in 0..DICO2_CLSP.len() / 3 {
        let mut temp = sub(lsp[3], DICO2_CLSP[i * 3]);
        let mut dist = l_mult0(temp, temp);
        for j in 4..6 {
            temp = sub(lsp[j], DICO2_CLSP[i * 3 + (j - 3)]);
            dist = l_mac0(dist, temp, temp);
        }
        if l_sub(dist, min) < 0 {
            min = dist;
            ind = i;
        }
    }
    indice[1] = ind as Word16;
    lsp_q[3..6].copy_from_slice(&DICO2_CLSP[ind * 3..ind * 3 + 3]);

    // Sub-codebook 3: lsp[6..10].
    min = MAX_32;
    ind = 0;
    for i in 0..DICO3_CLSP.len() / 4 {
        let mut temp = sub(lsp[6], DICO3_CLSP[i * 4]);
        let mut dist = l_mult0(temp, temp);
        for j in 7..10 {
            temp = sub(lsp[j], DICO3_CLSP[i * 4 + (j - 6)]);
            dist = l_mac0(dist, temp, temp);
        }
        if l_sub(dist, min) < 0 {
            min = dist;
            ind = i;
        }
    }
    indice[2] = ind as Word16;
    lsp_q[6..10].copy_from_slice(&DICO3_CLSP[ind * 4..ind * 4 + 4]);

    // Enforce minimum distance across the sub-vector boundaries.
    let mut temp = 917;
    temp = sub(temp, lsp_q[2]);
    temp = add(temp, lsp_q[3]);
    if temp > 0 {
        temp = shr(temp, 1);
        lsp_q[2] = add(lsp_q[2], temp);
        lsp_q[3] = sub(lsp_q[3], temp);
    }
    let mut temp = 1245;
    temp = sub(temp, lsp_q[5]);
    temp = add(temp, lsp_q[6]);
    if temp > 0 {
        temp = shr(temp, 1);
        lsp_q[5] = add(lsp_q[5], temp);
        lsp_q[6] = sub(lsp_q[6], temp);
    }

    // Keep previous set if not strictly decreasing.
    let mut bad = false;
    for i in 0..9 {
        if sub(lsp_q[i], lsp_q[i + 1]) <= 0 {
            bad = true;
        }
    }
    if bad {
        lsp_q[..P].copy_from_slice(&lsp_old_q[..P]);
    } else {
        lsp_old_q[..P].copy_from_slice(&lsp_q[..P]);
    }
}

/// Algebraic (innovative) codebook search.
///
/// `f`/`h` are the (shifted) shaping and combined impulse responses positioned
/// with their origin at index `ORIGIN` (= 64). The impulse-response correlation
/// matrix used by the search is computed internally from `h`. Returns
/// `(index, sign, shift)` and writes the code vector and its filtered version.
fn search_codebook(
    dn: &mut [Word16],
    f: &[Word16],
    h: &[Word16],
    cod: &mut [Word16],
    y: &mut [Word16],
) -> (Word16, Word16, Word16) {
    const ORIGIN: i32 = 64;

    // Correlation matrix phi[i][j] = sum h[n-i] h[n-j] of the combined response,
    // flattened with stride 32.
    let mut phi = [0i16; (DIM_RR * DIM_RR) as usize];
    {
        let hh = &h[ORIGIN as usize..ORIGIN as usize + L_SUBFR];
        let mut s = 0;
        for i in 0..L_SUBFR {
            s = l_mac0(s, hh[i], hh[i]);
        }
        let mut k = norm_l(s);
        k = shr(sub(k, 1), 1);

        let mut hs = [0i16; L_SUBFR];
        for i in 0..L_SUBFR {
            hs[i] = shl(hh[i], k);
        }

        let mut phi_fin_r = RR_LAST;
        let mut phi_fin_c = RR_LAST;
        let mut dec = 0usize;
        while dec < L_SUBFR {
            let mut ph = 0usize;
            let mut phd = dec;
            let mut phi_ij = phi_fin_c;
            let mut phi_ji = phi_fin_r;
            let mut s = 0;
            let mut kk = 0usize;
            while kk < L_SUBFR - dec {
                s = l_mac(s, hs[ph], hs[phd]);
                ph += 1;
                phd += 1;
                s = l_mac(s, hs[ph], hs[phd]);
                ph += 1;
                phd += 1;
                let v = extract_h(s);
                phi[phi_ij as usize] = v;
                phi[phi_ji as usize] = v;
                phi_ij -= DIAG;
                phi_ji -= DIAG;
                kk += 2;
            }
            phi_fin_c -= 1;
            phi_fin_r -= DIM_RR;
            dec += 2;
        }
    }
    let phi_at = |a: i32, b: i32| phi[(a * DIM_RR + b) as usize];

    dn[60] = 0;
    dn[61] = 0;
    dn[62] = 0;
    dn[63] = 0;

    // Extrema over pulse tracks for the search thresholds.
    let (mut min0, mut max0) = (0, 0);
    let mut i = 0;
    while i < 60 {
        let j = add(dn[i], dn[i + 1]);
        if sub(j, max0) > 0 {
            max0 = j;
        } else if sub(j, min0) < 0 {
            min0 = j;
        }
        i += 2;
    }
    max0 = shr(max0, 1);
    min0 = shr(min0, 1);
    max0 = store_high(l_mult0(max0, Q11_GAIN_I0), 5);
    min0 = store_high(l_mult0(min0, Q11_GAIN_I0), 5);

    let (mut min1, mut max1) = (0, 0);
    let mut i = 2;
    while i < 60 {
        let j = add(dn[i], dn[i + 1]);
        if sub(j, max1) > 0 {
            max1 = j;
        } else if sub(j, min1) < 0 {
            min1 = j;
        }
        i += 8;
    }
    max1 = shr(max1, 1);
    min1 = shr(min1, 1);

    let (mut min2, mut max2) = (0, 0);
    let mut i = 4;
    while i < 60 {
        let j = add(dn[i], dn[i + 1]);
        if sub(j, max2) > 0 {
            max2 = j;
        } else if sub(j, min2) < 0 {
            min2 = j;
        }
        i += 8;
    }
    max2 = shr(max2, 1);
    min2 = shr(min2, 1);

    max0 = sub(max0, min1);
    max2 = add(max0, max2);
    max1 = sub(max1, min0);
    let j = sub(max1, min2);
    if sub(max0, max1) > 0 {
        max1 = max0;
    }
    if sub(j, max2) > 0 {
        max2 = j;
    }
    let seuil1 = mult(max1, THRESHOLD1);
    let seuil2 = mult(max2, THRESHOLD2);

    let mut ip0 = 0;
    let mut ip1 = 2;
    let mut ip2 = 4;
    let mut ip3 = 6;
    let mut shift = 0;
    let mut ps = 0;
    let mut psc = 0;
    let mut best_den: Word16 = 255;
    let mut time = MAX_TIME;

    'search: for ii0 in 0..30i32 {
        let ps0 = store_high(l_mult0(Q11_GAIN_I0, dn[2 * ii0 as usize]), 5);
        let ps0a = store_high(l_mult0(Q11_GAIN_I0, dn[2 * ii0 as usize + 1]), 5);
        let den0_32 = load_shifted(phi_at(ii0, ii0), 14);

        let mut ii1 = 1i32;
        while ii1 < 30 {
            let ps1 = sub(ps0, dn[2 * ii1 as usize]);
            let ps1a = sub(ps0a, dn[2 * ii1 as usize + 1]);

            let mut l_tmp = add_shifted(den0_32, phi_at(ii1, ii1), 13);
            l_tmp = l_msu0(l_tmp, Q14_GAIN_I0, phi_at(ii0, ii1));
            let den1 = extract_h(l_tmp);
            let den1_32 = load_shifted(den1, 15);

            l_tmp = load_shifted(ps1, 15);
            l_tmp = l_abs(add_shifted(l_tmp, ps1a, 15));
            l_tmp = sub_high16(l_tmp, seuil1);
            if l_tmp > 0 {
                let mut ii2 = 2i32;
                while ii2 < 31 {
                    let ps2_0 = add(ps1, dn[2 * ii2 as usize]);
                    let ps2a = add(ps1a, dn[2 * ii2 as usize + 1]);

                    let mut l_tmp = add_shifted(den1_32, phi_at(ii2, ii2), 12);
                    l_tmp = l_mac0(l_tmp, Q13_GAIN_I0, phi_at(ii0, ii2));
                    l_tmp = sub_shifted(l_tmp, phi_at(ii1, ii2), 13);
                    let den2 = extract_h(l_tmp);
                    let den2_32 = load_high16(den2);

                    l_tmp = load_shifted(ps2_0, 15);
                    l_tmp = l_abs(add_shifted(l_tmp, ps2a, 15));
                    l_tmp = sub_high16(l_tmp, seuil2);
                    if l_tmp > 0 {
                        let mut shif = 0;
                        let mut ps2 = ps2_0;
                        if sub(abs_s(ps2a), abs_s(ps2)) > 0 {
                            ps2 = ps2a;
                            shif = 1;
                        }
                        let ps2_32 = load_shifted(ps2, 15);

                        let mut ii3 = 3i32;
                        while ii3 < 32 {
                            let l_tmp =
                                sub_shifted(ps2_32, dn[2 * ii3 as usize + shif as usize], 15);
                            let ps3 = extract_h(l_tmp);

                            let mut l_tmp = add_shifted(den2_32, phi_at(ii3, ii3), 12);
                            l_tmp = add_shifted(l_tmp, phi_at(ii1, ii3), 13);
                            l_tmp = l_msu0(l_tmp, Q13_GAIN_I0, phi_at(ii0, ii3));
                            l_tmp = sub_shifted(l_tmp, phi_at(ii2, ii3), 13);
                            let den3 = extract_h(l_tmp);

                            let ps3c = mult(ps3, ps3);
                            let l_tmp = l_mult(ps3c, best_den);
                            if l_msu(l_tmp, psc, den3) > 0 {
                                ps = ps3;
                                psc = ps3c;
                                best_den = den3;
                                ip0 = 2 * ii0;
                                ip1 = 2 * ii1;
                                ip2 = 2 * ii2;
                                ip3 = 2 * ii3;
                                shift = shif;
                            }
                            ii3 += 4;
                        }

                        time = sub(time, 3);
                        if time <= 0 {
                            break 'search;
                        }
                    }
                    ii2 += 4;
                }

                time = sub(time, 4);
                if time <= 0 {
                    break 'search;
                }
            }
            ii1 += 4;
        }
    }

    // Build the code vector: cod[i] = p0*gain - p1 + p2 - p3 (with sign).
    let fo = ORIGIN - shift as i32;
    let (p0, p1, p2, p3) = (fo - ip0, fo - ip1, fo - ip2, fo - ip3);
    let negative = ps < 0;
    for i in 0..LCODE as i32 {
        let mut l = l_mult0(f[(p0 + i) as usize], Q11_GAIN_I0);
        l = sub_shifted(l, f[(p1 + i) as usize], 11);
        l = add_shifted(l, f[(p2 + i) as usize], 11);
        l = sub_shifted(l, f[(p3 + i) as usize], 11);
        if negative {
            l = l_negate(l);
        }
        cod[i as usize] = store_high(l, 5);
    }

    let ho = ORIGIN - shift as i32;
    let (p0, p1, p2, p3) = (ho - ip0, ho - ip1, ho - ip2, ho - ip3);
    for i in 0..LCODE as i32 {
        let mut l = l_mult0(h[(p0 + i) as usize], Q11_GAIN_I0);
        l = sub_shifted(l, h[(p1 + i) as usize], 11);
        l = add_shifted(l, h[(p2 + i) as usize], 11);
        l = sub_shifted(l, h[(p3 + i) as usize], 11);
        if negative {
            l = l_negate(l);
        }
        y[i as usize] = store_high(l, 5);
    }

    let mut index = shr(ip0 as Word16, 1);
    index = add(index, shl(shr(ip1 as Word16, 3), 5));
    index = add(index, shl(shr(ip2 as Word16, 3), 8));
    index = add(index, shl(shr(ip3 as Word16, 3), 11));

    (index, if negative { 1 } else { 0 }, shift)
}

/// Gain of the pitch (adaptive) contribution, Q12, saturated to 1.2.
fn pitch_gain(pitch_target: &[Word16], filt_pitch: &[Word16], l_subfr: usize) -> Word16 {
    overflow::reset();

    let mut s = 1;
    for i in 0..l_subfr {
        s = l_mac0(s, pitch_target[i], filt_pitch[i]);
    }
    let mut exp_xy = norm_l(s);
    let mut xy = extract_h(l_shl(s, exp_xy));

    s = 1;
    for i in 0..l_subfr {
        s = l_mac0(s, filt_pitch[i], filt_pitch[i]);
    }
    let mut exp_yy = norm_l(s);
    let mut yy = extract_h(l_shl(s, exp_yy));

    if overflow::occurred() {
        s = 1;
        for i in 0..l_subfr {
            s = l_add(s, l_shr(l_mult0(pitch_target[i], filt_pitch[i]), 6));
        }
        exp_xy = norm_l(s);
        xy = extract_h(l_shl(s, exp_xy));

        s = 1;
        for i in 0..l_subfr {
            s = l_add(s, l_shr(l_mult0(filt_pitch[i], filt_pitch[i]), 6));
        }
        exp_yy = norm_l(s);
        yy = extract_h(l_shl(s, exp_yy));
    }

    if sub(xy, 4) < 0 {
        return 0;
    }
    xy = shr(xy, 1);
    let mut gain = div_s(xy, yy);
    let mut i = add(exp_xy, 2);
    i = sub(i, exp_yy);
    gain = shr(gain, i);
    if sub(gain, 4915) > 0 {
        gain = 4915;
    }
    gain
}

/// Gain of the innovative code.
fn code_gain(code_target: &[Word16], filt_code: &[Word16], l_subfr: usize) -> Word16 {
    overflow::reset();

    let mut s = 1;
    for i in 0..l_subfr {
        s = l_mac0(s, code_target[i], filt_code[i]);
    }
    let mut exp_xy = norm_l(s);
    let mut xy = extract_h(l_shl(s, exp_xy));

    s = 1;
    for i in 0..l_subfr {
        s = l_mac0(s, filt_code[i], filt_code[i]);
    }
    let mut exp_yy = norm_l(s);
    let mut yy = extract_h(l_shl(s, exp_yy));

    if overflow::occurred() {
        s = 1;
        for i in 0..l_subfr {
            s = l_add(s, l_shr(l_mult0(code_target[i], filt_code[i]), 6));
        }
        exp_xy = norm_l(s);
        xy = extract_h(l_shl(s, exp_xy));

        s = 1;
        for i in 0..l_subfr {
            s = l_add(s, l_shr(l_mult0(filt_code[i], filt_code[i]), 6));
        }
        exp_yy = norm_l(s);
        yy = extract_h(l_shl(s, exp_yy));
    }

    if xy <= 0 {
        return 0;
    }
    xy = shr(xy, 1);
    let gain = div_s(xy, yy);
    let mut i = add(exp_xy, 2);
    i = sub(i, exp_yy);
    shr(gain, i)
}

/// 8-tap interpolation at -1/3; `x` indexed [-4..3].
fn interpolate_down_8(x: &[Word16], c: usize) -> Word32 {
    let mut s = 0;
    for i in 0..8 {
        s = l_mac0(s, x[c + i - 4], INTER8_M1_3[i]);
    }
    s
}

/// 8-tap interpolation at +1/3; `x` indexed [-3..4].
fn interpolate_up_8(x: &[Word16], c: usize) -> Word32 {
    let mut s = 0;
    for i in 0..8 {
        s = l_mac0(s, x[c + i - 3], INTER8_1_3[i]);
    }
    s
}

/// Open-loop pitch lag with signal decimation.
fn open_loop_pitch(signal: &[Word16], sb: usize, l_frame: usize) -> Word16 {
    const OL_PIT_MAX: usize = 142;
    const SEUIL: Word16 = 27856; // 0.85 Q15

    overflow::reset();
    let mut t0 = 0;
    let mut i = sb as isize - OL_PIT_MAX as isize;
    let end = sb as isize + l_frame as isize;
    while i < end {
        t0 = l_mac0(t0, signal[i as usize], signal[i as usize]);
        i += 2;
    }

    let mut sig_dec = [0i16; 120];
    if overflow::occurred() {
        for (j, d) in sig_dec.iter_mut().enumerate().take(l_frame / 2) {
            *d = shr(signal[sb + 2 * j], 6);
        }
    } else if l_sub(t0, l_shl(1, 22)) < 0 {
        for (j, d) in sig_dec.iter_mut().enumerate().take(l_frame / 2) {
            *d = shl(signal[sb + 2 * j], 4);
        }
    } else {
        for (j, d) in sig_dec.iter_mut().enumerate().take(l_frame / 2) {
            *d = signal[sb + 2 * j];
        }
    }

    // Lag with maximum normalised correlation over a decimated lag range.
    let best_lag = |lag_hi: Word16, lag_min: Word16| -> (Word16, Word16) {
        let mut max = crate::fixed::MIN_32;
        let mut p_max = lag_min;

        let mut i = lag_hi;
        while i >= lag_min {
            let mut t0 = 0;
            let mut p = 0usize;
            let mut p1 = sb as isize - i as isize;
            let mut j = 0;
            while j < l_frame {
                t0 = l_mac0(t0, sig_dec[p], signal[p1 as usize]);
                p += 1;
                p1 += 2;
                j += 2;
            }
            if l_sub(t0, max) >= 0 {
                max = t0;
                p_max = i;
            }
            i -= 1;
        }

        max = l_shr(max, 1);
        let (max_h, max_l) = dpf_split(max);

        let mut t0 = 0;
        let mut p = sb as isize - p_max as isize;
        let mut i = 0;
        while i < l_frame {
            t0 = l_mac0(t0, signal[p as usize], signal[p as usize]);
            p += 2;
            i += 2;
        }

        t0 = inv_sqrt(t0);
        let (ener_h, ener_l) = dpf_split(t0);
        let t0 = mul_dpf(max_h, max_l, ener_h, ener_l);
        (p_max, extract_l(t0))
    };

    let (mut p_max1, max1) = best_lag(OL_PIT_MAX as Word16, 80);
    let (p_max2, max2) = best_lag(79, 40);
    let (p_max3, max3) = best_lag(39, 20);

    let mut max1 = max1;
    if sub(mult(max1, SEUIL), max2) < 0 {
        max1 = max2;
        p_max1 = p_max2;
    }
    if sub(mult(max1, SEUIL), max3) < 0 {
        p_max1 = p_max3;
    }
    p_max1
}

/// Closed-loop fractional pitch search (1/3 subsample resolution).
fn closed_loop_pitch(
    exc: &[Word16],
    eb: usize,
    pitch_target: &[Word16],
    h: &[Word16],
    l_subfr: usize,
    t0_min: Word16,
    t0_max: Word16,
    i_subfr: usize,
) -> (Word16, Word16) {
    const LG_INTER: Word16 = 4;
    let t_min = sub(t0_min, LG_INTER);
    let t_max = add(t0_max, LG_INTER);

    // corr indexed so that corr[i] maps to corr_v[i - t_min].
    let mut corr_v = [0i16; 40];
    let cb = (-(t_min as isize)) as usize;

    // Normalised correlation between the target and the filtered excitation,
    // evaluated for every integer delay in t_min..=t_max.
    {
        let mut excf = [0i16; 80];
        let mut k = -(t_min as isize);

        // Filtered excitation for the first delay.
        let start = (eb as isize + k) as usize;
        convolution(&exc[start..], h, &mut excf, l_subfr);

        for i in t_min..=t_max {
            let mut s = 0;
            for j in 0..l_subfr {
                s = l_mac0(s, pitch_target[j], excf[j]);
            }
            s = l_shr(s, 1);
            let (corr_h, corr_l) = dpf_split(s);

            let mut s = 0;
            for j in 0..l_subfr {
                s = l_mac0(s, excf[j], excf[j]);
            }
            s = inv_sqrt(s);
            let (norm_hi, norm_lo) = dpf_split(s);

            let s = mul_dpf(corr_h, corr_l, norm_hi, norm_lo);
            corr_v[(cb as isize + i as isize) as usize] = extract_l(s);

            if i != t_max {
                k -= 1;
                let excn = exc[(eb as isize + k) as usize];
                for j in (1..l_subfr).rev() {
                    let mut s = l_mult0(excn, h[j]);
                    s = add_shifted(s, excf[j - 1], 12);
                    excf[j] = store_high(s, 4);
                }
                excf[0] = excn;
            }
        }
    }
    let corr = |i: Word16| corr_v[(cb as isize + i as isize) as usize];

    let mut max = corr(t0_min);
    let mut lag = t0_min;
    for i in (t0_min + 1)..=t0_max {
        if sub(corr(i), max) >= 0 {
            max = corr(i);
            lag = i;
        }
    }

    if i_subfr == 0 && sub(lag, 84) > 0 {
        return (lag, 0);
    }

    let mut frac = 0;
    let mut l_max = load_shifted(max, 15);

    let ci = |i: Word16| (cb as isize + i as isize) as usize;

    let corr_int = interpolate_up_8(&corr_v, ci(lag));
    if l_sub(corr_int, l_max) >= 0 {
        l_max = corr_int;
        frac = 1;
    }
    let corr_int = interpolate_down_8(&corr_v, ci(lag + 1));
    if l_sub(corr_int, l_max) >= 0 {
        l_max = corr_int;
        frac = 2;
    }
    let corr_int = interpolate_up_8(&corr_v, ci(lag - 1));
    if l_sub(corr_int, l_max) >= 0 {
        l_max = corr_int;
        frac = -2;
    }
    let corr_int = interpolate_down_8(&corr_v, ci(lag));
    if l_sub(corr_int, l_max) >= 0 {
        frac = -1;
    }

    if frac == 2 {
        frac = -1;
        lag = add(lag, 1);
    }
    if frac == -2 {
        frac = 1;
        lag = sub(lag, 1);
    }
    (lag, frac)
}

/// Gain vector quantisation in the energy domain.
///
/// Returns the codebook index and writes the quantised pitch and code gains.
#[allow(clippy::too_many_arguments)]
fn quantize_gains(
    a: &[Word16],
    prd_lt: &[Word16],
    code: &[Word16],
    l_subfr: usize,
    adaptive_gain: &mut Word16,
    innov_gain: &mut Word16,
    last_ener_pit: &mut Word16,
    last_ener_cod: &mut Word16,
) -> Word16 {
    // Energy of the impulse response of 1/A(z).
    let l_tmp = lp_impulse_energy(a);
    let exp_lpc = norm_l(l_tmp);
    let ener_lpc = extract_h(l_shl(l_tmp, exp_lpc));

    // Energy from pitch.
    let mut l_tmp = 1;
    for i in 0..l_subfr {
        l_tmp = l_mac0(l_tmp, prd_lt[i], prd_lt[i]);
    }
    let mut exp_plt = norm_l(l_tmp);
    let ener_plt16 = extract_h(l_shl(l_tmp, exp_plt));

    let mut l_tmp = l_mult0(ener_plt16, ener_lpc);
    exp_plt = add(exp_plt, exp_lpc);
    let (exp, frac) = log2(l_tmp);
    l_tmp = load_high16(exp);
    l_tmp = add_shifted(l_tmp, frac, 1);
    l_tmp = sub_high16(l_tmp, exp_plt);
    l_tmp = add_shifted(l_tmp, 1710, 8);
    l_tmp = l_shr(l_tmp, 8);
    let ener_plt = extract_l(l_tmp);

    // ener_pit = Log2(adaptive_gain^2) + ener_plt
    let mut l_tmp = 1;
    l_tmp = l_mac0(l_tmp, *adaptive_gain, *adaptive_gain);
    let (exp, frac) = log2(l_tmp);
    l_tmp = load_high16(exp);
    l_tmp = add_shifted(l_tmp, frac, 1);
    l_tmp = sub_high16(l_tmp, 24);
    l_tmp = l_shr(l_tmp, 8);
    let mut ener_pit = extract_l(l_tmp);
    ener_pit = add(ener_pit, ener_plt);

    // Energy from code.
    let mut l_tmp = 0;
    for i in 0..l_subfr {
        l_tmp = l_mac0(l_tmp, code[i], code[i]);
    }
    let ener_c16 = extract_h(l_tmp);

    let mut l_tmp = l_mult0(ener_c16, ener_lpc);
    let (exp, frac) = log2(l_tmp);
    l_tmp = load_high16(exp);
    l_tmp = add_shifted(l_tmp, frac, 1);
    l_tmp = sub_high16(l_tmp, exp_lpc);
    l_tmp = sub_shifted(l_tmp, 4434, 8);
    l_tmp = l_shr(l_tmp, 8);
    let ener_c = extract_l(l_tmp);

    let mut l_tmp = 1;
    l_tmp = l_mac0(l_tmp, *innov_gain, *innov_gain);
    let (exp, frac) = log2(l_tmp);
    l_tmp = load_high16(exp);
    l_tmp = add_shifted(l_tmp, frac, 1);
    l_tmp = l_shr(l_tmp, 8);
    let mut ener_cod = extract_l(l_tmp);
    ener_cod = add(ener_cod, ener_c);

    // Predictions.
    let mut l_tmp = load_shifted(*last_ener_pit, 8);
    l_tmp = add_shifted(l_tmp, *last_ener_cod, 7);
    l_tmp = sub_shifted(l_tmp, 768, 9);
    if l_tmp < 0 {
        l_tmp = 0;
    }
    let pred_pit = store_high(l_tmp, 7);
    let err_pit = sub(ener_pit, pred_pit);

    let mut l_tmp = load_shifted(*last_ener_cod, 8);
    l_tmp = add_shifted(l_tmp, *last_ener_pit, 7);
    l_tmp = sub_shifted(l_tmp, 768, 9);
    if l_tmp < 0 {
        l_tmp = 0;
    }
    let pred_cod = store_high(l_tmp, 7);
    let err_cod = sub(ener_cod, pred_cod);

    // Codebook search.
    let mut dist_min = MAX_32;
    let mut index = 0;
    for i in 0..NB_QUA_ENER {
        let mut tmp = sub(T_QUA_ENER[i * 2], err_pit);
        let mut dist = l_mult0(tmp, tmp);
        tmp = sub(T_QUA_ENER[i * 2 + 1], err_cod);
        dist = l_mac0(dist, tmp, tmp);
        if l_sub(dist, dist_min) < 0 {
            dist_min = dist;
            index = i;
        }
    }

    *last_ener_pit = add(T_QUA_ENER[index * 2], pred_pit);
    *last_ener_cod = add(T_QUA_ENER[index * 2 + 1], pred_cod);
    if sub(*last_ener_pit, 6912) > 0 {
        *last_ener_pit = 6912;
    }
    if sub(*last_ener_cod, 6400) > 0 {
        *last_ener_cod = 6400;
    }

    // Quantised gains.
    let mut l_tmp = load_shifted(*last_ener_pit, 6);
    l_tmp = sub_shifted(l_tmp, ener_plt, 6);
    l_tmp = add_shifted(l_tmp, 12, 15);
    let (exp, frac) = dpf_split(l_tmp);
    let mut l_tmp = pow2(exp, frac);
    if l_sub(l_tmp, 4915) > 0 {
        l_tmp = 4915;
    }
    *adaptive_gain = extract_l(l_tmp);

    let mut l_tmp = load_shifted(*last_ener_cod, 6);
    l_tmp = sub_shifted(l_tmp, ener_c, 6);
    let (exp, frac) = dpf_split(l_tmp);
    let l_tmp = pow2(exp, frac);
    *innov_gain = extract_l(l_tmp);

    index as Word16
}

/// The 23-parameter encoder output for one 30 ms frame.
pub type FrameParams = [Word16; 23];

/// Number of bits for each of the 23 parameters (matches the frame layout).
const BITNO: [usize; 23] = [
    8, 9, 9, // split-VQ LSP
    8, 14, 1, 1, 6, // subframe 1
    5, 14, 1, 1, 6, // subframe 2
    5, 14, 1, 1, 6, // subframe 3
    5, 14, 1, 1, 6, // subframe 4
];

/// Pack the 23 parameters into a serial bit stream: `bits[0]` is the BFI flag
/// (0 at the encoder) followed by the 137 source-coded bits (MSB first).
pub fn params_to_bits(prm: &FrameParams, bits: &mut [Word16]) {
    bits[0] = 0;
    let mut pos = 1;
    for (i, &n) in BITNO.iter().enumerate() {
        int_to_bits(prm[i], n, &mut bits[pos..]);
        pos += n;
    }
}

/// TETRA source speech encoder (stateful across frames).
pub struct Encoder {
    pre: PreProcess,
    old_speech: [Word16; L_TOTAL],
    old_wsp: [Word16; L_FRAME + PIT_MAX],
    old_exc: [Word16; L_FRAME + PIT_MAX + L_INTER],
    mem_syn: [Word16; P],
    mem_w0: [Word16; P],
    mem_w: [Word16; P],
    lspold: [Word16; P],
    lspold_q: [Word16; P],
    last_ener_pit: Word16,
    last_ener_cod: Word16,
    f_gamma1: [Word16; P],
    f_gamma2: [Word16; P],
    f_gamma3: [Word16; P],
    f_gamma4: [Word16; P],
    levin_old_a: [Word16; PP1],
    clsp_old: [Word16; P],
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create an encoder with reset state.
    pub fn new() -> Self {
        let mut f_gamma1 = [0i16; P];
        let mut f_gamma2 = [0i16; P];
        let mut f_gamma3 = [0i16; P];
        let mut f_gamma4 = [0i16; P];
        weight_factors(GAMMA1, &mut f_gamma1);
        weight_factors(GAMMA2, &mut f_gamma2);
        weight_factors(GAMMA3, &mut f_gamma3);
        weight_factors(GAMMA4, &mut f_gamma4);
        Self {
            pre: PreProcess::new(),
            old_speech: [0; L_TOTAL],
            old_wsp: [0; L_FRAME + PIT_MAX],
            old_exc: [0; L_FRAME + PIT_MAX + L_INTER],
            mem_syn: [0; P],
            mem_w0: [0; P],
            mem_w: [0; P],
            lspold: LSP_INIT,
            lspold_q: LSP_INIT,
            last_ener_pit: 0,
            last_ener_cod: 0,
            f_gamma1,
            f_gamma2,
            f_gamma3,
            f_gamma4,
            levin_old_a: [4096, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            clsp_old: LSP_INIT,
        }
    }

    /// Encode one 30 ms frame (240 samples). Returns the 23 codec parameters
    /// and the local synthesis (for debugging/verification).
    pub fn encode_frame(&mut self, pcm: &[Word16; L_FRAME]) -> (FrameParams, [Word16; L_FRAME]) {
        // Load new speech into new_speech = old_speech[50..290] and pre-process.
        let new_off = L_TOTAL - L_FRAME; // 50
        self.old_speech[new_off..].copy_from_slice(pcm);
        self.pre.process(&mut self.old_speech[new_off..]);

        let mut ana = [0i16; 23];
        let mut synth = [0i16; L_FRAME];
        self.encode_core(&mut ana, &mut synth);
        postprocess(&mut synth);
        (ana, synth)
    }

    fn encode_core(&mut self, ana: &mut [Word16], synth: &mut [Word16]) {
        // Pointer offsets into the persistent buffers.
        const SPEECH: usize = P; // old_speech[10..] = speech
        const P_WINDOW: usize = L_TOTAL - L_WINDOW; // old_speech[34..]
        const WSP: usize = PIT_MAX; // old_wsp[143..]
        const EXC: usize = PIT_MAX + L_INTER; // old_exc[158..]

        let mut a_t = [0i16; PP1 * 4];
        let mut aq_t = [0i16; PP1 * 4];
        let mut r_h = [0i16; PP1];
        let mut r_l = [0i16; PP1];
        let mut lspnew = [0i16; P];
        let mut lspnew_q = [0i16; P];

        // LP analysis.
        autocorrelation(&self.old_speech[P_WINDOW..], &mut r_h, &mut r_l);
        apply_lag_window(&mut r_h, &mut r_l);
        levinson_durbin(&r_h, &r_l, &mut a_t, &mut self.levin_old_a);
        lp_to_lsp(&a_t, &mut lspnew, &self.lspold);
        quantize_lsp(&lspnew, &mut lspnew_q, &mut ana[0..3], &mut self.clsp_old);

        interpolate_lp(&self.lspold, &lspnew, &mut a_t);
        interpolate_lp(&self.lspold_q, &lspnew_q, &mut aq_t);
        self.lspold = lspnew;
        self.lspold_q = lspnew_q;

        // Weighted input speech and open-loop pitch.
        let mut ap1 = [0i16; PP1];
        let mut ap2 = [0i16; PP1];
        let mut i = 0;
        while i < L_FRAME {
            let a = &a_t[(i / L_SUBFR) * PP1..];
            weight_lp(a, &self.f_gamma1, &mut ap1);
            weight_lp(a, &self.f_gamma2, &mut ap2);
            lp_residual(
                &ap1,
                &self.old_speech[SPEECH + i - P..],
                &mut self.old_wsp[WSP + i..],
                L_SUBFR,
            );
            let mut xin = [0i16; L_SUBFR];
            xin.copy_from_slice(&self.old_wsp[WSP + i..WSP + i + L_SUBFR]);
            synthesis_filter(
                &ap2,
                &xin,
                &mut self.old_wsp[WSP + i..],
                L_SUBFR,
                &mut self.mem_w,
                true,
            );
            i += L_SUBFR;
        }

        let mut t0 = open_loop_pitch(&self.old_wsp, WSP, L_FRAME);
        let mut t0_min = sub(t0, 2);
        if t0_min < PIT_MIN {
            t0_min = PIT_MIN;
        }
        let mut t0_max = add(t0_min, 4);
        if t0_max > PIT_MAX as Word16 {
            t0_max = PIT_MAX as Word16;
            t0_min = sub(t0_max, 4);
        }

        // Subframe loop.
        let mut ana_i = 3;
        let mut ap3 = [0i16; PP1];
        let mut ap4 = [0i16; PP1];
        let mut impulse_in = [0i16; L_SUBFR + PP1];
        let mut zero_f = [0i16; L_SUBFR + 64];
        let mut zero_h2 = [0i16; L_SUBFR + 64];

        let mut i_subfr = 0;
        while i_subfr < L_FRAME {
            let base = (i_subfr / L_SUBFR) * PP1;
            let aq = &aq_t[base..base + PP1];
            weight_lp(aq, &self.f_gamma3, &mut ap3);
            weight_lp(aq, &self.f_gamma4, &mut ap4);

            // Impulse response h1 of the weighted synthesis filter.
            impulse_in[0] = 4096;
            for k in 1..=P {
                impulse_in[k] = 0;
            }
            let mut h1 = [0i16; L_SUBFR];
            let mut zero_mem = [0i16; P];
            synthesis_filter(&ap4, &impulse_in, &mut h1, L_SUBFR, &mut zero_mem, false);

            // LP residual -> exc.
            let mut res = [0i16; L_SUBFR];
            lp_residual(
                aq,
                &self.old_speech[SPEECH + i_subfr - P..],
                &mut res,
                L_SUBFR,
            );
            for k in 0..L_SUBFR {
                self.old_exc[EXC + i_subfr + k] = res[k];
            }

            // Target for pitch search.
            let mut pitch_target = [0i16; L_SUBFR];
            synthesis_filter(
                &ap4,
                &res,
                &mut pitch_target,
                L_SUBFR,
                &mut self.mem_w0,
                false,
            );

            // Closed-loop pitch.
            let (t0f, t0_frac) = closed_loop_pitch(
                &self.old_exc,
                EXC + i_subfr,
                &pitch_target,
                &h1,
                L_SUBFR,
                t0_min,
                t0_max,
                i_subfr,
            );
            t0 = t0f;

            let index;
            if i_subfr == 0 {
                if t0 <= 85 {
                    let mut idx = add(t0, add(t0, t0));
                    idx = sub(idx, 58);
                    idx = add(idx, t0_frac);
                    index = idx;
                } else {
                    index = add(t0, 112);
                }
                t0_min = sub(t0, 5);
                if t0_min < PIT_MIN {
                    t0_min = PIT_MIN;
                }
                t0_max = add(t0_min, 9);
                if t0_max > PIT_MAX as Word16 {
                    t0_max = PIT_MAX as Word16;
                    t0_min = sub(t0_max, 9);
                }
            } else {
                let ii = sub(t0, t0_min);
                let mut idx = add(ii, add(ii, ii));
                idx = add(idx, 2);
                idx = add(idx, t0_frac);
                index = idx;
            }
            ana[ana_i] = index;
            ana_i += 1;

            // Adaptive codebook vector and filtered version.
            long_term_predict(&mut self.old_exc, EXC + i_subfr, t0, t0_frac, L_SUBFR);
            let mut filt_pitch = [0i16; L_SUBFR];
            let mut zero_mem = [0i16; P];
            synthesis_filter(
                &ap4,
                &self.old_exc[EXC + i_subfr..],
                &mut filt_pitch,
                L_SUBFR,
                &mut zero_mem,
                false,
            );

            let mut adaptive_gain = pitch_gain(&pitch_target, &filt_pitch, L_SUBFR);

            let mut code_target = [0i16; L_SUBFR];
            for k in 0..L_SUBFR {
                let mut l = l_mult(filt_pitch[k], adaptive_gain);
                l = l_shl(l, 3);
                l = l_sub(load_high16(pitch_target[k]), l);
                code_target[k] = extract_h(l);
            }

            // Shaping filter response F[] and combined response h2[].
            for k in 0..=P {
                impulse_in[k] = ap3[k];
            }
            let f = &mut zero_f;
            let mut zero_mem = [0i16; P];
            {
                let mut ftmp = [0i16; L_SUBFR];
                synthesis_filter(&ap4, &impulse_in, &mut ftmp, L_SUBFR, &mut zero_mem, false);
                f[64..64 + L_SUBFR].copy_from_slice(&ftmp);
            }
            // Fixed-gain pitch contribution (0.8) to F[].
            for k in t0 as usize..L_SUBFR {
                let temp = mult(f[64 + k - t0 as usize], 26216);
                f[64 + k] = add(f[64 + k], temp);
            }

            let h2 = &mut zero_h2;
            let mut zero_mem = [0i16; P];
            {
                let fin = f[64..64 + L_SUBFR].to_vec();
                let mut h2tmp = [0i16; L_SUBFR];
                synthesis_filter(&ap4, &fin, &mut h2tmp, L_SUBFR, &mut zero_mem, false);
                h2[64..64 + L_SUBFR].copy_from_slice(&h2tmp);
            }

            // Backward-filtered target and codebook search.
            let mut dn = [0i16; L_SUBFR + 4];
            backward_filter(&code_target, &h2[64..64 + L_SUBFR], &mut dn, L_SUBFR);

            let mut code = [0i16; L_SUBFR + 4];
            let mut filt_code = [0i16; L_SUBFR];
            let (code_index, sign_code, shift_code) =
                search_codebook(&mut dn, f, h2, &mut code, &mut filt_code);
            ana[ana_i] = code_index;
            ana_i += 1;
            ana[ana_i] = sign_code;
            ana_i += 1;
            ana[ana_i] = shift_code;
            ana_i += 1;

            let mut innov_gain = code_gain(&code_target, &filt_code, L_SUBFR);

            // Gain VQ.
            ana[ana_i] = quantize_gains(
                aq,
                &self.old_exc[EXC + i_subfr..],
                &code,
                L_SUBFR,
                &mut adaptive_gain,
                &mut innov_gain,
                &mut self.last_ener_pit,
                &mut self.last_ener_cod,
            );
            ana_i += 1;

            // Total excitation and filter-memory update.
            for k in 0..L_SUBFR {
                let mut l = l_mult0(self.old_exc[EXC + i_subfr + k], adaptive_gain);
                l = l_mac0(l, code[k], innov_gain);
                self.old_exc[EXC + i_subfr + k] = extract_l(l_shr_r(l, 12));
            }
            for k in 0..L_SUBFR {
                res[k] = sub(res[k], self.old_exc[EXC + i_subfr + k]);
            }
            let mut tmp_code = [0i16; L_SUBFR];
            synthesis_filter(&ap4, &res, &mut tmp_code, L_SUBFR, &mut self.mem_w0, true);

            // Local synthesis.
            synthesis_filter(
                aq,
                &self.old_exc[EXC + i_subfr..],
                &mut synth[i_subfr..],
                L_SUBFR,
                &mut self.mem_syn,
                true,
            );

            i_subfr += L_SUBFR;
        }

        // Shift history for the next frame.
        self.old_speech.copy_within(L_FRAME.., 0);
        self.old_wsp.copy_within(L_FRAME..L_FRAME + PIT_MAX, 0);
        self.old_exc
            .copy_within(L_FRAME..L_FRAME + PIT_MAX + L_INTER, 0);
    }
}
