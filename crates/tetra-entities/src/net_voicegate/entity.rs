// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! Local announcement voice gate entity.
//!
//! While speech is detected on the configured live stream, the entity asks
//! CMCE to start a BS-originated group call on the dedicated announcement
//! talk group and feeds the downlink with paced 274-bit TCH/S blocks until
//! silence, stream loss, operator stop, or the configured max duration.
//! See `cc_bs/announcement.rs` for the CMCE side of the protocol.

use std::collections::VecDeque;
use std::thread::JoinHandle;

use crossbeam_channel::Receiver;
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{Sap, TdmaTime};
use tetra_saps::control::call_control::CallControl;
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmd::TmdCircuitDataReq;

use crate::MessageQueue;

use crate::entity_trait::TetraEntityTrait;
use crate::net_control::{ControlCommand, ControlEndpoint, ControlResponse};
use crate::net_telemetry::{TelemetryEvent, TelemetrySink};

use super::worker::{VoicegateCommand, VoicegateEvent, spawn_http_worker};

/// How many pending TCH/S blocks to keep while the floor is granted
/// (~30 s at 17.36 blocks/s); overflow drops the oldest to keep latency low.
const FRAME_QUEUE_MAX: usize = 512;
/// Give up on a call start that is not confirmed within this window.
const START_TIMEOUT: i32 = 144; // 2 s in timeslots

/// Radio call state of the voice gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CallState {
    /// No radio call; waiting for speech on the stream.
    Idle,
    /// AnnouncementStart sent; waiting for AnnouncementReady (or Rejected).
    Starting { since: TdmaTime },
    /// Floor granted; feeding DL voice frames.
    Speaking { call_id: u16, ts: u8, usage: u8 },
    /// AnnouncementStop sent; waiting for the call to end.
    Stopping,
}

impl CallState {
    pub fn as_u8(self) -> u8 {
        match self {
            CallState::Idle => 0,
            CallState::Starting { .. } => 1,
            CallState::Speaking { .. } => 2,
            CallState::Stopping => 3,
        }
    }
}

pub struct VoicegateEntity {
    config: SharedConfig,
    dltime: TdmaTime,
    event_rx: Receiver<VoicegateEvent>,
    cmd_tx: crossbeam_channel::Sender<VoicegateCommand>,
    control: Option<ControlEndpoint>,
    telemetry: Option<TelemetrySink>,
    frame_queue: VecDeque<Vec<u8>>,
    state: CallState,
    active_stream: String,
    worker_handle: Option<JoinHandle<()>>,
    last_state: u8,
}

impl VoicegateEntity {
    /// Create the entity and start the production HTTP worker thread.
    /// The config must contain a valid `[announcement]` section.
    pub fn new(config: SharedConfig, control: Option<ControlEndpoint>) -> Self {
        let cfg = config.config().announcement.clone().expect("announcement config required");
        let active_stream = cfg.active_stream.clone();
        let (event_tx, event_rx) = super::worker::event_channel();
        let (cmd_tx, cmd_rx) = super::worker::command_channel();
        let worker_handle = Some(spawn_http_worker(cfg, event_tx, cmd_rx));
        Self {
            config,
            dltime: TdmaTime::default(),
            event_rx,
            cmd_tx,
            control,
            telemetry: None,
            frame_queue: VecDeque::new(),
            state: CallState::Idle,
            active_stream,
            worker_handle,
            last_state: 0,
        }
    }

    /// Test constructor: use injected channels and no worker thread.
    #[doc(hidden)]
    pub fn new_for_test(
        config: SharedConfig,
        control: Option<ControlEndpoint>,
        event_rx: Receiver<VoicegateEvent>,
        cmd_tx: crossbeam_channel::Sender<VoicegateCommand>,
    ) -> Self {
        let cfg = config.config().announcement.clone().expect("announcement config required");
        Self {
            config,
            dltime: TdmaTime::default(),
            event_rx,
            cmd_tx,
            control,
            telemetry: None,
            frame_queue: VecDeque::new(),
            state: CallState::Idle,
            active_stream: cfg.active_stream.clone(),
            worker_handle: None,
            last_state: 0,
        }
    }

    pub fn set_telemetry_sink(&mut self, sink: TelemetrySink) {
        self.telemetry = Some(sink);
    }

    #[doc(hidden)]
    pub fn set_control(&mut self, control: ControlEndpoint) {
        self.control = Some(control);
    }

