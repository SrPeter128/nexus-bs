// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! Audio pipeline for the local announcement voice gate:
//! resample arbitrary-rate mono PCM to 8 kHz, run RMS voice activity
//! detection with hysteresis, and encode voiced audio into 274-bit TCH/S
//! blocks (EN 300 395-2 clause 4) via the vendored `tetra-acelp` encoder.
//!
//! The pipeline is deterministic and thread-free so it can be unit-tested
//! with synthetic audio; the worker thread only does stream I/O and drives
//! this struct.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tetra_acelp::{Encoder, SpeechFrame};

pub const TCH_S_BLOCK_BITS: usize = 274;

/// Why the pipeline ended a speech period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Sustained silence below `vad_stop_dbfs` for `silence_timeout_ms`.
    Silence,
    /// `max_call_duration_secs` reached.
    MaxDuration,
    /// No audio data for `stream_loss_grace_ms`.
    StreamLost,
    /// Operator requested a stop.
    Operator,
}

/// High-level events produced by the pipeline, consumed by the entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineEvent {
    /// VAD detected sustained voiced audio.
    SpeechStarted,
    /// A speech period ended (radio call can be released).
    SpeechEnded { reason: EndReason },
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub vad_start_dbfs: f32,
    pub vad_stop_dbfs: f32,
    pub vad_start_ms: u32,
    pub silence_timeout_ms: u32,
    pub stream_loss_grace_ms: u32,
    pub max_call_duration_secs: u32,
}

/// Linear-interpolation resampler to 8 kHz mono (16-bit).
#[derive(Debug)]
pub struct Resampler {
    rate_in: u32,
    /// Fractional read position inside `buf`.
    pos: f64,
    buf: VecDeque<i16>,
}

const RATE_OUT: u32 = 8_000;

impl Resampler {
    pub fn new() -> Self {
        Self { rate_in: 0, pos: 0.0, buf: VecDeque::new() }
    }

    /// Feed a chunk of `rate_in` Hz mono samples; returns 8 kHz output.
    /// Resets the buffer if the input rate changes mid-stream.
    pub fn push(&mut self, mut samples: VecDeque<i16>, rate_in: u32) -> Vec<i16> {
        if rate_in == 0 {
            return Vec::new();
        }
        if rate_in != self.rate_in {
            self.reset();
            self.rate_in = rate_in;
        }
        samples.make_contiguous();
        self.buf.extend(samples);
        if self.buf.len() < 2 {
            // Not enough to interpolate yet; keep for the next chunk.
            return Vec::new();
        }

        let ratio = self.rate_in as f64 / RATE_OUT as f64;
        let mut out = Vec::new();
        loop {
            let whole = self.pos.floor() as usize;
            let remaining = self.buf.len() as f64 - self.pos;
            if remaining < 1.0 {
                break;
            }
            let s0 = *self.buf.get(whole).unwrap_or(&0) as f64;
            let s1 = *self.buf.get(whole.saturating_add(1)).unwrap_or(&(s0 as i16)) as f64;
            let frac = (self.pos - whole as f64) as f64;
            out.push((s0 + (s1 - s0) * frac).round() as i16);
            self.pos += ratio;
        }

        let drop = self.pos.floor() as usize;
        if drop > 0 {
            for _ in 0..drop {
                self.buf.pop_front();
            }
            self.pos -= drop as f64;
        }
        out
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.buf.clear();
    }
}

/// VAD + TETRA encode pipeline. Feed arbitrary-rate mono i16 PCM; receive
/// 274-bit TCH/S blocks (one byte per bit, MSB-first bit order per block)
/// and speech events.
pub struct Pipeline {
    cfg: PipelineConfig,
    resampler: Resampler,
    /// 8 kHz samples waiting to be framed (240 per TETRA frame).
    frame_buf: VecDeque<i16>,
    encoder: Encoder,
    /// First frame of the current 2-frame TCH/S block, awaiting its pair.
    pending_frame: Option<SpeechFrame>,
    /// One-byte-per-bit TCH/S block queued because its pair is not ready yet
    /// is never possible (blocks are always emitted complete), so the only
    /// partial state is `pending_frame`.

