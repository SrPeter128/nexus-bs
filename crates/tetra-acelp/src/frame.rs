//! Public API: the [`SpeechFrame`] transport type and the [`Encoder`] /
//! [`Decoder`] front ends.
//!
//! A [`SpeechFrame`] is the canonical representation of one 30 ms coded frame:
//! exactly 137 ordered source-coded bits (`B1..B137` in the specification),
//! stored compactly as packed bytes. Frame-quality metadata such as the Bad
//! Frame Indicator is kept separate (see [`FrameQuality`]) and never mixed into
//! the 137 bits, so a `SpeechFrame` maps directly onto the TETRA TCH/S bit
//! layout used by the radio stack.

use crate::decoder::{Decoder as CoreDecoder, bits_to_params};
use crate::encoder::{Encoder as CoreEncoder, FrameParams, params_to_bits};

/// Number of PCM samples in one 30 ms frame (8 kHz).
pub const FRAME_SAMPLES: usize = 240;
/// Number of source-coded bits in one frame.
pub const FRAME_BITS: usize = 137;
/// Packed size of a [`SpeechFrame`] in bytes (`ceil(137 / 8)`).
pub const FRAME_BYTES: usize = 18;

/// One 30 ms coded speech frame: 137 ordered bits, stored packed MSB-first.
///
/// Bit 0 corresponds to `B1` in the specification. The final 7 bits of the
/// 18-byte storage are unused and always zero.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpeechFrame {
    bytes: [u8; FRAME_BYTES],
}

impl SpeechFrame {
    /// Build a frame from its 137 bits (MSB-first, `bits[0]` = `B1`).
    pub fn from_bits(bits: &[bool; FRAME_BITS]) -> Self {
        let mut bytes = [0u8; FRAME_BYTES];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        Self { bytes }
    }

    /// Return the 137 bits (MSB-first, `bits[0]` = `B1`).
    pub fn to_bits(&self) -> [bool; FRAME_BITS] {
        let mut bits = [false; FRAME_BITS];
        for (i, b) in bits.iter_mut().enumerate() {
            *b = (self.bytes[i / 8] >> (7 - (i % 8))) & 1 != 0;
        }
        bits
    }

    /// Build a frame from its packed byte representation.
    ///
    /// The 7 unused low bits of the last byte are ignored.
    pub fn from_bytes(bytes: [u8; FRAME_BYTES]) -> Self {
        let mut f = Self { bytes };
        // Clear the 7 unused trailing bits so equality/round-trips are stable.
        f.bytes[FRAME_BYTES - 1] &= 0xfe;
        f
    }

    /// Return the packed byte representation (137 bits MSB-first, 7 zero-padded).
    pub fn to_bytes(&self) -> [u8; FRAME_BYTES] {
        self.bytes
    }

    /// Decode the frame into its named codec parameters (for debugging/testing).
    pub fn parameters(&self) -> FrameParameters {
        let raw = self.raw_params();
        FrameParameters {
            lsp_indices: [raw[0] as u16, raw[1] as u16, raw[2] as u16],
            subframes: core::array::from_fn(|k| SubframeParameters {
                pitch_index: raw[3 + 5 * k] as u16,
                code_index: raw[4 + 5 * k] as u16,
                sign: raw[5 + 5 * k] as u16,
                shift: raw[6 + 5 * k] as u16,
                gain_index: raw[7 + 5 * k] as u16,
            }),
        }
    }

    /// Concatenate two frames into the 274-bit TETRA TCH/S speech block used for
    /// one transmission time slot: the bits of `first` (frame A) followed by the
    /// bits of `second` (frame B).
    pub fn tch_s_block(first: &SpeechFrame, second: &SpeechFrame) -> [bool; 2 * FRAME_BITS] {
        let mut out = [false; 2 * FRAME_BITS];
        out[..FRAME_BITS].copy_from_slice(&first.to_bits());
        out[FRAME_BITS..].copy_from_slice(&second.to_bits());
        out
    }

    pub(crate) fn from_raw_params(params: &FrameParams) -> Self {
        let mut serial = [0i16; 1 + FRAME_BITS];
        params_to_bits(params, &mut serial);
        let mut bits = [false; FRAME_BITS];
        for (i, b) in bits.iter_mut().enumerate() {
            *b = serial[i + 1] != 0;
        }
        Self::from_bits(&bits)
    }

    pub(crate) fn raw_params(&self) -> [i16; 23] {
        let bits = self.to_bits();
        let mut serial = [0i16; 1 + FRAME_BITS];
        for (i, &b) in bits.iter().enumerate() {
            serial[i + 1] = b as i16;
        }
        let prm = bits_to_params(&serial);
        let mut out = [0i16; 23];
        out.copy_from_slice(&prm[1..24]);
        out
    }
}