    pub fn state(&self) -> CallState {
        self.state
    }

    pub fn active_stream(&self) -> &str {
        &self.active_stream
    }

    fn gssi(&self) -> u32 {
        self.config.config().announcement.as_ref().map(|a| a.gssi).unwrap_or(0)
    }

    fn issi(&self) -> u32 {
        self.config.config().announcement.as_ref().map(|a| a.issi).unwrap_or(0)
    }

    fn emit(&mut self) {
        let state = self.state.as_u8();
        if state == self.last_state {
            return;
        }
        self.last_state = state;
        let call_id = match self.state {
            CallState::Speaking { call_id, .. } => Some(call_id),
            _ => None,
        };
        if let Some(sink) = &self.telemetry {
            sink.send(TelemetryEvent::VoicegateState { state, stream: self.active_stream.clone(), call_id });
        }
        // Keep the dashboard state fresh without a dedicated event bus.
        let mut st = self.config.state_write();
        st.voicegate = Some(tetra_config::bluestation::VoicegateRuntimeState {
            state,
            stream: self.active_stream.clone(),
            speaking: matches!(self.state, CallState::Speaking { .. }),
            call_id,
            ts: match self.state {
                CallState::Speaking { ts, .. } => Some(ts),
                _ => None,
            },
        });
    }

