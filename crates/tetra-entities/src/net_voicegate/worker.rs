// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! Background worker for the local announcement voice gate.
//!
//! Reads the configured live audio stream over HTTP(S), decodes it with
//! Symphonia (MP3/AAC/OGG/FLAC), resamples to 8 kHz mono, and drives the
//! [`super::pipeline::Pipeline`] (VAD + TETRA ACELP encode). Encoded
//! 274-bit TCH/S blocks and VAD events are sent to the entity through a
//! bounded channel; operator commands arrive through the reverse channel.
//! The worker reconnects automatically on stream loss.

use std::io::{Error as IoError, ErrorKind, Read};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use symphonia::core::audio::AudioBufferRef;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tetra_config::bluestation::{CfgAnnouncement, CfgAnnouncementAuth};

use super::pipeline::{Pipeline, PipelineConfig, PipelineEvent};

const EVENT_CHANNEL_CAPACITY: usize = 128;
const COMMAND_CHANNEL_CAPACITY: usize = 16;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Events from the worker to the entity.
#[derive(Debug)]
pub enum VoicegateEvent {
    /// A completed 274-bit TCH/S block (one byte per bit, MSB-first).
    Frame(Vec<u8>),
    /// Pipeline VAD event (speech start/end).
    Pipeline(PipelineEvent),
    /// Stream connection state change (for logging/dashboard).
    StreamStatus { stream: String, connected: bool },
}

/// Commands from the entity to the worker.
#[derive(Debug, Clone)]
pub enum VoicegateCommand {
    /// Switch the active stream (label must exist in the config).
    SelectStream { label: String },
    /// Operator-forced stop of the current speech period.
    ForceStop,
    /// Terminate the worker loop.
    Shutdown,
}

/// Audio source abstraction so tests can feed synthetic audio.
pub trait AudioSource: Send {
    /// Connect (or reconnect) and return a reader over the encoded stream.
    fn open(&mut self) -> Result<Box<dyn Read + Send + Sync>, String>;
}

/// One chunk of encoded stream data (or the end marker) from the feeder
/// thread to the decode loop.
enum FeedChunk {
    Data(Vec<u8>),
    End(Option<String>),
}

/// `io::Read` adapter over the feeder channel. Returns `TimedOut` when no
/// data arrives for a second, so a stalled stream surfaces as an IoError
/// and the worker reconnects (instead of blocking forever).
struct FeedStream {
    chunk_rx: Receiver<FeedChunk>,
    pending: std::collections::VecDeque<u8>,
}

impl Read for FeedStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            let bytes = self.pending.make_contiguous();
            buf[..n].copy_from_slice(&bytes[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }
        match self.chunk_rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(FeedChunk::Data(mut chunk)) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.pending.extend(chunk.split_off(n));
                }
                Ok(n)
            }
            Ok(FeedChunk::End(msg)) => {
                Err(IoError::new(ErrorKind::UnexpectedEof, msg.unwrap_or_else(|| "stream ended".to_string())))
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                Err(IoError::new(ErrorKind::TimedOut, "no stream data for 1 s"))
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(IoError::new(ErrorKind::UnexpectedEof, "feeder thread gone"))
            }
        }
    }
}

