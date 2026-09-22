//! Bit-exactness check for the decoder's bad-frame (BFI) error concealment.
//!
//! Encodes the same deterministic 500-frame signal as the clean-channel decoder
//! test, but marks a deterministic subset of frames as bad — scattered single
//! losses plus a run of consecutive losses (frames 40..=43) to exercise the
//! concealment energy decay. The decoded PCM is folded into a checksum whose
//! expected value was produced by the reference `sdecoder` on the identical
//! bitstream (same BFI flags), so a match proves the Rust concealment path is
//! bit-for-bit identical to the ETSI reference decoder.

use tetra_acelp::decoder::{Decoder, bits_to_params};
use tetra_acelp::encoder::{Encoder, params_to_bits};

const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;
const EXPECTED_DEC_BFI_CHECKSUM: u64 = 568732072569264857;

fn gen_signal(n: usize) -> Vec<i16> {
    let mut st: u32 = 0x1234;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        st = st.wrapping_mul(1103515245).wrapping_add(12345);
        let p = 60 + ((i / 2400) % 80);
        let phase = i % p;
        let tri = (phase as i32 * 40000) / p as i32 - 20000;
        let noise = ((st >> 15) & 0x1FFF) as i32 - 4096;
        out.push((tri >> 1).saturating_add(noise).clamp(-32768, 32767) as i16);
    }
    out
}

// Bad-frame pattern (0-based frame index): scattered single losses plus a run of
// consecutive losses (40..=43) to exercise concealment energy decay.
fn is_bad(frame: usize) -> bool {
    (frame % 11 == 5) || (40..=43).contains(&frame)
}

#[test]
fn decoder_bfi_matches_reference() {
    let input = gen_signal(120_000);
    let mut enc = Encoder::new();
    let mut dec = Decoder::new();

    let mut ck = FNV_OFFSET;
    for (fi, frame) in input.chunks_exact(240).enumerate() {
        let pcm: &[i16; 240] = frame.try_into().unwrap();
        let (ana, _synth) = enc.encode_frame(pcm);

        let mut bits = [0i16; 138];
        params_to_bits(&ana, &mut bits);
        bits[0] = is_bad(fi) as i16; // BFI flag
        let parm = bits_to_params(&bits);
        let dec_synth = dec.decode_frame(&parm);

        for &v in &dec_synth {
            ck = ck.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
        }
    }

    assert_eq!(
        ck, EXPECTED_DEC_BFI_CHECKSUM,
        "decoder BFI concealment diverged from reference"
    );
}
