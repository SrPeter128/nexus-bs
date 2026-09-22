//! Reference-compatible decoder CLI for differential testing.
//!
//! Reads a serial bitstream file (138 int16 per frame: BFI + 137 bits) and
//! writes decoded 16-bit PCM, matching the ETSI reference `sdecoder`.

use std::env;
use std::fs;
use tetra_acelp::decoder::{Decoder, bits_to_params};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: dec <serial.in> <synth.out>");
        std::process::exit(1);
    }

    let raw = fs::read(&args[1]).expect("read input");
    let words: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    let mut dec = Decoder::new();
    let mut out: Vec<u8> = Vec::new();

    for frame in words.chunks_exact(138) {
        let parm = bits_to_params(frame);
        let synth = dec.decode_frame(&parm);
        for s in synth {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }

    fs::write(&args[2], out).expect("write synth");
}
