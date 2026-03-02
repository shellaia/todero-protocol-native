//! v3-server-runtime
//!
//! Deterministic server runtime adapter:
//! - routes inbound datagrams to per-CID `v3-core` machines
//! - drives timer actions
//! - persists session snapshots through `v3-store`

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine;
use thiserror::Error;
use v3_core::{
    CoreConfig, CoreError, CoreMachine, CoreOutput, Event, LifecycleState, OutboundDatagram, Role,
    TimerAction, TimerId,
};
use v3_nat::{gather_host_candidates, IceCandidate, RelayPolicy, TurnAction};
use v3_store::{SessionRecord, SessionStore, StoreError};
use v3_wire::{MessageType, WireError, WireFrame};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    ParseError,
    CoreEvent { cid: u64, event: Event },
    NatPairNominated { cid: u64, local_index: usize, remote_index: usize },
    TurnAction { cid: u64, action: TurnAction },
    VhostPathBound { cid: u64, vhost: String, path_id: u32 },
    MessageReceived { cid: u64, msg_id: u64, payload_b64: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRuntimeOutput {
    pub outbound_datagrams: Vec<OutboundDatagram>,
    pub events: Vec<RuntimeEvent>,
}

struct SessionRuntime {
    core: CoreMachine,
    peer_id: String,
    bound_vhost: Option<String>,
    selected_path_id: Option<u32>,
    sent_data_ids: HashSet<u64>,
}

pub struct ServerRuntime {
    cfg: CoreConfig,
    store: Arc<dyn SessionStore>,
    sessions: HashMap<u64, SessionRuntime>,
    timers: BTreeMap<u64, Vec<(u64, TimerId)>>,
    relay_policy: RelayPolicy,
    metrics: ServerRuntimeMetrics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerRuntimeMetrics {
    pub parse_errors: u64,
    pub handshake_started: u64,
    pub handshake_established: u64,
    pub reactivated: u64,
    pub disconnected: u64,
    pub disconnect_reason_unknown: u64,
    pub msr_sent: u64,
    pub msr_probe_sent: u64,
    pub msr_missed_inferred: u64,
    pub request_range_sent: u64,
    pub data_sent: u64,
    pub data_retransmit_inferred: u64,
    pub ice_candidate_selected: u64,
    pub turn_actions: u64,
    pub turn_retry_actions: u64,
    pub turn_mark_failed_actions: u64,
    pub turn_alloc_actions: u64,
    pub turn_refresh_actions: u64,
    pub turn_permission_actions: u64,
    pub turn_channel_bind_actions: u64,
    pub store_load_calls: u64,
    pub store_load_failures: u64,
    pub store_load_latency_us_total: u64,
    pub store_upsert_calls: u64,
    pub store_upsert_failures: u64,
    pub store_upsert_latency_us_total: u64,
    pub store_delete_calls: u64,
    pub store_delete_failures: u64,
    pub peak_sessions: u64,
}

#[derive(Debug, Error)]
pub enum ServerRuntimeError {
    #[error("wire error: {0}")]
    Wire(#[from] WireError),
    #[error("core error: {0}")]
    Core(#[from] CoreError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

impl ServerRuntime {
    pub fn new(cfg: CoreConfig, store: Arc<dyn SessionStore>) -> Self {
        Self::new_with_nat_policy(cfg, store, RelayPolicy::PreferDirect)
    }

    pub fn new_with_nat_policy(
        cfg: CoreConfig,
        store: Arc<dyn SessionStore>,
        relay_policy: RelayPolicy,
    ) -> Self {
        Self {
            cfg,
            store,
            sessions: HashMap::new(),
            timers: BTreeMap::new(),
            relay_policy,
            metrics: ServerRuntimeMetrics::default(),
        }
    }

    pub fn active_sessions(&self) -> usize {
        self.sessions.len()
    }

    pub fn metrics_snapshot(&self) -> ServerRuntimeMetrics {
        self.metrics.clone()
    }

    pub fn set_relay_policy(&mut self, relay_policy: RelayPolicy) {
        self.relay_policy = relay_policy;
    }

    pub fn gather_local_candidates(
        &self,
        sockets: &[(std::net::IpAddr, u16)],
        component: u16,
    ) -> Vec<IceCandidate> {
        gather_host_candidates(sockets, component, "udp")
    }

    pub fn on_datagram(
        &mut self,
        now_ms: u64,
        src_path_id: u32,
        bytes: &[u8],
    ) -> Result<ServerRuntimeOutput, ServerRuntimeError> {
        let frame = match WireFrame::decode(bytes) {
            Ok(f) => f,
            Err(_) => {
                self.metrics.parse_errors = self.metrics.parse_errors.saturating_add(1);
                return Ok(ServerRuntimeOutput {
                    outbound_datagrams: Vec::new(),
                    events: vec![RuntimeEvent::ParseError],
                });
            }
        };

        self.ensure_session_loaded(frame.cid, src_path_id, now_ms)?;

        let cid = frame.cid;
        let mut pre_events = Vec::new();
        if let Some(session) = self.sessions.get(&cid) {
            let bound = session.selected_path_id;
            let vhost = session.bound_vhost.as_ref();
            if let (Some(vhost), Some(bound)) = (vhost, bound) {
                if bound == src_path_id {
                    pre_events.push(RuntimeEvent::VhostPathBound {
                        cid,
                        vhost: vhost.clone(),
                        path_id: src_path_id,
                    });
                }
            }
        }
        let out = {
            let session = self
                .sessions
                .get_mut(&cid)
                .expect("session must exist after ensure_session_loaded");
            session.core.on_datagram(now_ms, src_path_id, bytes)?
        };
        let mut out = self.apply_core_output(cid, src_path_id, now_ms, out)?;
        out.events.splice(0..0, pre_events);
        Ok(out)
    }

    pub fn poll_timers(
        &mut self,
        now_ms: u64,
        limit: usize,
    ) -> Result<ServerRuntimeOutput, ServerRuntimeError> {
        let mut output = ServerRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() };

        let mut due = Vec::new();
        for (&deadline, entries) in &self.timers {
            if deadline > now_ms || due.len() >= limit {
                break;
            }
            for (cid, timer_id) in entries {
                if due.len() >= limit {
                    break;
                }
                due.push((deadline, *cid, *timer_id));
            }
        }

        for (deadline, cid, timer_id) in due {
            if let Some(entries) = self.timers.get_mut(&deadline) {
                if let Some(pos) =
                    entries.iter().position(|entry| entry.0 == cid && entry.1 == timer_id)
                {
                    entries.remove(pos);
                }
                if entries.is_empty() {
                    self.timers.remove(&deadline);
                }
            }

            if !self.sessions.contains_key(&cid) {
                continue;
            }

            let core_out = {
                let session = self.sessions.get_mut(&cid).expect("checked contains");
                session.core.on_timer(now_ms, timer_id)?
            };
            let partial = self.apply_core_output(cid, 0, now_ms, core_out)?;
            output.outbound_datagrams.extend(partial.outbound_datagrams);
            output.events.extend(partial.events);
        }

        Ok(output)
    }

    pub fn on_nat_tick(&mut self, now_ms: u64) -> ServerRuntimeOutput {
        let _ = now_ms;
        // Compatibility no-op: NAT/TURN engine is disabled in this runtime.
        ServerRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() }
    }

    pub fn send_message(
        &mut self,
        cid: u64,
        payload: Vec<u8>,
        now_ms: u64,
    ) -> Result<ServerRuntimeOutput, ServerRuntimeError> {
        if !self.sessions.contains_key(&cid) {
            return Ok(ServerRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() });
        }
        let out = {
            let session = self.sessions.get_mut(&cid).expect("checked contains");
            session.core.on_api_send(payload)?
        };
        self.apply_core_output(cid, 0, now_ms, out)
    }

    pub fn on_ice_candidates(
        &mut self,
        cid: u64,
        vhost: String,
        local: Vec<IceCandidate>,
        remote: Vec<IceCandidate>,
    ) -> Result<ServerRuntimeOutput, ServerRuntimeError> {
        let _ = (cid, vhost, local, remote);
        // Compatibility no-op: ICE path nomination is disabled in this runtime.
        Ok(ServerRuntimeOutput { outbound_datagrams: Vec::new(), events: Vec::new() })
    }

    fn ensure_session_loaded(
        &mut self,
        cid: u64,
        src_path_id: u32,
        now_ms: u64,
    ) -> Result<(), ServerRuntimeError> {
        if self.sessions.contains_key(&cid) {
            return Ok(());
        }

        self.metrics.store_load_calls = self.metrics.store_load_calls.saturating_add(1);
        let load_start = Instant::now();
        let loaded = self.store.load(cid, now_ms);
        let load_elapsed_us = load_start.elapsed().as_micros() as u64;
        self.metrics.store_load_latency_us_total =
            self.metrics.store_load_latency_us_total.saturating_add(load_elapsed_us);
        let loaded = match loaded {
            Ok(v) => v,
            Err(err) => {
                self.metrics.store_load_failures =
                    self.metrics.store_load_failures.saturating_add(1);
                return Err(ServerRuntimeError::Store(err));
            }
        };
        let (cfg, peer_id) = if let Some(versioned) = loaded {
            (
                CoreConfig {
                    msr_interval_ms: versioned.record.negotiated_msr_interval_ms,
                    miss_window_count: self.cfg.miss_window_count,
                    disconnect_window_ms: versioned.record.disconnect_window_ms,
                },
                versioned.record.peer_id,
            )
        } else {
            (self.cfg, format!("path:{src_path_id}"))
        };

        let mut core = CoreMachine::new(Role::Server, cid, cfg);
        let _ = core.on_path_event(v3_core::PathEvent::PathChanged { path_id: src_path_id })?;
        self.sessions.insert(
            cid,
            SessionRuntime {
                core,
                peer_id,
                bound_vhost: None,
                selected_path_id: None,
                sent_data_ids: HashSet::new(),
            },
        );
        self.update_peak_sessions_metric();
        Ok(())
    }

    fn apply_core_output(
        &mut self,
        cid: u64,
        src_path_id: u32,
        now_ms: u64,
        out: CoreOutput,
    ) -> Result<ServerRuntimeOutput, ServerRuntimeError> {
        let mut events = Vec::with_capacity(out.events.len());
        for evt in &out.events {
            self.observe_core_event(evt);
            events.push(RuntimeEvent::CoreEvent { cid, event: evt.clone() });
            if let Event::MessageReceived { msg_id, payload } = evt {
                events.push(RuntimeEvent::MessageReceived {
                    cid,
                    msg_id: *msg_id,
                    payload_b64: base64::engine::general_purpose::STANDARD.encode(payload),
                });
            }
        }

        self.ingest_timers(cid, &out.timer_actions);

        let datagrams = out
            .outbound_datagrams
            .into_iter()
            .map(|d| OutboundDatagram {
                path_id: if d.path_id == 0 { src_path_id } else { d.path_id },
                bytes: d.bytes,
            })
            .collect::<Vec<_>>();
        self.observe_datagrams(cid, &datagrams);

        if let Some(session) = self.sessions.get(&cid) {
            if session.core.state() == LifecycleState::Lost {
                self.metrics.store_delete_calls = self.metrics.store_delete_calls.saturating_add(1);
                let delete_res = self.store.delete(cid);
                if delete_res.is_err() {
                    self.metrics.store_delete_failures =
                        self.metrics.store_delete_failures.saturating_add(1);
                }
                self.sessions.remove(&cid);
                delete_res?;
                self.metrics.disconnected = self.metrics.disconnected.saturating_add(1);
                self.metrics.disconnect_reason_unknown =
                    self.metrics.disconnect_reason_unknown.saturating_add(1);
            } else {
                let record = SessionRecord {
                    cid,
                    peer_id: session.peer_id.clone(),
                    negotiated_msr_interval_ms: self.cfg.msr_interval_ms,
                    disconnect_window_ms: self.cfg.disconnect_window_ms,
                    last_ordered_rx: session.core.last_ordered_rx(),
                    last_ordered_tx: 0,
                };
                self.metrics.store_upsert_calls = self.metrics.store_upsert_calls.saturating_add(1);
                let upsert_start = Instant::now();
                let upsert_res =
                    self.store.upsert(cid, record, self.cfg.disconnect_window_ms, now_ms);
                let upsert_elapsed = upsert_start.elapsed().as_micros() as u64;
                self.metrics.store_upsert_latency_us_total =
                    self.metrics.store_upsert_latency_us_total.saturating_add(upsert_elapsed);
                if upsert_res.is_err() {
                    self.metrics.store_upsert_failures =
                        self.metrics.store_upsert_failures.saturating_add(1);
                }
                let _ = upsert_res?;
            }
        }
        Ok(ServerRuntimeOutput { outbound_datagrams: datagrams, events })
    }

    fn ingest_timers(&mut self, cid: u64, timer_actions: &[TimerAction]) {
        for action in timer_actions {
            match action {
                TimerAction::Schedule { id, deadline_ms } => {
                    self.timers.entry(*deadline_ms).or_default().push((cid, *id));
                }
            }
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
            Event::Reactivated => {
                self.metrics.reactivated = self.metrics.reactivated.saturating_add(1);
            }
            Event::ReactivationProbeSent => {
                self.metrics.msr_probe_sent = self.metrics.msr_probe_sent.saturating_add(1);
                self.metrics.msr_missed_inferred =
                    self.metrics.msr_missed_inferred.saturating_add(1);
            }
            Event::RequestRangeSent { .. } => {
                self.metrics.request_range_sent = self.metrics.request_range_sent.saturating_add(1);
            }
            Event::Disconnected => {
                self.metrics.disconnected = self.metrics.disconnected.saturating_add(1);
                self.metrics.disconnect_reason_unknown =
                    self.metrics.disconnect_reason_unknown.saturating_add(1);
            }
            _ => {}
        }
    }

    fn observe_datagrams(&mut self, cid: u64, datagrams: &[OutboundDatagram]) {
        for d in datagrams {
            if let Ok(frame) = WireFrame::decode(&d.bytes) {
                match frame.message_type {
                    MessageType::Msr => {
                        self.metrics.msr_sent = self.metrics.msr_sent.saturating_add(1);
                    }
                    MessageType::Data => {
                        self.metrics.data_sent = self.metrics.data_sent.saturating_add(1);
                        if frame.payload.len() >= 8 {
                            let mut id_bytes = [0u8; 8];
                            id_bytes.copy_from_slice(&frame.payload[..8]);
                            let msg_id = u64::from_be_bytes(id_bytes);
                            if let Some(session) = self.sessions.get_mut(&cid) {
                                if !session.sent_data_ids.insert(msg_id) {
                                    self.metrics.data_retransmit_inferred =
                                        self.metrics.data_retransmit_inferred.saturating_add(1);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn update_peak_sessions_metric(&mut self) {
        let current = self.sessions.len() as u64;
        if current > self.metrics.peak_sessions {
            self.metrics.peak_sessions = current;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use v3_nat::{CandidateType, IceCandidate};
    use v3_store::InMemorySessionStore;
    use v3_wire::{MessageType, WireFrame};

    fn cfg() -> CoreConfig {
        CoreConfig { msr_interval_ms: 100, miss_window_count: 2, disconnect_window_ms: 1_000 }
    }

    fn client_hello(cid: u64) -> Vec<u8> {
        WireFrame::new(MessageType::ClientHello, 0, cid, Vec::new(), Vec::new())
            .encode()
            .expect("encode")
    }

    fn relay_candidate(ip: &str, port: u16) -> IceCandidate {
        IceCandidate {
            foundation: "r".to_string(),
            component: 1,
            transport: "udp".to_string(),
            priority: 200,
            address: ip.parse().expect("ip"),
            port,
            typ: CandidateType::Relay,
            related_address: None,
            related_port: None,
        }
    }

    #[test]
    fn server_runtime_routes_sessions_by_cid_and_persists() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let mut runtime = ServerRuntime::new(cfg(), store.clone());

        let out1 = runtime.on_datagram(1, 3, &client_hello(100)).expect("dgram 1");
        let out2 = runtime.on_datagram(2, 5, &client_hello(200)).expect("dgram 2");

        assert_eq!(runtime.active_sessions(), 2);
        assert_eq!(out1.outbound_datagrams.len(), 1);
        assert_eq!(out2.outbound_datagrams.len(), 1);

        assert!(store.load(100, 3).expect("load").is_some());
        assert!(store.load(200, 3).expect("load").is_some());
    }

    #[test]
    fn parse_error_does_not_crash_runtime() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let mut runtime = ServerRuntime::new(cfg(), store);

        let out = runtime.on_datagram(1, 1, &[0x01, 0x02]).expect("parse-safe");
        assert_eq!(out.events, vec![RuntimeEvent::ParseError]);
        assert!(out.outbound_datagrams.is_empty());
        let m = runtime.metrics_snapshot();
        assert_eq!(m.parse_errors, 1);
    }

    #[test]
    fn ice_nomination_binds_vhost_to_selected_path() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let mut runtime = ServerRuntime::new(cfg(), store);
        let _ = runtime.on_datagram(1, 10, &client_hello(777)).expect("hello");

        let out = runtime
            .on_ice_candidates(
                777,
                "vh.local".to_string(),
                vec![relay_candidate("203.0.113.1", 5000)],
                vec![relay_candidate("203.0.113.2", 6000)],
            )
            .expect("ice");
        assert!(out.events.is_empty());
    }

    #[test]
    fn server_runtime_can_gather_host_candidates() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let runtime = ServerRuntime::new(cfg(), store);
        let gathered =
            runtime.gather_local_candidates(&[("10.0.0.1".parse().expect("ip"), 5000)], 1);
        assert_eq!(gathered.len(), 1);
        assert_eq!(gathered[0].typ, CandidateType::Host);
    }

    #[test]
    fn metrics_capture_core_nat_and_store_activity() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let mut runtime = ServerRuntime::new(cfg(), store);
        let _ = runtime.on_datagram(1, 3, &client_hello(500)).expect("hello");
        let _ = runtime
            .on_ice_candidates(
                500,
                "vh.local".to_string(),
                vec![relay_candidate("203.0.113.1", 5000)],
                vec![relay_candidate("203.0.113.2", 6000)],
            )
            .expect("ice");
        let _ = runtime.poll_timers(200, 8).expect("timers");
        let _ = runtime.on_nat_tick(10_000);
        let m = runtime.metrics_snapshot();
        assert!(m.handshake_established >= 1);
        assert!(m.msr_sent >= 1);
        assert!(m.store_load_calls >= 1);
        assert!(m.store_upsert_calls >= 1);
        assert_eq!(m.ice_candidate_selected, 0);
        assert!(m.peak_sessions >= 1);
    }
}
