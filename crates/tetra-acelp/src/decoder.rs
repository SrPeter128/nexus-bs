//! TETRA speech source decoder (clause 4.2.3).
//!
//! Reconstructs 30 ms of speech from the 137-bit frame through the standard
//! decoder stages:
//!
//! 1. LSP decoding and interpolation to per-subframe LP filters.
//! 2. Adaptive (pitch) codebook reconstruction with fractional interpolation.
//! 3. Innovative (algebraic) codebook reconstruction.
//! 4. Gain decoding.
//! 5. Excitation formation and synthesis filtering.
//! 6. Post-processing, with error concealment on bad frames.

use crate::dsp::*;
use crate::fixed::Word16;
use crate::fixed::dsp_ops::*;
use crate::fixed::ext::dpf_split;
use crate::fixed::math::*;
use crate::fixed::ops::*;
use crate::tables::{DICO1_CLSP, DICO2_CLSP, DICO3_CLSP, T_QUA_ENER};

const L_FRAME: usize = 240;
const L_SUBFR: usize = 60;
const PP1: usize = P + 1;
const PIT_MIN: Word16 = 20;
const PIT_MAX: usize = 143;
const L_INTER: usize = 15;

const GAMMA3: Word16 = 24576;
const GAMMA4: Word16 = 27853;

const LCODE: usize = 60;
const Q11_GAIN_I0: Word16 = 2896;

const LSP_INIT: [Word16; P] = [
    30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
];

const BITNO: [usize; 23] = [
    8, 9, 9, 8, 14, 1, 1, 6, 5, 14, 1, 1, 6, 5, 14, 1, 1, 6, 5, 14, 1, 1, 6,
];

/// The 24-value decoder input for one frame: `[BFI, 23 codec parameters]`.
pub type FrameParams = [Word16; 24];

/// Unpack a serial bit stream (`bits[0]` = BFI, then 137 bits, MSB first) into
/// the BFI flag plus 23 codec parameters.
pub fn bits_to_params(bits: &[Word16]) -> FrameParams {
    let mut prm = [0i16; 24];
    prm[0] = bits[0];
    let mut pos = 1;
    for (i, &n) in BITNO.iter().enumerate() {
        prm[i + 1] = bits_to_int(n, &bits[pos..]);
        pos += n;
    }
    prm
}

/// Decode the split-VQ LSPs from their indices, keeping the previous set if the
/// decoded LSPs are not strictly ordered.
fn decode_lsp(indice: &[Word16], lsp: &mut [Word16], lsp_old: &[Word16]) {
    let i0 = indice[0] as usize;
    let i1 = indice[1] as usize;
    let i2 = indice[2] as usize;
    debug_assert!(
        i0 * 3 + 3 <= DICO1_CLSP.len()
            && i1 * 3 + 3 <= DICO2_CLSP.len()
            && i2 * 4 + 4 <= DICO3_CLSP.len(),
        "LSP codebook index out of range"
    );
    lsp[0..3].copy_from_slice(&DICO1_CLSP[i0 * 3..i0 * 3 + 3]);
    lsp[3..6].copy_from_slice(&DICO2_CLSP[i1 * 3..i1 * 3 + 3]);
    lsp[6..10].copy_from_slice(&DICO3_CLSP[i2 * 4..i2 * 4 + 4]);

    let mut temp = 917;
    temp = sub(temp, lsp[2]);
    temp = add(temp, lsp[3]);
    if temp > 0 {
        temp = shr(temp, 1);
        lsp[2] = add(lsp[2], temp);
        lsp[3] = sub(lsp[3], temp);
    }
    let mut temp = 1245;
    temp = sub(temp, lsp[5]);
    temp = add(temp, lsp[6]);
    if temp > 0 {
        temp = shr(temp, 1);
        lsp[5] = add(lsp[5], temp);
        lsp[6] = sub(lsp[6], temp);
    }

    let mut bad = false;
    for i in 0..9 {
        if sub(lsp[i], lsp[i + 1]) <= 0 {
            bad = true;
        }
    }
    if bad {
        lsp[..P].copy_from_slice(&lsp_old[..P]);
    }
}

/// Decode the innovative code vector from its index/sign/shift by convolving the
/// four pulses with the shaping response `f` (origin at index 64).
fn decode_codebook(index: Word16, sign: Word16, shift: Word16, f: &[Word16], cod: &mut [Word16]) {
    let pos0 = shl(index & 31, 1) as i32;
    let pos1 = (shr(index & 224, 2) + 2) as i32;
    let pos2 = (shr(index & 1792, 5) + 4) as i32;
    let pos3 = (shr(index & 14336, 8) + 6) as i32;

    let fo = 64 - shift as i32;
    let (p0, p1, p2, p3) = (fo - pos0, fo - pos1, fo - pos2, fo - pos3);
    let negative = sign != 0;
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
}

