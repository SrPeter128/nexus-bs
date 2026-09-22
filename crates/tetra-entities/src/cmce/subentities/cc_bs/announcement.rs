// SPDX-FileCopyrightText: 2026 Nexus-BS contributors
// SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0

use super::*;

impl CcBsSubentity {
    /// Voicegate -> CMCE: the live stream has speech; start a BS-originated
    /// group call on the dedicated announcement talk group.
    ///
    /// EN 300 392-2 clause 14.5.2 group call setup, with the SwMI itself as
    /// the calling party (mirrors the network-originated group call path, but
    /// the media source is the local voice gate instead of Brew).
    pub(super) fn rx_announcement_start(&mut self, queue: &mut MessageQueue, gssi: u32, issi: u32) {
        let cfg = self.config.config();
        let Some(ann) = cfg.announcement.as_ref() else {
            tracing::info!("CMCE: rejecting announcement start for gssi={}: feature not configured", gssi);
            self.reject_announcement(queue, gssi, AnnouncementRejectReason::Disabled);
            return;
        };
        if !ann.enabled {
            tracing::info!("CMCE: rejecting announcement start for gssi={}: feature disabled", gssi);
            self.reject_announcement(queue, gssi, AnnouncementRejectReason::Disabled);
            return;
        }
        if gssi != ann.gssi || issi != ann.issi {
            tracing::info!(
                "CMCE: rejecting announcement start for gssi={}/issi={}: does not match configured announcement identity gssi={}/issi={}",
                gssi,
                issi,
                ann.gssi,
                ann.issi
            );
            self.reject_announcement(queue, gssi, AnnouncementRejectReason::WrongIdentity);
            return;
        }
        if !self.has_local_listener(gssi) {
            tracing::info!("CMCE: rejecting announcement start for gssi={}: no local subscriber", gssi);
            self.reject_announcement(queue, gssi, AnnouncementRejectReason::NoLocalListener);
            return;
        }
        if self.active_calls.values().any(|call| call.dest_gssi == gssi)
            || self.pending_announcement_readies.values().any(|pending| pending.dest_gssi == gssi)
        {
            tracing::info!("CMCE: rejecting announcement start for gssi={}: group already in use", gssi);
            self.reject_announcement(queue, gssi, AnnouncementRejectReason::TgBusy);
            return;
        }

        self.fsm_on_announcement_start(queue, gssi, issi);
    }

