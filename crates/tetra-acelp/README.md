# tetra-acelp

A Rust implementation of the TETRA full-rate speech codec from ETSI EN 300 395-2
(clause 4). This is the ACELP encoder and decoder that runs on the TETRA speech
traffic channel.

What you get:

- 8 kHz, 16-bit PCM in 30 ms frames of 240 samples, coded down to 137 bits per
  frame (4 567 bit/s) and back again.
- Both the encoder and the decoder, including bad-frame error concealment.
- Bit-exact output, so it interoperates with conformant TETRA equipment.
- Plain, safe Rust with no runtime dependencies.

## First-time setup: fetch the codec data

The codec needs a set of numeric tables: the LSP and gain codebooks, the
analysis windows, and the interpolation filters. Those tables belong to the ETSI
reference and are not shipped with this crate. ETSI publishes the reference for
free, so a small helper downloads that archive and generates the table module
locally. It writes `src/tables.rs`, which is git-ignored:

```sh
cargo run -p populate
```

If you already have the archive (`en_30039502v010301p0.zip`), point the tool at
it instead:

```sh
cargo run -p populate -- path/to/en_30039502v010301p0.zip
```

The crate will not build until you have done this once, and `build.rs` will tell
you the same thing if you forget. After that, build and test as normal:

```sh
cargo build
cargo test
```

## Usage

```rust
use tetra_acelp::{Encoder, Decoder, FrameQuality, FRAME_SAMPLES};

let mut encoder = Encoder::new();
let mut decoder = Decoder::new();

// One 30 ms frame of 8 kHz, 16-bit PCM.
let pcm = [0i16; FRAME_SAMPLES];

let frame = encoder.encode(&pcm);        // a SpeechFrame of 137 bits
let out   = decoder.decode(&frame, FrameQuality::good());
assert_eq!(out.len(), FRAME_SAMPLES);
```

The encoder and decoder both keep state between frames, so make one of each per
stream and feed it 240 samples at a time:

```rust
use tetra_acelp::{Encoder, SpeechFrame, FRAME_SAMPLES};

let mut encoder = Encoder::new();
let samples: Vec<i16> = load_pcm(); // your 8 kHz, mono, 16-bit PCM

for chunk in samples.chunks_exact(FRAME_SAMPLES) {
    let pcm: [i16; FRAME_SAMPLES] = chunk.try_into().unwrap();
    let frame: SpeechFrame = encoder.encode(&pcm);
    let bytes = frame.to_bytes();            // [u8; 18], ready to store or send
    // on the far end: SpeechFrame::from_bytes(bytes)
}
```

### API overview

`SpeechFrame` is one coded frame: exactly 137 ordered bits, stored packed. It
converts to and from bits (`to_bits` / `from_bits`) and packed bytes
(`to_bytes` / `from_bytes`), it hands you the decoded parameters through
`parameters()`, and `SpeechFrame::tch_s_block(a, b)` joins two frames into the
274-bit TETRA TCH/S speech block.

`FrameQuality` carries the Bad Frame Indicator and stays separate from the coded
bits. Pass `FrameQuality::good()` for a clean frame, or `FrameQuality::bad()` to
have the decoder conceal a lost or corrupt one.

`Encoder` and `Decoder` are the stateful front ends. Use one of each per audio
stream.

If you need them, the lower-level pieces (`fixed`, `dsp`, `encoder`, `decoder`,
`tables`) are public too.

## Adding it to your project

The data tables are generated locally instead of being committed, so depend on a
checkout where you have already run `cargo run -p populate`:

```toml
[dependencies]
tetra-acelp = { path = "../tetra-acelp" }
```

## License

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)), whichever you prefer.

The ETSI codec data tables are not part of this repository. They are fetched from
ETSI's own distribution by `cargo run -p populate` and stay under ETSI's terms.
This project is not affiliated with or endorsed by ETSI, and "TETRA" is used only
to describe what the codec is.
