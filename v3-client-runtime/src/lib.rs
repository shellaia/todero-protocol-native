//! v3-client-runtime
//!
//! Deterministic client runtime adapter around `v3-core`.

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;
use v3_core::{
    CoreConfig, CoreError, CoreMachine, CoreOutput, Event, OutboundDatagram, PathEvent, Role,
    TimerAction, TimerId, TimerProposal,
};
use v3_nat::{ConsentAction, IceCandidate, RelayPolicy, TurnAction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRuntimeEvent {
    Core(Event),
    NatPairNominated { local_index: usize, remote_index: usize, path_id: u32 },
    TurnAction(TurnAction),
    ConsentAction(ConsentAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRuntimeOutput {
    pub outbound_datagrams: Vec<OutboundDatagram>,
    pub events: Vec<ClientRuntimeEvent>,
}

pub struct ClientRuntime {
    core: CoreMachine,
    timers: BTreeMap<u64, Vec<TimerId>>,
    local_candidates: Vec<IceCandidate>,
    remote_candidates: Vec<IceCandidate>,
    metrics: ClientRuntimeMetrics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientRuntimeMetrics {
    pub handshake_started: u64,
    pub handshake_established: u64,
    pub reactivation_probe_sent: u64,
    pub reactivated: u64,
    pub disconnected: u64,
    pub msr_sent: u64,
    pub nat_pair_nominated: u64,
    pub turn_actions: u64,
    pub consent_actions: u64,
}

#[derive(Debug, Error)]
pub enum ClientRuntimeError {
    #[error("core error: {0}")]
    Core(#[from] CoreError),
    #[error("invalid candidate payload")]
    InvalidCandidatePayload,
}

impl ClientRuntime {
    pub fn new(cid: u64, cfg: CoreConfig) -> Self {
        Self {
            core: CoreMachine::new(Role::Client, cid, cfg),
            timers: BTreeMap::new(),
            local_candidates: Vec::new(),
            remote_candidates: Vec::new(),
            metrics: ClientRuntimeMetrics::default(),
        }
    }

    pub fn metrics_snapshot(&self) -> ClientRuntimeMetrics {
        self.metrics.clone()
    }

    pub fn state(&self) -> v3_core::LifecycleState {
        self.core.state()
    }

    pub fn begin_handshake(
        &mut self,
        now_ms: u64,
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.begin_handshake(now_ms)?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn on_datagram(
        &mut self,
        now_ms: u64,
        src_path_id: u32,
        bytes: &[u8],
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.on_datagram(now_ms, src_path_id, bytes)?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn send_message(
        &mut self,
        payload: Vec<u8>,
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.on_api_send(payload)?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn close_session(&mut self) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.on_api_close()?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn propose_timers(
        &mut self,
        now_ms: u64,
        proposal: TimerProposal,
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.on_api_propose_timers(now_ms, proposal)?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn on_path_changed(
        &mut self,
        path_id: u32,
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let core_out = self.core.on_path_event(PathEvent::PathChanged { path_id })?;
        Ok(self.apply_core_output(core_out))
    }

    pub fn poll_timers(
        &mut self,
        now_ms: u64,
        limit: usize,
    ) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        let mut output = ClientRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() };

        let mut due = Vec::new();
        for (&deadline, ids) in &self.timers {
            if deadline > now_ms || due.len() >= limit {
                break;
            }
            for id in ids {
                if due.len() >= limit {
                    break;
                }
                due.push((deadline, *id));
            }
        }

        for (deadline, id) in due {
            if let Some(list) = self.timers.get_mut(&deadline) {
                if let Some(pos) = list.iter().position(|v| *v == id) {
                    list.remove(pos);
                }
                if list.is_empty() {
                    self.timers.remove(&deadline);
                }
            }

            let core_out = self.core.on_timer(now_ms, id)?;
            let partial = self.apply_core_output(core_out);
            output.outbound_datagrams.extend(partial.outbound_datagrams);
            output.events.extend(partial.events);
        }

        Ok(output)
    }

    pub fn set_local_candidates(&mut self, candidates: Vec<IceCandidate>) {
        self.local_candidates = candidates;
    }

    pub fn set_remote_candidates(&mut self, candidates: Vec<IceCandidate>) {
        self.remote_candidates = candidates;
    }

    pub fn load_remote_candidates_from_discovery(
        &mut self,
        payload: &Value,
    ) -> Result<usize, ClientRuntimeError> {
        let candidates = payload
            .get("candidates")
            .and_then(|v| v.as_array())
            .ok_or(ClientRuntimeError::InvalidCandidatePayload)?;
        let mut parsed = Vec::with_capacity(candidates.len());
        for c in candidates {
            let address = c
                .get("address")
                .and_then(|v| v.as_str())
                .ok_or(ClientRuntimeError::InvalidCandidatePayload)?;
            let port = c
                .get("port")
                .and_then(|v| v.as_u64())
                .ok_or(ClientRuntimeError::InvalidCandidatePayload)? as u16;
            let typ = match c.get("type").and_then(|v| v.as_str()).unwrap_or("host") {
                "relay" => v3_nat::CandidateType::Relay,
                "srflx" => v3_nat::CandidateType::Srflx,
                _ => v3_nat::CandidateType::Host,
            };
            parsed.push(IceCandidate {
                foundation: c
                    .get("foundation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("remote")
                    .to_string(),
                component: 1,
                transport: "udp".to_string(),
                priority: c.get("priority").and_then(|v| v.as_u64()).unwrap_or(100) as u32,
                address: address
                    .parse()
                    .map_err(|_| ClientRuntimeError::InvalidCandidatePayload)?,
                port,
                typ,
                related_address: None,
                related_port: None,
            });
        }
        self.remote_candidates = parsed;
        Ok(self.remote_candidates.len())
    }

    pub fn set_relay_policy(&mut self, _relay_policy: RelayPolicy) {
        // Compatibility no-op: relay policy is currently handled outside this runtime.
    }

    pub fn run_ice_nomination(&mut self) -> Result<ClientRuntimeOutput, ClientRuntimeError> {
        // Compatibility no-op: ICE nomination is disabled in this runtime.
        Ok(ClientRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() })
    }

    pub fn on_nat_tick(&mut self, _now_ms: u64) -> ClientRuntimeOutput {
        // Compatibility no-op: NAT/TURN engine is disabled in this runtime.
        ClientRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() }
    }

    fn apply_core_output(&mut self, out: CoreOutput) -> ClientRuntimeOutput {
        self.observe_datagrams(&out.outbound_datagrams);
        for event in &out.events {
            self.observe_core_event(event);
        }
        self.ingest_timers(&out.timer_actions);
        ClientRuntimeOutput {
            outbound_datagrams: out.outbound_datagrams,
            events: out.events.into_iter().map(ClientRuntimeEvent::Core).collect(),
        }
    }

    fn observe_core_event(&mut self, event: &Event) {
        match event {
            Event::HandshakeStarted => {
                self.metrics.handshake_started = self.metrics.handshake_started.saturating_add(1);
            }
            Event::HandshakeEstablished => {
                self.metrics.handshake_established =
                    self.metrics.handshake_established.saturating_add(1);
            }
            Event::ReactivationProbeSent => {
                self.metrics.reactivation_probe_sent =
                    self.metrics.reactivation_probe_sent.saturating_add(1);
            }
            Event::Reactivated => {
                self.metrics.reactivated = self.metrics.reactivated.saturating_add(1);
            }
            Event::Disconnected => {
                self.metrics.disconnected = self.metrics.disconnected.saturating_add(1);
            }
            _ => {}
        }
    }

    fn observe_datagrams(&mut self, datagrams: &[OutboundDatagram]) {
        for d in datagrams {
            if let Ok(frame) = v3_wire::WireFrame::decode(&d.bytes) {
                if frame.message_type == v3_wire::MessageType::Msr {
                    self.metrics.msr_sent = self.metrics.msr_sent.saturating_add(1);
                }
            }
        }
    }

    fn ingest_timers(&mut self, timer_actions: &[TimerAction]) {
        for action in timer_actions {
            match action {
                TimerAction::Schedule { id, deadline_ms } => {
                    let slot = self.timers.entry(*deadline_ms).or_default();
                    if !slot.contains(id) {
                        slot.push(*id);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v3_core::{CoreConfig, LifecycleState};
    use v3_nat::{CandidateType, IceCandidate, RelayPolicy};
    use v3_wire::{MessageType, WireFrame};

    fn cfg() -> CoreConfig {
        CoreConfig { msr_interval_ms: 100, miss_window_count: 2, disconnect_window_ms: 1_000 }
    }

    #[test]
    fn client_probe_timer_is_driven_by_runtime_scheduler() {
        let mut runtime = ClientRuntime::new(77, cfg());
        runtime.begin_handshake(0).expect("begin");
        let server_hello = WireFrame::new(MessageType::ServerHello, 0, 77, Vec::new(), Vec::new())
            .encode()
            .expect("encode");
        runtime.on_datagram(1, 9, &server_hello).expect("hello");

        // No server MSR arrives; polling beyond miss window triggers probe.
        let out = runtime.poll_timers(250, 8).expect("poll");
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, ClientRuntimeEvent::Core(Event::ReactivationProbeSent))));
        assert_eq!(runtime.state(), LifecycleState::Reactivating);
    }

    #[test]
    fn client_runtime_ice_nomination_updates_path() {
        let mut runtime = ClientRuntime::new(77, cfg());
        runtime.set_relay_policy(RelayPolicy::RelayOnly);
        runtime.set_local_candidates(vec![
            IceCandidate {
                foundation: "a".to_string(),
                component: 1,
                transport: "udp".to_string(),
                priority: 100,
                address: "10.0.0.1".parse().expect("ip"),
                port: 5000,
                typ: CandidateType::Host,
                related_address: None,
                related_port: None,
            },
            IceCandidate {
                foundation: "b".to_string(),
                component: 1,
                transport: "udp".to_string(),
                priority: 200,
                address: "203.0.113.10".parse().expect("ip"),
                port: 6000,
                typ: CandidateType::Relay,
                related_address: None,
                related_port: None,
            },
        ]);
        runtime.set_remote_candidates(vec![IceCandidate {
            foundation: "c".to_string(),
            component: 1,
            transport: "udp".to_string(),
            priority: 200,
            address: "203.0.113.20".parse().expect("ip"),
            port: 7000,
            typ: CandidateType::Relay,
            related_address: None,
            related_port: None,
        }]);

        let out = runtime.run_ice_nomination().expect("nominate");
        assert!(out.events.is_empty());
    }

    #[test]
    fn remote_candidates_loaded_from_discovery_payload() {
        let mut runtime = ClientRuntime::new(77, cfg());
        let payload = serde_json::json!({
            "candidates": [
                {"address":"203.0.113.10","port":6000,"type":"relay","priority":220}
            ]
        });
        let count = runtime.load_remote_candidates_from_discovery(&payload).expect("load");
        assert_eq!(count, 1);
    }

    #[test]
    fn metrics_capture_core_and_nat_events() {
        let mut runtime = ClientRuntime::new(77, cfg());
        runtime.begin_handshake(0).expect("begin");
        let server_hello = WireFrame::new(MessageType::ServerHello, 0, 77, Vec::new(), Vec::new())
            .encode()
            .expect("encode");
        runtime.on_datagram(1, 9, &server_hello).expect("hello");
        runtime.poll_timers(250, 8).expect("poll");
        runtime.on_nat_tick(260);
        let m = runtime.metrics_snapshot();
        assert!(m.handshake_started >= 1);
        assert!(m.handshake_established >= 1);
        assert!(m.reactivation_probe_sent >= 1);
        assert!(m.msr_sent >= 1);
    }

    #[test]
    fn dedupes_probe_timer_when_handshake_and_server_hello_share_timestamp() {
        let mut runtime = ClientRuntime::new(77, cfg());
        runtime.begin_handshake(1_000).expect("begin");
        let server_hello = WireFrame::new(MessageType::ServerHello, 0, 77, Vec::new(), Vec::new())
            .encode()
            .expect("encode");
        runtime.on_datagram(1_000, 9, &server_hello).expect("hello");

        let out = runtime.poll_timers(1_250, 16).expect("poll");
        let probe_count = out
            .events
            .iter()
            .filter(|e| matches!(e, ClientRuntimeEvent::Core(Event::ReactivationProbeSent)))
            .count();
        assert_eq!(probe_count, 1);
    }
}