/// Decode the pitch and innovative codebook gains from the energy VQ index,
/// applying error concealment when `bfi` is set.
#[allow(clippy::too_many_arguments)]
fn decode_gains(
    index: Word16,
    bfi: Word16,
    a: &[Word16],
    prd_lt: &[Word16],
    code: &[Word16],
    l_subfr: usize,
    adaptive_gain: &mut Word16,
    innov_gain: &mut Word16,
    last_ener_pit: &mut Word16,
    last_ener_cod: &mut Word16,
) {
    let l_tmp = lp_impulse_energy(a);
    let exp_lpc = norm_l(l_tmp);
    let ener_lpc = extract_h(l_shl(l_tmp, exp_lpc));

    // Energy of adaptive codebook.
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

    if bfi != 0 {
        *last_ener_pit = sub(*last_ener_pit, 128);
        if *last_ener_pit < 0 {
            *last_ener_pit = 0;
        }
        *last_ener_cod = sub(*last_ener_cod, 128);
        if *last_ener_cod < 0 {
            *last_ener_cod = 0;
        }
    } else {
        let mut l_tmp = load_shifted(*last_ener_pit, 8);
        l_tmp = add_shifted(l_tmp, *last_ener_cod, 7);
        l_tmp = sub_shifted(l_tmp, 768, 9);
        if l_tmp < 0 {
            l_tmp = 0;
        }
        let pred_pit = store_high(l_tmp, 7);

        let mut l_tmp = load_shifted(*last_ener_cod, 8);
        l_tmp = add_shifted(l_tmp, *last_ener_pit, 7);
        l_tmp = sub_shifted(l_tmp, 768, 9);
        if l_tmp < 0 {
            l_tmp = 0;
        }
        let pred_cod = store_high(l_tmp, 7);

        let j = shl(index, 1) as usize;
        debug_assert!(j + 1 < T_QUA_ENER.len(), "gain energy index out of range");
        *last_ener_pit = add(T_QUA_ENER[j], pred_pit);
        *last_ener_cod = add(T_QUA_ENER[j + 1], pred_cod);
        if sub(*last_ener_pit, 6912) > 0 {
            *last_ener_pit = 6912;
        }
        if sub(*last_ener_cod, 6400) > 0 {
            *last_ener_cod = 6400;
        }
    }

    // Quantised pitch gain.
    let mut l_tmp = load_shifted(*last_ener_pit, 6);
    l_tmp = sub_shifted(l_tmp, ener_plt, 6);
    l_tmp = add_shifted(l_tmp, 12, 15);
    let (exp, frac) = dpf_split(l_tmp);
    let mut l_tmp = pow2(exp, frac);
    if l_sub(l_tmp, 4915) > 0 {
        l_tmp = 4915;
    }
    *adaptive_gain = extract_l(l_tmp);

    // Quantised code gain.
    let mut l_tmp = load_shifted(*last_ener_cod, 6);
    l_tmp = sub_shifted(l_tmp, ener_c, 6);
    let (exp, frac) = dpf_split(l_tmp);
    let l_tmp = pow2(exp, frac);
    *innov_gain = extract_l(l_tmp);
}

/// TETRA source speech decoder (stateful across frames).
pub struct Decoder {
    old_exc: [Word16; L_FRAME + PIT_MAX + L_INTER],
    mem_syn: [Word16; P],
    lspold: [Word16; P],
    lspnew: [Word16; P],
    old_parm: [Word16; 23],
    old_t0: Word16,
    last_ener_pit: Word16,
    last_ener_cod: Word16,
    f_gamma3: [Word16; P],
    f_gamma4: [Word16; P],
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// Create a decoder with reset state.
    pub fn new() -> Self {
        let mut f_gamma3 = [0i16; P];
        let mut f_gamma4 = [0i16; P];
        weight_factors(GAMMA3, &mut f_gamma3);
        weight_factors(GAMMA4, &mut f_gamma4);
        Self {
            old_exc: [0; L_FRAME + PIT_MAX + L_INTER],
            mem_syn: [0; P],
            lspold: LSP_INIT,
            lspnew: [0; P],
            old_parm: [0; 23],
            old_t0: 60,
            last_ener_pit: 0,
            last_ener_cod: 0,
            f_gamma3,
            f_gamma4,
        }
    }

    /// Decode one frame from `[BFI, 23 params]`, returning 240 PCM samples.
    pub fn decode_frame(&mut self, parm: &FrameParams) -> [Word16; L_FRAME] {
        let mut synth = [0i16; L_FRAME];
        self.decode_core(parm, &mut synth);
        postprocess(&mut synth);
        synth
    }

