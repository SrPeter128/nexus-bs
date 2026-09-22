//! `tetra-acelp` — a clean-room Rust implementation of the ETSI TETRA
//! full-rate speech codec (EN 300 395-2, clause 4): the source encoder and
//! decoder.
//!
//! The codec operates on 8 kHz, 16-bit PCM in 30 ms frames of 240 samples,
//! producing 137 bits per frame (4 567 bit/s). It is a bit-exact fixed-point
//! implementation of EN 300 395-2 clause 4, verified by differential testing
//! against the standard's reference codec.
//!
//! # Quick start
//!
//! ```
//! use tetra_acelp::{Encoder, Decoder, FrameQuality, FRAME_SAMPLES};
//!
//! let mut encoder = Encoder::new();
//! let mut decoder = Decoder::new();
//!
//! let pcm = [0i16; FRAME_SAMPLES];        // one 30 ms frame of audio
//! let frame = encoder.encode(&pcm);        // -> SpeechFrame (137 bits)
//! let out = decoder.decode(&frame, FrameQuality::good());
//! assert_eq!(out.len(), FRAME_SAMPLES);
//! ```
//!
//! [`SpeechFrame`] is the canonical transport type (exactly 137 ordered bits);
//! it converts to/from packed bytes and individual bits, and two frames
//! concatenate into the 274-bit TETRA TCH/S speech block.
//!
//! # Module layout
//!
//! The high-level API lives at the crate root ([`Encoder`], [`Decoder`],
//! [`SpeechFrame`]). The lower-level building blocks are also public for
//! advanced use: [`fixed`] (fixed-point operators), [`dsp`] (LP-analysis and
//! filtering primitives), [`tables`] (codebooks), and the [`encoder`] /
//! [`decoder`] cores.

// The DSP routines deliberately use index-based loops and wide argument lists
// to stay faithful to the fixed-point algorithms of the specification; their
// correctness is proven by the oracle tests rather than by idiomatic
// restructuring.
#![allow(
    clippy::needless_range_loop,
    clippy::manual_memcpy,
    clippy::too_many_arguments
)]

pub mod decoder;
pub mod dsp;
pub mod encoder;
pub mod fixed;
pub mod frame;
pub mod tables;

pub use frame::{
    Decoder, Encoder, FRAME_BITS, FRAME_BYTES, FRAME_SAMPLES, FrameParameters, FrameQuality,
    SpeechFrame, SubframeParameters,
};
