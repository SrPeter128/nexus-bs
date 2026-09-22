// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! End-to-end voice gate worker test: a local TCP listener serves a
//! generated WAV tone over HTTP; the production worker (reqwest + feeder
//! + Symphonia) must decode it, resample to 8 kHz, detect speech (VAD)
//! and emit 274-bit TCH/S blocks. No external network access required.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use tetra_config::bluestation::{CfgAnnouncement, CfgAnnouncementStream};
use tetra_entities::net_voicegate::pipeline::PipelineEvent;
use tetra_entities::net_voicegate::worker::{command_channel, event_channel, spawn_http_worker, VoicegateCommand, VoicegateEvent};

/// Minimal 16-bit PCM WAV file (44.1 kHz mono, 440 Hz sine, 3 s).
fn build_wav() -> Vec<u8> {
    const RATE: u32 = 44_100;
    const SECS: u32 = 3;
    const N: usize = (RATE * SECS) as usize;
    let mut pcm = Vec::with_capacity(N * 2);
    for i in 0..N {
        let s = (20_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / RATE as f64).sin()) as i16;
        pcm.extend_from_slice(&s.to_le_bytes());
    }
    let data_len = (pcm.len() as u32).to_le_bytes();
    let mut v = Vec::with_capacity(44 + pcm.len());
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + pcm.len() as u32).to_le_bytes()));
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2u32).to_le_bytes()); // byte rate
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes()); // bits
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len);
    v.extend_from_slice(&pcm);
    v
}

/// Tiny single-file HTTP server (serves the body for every connection).
fn serve_wav(body: Arc<Vec<u8>>) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local port");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut req = [0u8; 1024];
        let _ = stream.read(&mut req);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut resp = resp.into_bytes();
        resp.extend_from_slice(&body);
        let _ = stream.write_all(&resp);
        std::thread::sleep(Duration::from_millis(100));
    });
    (addr, handle)
}

#[test]
fn worker_decodes_wav_stream_and_emits_tch_s_blocks() {
    let (addr, server) = serve_wav(Arc::new(build_wav()));

    let cfg = CfgAnnouncement {
        enabled: true,
        gssi: 500,
        issi: 55_000,
        streams: vec![CfgAnnouncementStream {
            label: "main".to_string(),
            url: format!("http://{addr}/tone.wav"),
            auth: None,
        }],
        active_stream: "main".to_string(),
        vad_start_dbfs: -40.0,
        vad_stop_dbfs: -50.0,
        vad_start_ms: 150,
        silence_timeout_ms: 4000,
        stream_loss_grace_ms: 15000,
        max_call_duration_secs: 600,
    };

    let (event_tx, event_rx) = event_channel();
    let (cmd_tx, cmd_rx) = command_channel();
    let worker = spawn_http_worker(cfg, event_tx, cmd_rx);

    // Collect events: expect StreamStatus(connected), SpeechStarted, a
    // healthy batch of 274-bit frames, and the clean stream end marker.
    let mut frames: Vec<usize> = Vec::new();
    let mut speech_started = false;
    let mut disconnected = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(VoicegateEvent::Frame(block)) => frames.push(block.len()),
            Ok(VoicegateEvent::Pipeline(PipelineEvent::SpeechStarted)) => speech_started = true,
            Ok(VoicegateEvent::StreamStatus { connected, .. }) if !connected => {
                disconnected = true;
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        if speech_started && frames.len() >= 30 && disconnected {
            break;
        }
    }

    let _ = cmd_tx.send(VoicegateCommand::Shutdown);
    let join_result = worker.join();
    if let Err(payload) = &join_result {
        eprintln!("worker thread panicked: {:?}", payload.downcast_ref::<String>().unwrap_or(&String::from("non-string payload")));
    }
    drop(server); // server thread exits on its next accept timeout

    assert!(speech_started, "VAD must detect the sustained tone (frames={})", frames.len());
    assert!(frames.len() >= 30, "expected ~50 TCH/S blocks in 3 s of audio, got {}", frames.len());
    assert!(frames.iter().all(|&len| len == 274), "every frame must be a 274-bit TCH/S block");
}