    fn decode_core(&mut self, parm_in: &FrameParams, synth: &mut [Word16]) {
        const EXC: usize = PIT_MAX + L_INTER; // 158

        let bfi = parm_in[0];
        // Working copy of the 23 parameters (may be replaced by old_parm on BFI).
        let mut parm = [0i16; 23];
        parm.copy_from_slice(&parm_in[1..24]);

        if bfi == 0 {
            let mut lspnew = self.lspnew;
            decode_lsp(&parm[0..3], &mut lspnew, &self.lspold);
            self.lspnew = lspnew;
            self.old_parm.copy_from_slice(&parm);
        } else {
            for i in 1..P {
                self.lspnew[i] = self.lspold[i];
            }
            parm.copy_from_slice(&self.old_parm);
        }

        let mut a_t = [0i16; PP1 * 4];
        interpolate_lp(&self.lspold, &self.lspnew, &mut a_t);
        self.lspold = self.lspnew;

        let mut zero_f = [0i16; L_SUBFR + 64];
        let mut ap3 = [0i16; PP1];
        let mut ap4 = [0i16; PP1];
        let mut t0 = 0;
        let mut t0_min = 0;

        // parm layout: [0..3] LSP, then per subframe [pitch, code, sign, shift, gain].
        let mut pi = 3;
        let mut i_subfr = 0;
        while i_subfr < L_FRAME {
            let a = &a_t[(i_subfr / L_SUBFR) * PP1..(i_subfr / L_SUBFR) * PP1 + PP1];

            let index = parm[pi];
            pi += 1;
            let mut t0_frac = 0;

            if i_subfr == 0 {
                if bfi == 0 {
                    if index < 197 {
                        let mut i = add(index, 2);
                        i = mult(i, 10923);
                        t0 = add(i, 19);
                        let mut i = add(t0, add(t0, t0));
                        i = sub(58, i);
                        t0_frac = add(index, i);
                    } else {
                        t0 = sub(index, 112);
                        t0_frac = 0;
                    }
                } else {
                    t0 = self.old_t0;
                    t0_frac = 0;
                }
                t0_min = sub(t0, 5);
                if t0_min < PIT_MIN {
                    t0_min = PIT_MIN;
                }
                let mut t0_max = add(t0_min, 9);
                if t0_max > PIT_MAX as Word16 {
                    t0_max = PIT_MAX as Word16;
                    t0_min = sub(t0_max, 9);
                }
            } else if bfi == 0 {
                let mut i = add(index, 2);
                i = mult(i, 10923);
                i = sub(i, 1);
                t0 = add(t0_min, i);
                i = add(i, add(i, i));
                t0_frac = sub(index, add(i, 2));
            }

            // Adaptive codebook vector.
            long_term_predict(&mut self.old_exc, EXC + i_subfr, t0, t0_frac, L_SUBFR);

            // Shaping filter response F[].
            weight_lp(a, &self.f_gamma3, &mut ap3);
            weight_lp(a, &self.f_gamma4, &mut ap4);
            for k in 0..=P {
                zero_f[64 + k] = ap3[k];
            }
            for k in PP1..L_SUBFR {
                zero_f[64 + k] = 0;
            }
            {
                let fin: [i16; L_SUBFR] = zero_f[64..64 + L_SUBFR].try_into().unwrap();
                let mut fout = [0i16; L_SUBFR];
                let mut zero_mem = [0i16; P];
                synthesis_filter(&ap4, &fin, &mut fout, L_SUBFR, &mut zero_mem, false);
                zero_f[64..64 + L_SUBFR].copy_from_slice(&fout);
            }
            for k in t0 as usize..L_SUBFR {
                let temp = mult(zero_f[64 + k - t0 as usize], 26216);
                zero_f[64 + k] = add(zero_f[64 + k], temp);
            }

            let index = parm[pi];
            pi += 1;
            let sign_code = parm[pi];
            pi += 1;
            let shift_code = parm[pi];
            pi += 1;

            let mut code = [0i16; L_SUBFR + 4];
            decode_codebook(index, sign_code, shift_code, &zero_f, &mut code);

            let index = parm[pi];
            pi += 1;
            let mut adaptive_gain = 0;
            let mut innov_gain = 0;
            decode_gains(
                index,
                bfi,
                a,
                &self.old_exc[EXC + i_subfr..],
                &code,
                L_SUBFR,
                &mut adaptive_gain,
                &mut innov_gain,
                &mut self.last_ener_pit,
                &mut self.last_ener_cod,
            );

            // Total excitation and synthesis.
            for k in 0..L_SUBFR {
                let mut l = l_mult0(self.old_exc[EXC + i_subfr + k], adaptive_gain);
                l = l_mac0(l, code[k], innov_gain);
                self.old_exc[EXC + i_subfr + k] = extract_l(l_shr_r(l, 12));
            }
            synthesis_filter(
                a,
                &self.old_exc[EXC + i_subfr..],
                &mut synth[i_subfr..],
                L_SUBFR,
                &mut self.mem_syn,
                true,
            );

            i_subfr += L_SUBFR;
        }

        self.old_exc
            .copy_within(L_FRAME..L_FRAME + PIT_MAX + L_INTER, 0);
        self.old_t0 = t0;
    }
}
