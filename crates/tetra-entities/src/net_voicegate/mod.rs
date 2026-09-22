// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! Local announcement voice gate.
//!
//! Streams a configured live audio source (e.g. a public radio MP3 stream)
//! into a dedicated subscribable TETRA talk group: the BS detects speech,
//! starts a group call as the talking party, and feeds the downlink with
//! ACELP TCH/S blocks until silence, stream loss, or the max duration.
//!
//! - [`pipeline`] — resample + VAD + TETRA encode (thread-free, testable)
//! - [`worker`] — background stream decode thread
//! - [`entity`] — router entity bridging CMCE call control and RF pacing

pub mod entity;
pub mod pipeline;
pub mod worker;

pub use entity::VoicegateEntity;
