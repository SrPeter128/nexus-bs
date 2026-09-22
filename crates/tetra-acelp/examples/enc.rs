//! Reference-compatible encoder CLI for differential testing.
//!
//! Reads raw 16-bit PCM (240 samples/frame), writes a serial bitstream file
//! (138 int16 per frame: BFI + 137 bits) and a local-synthesis PCM file,
//! matching the layout produced by the ETSI reference `scoder`.

use std::env;
use std::fs;
use tetra_acelp::encoder::{Encoder, params_to_bits};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: enc <speech.in> <serial.out> <synth.out>");
        std::process::exit(1);
    }

    let raw = fs::read(&args[1]).expect("read input");
    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    let mut enc = Encoder::new();
    let mut serial_out: Vec<u8> = Vec::new();
    let mut synth_out: Vec<u8> = Vec::new();

    for frame in samples.chunks_exact(240) {
        let pcm: &[i16; 240] = frame.try_into().unwrap();
        let (ana, synth) = enc.encode_frame(pcm);

        let mut bits = [0i16; 138];
        params_to_bits(&ana, &mut bits);
        for b in bits {
            serial_out.extend_from_slice(&b.to_le_bytes());
        }
        for s in synth {
            synth_out.extend_from_slice(&s.to_le_bytes());
        }
    }

    fs::write(&args[2], serial_out).expect("write serial");
    fs::write(&args[3], synth_out).expect("write synth");
}
