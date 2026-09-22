// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

use std::collections::HashMap;

use serde::Deserialize;
use toml::Value;

/// Authentication for one announcement stream.
#[derive(Debug, Clone)]
pub enum CfgAnnouncementAuth {
    /// `Authorization: Bearer <value>` header.
    Bearer(String),
    /// Arbitrary header: `name: value`.
    Header { name: String, value: String },
    /// Query parameter appended to the stream URL: `name=value`.
    Query { name: String, value: String },
}

/// One configured live audio stream (the announcement voice gate's input).
#[derive(Debug, Clone)]
pub struct CfgAnnouncementStream {
    pub label: String,
    pub url: String,
    pub auth: Option<CfgAnnouncementAuth>,
}

#[derive(Deserialize)]
pub struct CfgAnnouncementStreamDto {
    pub label: String,
    pub url: String,
    /// Auth kind: "bearer" | "header" | "query".
    pub auth_kind: Option<String>,
    pub auth_name: Option<String>,
    pub auth_value: Option<String>,
}

/// Local announcement voice gate configuration (dedicated subscribable talk
/// group fed with live external audio, e.g. a public radio stream).
///
/// When the voice gate detects speech on the active stream it asks CMCE to
/// start a BS-originated group call on `gssi` with `issi` as calling party;
/// subscribers to that group receive the live audio as ordinary TETRA group
/// speech until silence, stream loss, or the configured max duration.
#[derive(Debug, Clone)]
pub struct CfgAnnouncement {
    /// Master switch.
    pub enabled: bool,
    /// Dedicated announcement talk group.
    pub gssi: u32,
    /// Calling party ISSI shown on subscriber terminals.
    pub issi: u32,
    /// Configured live streams (at least one).
    pub streams: Vec<CfgAnnouncementStream>,
    /// Label of the stream currently in use.
    pub active_stream: String,
    /// RMS level (dBFS) that must be exceeded for speech start.
    pub vad_start_dbfs: f32,
    /// RMS level (dBFS) below which speech is considered ended (hysteresis).
    pub vad_stop_dbfs: f32,
    /// Sustained voiced time (ms) required before the call starts.
    pub vad_start_ms: u32,
    /// Sustained silence (ms) after voiced audio before the call ends.
    pub silence_timeout_ms: u32,
    /// How long (ms) a stalled/lost stream is tolerated before the call ends.
    pub stream_loss_grace_ms: u32,
    /// Hard cap for a single announcement call.
    pub max_call_duration_secs: u32,
}