/// Named codec parameters for one subframe (indices into the codebooks).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubframeParameters {
    /// Pitch-delay index (8 bits in the first subframe, 5 bits otherwise).
    pub pitch_index: u16,
    /// Algebraic (innovative) codebook index (14 bits).
    pub code_index: u16,
    /// Global sign of the algebraic pulses (0 or 1).
    pub sign: u16,
    /// Pulse-position shift flag (0 or 1).
    pub shift: u16,
    /// Gain vector-quantiser index (6 bits).
    pub gain_index: u16,
}

/// Named codec parameters for one frame (for debugging/testing only; the
/// canonical transport representation is [`SpeechFrame`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameParameters {
    /// The three split-VQ LSP codebook indices (8, 9, 9 bits).
    pub lsp_indices: [u16; 3],
    /// Per-subframe parameters for the four subframes.
    pub subframes: [SubframeParameters; 4],
}

/// Per-frame quality metadata, kept separate from the coded bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameQuality {
    /// Bad Frame Indicator: when `true`, the decoder performs error concealment
    /// using the previous good frame instead of the received parameters.
    pub bad_frame: bool,
}

impl FrameQuality {
    /// A good (error-free) frame.
    pub fn good() -> Self {
        Self { bad_frame: false }
    }

    /// A bad frame, triggering error concealment.
    pub fn bad() -> Self {
        Self { bad_frame: true }
    }
}

/// The TETRA source speech encoder.
///
/// Stateful across frames; create one [`Encoder`] per audio stream.
pub struct Encoder {
    inner: CoreEncoder,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create a new encoder with reset state.
    pub fn new() -> Self {
        Self {
            inner: CoreEncoder::new(),
        }
    }

    /// Encode one 30 ms frame of 16-bit PCM (240 samples) into a [`SpeechFrame`].
    pub fn encode(&mut self, pcm: &[i16; FRAME_SAMPLES]) -> SpeechFrame {
        let (params, _synth) = self.inner.encode_frame(pcm);
        SpeechFrame::from_raw_params(&params)
    }

    /// Encode one frame, also returning the encoder's local synthesis (the
    /// speech the decoder will reconstruct on a clean channel). Useful for
    /// analysis and testing.
    pub fn encode_with_synthesis(
        &mut self,
        pcm: &[i16; FRAME_SAMPLES],
    ) -> (SpeechFrame, [i16; FRAME_SAMPLES]) {
        let (params, synth) = self.inner.encode_frame(pcm);
        (SpeechFrame::from_raw_params(&params), synth)
    }
}

/// The TETRA source speech decoder.
///
/// Stateful across frames; create one [`Decoder`] per audio stream.
pub struct Decoder {
    inner: CoreDecoder,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// Create a new decoder with reset state.
    pub fn new() -> Self {
        Self {
            inner: CoreDecoder::new(),
        }
    }

    /// Decode one [`SpeechFrame`] into 240 PCM samples.
    ///
    /// `quality` carries the Bad Frame Indicator; pass [`FrameQuality::good`]
    /// for an error-free frame or [`FrameQuality::bad`] to trigger error
    /// concealment (the frame's bits are then ignored in favour of the previous
    /// good frame).
    pub fn decode(&mut self, frame: &SpeechFrame, quality: FrameQuality) -> [i16; FRAME_SAMPLES] {
        let raw = frame.raw_params();
        let mut parm = [0i16; 24];
        parm[0] = quality.bad_frame as i16;
        parm[1..24].copy_from_slice(&raw);
        self.inner.decode_frame(&parm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bit_byte_roundtrip() {
        let mut bits = [false; FRAME_BITS];
        for (i, b) in bits.iter_mut().enumerate() {
            *b = (i * 7 + 3) % 5 < 2;
        }
        let frame = SpeechFrame::from_bits(&bits);
        assert_eq!(frame.to_bits(), bits);
        assert_eq!(SpeechFrame::from_bytes(frame.to_bytes()), frame);
    }

    #[test]
    fn tch_s_block_is_concatenation() {
        let a = SpeechFrame::from_bits(&[true; FRAME_BITS]);
        let b = SpeechFrame::from_bits(&[false; FRAME_BITS]);
        let block = SpeechFrame::tch_s_block(&a, &b);
        assert!(block[..FRAME_BITS].iter().all(|&x| x));
        assert!(block[FRAME_BITS..].iter().all(|&x| !x));
    }

    #[test]
    fn encode_decode_roundtrip_is_clean() {
        let mut enc = Encoder::new();
        let mut dec = Decoder::new();
        let pcm = [1234i16; FRAME_SAMPLES];
        let (frame, synth) = enc.encode_with_synthesis(&pcm);
        let out = dec.decode(&frame, FrameQuality::good());
        assert_eq!(out, synth);
        // Round-tripping the frame through bits/bytes changes nothing.
        let frame2 = SpeechFrame::from_bytes(frame.to_bytes());
        assert_eq!(frame2, frame);
    }
}
