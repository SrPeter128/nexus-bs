//! End-to-end bit-exactness check for the encoder.
//!
//! Generates a deterministic, varied 500-frame signal (identical to the one fed
//! to the reference `scoder`), encodes it, and folds the serial bitstream and
//! local synthesis into the same checksum the reference produced. A match proves
//! the Rust encoder is bit-for-bit identical to the ETSI reference encoder.

use tetra_acelp::encoder::{Encoder, params_to_bits};

const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;
const EXPECTED_CHECKSUM: u64 = 12641414162828697598;

fn gen_signal(n: usize) -> Vec<i16> {
    let mut st: u32 = 0x1234;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        st = st.wrapping_mul(1103515245).wrapping_add(12345);
        let p = 60 + ((i / 2400) % 80);
        let phase = i % p;
        let tri = (phase as i32 * 40000) / p as i32 - 20000;
        let noise = ((st >> 15) & 0x1FFF) as i32 - 4096;
        let mut s = (tri >> 1) + noise;
        s = s.clamp(-32768, 32767);
        out.push(s as i16);
    }
    out
}

#[test]
fn encoder_matches_reference() {
    let input = gen_signal(120_000); // 500 frames
    let mut enc = Encoder::new();

    let mut cod: Vec<i16> = Vec::new();
    let mut syn: Vec<i16> = Vec::new();

    for frame in input.chunks_exact(240) {
        let pcm: &[i16; 240] = frame.try_into().unwrap();
        let (ana, synth) = enc.encode_frame(pcm);
        let mut bits = [0i16; 138];
        params_to_bits(&ana, &mut bits);
        cod.extend_from_slice(&bits);
        syn.extend_from_slice(&synth);
    }

    let mut ck = FNV_OFFSET;
    for &v in cod.iter().chain(syn.iter()) {
        ck = ck.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
    }

    assert_eq!(ck, EXPECTED_CHECKSUM, "encoder diverged from reference");
}