#[derive(Deserialize)]
pub struct CfgAnnouncementDto {
    #[serde(default)]
    pub enabled: bool,
    pub gssi: u32,
    pub issi: u32,
    pub streams: Vec<CfgAnnouncementStreamDto>,
    pub active_stream: String,
    #[serde(default = "default_vad_start_dbfs")]
    pub vad_start_dbfs: f32,
    #[serde(default = "default_vad_stop_dbfs")]
    pub vad_stop_dbfs: f32,
    #[serde(default = "default_vad_start_ms")]
    pub vad_start_ms: u32,
    #[serde(default = "default_silence_timeout_ms")]
    pub silence_timeout_ms: u32,
    #[serde(default = "default_stream_loss_grace_ms")]
    pub stream_loss_grace_ms: u32,
    #[serde(default = "default_max_call_duration_secs")]
    pub max_call_duration_secs: u32,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn default_vad_start_dbfs() -> f32 {
    -40.0
}
fn default_vad_stop_dbfs() -> f32 {
    -50.0
}
fn default_vad_start_ms() -> u32 {
    150
}
fn default_silence_timeout_ms() -> u32 {
    4000
}
fn default_stream_loss_grace_ms() -> u32 {
    15000
}
fn default_max_call_duration_secs() -> u32 {
    600
}

pub fn apply_announcement_patch(dto: CfgAnnouncementDto) -> Result<CfgAnnouncement, Box<dyn std::error::Error>> {
    if !dto.extra.is_empty() {
        let mut keys: Vec<&str> = dto.extra.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        return Err(format!("Unrecognized fields in announcement config: {:?}", keys).into());
    }

    if !(1..=0xFFFFFF).contains(&dto.gssi) {
        return Err("announcement.gssi must be 1-0xFFFFFF".into());
    }
    if !(1..=0xFFFFFF).contains(&dto.issi) {
        return Err("announcement.issi must be 1-0xFFFFFF".into());
    }
    if !(30..=3600).contains(&dto.max_call_duration_secs) {
        return Err("announcement.max_call_duration_secs must be 30-3600".into());
    }
    if !(-60.0..=0.0).contains(&dto.vad_start_dbfs) {
        return Err("announcement.vad_start_dbfs must be -60..=0".into());
    }
    if !(-80.0..=0.0).contains(&dto.vad_stop_dbfs) {
        return Err("announcement.vad_stop_dbfs must be -80..=0".into());
    }
    if dto.vad_stop_dbfs >= dto.vad_start_dbfs {
        return Err("announcement.vad_stop_dbfs must be below vad_start_dbfs (hysteresis)".into());
    }
    if !(20..=5000).contains(&dto.vad_start_ms) {
        return Err("announcement.vad_start_ms must be 20-5000".into());
    }
    if !(100..=60000).contains(&dto.silence_timeout_ms) {
        return Err("announcement.silence_timeout_ms must be 100-60000".into());
    }
    if dto.stream_loss_grace_ms > 60000 {
        return Err("announcement.stream_loss_grace_ms must be 0-60000".into());
    }
    if dto.streams.is_empty() {
        return Err("announcement.streams must contain at least one stream".into());
    }
    if dto.streams.len() > 8 {
        return Err("announcement.streams supports at most 8 entries".into());
    }

    let mut streams = Vec::with_capacity(dto.streams.len());
    let mut seen_labels = std::collections::HashSet::new();
    for (i, s) in dto.streams.into_iter().enumerate() {
        if s.label.trim().is_empty() {
            return Err(format!("announcement.streams[{}].label must not be empty", i).into());
        }
        if !seen_labels.insert(s.label.clone()) {
            return Err(format!("announcement.streams[{}]: duplicate label {:?}", i, s.label).into());
        }
        let scheme_ok = s.url.starts_with("https://") || s.url.starts_with("http://");
        if !scheme_ok {
            return Err(format!("announcement.streams[{}].url must start with http:// or https://", i).into());
        }
        if let Some(kind) = s.auth_kind.as_deref() {
            let value = s.auth_value.clone().ok_or_else(|| {
                format!("announcement.streams[{}].auth_value is required when auth_kind is set", i)
            })?;
            let auth = match kind {
                "bearer" => CfgAnnouncementAuth::Bearer(value),
                "header" => {
                    let name = s.auth_name.clone().ok_or_else(|| {
                        format!("announcement.streams[{}].auth_name is required for auth_kind=header", i)
                    })?;
                    CfgAnnouncementAuth::Header { name, value }
                }
                "query" => {
                    let name = s.auth_name.clone().ok_or_else(|| {
                        format!("announcement.streams[{}].auth_name is required for auth_kind=query", i)
                    })?;
                    CfgAnnouncementAuth::Query { name, value }
                }
                other => {
                    return Err(format!(
                        "announcement.streams[{}].auth_kind must be bearer, header, or query (got {:?})",
                        i, other
                    )
                    .into())
                }
            };
            streams.push(CfgAnnouncementStream { label: s.label, url: s.url, auth: Some(auth) });
        } else {
            if s.auth_name.is_some() || s.auth_value.is_some() {
                return Err(format!("announcement.streams[{}]: auth_name/auth_value require auth_kind", i).into());
            }
            streams.push(CfgAnnouncementStream { label: s.label, url: s.url, auth: None });
        }
    }

    if !streams.iter().any(|s| s.label == dto.active_stream) {
        return Err(format!(
            "announcement.active_stream {:?} must match one of the configured stream labels",
            dto.active_stream
        )
        .into());
    }

    Ok(CfgAnnouncement {
        enabled: dto.enabled,
        gssi: dto.gssi,
        issi: dto.issi,
        streams,
        active_stream: dto.active_stream,
        vad_start_dbfs: dto.vad_start_dbfs,
        vad_stop_dbfs: dto.vad_stop_dbfs,
        vad_start_ms: dto.vad_start_ms,
        silence_timeout_ms: dto.silence_timeout_ms,
        stream_loss_grace_ms: dto.stream_loss_grace_ms,
        max_call_duration_secs: dto.max_call_duration_secs,
    })
}
