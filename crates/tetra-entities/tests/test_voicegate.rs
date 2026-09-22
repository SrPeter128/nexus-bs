// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

//! Voice gate entity state machine tests (no worker thread; events and
//! CMCE control primitives are injected directly).

mod common;

use tetra_config::bluestation::{CfgAnnouncement, SharedConfig, StackMode};
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{Sap, TdmaTime};
use tetra_entities::net_voicegate::entity::{CallState, VoicegateEntity};
use tetra_entities::net_voicegate::pipeline::{EndReason, PipelineEvent};
use tetra_entities::net_voicegate::worker::{VoicegateCommand, VoicegateEvent};
use tetra_entities::{MessageQueue, TetraEntityTrait};
use tetra_saps::control::call_control::{AnnouncementRejectReason, CallControl};
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};

const ANN_GSSI: u32 = 500;
const ANN_ISSI: u32 = 55_000;

fn announcement_cfg() -> tetra_config::bluestation::StackConfig {
    let mut config = common::ComponentTest::get_default_test_config(StackMode::Bs);
    config.announcement = Some(CfgAnnouncement {
        enabled: true,
        gssi: ANN_GSSI,
        issi: ANN_ISSI,
        streams: vec![],
        active_stream: "main".to_string(),
        vad_start_dbfs: -40.0,
        vad_stop_dbfs: -50.0,
        vad_start_ms: 150,
        silence_timeout_ms: 4000,
        stream_loss_grace_ms: 15000,
        max_call_duration_secs: 600,
    });
    config
}

fn test_entity() -> (
    VoicegateEntity,
    crossbeam_channel::Sender<VoicegateEvent>,
    crossbeam_channel::Receiver<VoicegateCommand>,
) {
    let config = SharedConfig::from_parts(announcement_cfg(), None);
    let (event_tx, event_rx) = tetra_entities::net_voicegate::worker::event_channel();
    let (cmd_tx, cmd_rx) = tetra_entities::net_voicegate::worker::command_channel();
    let entity = VoicegateEntity::new_for_test(config, None, event_rx, cmd_tx);
    (entity, event_tx, cmd_rx)
}

fn tick(entity: &mut VoicegateEntity, ts: TdmaTime) -> Vec<SapMsg> {
    let mut queue = MessageQueue::new();
    entity.tick_start(&mut queue, ts);
    let mut out = Vec::new();
    while let Some(msg) = queue.pop_front() {
        out.push(msg);
    }
    out
}

fn tmd_reqs(msgs: &[SapMsg], ts: u8) -> Vec<Vec<u8>> {
    msgs
        .iter()
        .filter_map(|msg| match &msg.msg {
            SapMsgInner::TmdCircuitDataReq(req) if req.ts == ts && msg.dest == TetraEntity::Umac => Some(req.data.clone()),
            _ => None,
        })
        .collect()
}

fn control_msgs(msgs: &[SapMsg], dest: TetraEntity) -> Vec<CallControl> {
    msgs
        .iter()
        .filter(|msg| msg.dest == dest)
        .filter_map(|msg| match &msg.msg {
            SapMsgInner::CmceCallControl(prim) => Some(prim.clone()),
            _ => None,
        })
        .collect()
}

fn ready_msg(call_id: u16, ts: u8) -> SapMsg {
    SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Cmce,
        dest: TetraEntity::Voicegate,
        msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementReady { gssi: ANN_GSSI, call_id, ts, usage: 4 }),
    }
}

fn speak(entity: &mut VoicegateEntity, event_tx: &crossbeam_channel::Sender<VoicegateEvent>) {
    let _ = event_tx.send(VoicegateEvent::Pipeline(PipelineEvent::SpeechStarted));
    let _ = tick(entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });
    entity.rx_prim(&mut MessageQueue::new(), ready_msg(42, 2));
}

#[test]
fn voicegate_speech_start_requests_call_and_ready_feeds_frames() {
    let (mut entity, event_tx, _cmd_rx) = test_entity();
    assert_eq!(entity.state(), CallState::Idle);

    // 1) Speech detected -> AnnouncementStart to CMCE.
    let _ = event_tx.send(VoicegateEvent::Pipeline(PipelineEvent::SpeechStarted));
    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });
    assert_eq!(entity.state(), CallState::Starting { since: TdmaTime { h: 0, m: 0, f: 1, t: 1 } });
    assert!(control_msgs(&msgs, TetraEntity::Cmce)
        .iter()
        .any(|p| matches!(p, CallControl::AnnouncementStart { gssi, issi } if *gssi == ANN_GSSI && *issi == ANN_ISSI)));

    // 2) CMCE confirms the floor -> Speaking.
    entity.rx_prim(&mut MessageQueue::new(), ready_msg(42, 2));
    assert_eq!(entity.state(), CallState::Speaking { call_id: 42, ts: 2, usage: 4 });

    // 3) Frames on the right timeslot are fed to UMAC as TmdCircuitDataReq.
    let block: Vec<u8> = vec![0u8; 274];
    let _ = event_tx.send(VoicegateEvent::Frame(block.clone()));
    let _ = event_tx.send(VoicegateEvent::Frame(block.clone()));

    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 2 });
    assert_eq!(tmd_reqs(&msgs, 2).len(), 1, "one block per timeslot");
    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 2, t: 2 });
    assert_eq!(tmd_reqs(&msgs, 2).len(), 1);

    // Frame 18 is not fed (extended frame).
    let _ = event_tx.send(VoicegateEvent::Frame(block.clone()));
    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 18, t: 2 });
    assert_eq!(tmd_reqs(&msgs, 2).len(), 0, "frame 18 must not carry announcement media");
}