/// Feeds HTTP response bytes into a bounded channel from a dedicated
/// thread, decoupling network reads from the decode loop.
fn spawn_feeder(resp: reqwest::blocking::Response, tx: Sender<FeedChunk>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voicegate-feeder".to_string())
        .spawn(move || {
            let mut resp = resp;
            let mut buf = [0u8; 8192];
            loop {
                match resp.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(FeedChunk::End(None));
                        break;
                    }
                    Ok(n) => {
                        if tx.send(FeedChunk::Data(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(FeedChunk::End(Some(e.to_string())));
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn voicegate feeder thread")
}

struct HttpStreamSource {
    cfg: CfgAnnouncement,
    active_label: String,
    client: reqwest::blocking::Client,
}

impl HttpStreamSource {
    fn new(cfg: CfgAnnouncement) -> Self {
        let active_label = if cfg.streams.iter().any(|s| s.label == cfg.active_stream) {
            cfg.active_stream.clone()
        } else {
            cfg.streams.first().map(|s| s.label.clone()).unwrap_or_default()
        };
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("valid reqwest client");
        Self { cfg, active_label, client }
    }
}

impl AudioSource for HttpStreamSource {
    fn open(&mut self) -> Result<Box<dyn Read + Send + Sync>, String> {
        let Some(stream) = self.cfg.streams.iter().find(|s| s.label == self.active_label) else {
            return Err(format!("stream {:?} not configured", self.active_label));
        };
        let mut url = stream.url.clone();
        if let Some(CfgAnnouncementAuth::Query { name, value }) = &stream.auth {
            let sep = if url.contains('?') { '&' } else { '?' };
            url = format!("{url}{sep}{name}={value}");
        }
        tracing::info!("Voicegate: opening stream {:?} ({url})", self.active_label);
        let mut req = self.client.get(&url);
        match &stream.auth {
            Some(CfgAnnouncementAuth::Bearer(token)) => {
                req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
            }
            Some(CfgAnnouncementAuth::Header { name, value }) => {
                req = req.header(name.as_str(), value.as_str());
            }
            Some(CfgAnnouncementAuth::Basic { username, password }) => {
                req = req.basic_auth(username.as_str(), Some(password.as_str()));
            }
            _ => {}
        }
        let resp = req.send().map_err(|e| format!("request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let (tx, rx) = bounded::<FeedChunk>(512);
        let _feeder = spawn_feeder(resp, tx);
        Ok(Box::new(FeedStream { chunk_rx: rx, pending: std::collections::VecDeque::new() }))
    }
}

/// Decodes an [`AudioSource`] and pushes pipeline output into the event
/// channel. Intended to run on a dedicated thread via [`Self::run`].
pub struct VoicegateWorker {
    source: Box<dyn AudioSource>,
    pipeline: Pipeline,
    event_tx: Sender<VoicegateEvent>,
    cmd_rx: Receiver<VoicegateCommand>,
    active_label: String,
    configured_labels: Vec<String>,
    loss_grace_ms: u32,
    shutdown: bool,
}

impl VoicegateWorker {
    pub fn new(
        source: Box<dyn AudioSource>,
        cfg: &CfgAnnouncement,
        event_tx: Sender<VoicegateEvent>,
        cmd_rx: Receiver<VoicegateCommand>,
    ) -> Self {
        let active_label = if cfg.streams.iter().any(|s| s.label == cfg.active_stream) {
            cfg.active_stream.clone()
        } else {
            cfg.streams.first().map(|s| s.label.clone()).unwrap_or_default()
        };
        let loss_grace_ms = cfg.stream_loss_grace_ms;
        let pipeline = Pipeline::new(PipelineConfig {
            vad_start_dbfs: cfg.vad_start_dbfs,
            vad_stop_dbfs: cfg.vad_stop_dbfs,
            vad_start_ms: cfg.vad_start_ms,
            silence_timeout_ms: cfg.silence_timeout_ms,
            stream_loss_grace_ms: loss_grace_ms,
            max_call_duration_secs: cfg.max_call_duration_secs,
        });
        Self {
            source,
            pipeline,
            event_tx,
            cmd_rx,
            active_label,
            configured_labels: cfg.streams.iter().map(|s| s.label.clone()).collect(),
            loss_grace_ms,
            shutdown: false,
        }
    }

    /// Run until Shutdown. Intended to be called from a dedicated thread.
    pub fn run(&mut self) {
        tracing::info!("Voicegate worker started (active stream {:?})", self.active_label);
        loop {
            if self.shutdown {
                break;
            }
            // Poll commands (non-blocking) between stream attempts.
            self.drain_commands();
            if self.shutdown {
                break;
            }
            match self.connect_and_decode() {
                Ok(true) => tracing::info!("Voicegate: stream {:?} ended cleanly", self.active_label),
                Ok(false) => tracing::info!("Voicegate: stream {:?} lost or idle, reconnecting", self.active_label),
                Err(e) => tracing::warn!("Voicegate: stream {:?} error: {e}", self.active_label),
            }
            // A speaking call must end when the audio source is lost.
            self.report_stream_loss();
            let _ = self.event_tx.send(VoicegateEvent::StreamStatus { stream: self.active_label.clone(), connected: false });
            if self.shutdown {
                break;
            }
            std::thread::sleep(RECONNECT_DELAY);
        }
        tracing::info!("Voicegate worker stopped");
    }

    fn drain_commands(&mut self) {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(cmd) => {
                    self.apply_command(cmd);
                    if self.shutdown {
                        return;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.shutdown = true;
                    return;
                }
            }
        }
    }

    fn apply_command(&mut self, cmd: VoicegateCommand) {
        match cmd {
            VoicegateCommand::SelectStream { label } => {
                if !self.configured_labels.contains(&label) {
                    tracing::warn!("Voicegate: unknown stream label {label:?}, ignoring");
                    return;
                }
                if label != self.active_label {
                    tracing::info!("Voicegate: switching active stream {:?} -> {label:?}", self.active_label);
                    self.active_label = label;
                }
            }
            VoicegateCommand::ForceStop => {
                let now = Instant::now();
                if let Some(event) = self.pipeline.operator_stop(now) {
                    let _ = self.event_tx.send(VoicegateEvent::Pipeline(event));
                }
            }
            VoicegateCommand::Shutdown => self.shutdown = true,
        }
    }

    /// Connect the stream and decode until loss/end. Returns `true` when
    /// the stream ended cleanly, `false` on idle/loss (reconnect), or
    /// `Err` on hard failure.
    fn connect_and_decode(&mut self) -> Result<bool, String> {
        let reader = self.source.open()?;
        let _ = self.event_tx.send(VoicegateEvent::StreamStatus { stream: self.active_label.clone(), connected: true });

        let mss = MediaSourceStream::new(Box::new(symphonia::core::io::ReadOnlySource::new(reader)), Default::default());
        let meta_opts: MetadataOptions = Default::default();
        let fmt_opts: FormatOptions = Default::default();
        // Content-based probing (no extension hint): works for raw MP3/ADTS,
        // OGG and MP4-fragmented streams as delivered by public feeds.
        let probed = symphonia::default::get_probe()
            .format(&Default::default(), mss, &fmt_opts, &meta_opts)
            .map_err(|e| format!("no supported audio format in stream: {e:?}"))?;
        let mut format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or_else(|| "no supported audio track in stream".to_string())?;
        let dec_opts: DecoderOptions = Default::default();
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &dec_opts)
            .map_err(|e| format!("no supported audio codec in stream: {e:?}"))?;
        let track_id = track.id;
        tracing::info!("Voicegate: decoding with codec {:?}", track.codec_params.codec);

        loop {
            if self.shutdown {
                return Ok(false);
            }
            self.drain_commands();
            if self.shutdown {
                return Ok(false);
            }

            let packet = match format.next_packet() {
                Ok(packet) => packet,
                // read_timeout / EOF / connection reset: the stream is lost.
                Err(SymphoniaError::IoError(_)) => return Ok(false),
                Err(SymphoniaError::ResetRequired) => return Ok(false),
                Err(e) => return Err(format!("stream read error: {e:?}")),
            };
            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(buffer) => {
                    if buffer.frames() == 0 {
                        continue;
                    }
                    let sample_rate = buffer.spec().rate;
                    let pcm = frame_to_mono_i16(&buffer);
                    if !pcm.is_empty() {
                        let (blocks, events) = self.pipeline.process(pcm, sample_rate);
                        for block in blocks {
                            let _ = self.event_tx.send(VoicegateEvent::Frame(block));
                        }
                        for event in events {
                            let _ = self.event_tx.send(VoicegateEvent::Pipeline(event));
                        }
                    }
                }
                Err(SymphoniaError::IoError(_)) => return Ok(false),
                Err(e) => {
                    tracing::debug!("Voicegate: skipping undecodable packet: {e:?}");
                }
            }
        }
    }

    /// End a speaking call when the stream was lost (called from the run
    /// loop before each reconnect).
    pub fn report_stream_loss(&mut self) {
        let now = Instant::now();
        if let Some(event) = self.pipeline.check_stream_loss(now) {
            let _ = self.event_tx.send(VoicegateEvent::Pipeline(event));
        }
    }
}

/// Convert a Symphonia decode buffer to mono i16 samples.
fn frame_to_mono_i16(buffer: &AudioBufferRef) -> std::collections::VecDeque<i16> {
    let frames = buffer.frames();
    let channels = buffer.spec().channels.count();
    if frames == 0 || channels == 0 {
        return std::collections::VecDeque::new();
    }
    let mut buf16 = buffer.make_equivalent::<i16>();
    buffer.convert(&mut buf16);
    let audio_planes = buf16.planes();
    let planes: &[&[i16]] = audio_planes.planes();
    if channels <= 1 {
        let Some(plane) = planes.first() else {
            return std::collections::VecDeque::new();
        };
        let mut out = std::collections::VecDeque::with_capacity(frames);
        out.extend(plane.iter().copied().take(frames));
        out
    } else {
        let l = planes.first().copied();
        let r = planes.get(1).copied();
        let (Some(l), Some(r)) = (l, r) else {
            return std::collections::VecDeque::new();
        };
        let mut out = std::collections::VecDeque::with_capacity(frames);
        for i in 0..frames {
            let li = l.get(i).copied().unwrap_or(0) as i32;
            let ri = r.get(i).copied().unwrap_or(0) as i32;
            out.push_back(((li + ri) / 2) as i16);
        }
        out
    }
}

/// Build the worker -> entity event channel.
pub fn event_channel() -> (Sender<VoicegateEvent>, Receiver<VoicegateEvent>) {
    bounded::<VoicegateEvent>(EVENT_CHANNEL_CAPACITY)
}

/// Build the entity -> worker command channel.
pub fn command_channel() -> (Sender<VoicegateCommand>, Receiver<VoicegateCommand>) {
    bounded::<VoicegateCommand>(COMMAND_CHANNEL_CAPACITY)
}

/// Start the worker thread for the production HTTP source.
pub fn spawn_http_worker(
    cfg: CfgAnnouncement,
    event_tx: Sender<VoicegateEvent>,
    cmd_rx: Receiver<VoicegateCommand>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voicegate-worker".to_string())
        .spawn(move || {
            let mut worker = VoicegateWorker::new(Box::new(HttpStreamSource::new(cfg.clone())), &cfg, event_tx, cmd_rx);
            worker.run();
        })
        .expect("failed to spawn voicegate worker thread")
}