    fn reject_announcement(&mut self, queue: &mut MessageQueue, gssi: u32, reason: AnnouncementRejectReason) {
        queue.push_back(SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Voicegate,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementRejected { gssi, reason }),
        });
    }

    /// Allocate a circuit and set up the announcement group call. The call
    /// becomes feedable for the voice gate once the D-SETUP reached the air
    /// (see `drain_pending_announcement_readies`).
    pub(super) fn fsm_on_announcement_start(&mut self, queue: &mut MessageQueue, gssi: u32, issi: u32) {
        let occupied_call_ids = self.occupied_call_ids();
        let circuit = match {
            let mut state = self.config.state_write();
            self.circuits.allocate_circuit_with_allocator_duplex_avoiding(
                Direction::Both,
                CommunicationType::P2Mp,
                false,
                &mut state.timeslot_alloc,
                TimeslotOwner::Cmce,
                &occupied_call_ids,
            )
        } {
            Ok(c) => c.clone(),
            Err(err) => {
                tracing::warn!("CMCE: failed to allocate circuit for announcement call gssi={}: {:?}", gssi, err);
                self.reject_announcement(queue, gssi, AnnouncementRejectReason::NoTimeslot);
                return;
            }
        };

        let call_id = circuit.call_id;
        let ts = circuit.ts;
        let usage = circuit.usage;

        tracing::info!(
            "CMCE: starting NEW announcement call gssi={} speaker={} ts={} call_id={}",
            gssi,
            issi,
            ts,
            call_id
        );

        Self::signal_umac_circuit_open_with_secondary(
            queue,
            &circuit,
            None,
            CircuitDlMediaSource::LocalAnnouncement,
            Some(TetraAddress::new(gssi, SsiType::Gssi)),
            vec![TetraAddress::issi(issi)],
        );

        let dest_addr = TetraAddress::new(gssi, SsiType::Gssi);
        let d_setup = DSetup {
            call_identifier: call_id,
            call_time_out: CallTimeout::Infinite,
            hook_method_selection: false,
            simplex_duplex_selection: false,
            basic_service_information: BasicServiceInformation {
                circuit_mode_type: CircuitModeType::TchS,
                encryption_flag: false,
                communication_type: CommunicationType::P2Mp,
                slots_per_frame: None,
                speech_service: Some(0),
            },
            transmission_grant: TransmissionGrant::GrantedToOtherUser,
            transmission_request_permission: false,
            call_priority: 0,
            notification_indicator: None,
            temporary_address: None,
            calling_party_address_ssi: Some(issi),
            calling_party_extension: None,
            external_subscriber_number: None,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };

        self.cached_setups.insert(
            call_id,
            CachedSetup {
                pdu: d_setup,
                dest_addr: dest_addr.clone(),
                resend: true,
                last_resend_reporter: None,
                is_individual: false,
            },
        );
        let d_setup_ref = &self.cached_setups.get(&call_id).unwrap().pdu;

        let setup_reporter = TxReporter::new_unacked();
        let (setup_sdu, setup_chan_alloc) = Self::build_d_setup_prim(d_setup_ref, usage, ts, UlDlAssignment::Both);
        let setup_msg = Self::build_sapmsg(
            setup_sdu,
            Some(setup_chan_alloc),
            dest_addr.clone(),
            Layer2Service::Unacknowledged,
            Some(setup_reporter.clone()),
        );
        queue.push_back(setup_msg);

        // The voice gate enforces max_call_duration and stream-loss handling,
        // and a stalled gate is caught by the UMAC UL-inactivity watchdog, so
        // the radio call itself needs no T310 cap.
        self.active_calls.insert(
            call_id,
            ActiveCall::new_announcement(gssi, issi, ts, usage, self.dltime, CallTimeout::Infinite, 0),
        );

        self.emit(crate::net_telemetry::TelemetryEvent::GroupCallStarted {
            call_id,
            gssi,
            caller_issi: issi,
            ts,
        });

        self.queue_announcement_ready(call_id, issi, gssi, ts, usage, vec![setup_reporter]);
    }

    /// Voicegate -> CMCE: release the announcement speech (silence, stream
    /// loss, operator stop, or max duration). Mirrors the network call end
    /// path: floor release into hangtime, teardown by the hangtime expiry.
    pub(super) fn rx_announcement_stop(&mut self, queue: &mut MessageQueue, gssi: u32) {
        let Some((call_id, call)) = self
            .active_calls
            .iter()
            .find(|(_, c)| c.dest_gssi == gssi)
            .map(|(id, c)| (*id, c.clone()))
        else {
            // Call already gone (hangtime expiry / release) — nothing to do.
            self.cancel_announcement_ready_for_gssi(gssi);
            tracing::debug!("CMCE: announcement stop for gssi={} with no active call", gssi);
            return;
        };

        if self.pending_group_releases.contains_key(&call_id) {
            tracing::debug!("CMCE: announcement stop for pending release call_id={}", call_id);
            self.cancel_announcement_ready(call_id, "announcement group release pending");
            return;
        }

        tracing::info!(
            "CMCE: announcement stop gssi={} call_id={} speaker={} state={:?}",
            gssi,
            call_id,
            call.source_issi,
            call.state()
        );

        self.cancel_announcement_ready(call_id, "announcement speech stopped");

        if matches!(call.state(), GroupCallState::Transmitting) {
            if let Some(active_call) = self.active_calls.get_mut(&call_id) {
                active_call.enter_hangtime(self.dltime);
            }

            self.send_d_tx_ceased_facch(queue, call_id, call.dest_gssi, call.ts, call.usage);
            queue.push_back(SapMsg {
                sap: Sap::Control,
                src: TetraEntity::Cmce,
                dest: TetraEntity::Umac,
                msg: SapMsgInner::CmceCallControl(CallControl::FloorReleased { call_id, ts: call.ts }),
            });
        }

        self.announce_announcement_ended(queue, gssi, call_id);
    }

    fn announce_announcement_ended(&mut self, queue: &mut MessageQueue, gssi: u32, call_id: u16) {
        queue.push_back(SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Voicegate,
            msg: SapMsgInner::CmceCallControl(CallControl::AnnouncementEnded { gssi, call_id }),
        });
    }
}
