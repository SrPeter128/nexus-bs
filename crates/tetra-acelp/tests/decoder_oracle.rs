//! End-to-end bit-exactness check for the decoder.
//!
//! Generates the same deterministic 500-frame signal used for the encoder test,
//! encodes it with the (reference-verified) encoder, decodes the bitstream, and
//! folds the decoded PCM into a checksum. The expected value is produced by the
//! reference `sdecoder` on the same bitstream, so a match proves the Rust
//! decoder is bit-for-bit identical to the ETSI reference decoder. A second
//! assertion checks the round-trip invariant: decoded speech equals the
//! encoder's own local synthesis on a clean channel.

use tetra_acelp::decoder::{Decoder, bits_to_params};
use tetra_acelp::encoder::{Encoder, params_to_bits};

const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;
const EXPECTED_DEC_CHECKSUM: u64 = 2774893027836146597;

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

#[test]
fn decoder_matches_reference() {
    let input = gen_signal(120_000);
    let mut enc = Encoder::new();
    let mut dec = Decoder::new();

    let mut ck = FNV_OFFSET;
    for frame in input.chunks_exact(240) {
        let pcm: &[i16; 240] = frame.try_into().unwrap();
        let (ana, enc_synth) = enc.encode_frame(pcm);

        let mut bits = [0i16; 138];
        params_to_bits(&ana, &mut bits);
        let parm = bits_to_params(&bits);
        let dec_synth = dec.decode_frame(&parm);

        // Round-trip invariant: clean-channel decode == encoder local synthesis.
        assert_eq!(dec_synth, enc_synth, "round-trip synthesis mismatch");

        for &v in &dec_synth {
            ck = ck.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
        }
    }

    assert_eq!(ck, EXPECTED_DEC_CHECKSUM, "decoder diverged from reference");
}