    // VAD / call timing state
    speaking: bool,
    voiced_ms: u32,
    silence_ms: u32,
    speech_started_at: Option<Instant>,
    last_audio_at: Option<Instant>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            vad_start_dbfs: -40.0,
            vad_stop_dbfs: -50.0,
            vad_start_ms: 150,
            silence_timeout_ms: 4000,
            stream_loss_grace_ms: 15000,
            max_call_duration_secs: 600,
        }
    }
}

impl Pipeline {
    pub fn new(cfg: PipelineConfig) -> Self {
        Self {
            cfg,
            resampler: Resampler::new(),
            frame_buf: VecDeque::new(),
            encoder: Encoder::new(),
            pending_frame: None,
            speaking: false,
            voiced_ms: 0,
            silence_ms: 0,
            speech_started_at: None,
            last_audio_at: None,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    fn rms_dbfs(frame: &[i16]) -> f32 {
        if frame.is_empty() {
            return f32::NEG_INFINITY;
        }
        let sum: f64 = frame.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        let rms = (sum / frame.len() as f64).sqrt();
        if rms <= 0.0 {
            f32::NEG_INFINITY
        } else {
            (20.0 * (rms / 32768.0).log10()) as f32
        }
    }

    /// Encode one 240-sample frame and pair it into a 274-bit TCH/S block.
    /// Returns the block as one-byte-per-bit (274 bytes) when a pair is
    /// complete.
    fn emit_frame(&mut self, samples: &[i16; 240]) -> Option<Vec<u8>> {
        let speech = self.encoder.encode(samples);
        match self.pending_frame.take() {
            Some(first) => Some(SpeechFrame::tch_s_block(&first, &speech).into_iter().map(|b| b as u8).collect()),
            None => {
                self.pending_frame = Some(speech);
                None
            }
        }
    }

    /// Flush a pending single frame paired with one silence frame so speech
    /// is not cut off mid-block when a call ends.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        let pending = self.pending_frame.take()?;
        let silence = [0i16; 240];
        let tail = self.encoder.encode(&silence);
        Some(SpeechFrame::tch_s_block(&pending, &tail).into_iter().map(|b| b as u8).collect())
    }

    /// Update stream-loss tracking (call on every worker loop iteration).
    /// Returns a `SpeechEnded { reason: StreamLost }` event when a speaking
    /// call loses its audio source for longer than `stream_loss_grace_ms`.
    pub fn check_stream_loss(&mut self, now: Instant) -> Option<PipelineEvent> {
        let last = self.last_audio_at?;
        if !self.speaking {
            return None;
        }
        if now.duration_since(last) <= Duration::from_millis(self.cfg.stream_loss_grace_ms as u64) {
            return None;
        }
        self.end_speech(now, EndReason::StreamLost)
    }

    /// Operator-forced stop. Returns the end event when a speech period was
    /// active.
    pub fn operator_stop(&mut self, now: Instant) -> Option<PipelineEvent> {
        if !self.speaking {
            return None;
        }
        self.end_speech(now, EndReason::Operator)
    }

    fn end_speech(&mut self, _now: Instant, reason: EndReason) -> Option<PipelineEvent> {
        if !self.speaking {
            return None;
        }
        self.speaking = false;
        self.voiced_ms = 0;
        self.silence_ms = 0;
        self.speech_started_at = None;
        Some(PipelineEvent::SpeechEnded { reason })
    }

    /// Feed decoded mono PCM at `sample_rate` Hz. Returns newly completed
    /// 274-bit TCH/S blocks and any VAD events.
    pub fn process(&mut self, pcm: VecDeque<i16>, sample_rate: u32) -> (Vec<Vec<u8>>, Vec<PipelineEvent>) {
        let mut blocks = Vec::new();
        let mut events = Vec::new();
        if pcm.is_empty() || sample_rate == 0 {
            return (blocks, events);
        }
        let now = Instant::now();
        self.last_audio_at = Some(now);

        let audio_8k = self.resampler.push(pcm, sample_rate);
        self.frame_buf.extend(audio_8k);

        // VAD on every 30 ms slice of 8 kHz audio (240 samples).
        while self.frame_buf.len() >= 240 {
            let slice: Vec<i16> = self.frame_buf.drain(..240).collect();
            let arr: [i16; 240] = slice.try_into().expect("drained exactly 240 samples");
            let dbfs = Self::rms_dbfs(&arr);
            self.update_vad(dbfs, now, &mut events);

            if let Some(block) = self.emit_frame(&arr) {
                blocks.push(block);
            }
        }

        // Enforce the max call duration once per process() call.
        if self.speaking
            && let Some(started) = self.speech_started_at
            && now.duration_since(started) >= Duration::from_secs(self.cfg.max_call_duration_secs as u64)
        {
            if let Some(ev) = self.end_speech(now, EndReason::MaxDuration) {
                events.push(ev);
            }
        }

        (blocks, events)
    }

    fn update_vad(&mut self, dbfs: f32, now: Instant, events: &mut Vec<PipelineEvent>) {
        const SLICE_MS: u32 = 30;
        if !self.speaking {
            if dbfs >= self.cfg.vad_start_dbfs {
                self.voiced_ms = self.voiced_ms.saturating_add(SLICE_MS);
            } else {
                self.voiced_ms = 0;
            }
            if self.voiced_ms >= self.cfg.vad_start_ms {
                self.speaking = true;
                self.voiced_ms = 0;
                self.silence_ms = 0;
                self.speech_started_at = Some(now);
                events.push(PipelineEvent::SpeechStarted);
            }
            return;
        }

        if dbfs < self.cfg.vad_stop_dbfs {
            self.silence_ms = self.silence_ms.saturating_add(SLICE_MS);
        } else {
            self.silence_ms = 0;
        }
        if self.silence_ms >= self.cfg.silence_timeout_ms {
            if let Some(ev) = self.end_speech(now, EndReason::Silence) {
                events.push(ev);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_8k(hz: f64, amp: i16, secs: u32) -> (VecDeque<i16>, u32) {
        let n = 8_000 * secs as usize;
        let mut q = VecDeque::with_capacity(n);
        for i in 0..n {
            q.push_back((amp as f64 * (2.0 * std::f64::consts::PI * hz * i as f64 / 8_000.0).sin()) as i16);
        }
        (q, 8_000)
    }

    fn silence_8k(secs: u32) -> (VecDeque<i16>, u32) {
        (VecDeque::from(vec![0i16; 8_000 * secs as usize]), 8_000)
    }

    #[test]
    fn resampler_44100_to_8000_keeps_tone_and_ratio() {
        let mut r = Resampler::new();
        // 44.1 kHz, 1 kHz tone, 0.1 s
        let n = 4410;
        let mut in44 = VecDeque::with_capacity(n);
        for i in 0..n {
            in44.push_back(10_000i16);
            let _ = i;
        }
        // Replace with a real tone for zero-crossing sanity
        in44.clear();
        for i in 0..n {
            in44.push_back((10_000.0 * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 44_100.0).sin()) as i16);
        }
        let out = r.push(in44, 44_100);
        assert_eq!(out.len(), 800, "0.1 s at 8 kHz must produce 800 samples");
        // 1 kHz tone at 8 kHz: 8 samples per period, 100 periods, ~2 zero
        // crossings per period.
        let crossings = (1..out.len()).filter(|&i| (out[i] >= 0) != (out[i - 1] >= 0)).count();
        assert!((185..=215).contains(&crossings), "expected ~200 zero crossings, got {crossings}");
    }

    #[test]
    fn vad_start_requires_sustained_voicing() {
        let mut p = Pipeline::new(PipelineConfig { vad_start_ms: 150, ..PipelineConfig::default() });
        // 120 ms voiced then silence: no start.
        let (tone, rate) = tone_8k(440.0, 20_000, 0); // placeholder length
        let _ = tone;
        let mut voiced = VecDeque::new();
        for i in 0..(8_000 * 120 / 1000) {
            voiced.push_back((20_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8_000.0).sin()) as i16);
        }
        let (blocks, events) = p.process(voiced, rate);
        assert!(events.is_empty(), "120 ms voiced (< 150 ms) must not start speech: {events:?}");
        assert!(blocks.len() >= 1);

        // Continue with another 60 ms voiced -> total 180 ms -> start.
        let mut more = VecDeque::new();
        for i in 0..(8_000 * 60 / 1000) {
            more.push_back((20_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8_000.0).sin()) as i16);
        }
        let (_, events) = p.process(more, rate);
        assert_eq!(events, vec![PipelineEvent::SpeechStarted]);
        assert!(p.is_speaking());
    }

    #[test]
    fn vad_stop_after_silence_timeout() {
        let mut p = Pipeline::new(PipelineConfig { vad_start_ms: 30, silence_timeout_ms: 100, ..PipelineConfig::default() });
        let (tone, rate) = tone_8k(440.0, 20_000, 0);
        let mut voiced = VecDeque::new();
        for i in 0..(8_000 * 60 / 1000) {
            voiced.push_back((20_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8_000.0).sin()) as i16);
        }
        let (_, events) = p.process(voiced, rate);
        assert_eq!(events, vec![PipelineEvent::SpeechStarted]);
        let _ = tone;

        // 150 ms of silence (5 x 30 ms slices >= 100 ms timeout) -> end.
        let (sil, rate) = silence_8k(1);
        let sil150: VecDeque<i16> = sil.into_iter().take(8_000 * 150 / 1000).collect();
        let (_, events) = p.process(sil150, rate);
        assert_eq!(events, vec![PipelineEvent::SpeechEnded { reason: EndReason::Silence }]);
        assert!(!p.is_speaking());
    }