    fn start_call(&mut self, queue: &mut MessageQueue) {
        let gssi = self.gssi();
        let issi = self.issi();
        tracing::info!("Voicegate: requesting announcement call start gssi={gssi} issi={issi}");
        queue.push_back(SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Voicegate,
            dest: TetraEntity::Cmce,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementStart { gssi, issi }),
        });
        self.state = CallState::Starting { since: self.dltime };
        self.emit();
    }

    fn stop_call(&mut self, queue: &mut MessageQueue) {
        let gssi = self.gssi();
        tracing::info!("Voicegate: releasing announcement call gssi={gssi}");
        self.frame_queue.clear();
        queue.push_back(SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Voicegate,
            dest: TetraEntity::Cmce,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementStop { gssi }),
        });
        self.state = CallState::Stopping;
        self.emit();
    }

    fn handle_event(&mut self, queue: &mut MessageQueue, event: VoicegateEvent) {
        match event {
            VoicegateEvent::Frame(block) => {
                if matches!(self.state, CallState::Speaking { .. }) {
                    if self.frame_queue.len() >= FRAME_QUEUE_MAX {
                        self.frame_queue.pop_front();
                    }
                    self.frame_queue.push_back(block);
                }
            }
            VoicegateEvent::Pipeline(super::pipeline::PipelineEvent::SpeechStarted) => {
                if matches!(self.state, CallState::Idle) {
                    self.start_call(queue);
                }
            }
            VoicegateEvent::Pipeline(super::pipeline::PipelineEvent::SpeechEnded { reason }) => {
                match self.state {
                    CallState::Speaking { .. } | CallState::Starting { .. } => {
                        tracing::info!("Voicegate: speech ended ({reason:?}), releasing call");
                        self.stop_call(queue);
                    }
                    CallState::Idle | CallState::Stopping => {}
                }
            }
            VoicegateEvent::StreamStatus { stream, connected } => {
                tracing::info!("Voicegate: stream {stream:?} connected={connected}");
                self.active_stream = stream;
            }
        }
    }

    fn rx_call_control(&mut self, queue: &mut MessageQueue, prim: &CallControl) {
        match prim {
            CallControl::AnnouncementReady { gssi, call_id, ts, usage } => {
                match self.state {
                    CallState::Starting { .. } => {
                        self.frame_queue.clear();
                        tracing::info!("Voicegate: floor ready call_id={call_id} gssi={gssi} ts={ts}");
                        self.state = CallState::Speaking { call_id: *call_id, ts: *ts, usage: *usage };
                        self.emit();
                    }
                    // Ready raced with a stop we already issued; tell CMCE
                    // to release the freshly opened call.
                    _ => {
                        tracing::warn!("Voicegate: ready for gssi={gssi} outside starting state; re-sending stop");
                        queue.push_back(SapMsg {
                            sap: Sap::Control,
                            src: TetraEntity::Voicegate,
                            dest: TetraEntity::Cmce,
                            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementStop { gssi: *gssi }),
                        });
                    }
                }
            }
            CallControl::AnnouncementRejected { gssi, reason } => {
                if matches!(self.state, CallState::Starting { .. }) {
                    tracing::info!("Voicegate: announcement start rejected for gssi={gssi}: {reason:?}");
                    self.state = CallState::Idle;
                    self.emit();
                }
            }
            CallControl::AnnouncementEnded { gssi, call_id } => {
                match self.state {
                    CallState::Stopping => {
                        tracing::info!("Voicegate: announcement call {call_id} (gssi={gssi}) ended");
                        self.state = CallState::Idle;
                        self.emit();
                    }
                    CallState::Speaking { .. } => {
                        tracing::warn!("Voicegate: call {call_id} (gssi={gssi}) ended externally; going idle");
                        self.frame_queue.clear();
                        self.state = CallState::Idle;
                        self.emit();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn handle_control(&mut self, queue: &mut MessageQueue, cmd: ControlCommand) {
        match cmd {
            ControlCommand::VoicegateStart { handle } => {
                let (ok, detail) = match self.state {
                    CallState::Idle => {
                        self.start_call(queue);
                        (true, "start requested".to_string())
                    }
                    CallState::Starting { .. } | CallState::Speaking { .. } => (true, "already active".to_string()),
                    CallState::Stopping => (false, "stopping; try again after the call ends".to_string()),
                };
                self.respond(ControlResponse::VoicegateResponse { handle, ok, detail });
            }
            ControlCommand::VoicegateStop { handle } => {
                let (ok, detail) = match self.state {
                    CallState::Speaking { .. } | CallState::Starting { .. } => {
                        let _ = self.cmd_tx.send(VoicegateCommand::ForceStop);
                        (true, "stop requested".to_string())
                    }
                    _ => (false, "no active announcement".to_string()),
                };
                self.respond(ControlResponse::VoicegateResponse { handle, ok, detail });
            }
            ControlCommand::VoicegateSelectStream { handle, label } => {
                let ok = self.cmd_tx.send(VoicegateCommand::SelectStream { label: label.clone() }).is_ok();
                self.respond(ControlResponse::VoicegateResponse {
                    handle,
                    ok,
                    detail: format!("select {label:?}"),
                });
            }
            _ => {
                tracing::warn!("Voicegate: unexpected control command {cmd:?}");
            }
        }
    }

    fn respond(&mut self, response: ControlResponse) {
        if let Some(cep) = &self.control {
            cep.respond(response);
        }
    }
}

impl TetraEntityTrait for VoicegateEntity {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Voicegate
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        if let SapMsgInner::CmceCallControl(prim) = &message.msg {
            self.rx_call_control(queue, prim);
        }
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.dltime = ts;

        // 1) Worker events: frames, VAD, stream status.
        while let Ok(event) = self.event_rx.try_recv() {
            self.handle_event(queue, event);
        }

        // 2) Start timeout: CMCE never confirmed (or rejected was missed).
        if let CallState::Starting { since } = self.state {
            if since.age(ts) >= START_TIMEOUT {
                tracing::warn!("Voicegate: start not confirmed within {START_TIMEOUT} timeslots, going idle");
                self.state = CallState::Idle;
                self.emit();
            }
        }

        // 3) Feed DL voice: one block per timeslot when this is our slot.
        if let CallState::Speaking { ts: call_ts, .. } = self.state {
            if ts.t == call_ts && ts.f != 18 {
                if let Some(block) = self.frame_queue.pop_front() {
                    queue.push_back(SapMsg {
                        sap: Sap::TmdSap,
                        src: TetraEntity::Voicegate,
                        dest: TetraEntity::Umac,
                        msg: SapMsgInner::TmdCircuitDataReq(TmdCircuitDataReq { ts: call_ts, data: block, raw_tch_s_block: None }),
                    });
                }
            }
        }

        // 4) Operator control commands.
        let pending: Vec<ControlCommand> = self
            .control
            .as_ref()
            .map(|cep| {
                let mut cmds = Vec::new();
                while let Some(cmd) = cep.try_recv() {
                    cmds.push(cmd);
                }
                cmds
            })
            .unwrap_or_default();
        for cmd in pending {
            self.handle_control(queue, cmd);
        }
    }
}

impl Drop for VoicegateEntity {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(VoicegateCommand::Shutdown);
        // The worker notices Shutdown on its next decode poll (<= ~250 ms);
        // no join needed during process shutdown.
        self.worker_handle.take();
    }
}