#[test]
fn voicegate_speech_end_stops_call_and_ended_returns_idle() {
    let (mut entity, event_tx, _cmd_rx) = test_entity();
    speak(&mut entity, &event_tx);
    assert_eq!(entity.state(), CallState::Speaking { call_id: 42, ts: 2, usage: 4 });

    // Silence timeout -> AnnouncementStop to CMCE, state Stopping.
    let _ = event_tx.send(VoicegateEvent::Pipeline(PipelineEvent::SpeechEnded { reason: EndReason::Silence }));
    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });
    assert_eq!(entity.state(), CallState::Stopping);
    assert!(control_msgs(&msgs, TetraEntity::Cmce)
        .iter()
        .any(|p| matches!(p, CallControl::AnnouncementStop { gssi } if *gssi == ANN_GSSI)));

    // CMCE confirms the end -> Idle.
    entity.rx_prim(
        &mut MessageQueue::new(),
        SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Voicegate,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementEnded { gssi: ANN_GSSI, call_id: 42 }),
        },
    );
    assert_eq!(entity.state(), CallState::Idle);
}

#[test]
fn voicegate_rejection_goes_idle_and_late_ready_re_stops() {
    let (mut entity, event_tx, _cmd_rx) = test_entity();

    let _ = event_tx.send(VoicegateEvent::Pipeline(PipelineEvent::SpeechStarted));
    let _ = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });

    // CMCE rejects (e.g. no local listener) -> back to Idle.
    entity.rx_prim(
        &mut MessageQueue::new(),
        SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Voicegate,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementRejected {
                gssi: ANN_GSSI,
                reason: AnnouncementRejectReason::NoLocalListener,
            }),
        },
    );
    assert_eq!(entity.state(), CallState::Idle);

    // A late Ready arriving while idle must re-send a stop so CMCE does not
    // leave an orphaned floor granted.
    let mut queue = MessageQueue::new();
    entity.rx_prim(&mut queue, ready_msg(9, 2));
    assert_eq!(entity.state(), CallState::Idle);
    let msgs: Vec<SapMsg> = {
        let mut out = Vec::new();
        while let Some(msg) = queue.pop_front() {
            out.push(msg);
        }
        out
    };
    assert!(control_msgs(&msgs, TetraEntity::Cmce)
        .iter()
        .any(|p| matches!(p, CallControl::AnnouncementStop { gssi } if *gssi == ANN_GSSI)));
}

#[test]
fn voicegate_start_timeout_goes_idle() {
    let (mut entity, event_tx, _cmd_rx) = test_entity();

    let _ = event_tx.send(VoicegateEvent::Pipeline(PipelineEvent::SpeechStarted));
    let _ = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });
    assert_eq!(entity.state(), CallState::Starting { since: TdmaTime { h: 0, m: 0, f: 1, t: 1 } });

    // Tick past the 2 s (144 timeslot) start timeout without a Ready.
    let mut ts = TdmaTime { h: 0, m: 0, f: 2, t: 2 };
    for _ in 0..150 {
        let _ = tick(&mut entity, ts);
        ts = ts.add_timeslots(1);
    }
    assert_eq!(entity.state(), CallState::Idle);
}

#[test]
fn voicegate_control_stop_sends_force_stop_to_worker() {
    let (mut entity, event_tx, cmd_rx) = test_entity();
    speak(&mut entity, &event_tx);
    assert_eq!(entity.state(), CallState::Speaking { call_id: 42, ts: 2, usage: 4 });

    let (dispatcher, endpoint) = tetra_entities::net_control::make_control_link();
    let _ = dispatcher
        .send(tetra_entities::net_control::ControlCommand::VoicegateStop { handle: 1 });
    entity.set_control(endpoint);

    let msgs = tick(&mut entity, TdmaTime { h: 0, m: 0, f: 1, t: 1 });
    let cmd = cmd_rx.try_recv().expect("ForceStop must reach the worker channel");
    assert!(matches!(cmd, VoicegateCommand::ForceStop));

    // The response is reported back over the control link.
    let response = dispatcher.try_recv_response().expect("VoicegateStop response");
    assert!(matches!(
        response,
        tetra_entities::net_control::ControlResponse::VoicegateResponse { handle: 1, ok: true, .. }
    ));
    let _ = msgs;
}