    #[test]
    fn blocks_are_274_bits_and_encoder_state_is_consistent() {
        let mut p = Pipeline::new(PipelineConfig::default());
        let (tone, rate) = tone_8k(440.0, 20_000, 1);
        let (blocks, _) = p.process(tone, rate);
        // 1 s = 8000 samples = 33 full frames + 80 leftovers -> 16 full blocks (+ possibly one).
        assert!(blocks.len() >= 15, "expected ~16 TCH/S blocks per second, got {}", blocks.len());
        for b in &blocks {
            assert_eq!(b.len(), TCH_S_BLOCK_BITS, "each TCH/S block must be 274 one-bit-per-byte");
            assert!(b.iter().all(|&x| x == 0 || x == 1));
        }
        let flushed = p.flush();
        assert!(flushed.is_some(), "odd frame count must flush a final block");
    }

    #[test]
    fn operator_stop_ends_speech_period() {
        let mut p = Pipeline::new(PipelineConfig { vad_start_ms: 30, ..PipelineConfig::default() });
        let mut voiced = VecDeque::new();
        for i in 0..(8_000 * 60 / 1000) {
            voiced.push_back((20_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8_000.0).sin()) as i16);
        }
        let (_, events) = p.process(voiced, 8_000);
        assert_eq!(events, vec![PipelineEvent::SpeechStarted]);

        let now = Instant::now();
        assert_eq!(p.operator_stop(now), Some(PipelineEvent::SpeechEnded { reason: EndReason::Operator }));
        assert!(!p.is_speaking());
        // A second operator stop is a no-op.
        assert_eq!(p.operator_stop(Instant::now()), None);
    }
}
