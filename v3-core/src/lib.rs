//! v3-core
//!
//! Deterministic protocol state machine for Todero V3.
//! No sockets, no threads, and no wall-clock reads are performed here.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use v3_wire::{MessageType, Tlv, WireFrame};

const TLV_TIMER_PROPOSAL: u8 = 0x30;
const TLV_TIMER_ACK: u8 = 0x31;
const TLV_MSR_SIGNAL: u8 = 0x32;
const CLIENT_KEEPALIVE_EVERY_SERVER_MSR: u32 = 4;
const SERVER_MAX_CONSECUTIVE_SERVER_MSR_WITHOUT_CLIENT_MSR: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    PreHandshake,
    Established,
    Reactivating,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimerId {
    ServerMsr,
    ClientProbe,
    Disconnect,
    RenegotiationTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathEvent {
    PathChanged { path_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundDatagram {
    pub path_id: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    HandshakeStarted,
    HandshakeEstablished,
    ReactivationProbeSent,
    Reactivated,
    Disconnected,
    QueueAdvertised { next_pending_id: u64 },
    RequestRangeSent { start: u64, end: u64 },
    MessageDelivered { msg_id: u64 },
    MessageReceived { msg_id: u64, payload: Vec<u8> },
    TimerRenegotiationProposed { proposal_id: u32 },
    TimerRenegotiationCommitted { proposal_id: u32 },
    TimerRenegotiationRejected { proposal_id: u32 },
    TimerRenegotiationRolledBack { proposal_id: u32 },
    ParseError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerAction {
    Schedule { id: TimerId, deadline_ms: u64 },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoreOutput {
    pub outbound_datagrams: Vec<OutboundDatagram>,
    pub events: Vec<Event>,
    pub timer_actions: Vec<TimerAction>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoreConfig {
    pub msr_interval_ms: u64,
    pub miss_window_count: u64,
    pub disconnect_window_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerProposal {
    pub msr_interval_ms: u64,
    pub miss_window_count: u64,
    pub disconnect_window_ms: u64,
}

impl TimerProposal {
    fn to_config(self) -> CoreConfig {
        CoreConfig {
            msr_interval_ms: self.msr_interval_ms,
            miss_window_count: self.miss_window_count,
            disconnect_window_ms: self.disconnect_window_ms,
        }
    }
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self { msr_interval_ms: 15_000, miss_window_count: 3, disconnect_window_ms: 180_000 }
    }
}

#[derive(Debug)]
pub struct CoreMachine {
    role: Role,
    cid: u64,
    state: LifecycleState,
    cfg: CoreConfig,
    current_path_id: u32,
    next_outbound_id: u64,
    outbound_messages: BTreeMap<u64, Vec<u8>>,
    received_ids: BTreeSet<u64>,
    received_buffer: BTreeMap<u64, Vec<u8>>,
    received_fragments: BTreeMap<u64, FragmentAccumulator>,
    last_ordered_rx: u64,
    last_requested_end: u64,
    last_server_msr_at_ms: Option<u64>,
    disconnect_deadline_ms: Option<u64>,
    client_server_msr_rx_since_last_keepalive: u32,
    server_consecutive_msr_without_client_msr: u32,
    next_proposal_id: u32,
    pending_local_proposal: Option<PendingRenegotiation>,
}

#[derive(Debug, Clone, Copy)]
struct PendingRenegotiation {
    proposal_id: u32,
    proposal: TimerProposal,
    expires_at_ms: u64,
}

#[derive(Debug, Default)]
struct FragmentAccumulator {
    total: u16,
    chunks: BTreeMap<u16, Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsrSource {
    ControlMsrFrame,
    PiggybackTlv,
}

const MAX_DATA_PAYLOAD_PER_FRAME: usize = 1000;
const FRAGMENT_HEADER_LEN: usize = 12; // msg_id(8) + frag_idx(2) + frag_total(2)

impl CoreMachine {
    pub fn new(role: Role, cid: u64, cfg: CoreConfig) -> Self {
        Self {
            role,
            cid,
            state: LifecycleState::PreHandshake,
            cfg,
            current_path_id: 0,
            next_outbound_id: 1,
            outbound_messages: BTreeMap::new(),
            received_ids: BTreeSet::new(),
            received_buffer: BTreeMap::new(),
            received_fragments: BTreeMap::new(),
            last_ordered_rx: 0,
            last_requested_end: 0,
            last_server_msr_at_ms: None,
            disconnect_deadline_ms: None,
            client_server_msr_rx_since_last_keepalive: 0,
            server_consecutive_msr_without_client_msr: 0,
            next_proposal_id: 1,
            pending_local_proposal: None,
        }
    }

    pub fn state(&self) -> LifecycleState {
        self.state
    }

    pub fn last_ordered_rx(&self) -> u64 {
        self.last_ordered_rx
    }

    pub fn begin_handshake(&mut self, _now_ms: u64) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();
        if self.role == Role::Client {
            out.outbound_datagrams.push(OutboundDatagram {
                path_id: self.current_path_id,
                bytes: encode_frame(MessageType::ClientHello, self.cid, &[])?,
            });
            out.events.push(Event::HandshakeStarted);
        }
        Ok(out)
    }

    pub fn on_datagram(
        &mut self,
        now_ms: u64,
        src_path_id: u32,
        bytes: &[u8],
    ) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();
        let frame = match WireFrame::decode(bytes) {
            Ok(v) => v,
            Err(_) => {
                out.events.push(Event::ParseError);
                return Ok(out);
            }
        };

        if self.cid != 0 && frame.cid != self.cid {
            return Err(CoreError::CidMismatch { expected: self.cid, got: frame.cid });
        }

        self.current_path_id = src_path_id;
        self.touch_disconnect_deadline(now_ms);
        self.on_valid_peer_datagram(now_ms);

        match frame.message_type {
            MessageType::ClientHello if self.role == Role::Server => {
                if self.state == LifecycleState::PreHandshake {
                    self.state = LifecycleState::Established;
                    out.events.push(Event::HandshakeEstablished);
                    out.outbound_datagrams.push(OutboundDatagram {
                        path_id: src_path_id,
                        bytes: encode_frame(MessageType::ServerHello, self.cid, &[])?,
                    });
                    self.schedule_server_timers(now_ms, &mut out);
                }
            }
            MessageType::ServerHello if self.role == Role::Client => {
                if self.state == LifecycleState::PreHandshake
                    || self.state == LifecycleState::Reactivating
                {
                    let was_reactivating = self.state == LifecycleState::Reactivating;
                    self.state = LifecycleState::Established;
                    self.last_server_msr_at_ms = Some(now_ms);
                    out.outbound_datagrams.push(OutboundDatagram {
                        path_id: src_path_id,
                        bytes: encode_frame(MessageType::HelloAck, self.cid, &[])?,
                    });
                    out.events.push(if was_reactivating {
                        Event::Reactivated
                    } else {
                        Event::HandshakeEstablished
                    });
                    self.schedule_client_timers(now_ms, &mut out);
                }
            }
            MessageType::HelloAck if self.role == Role::Server => {
                if self.state == LifecycleState::PreHandshake {
                    self.state = LifecycleState::Established;
                    out.events.push(Event::HandshakeEstablished);
                    self.schedule_server_timers(now_ms, &mut out);
                }
            }
            MessageType::Msr => {
                self.handle_msr(now_ms, src_path_id, &frame, &mut out)?;
            }
            MessageType::RequestRange if self.state == LifecycleState::Established => {
                self.handle_request_range(src_path_id, &frame.payload, &mut out)?;
            }
            MessageType::Data if self.state == LifecycleState::Established => {
                self.handle_data(&frame.payload, &mut out)?;
            }
            MessageType::Fragment if self.state == LifecycleState::Established => {
                self.handle_fragment(&frame.payload, &mut out)?;
            }
            MessageType::Close | MessageType::Error => {
                self.state = LifecycleState::Lost;
                out.events.push(Event::Disconnected);
            }
            _ => {}
        }

        if frame.message_type != MessageType::Msr {
            if let Some((_acked, next_pending)) = find_msr_signal_tlv(&frame.tlvs) {
                self.handle_msr_signal(
                    now_ms,
                    src_path_id,
                    next_pending,
                    MsrSource::PiggybackTlv,
                    &mut out,
                )?;
            }
        }

        Ok(out)
    }

    pub fn on_timer(&mut self, now_ms: u64, timer_id: TimerId) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();

        match timer_id {
            TimerId::ServerMsr
                if self.role == Role::Server && self.state == LifecycleState::Established =>
            {
                self.server_consecutive_msr_without_client_msr =
                    self.server_consecutive_msr_without_client_msr.saturating_add(1);
                if self.server_consecutive_msr_without_client_msr
                    > SERVER_MAX_CONSECUTIVE_SERVER_MSR_WITHOUT_CLIENT_MSR
                {
                    self.state = LifecycleState::Lost;
                    out.events.push(Event::Disconnected);
                    return Ok(out);
                }
                let msr = encode_msr_payload(self.last_ordered_rx, 0);
                out.outbound_datagrams.push(OutboundDatagram {
                    path_id: self.current_path_id,
                    bytes: encode_frame(MessageType::Msr, self.cid, &msr)?,
                });
                self.touch_disconnect_deadline(now_ms);
                out.timer_actions.push(TimerAction::Schedule {
                    id: TimerId::ServerMsr,
                    deadline_ms: now_ms + self.cfg.msr_interval_ms,
                });
                out.timer_actions.push(TimerAction::Schedule {
                    id: TimerId::Disconnect,
                    deadline_ms: now_ms + self.cfg.disconnect_window_ms,
                });
            }
            TimerId::ClientProbe if self.role == Role::Client => {
                if self.state == LifecycleState::Established
                    || self.state == LifecycleState::Reactivating
                {
                    let since_last = now_ms.saturating_sub(self.last_server_msr_at_ms.unwrap_or(0));
                    let miss_window =
                        self.cfg.msr_interval_ms.saturating_mul(self.cfg.miss_window_count);
                    if since_last > miss_window {
                        self.state = LifecycleState::Reactivating;
                        let msr = encode_msr_payload(0, self.max_pending_id());
                        out.outbound_datagrams.push(OutboundDatagram {
                            path_id: self.current_path_id,
                            bytes: encode_frame(MessageType::Msr, self.cid, &msr)?,
                        });
                        out.events.push(Event::ReactivationProbeSent);
                    }
                    out.timer_actions.push(TimerAction::Schedule {
                        id: TimerId::ClientProbe,
                        deadline_ms: now_ms + self.cfg.msr_interval_ms,
                    });
                }
            }
            TimerId::Disconnect => {
                if let Some(deadline) = self.disconnect_deadline_ms {
                    if now_ms >= deadline && self.state != LifecycleState::Lost {
                        self.state = LifecycleState::Lost;
                        out.events.push(Event::Disconnected);
                    } else {
                        out.timer_actions.push(TimerAction::Schedule {
                            id: TimerId::Disconnect,
                            deadline_ms: deadline,
                        });
                    }
                }
            }
            TimerId::RenegotiationTimeout => {
                if let Some(pending) = self.pending_local_proposal {
                    if now_ms >= pending.expires_at_ms {
                        self.pending_local_proposal = None;
                        out.events.push(Event::TimerRenegotiationRolledBack {
                            proposal_id: pending.proposal_id,
                        });
                    }
                }
            }
            _ => {}
        }

        Ok(out)
    }

    pub fn on_api_send(&mut self, payload: Vec<u8>) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();
        let msg_id = self.next_outbound_id;
        self.next_outbound_id += 1;
        self.outbound_messages.insert(msg_id, payload);

        if self.state == LifecycleState::Established {
            if self.role == Role::Client {
                let pending = self.max_pending_id();
                let msr = encode_msr_payload(0, pending);
                out.outbound_datagrams.push(OutboundDatagram {
                    path_id: self.current_path_id,
                    bytes: encode_frame(MessageType::Msr, self.cid, &msr)?,
                });
                out.events.push(Event::QueueAdvertised { next_pending_id: pending });
            } else {
                // Server push-anytime trigger via urgent MSR.
                let msr = encode_msr_payload(self.last_ordered_rx, self.max_pending_id());
                out.outbound_datagrams.push(OutboundDatagram {
                    path_id: self.current_path_id,
                    bytes: encode_frame(MessageType::Msr, self.cid, &msr)?,
                });
            }
        }

        Ok(out)
    }

    pub fn on_api_close(&mut self) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();
        if self.state == LifecycleState::Lost {
            return Ok(out);
        }
        out.outbound_datagrams.push(OutboundDatagram {
            path_id: self.current_path_id,
            bytes: encode_frame(MessageType::Close, self.cid, &[])?,
        });
        self.state = LifecycleState::Lost;
        out.events.push(Event::Disconnected);
        Ok(out)
    }

    pub fn on_api_propose_timers(
        &mut self,
        now_ms: u64,
        proposal: TimerProposal,
    ) -> Result<CoreOutput, CoreError> {
        let mut out = CoreOutput::default();
        if self.state != LifecycleState::Established {
            return Err(CoreError::InvalidStateForRenegotiation);
        }
        validate_timer_proposal(self.cfg, proposal)?;

        let proposal_id = self.next_proposal_id;
        self.next_proposal_id = self.next_proposal_id.saturating_add(1);
        let expires_at_ms = now_ms.saturating_add(self.cfg.msr_interval_ms.saturating_mul(2));
        self.pending_local_proposal =
            Some(PendingRenegotiation { proposal_id, proposal, expires_at_ms });

        let msr = encode_msr_payload(0, self.max_pending_id());
        let tlvs = vec![encode_timer_proposal_tlv(proposal_id, proposal)];
        out.outbound_datagrams.push(OutboundDatagram {
            path_id: self.current_path_id,
            bytes: encode_frame_with_tlvs(MessageType::Msr, self.cid, &msr, tlvs)?,
        });
        out.events.push(Event::TimerRenegotiationProposed { proposal_id });
        out.timer_actions.push(TimerAction::Schedule {
            id: TimerId::RenegotiationTimeout,
            deadline_ms: expires_at_ms,
        });

        Ok(out)
    }

    pub fn on_path_event(&mut self, event: PathEvent) -> Result<CoreOutput, CoreError> {
        match event {
            PathEvent::PathChanged { path_id } => {
                self.current_path_id = path_id;
            }
        }
        Ok(CoreOutput::default())
    }

    fn handle_msr(
        &mut self,
        now_ms: u64,
        src_path_id: u32,
        frame: &WireFrame,
        out: &mut CoreOutput,
    ) -> Result<(), CoreError> {
        let (_acked, next_pending) =
            decode_msr_payload(&frame.payload).ok_or(CoreError::MalformedMsr)?;

        for tlv in &frame.tlvs {
            if tlv.kind == TLV_TIMER_PROPOSAL {
                let (proposal_id, proposal) =
                    decode_timer_proposal_tlv(tlv).ok_or(CoreError::MalformedTimerProposal)?;
                let accepted = validate_timer_proposal(self.cfg, proposal).is_ok();
                let ack_tlv = encode_timer_ack_tlv(proposal_id, accepted);
                let msr = encode_msr_payload(self.last_ordered_rx, self.max_pending_id());
                out.outbound_datagrams.push(OutboundDatagram {
                    path_id: src_path_id,
                    bytes: encode_frame_with_tlvs(MessageType::Msr, self.cid, &msr, vec![ack_tlv])?,
                });
                if accepted {
                    self.cfg = proposal.to_config();
                    out.events.push(Event::TimerRenegotiationCommitted { proposal_id });
                } else {
                    out.events.push(Event::TimerRenegotiationRejected { proposal_id });
                }
            } else if tlv.kind == TLV_TIMER_ACK {
                let (proposal_id, accepted) =
                    decode_timer_ack_tlv(tlv).ok_or(CoreError::MalformedTimerAck)?;
                if let Some(pending) = self.pending_local_proposal {
                    if pending.proposal_id == proposal_id {
                        if accepted {
                            self.cfg = pending.proposal.to_config();
                            out.events.push(Event::TimerRenegotiationCommitted { proposal_id });
                        } else {
                            out.events.push(Event::TimerRenegotiationRejected { proposal_id });
                        }
                        self.pending_local_proposal = None;
                    }
                }
            }
        }

        self.handle_msr_signal(now_ms, src_path_id, next_pending, MsrSource::ControlMsrFrame, out)
    }

    fn handle_msr_signal(
        &mut self,
        now_ms: u64,
        src_path_id: u32,
        next_pending: u64,
        source: MsrSource,
        out: &mut CoreOutput,
    ) -> Result<(), CoreError> {
        if self.role == Role::Client {
            self.last_server_msr_at_ms = Some(now_ms);
            if source == MsrSource::ControlMsrFrame {
                self.client_server_msr_rx_since_last_keepalive =
                    self.client_server_msr_rx_since_last_keepalive.saturating_add(1);
                if self.client_server_msr_rx_since_last_keepalive
                    >= CLIENT_KEEPALIVE_EVERY_SERVER_MSR
                {
                    self.client_server_msr_rx_since_last_keepalive = 0;
                    let keepalive = encode_msr_payload(self.last_ordered_rx, self.max_pending_id());
                    out.outbound_datagrams.push(OutboundDatagram {
                        path_id: src_path_id,
                        bytes: encode_frame(MessageType::Msr, self.cid, &keepalive)?,
                    });
                }
            }
            if self.state == LifecycleState::Reactivating {
                self.state = LifecycleState::Established;
                out.events.push(Event::Reactivated);
            }
        }
        if self.role == Role::Server {
            self.server_consecutive_msr_without_client_msr = 0;
        }
        if self.state == LifecycleState::Established && next_pending > self.last_requested_end {
            let start = self.last_requested_end + 1;
            let end = next_pending;
            self.last_requested_end = end;
            let req = encode_range(start, end);
            let tlvs = vec![encode_msr_signal_tlv(self.last_ordered_rx, self.max_pending_id())];
            out.outbound_datagrams.push(OutboundDatagram {
                path_id: src_path_id,
                bytes: encode_frame_with_tlvs(MessageType::RequestRange, self.cid, &req, tlvs)?,
            });
            out.events.push(Event::RequestRangeSent { start, end });
        }

        Ok(())
    }

    fn handle_request_range(
        &mut self,
        src_path_id: u32,
        payload: &[u8],
        out: &mut CoreOutput,
    ) -> Result<(), CoreError> {
        let (start, end) = decode_range(payload).ok_or(CoreError::MalformedRequestRange)?;
        for id in start..=end {
            if let Some(msg) = self.outbound_messages.get(&id) {
                if 8 + msg.len() <= MAX_DATA_PAYLOAD_PER_FRAME {
                    let mut data = Vec::with_capacity(8 + msg.len());
                    data.extend_from_slice(&id.to_be_bytes());
                    data.extend_from_slice(msg);
                    let tlvs =
                        vec![encode_msr_signal_tlv(self.last_ordered_rx, self.max_pending_id())];
                    out.outbound_datagrams.push(OutboundDatagram {
                        path_id: src_path_id,
                        bytes: encode_frame_with_tlvs(MessageType::Data, self.cid, &data, tlvs)?,
                    });
                } else {
                    let chunk_size = MAX_DATA_PAYLOAD_PER_FRAME.saturating_sub(FRAGMENT_HEADER_LEN);
                    if chunk_size == 0 {
                        return Err(CoreError::MalformedData);
                    }
                    let total_frags = msg.len().div_ceil(chunk_size);
                    let total_frags_u16 =
                        u16::try_from(total_frags).map_err(|_| CoreError::MalformedData)?;
                    for (idx, chunk) in msg.chunks(chunk_size).enumerate() {
                        let idx_u16 = u16::try_from(idx).map_err(|_| CoreError::MalformedData)?;
                        let mut data = Vec::with_capacity(FRAGMENT_HEADER_LEN + chunk.len());
                        data.extend_from_slice(&id.to_be_bytes());
                        data.extend_from_slice(&idx_u16.to_be_bytes());
                        data.extend_from_slice(&total_frags_u16.to_be_bytes());
                        data.extend_from_slice(chunk);
                        let tlvs = vec![encode_msr_signal_tlv(
                            self.last_ordered_rx,
                            self.max_pending_id(),
                        )];
                        out.outbound_datagrams.push(OutboundDatagram {
                            path_id: src_path_id,
                            bytes: encode_frame_with_tlvs(
                                MessageType::Fragment,
                                self.cid,
                                &data,
                                tlvs,
                            )?,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_data(&mut self, payload: &[u8], out: &mut CoreOutput) -> Result<(), CoreError> {
        if payload.len() < 8 {
            return Err(CoreError::MalformedData);
        }
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&payload[..8]);
        let msg_id = u64::from_be_bytes(id_bytes);

        if self.received_ids.contains(&msg_id) {
            return Ok(());
        }
        self.received_ids.insert(msg_id);
        self.received_buffer.insert(msg_id, payload[8..].to_vec());
        self.received_fragments.remove(&msg_id);
        self.flush_ordered(out);
        Ok(())
    }

    fn handle_fragment(&mut self, payload: &[u8], out: &mut CoreOutput) -> Result<(), CoreError> {
        if payload.len() < FRAGMENT_HEADER_LEN {
            return Err(CoreError::MalformedData);
        }
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&payload[..8]);
        let msg_id = u64::from_be_bytes(id_bytes);

        if self.received_ids.contains(&msg_id) {
            return Ok(());
        }
        let frag_idx = u16::from_be_bytes([payload[8], payload[9]]);
        let frag_total = u16::from_be_bytes([payload[10], payload[11]]);
        if frag_total == 0 || frag_idx >= frag_total {
            return Err(CoreError::MalformedData);
        }

        let acc = self.received_fragments.entry(msg_id).or_default();
        if acc.total == 0 {
            acc.total = frag_total;
        }
        if acc.total != frag_total {
            return Err(CoreError::MalformedData);
        }
        acc.chunks.entry(frag_idx).or_insert_with(|| payload[FRAGMENT_HEADER_LEN..].to_vec());

        if acc.chunks.len() == usize::from(acc.total) {
            let total = acc.total;
            let mut merged = Vec::new();
            for i in 0..total {
                let Some(chunk) = acc.chunks.get(&i) else {
                    return Err(CoreError::MalformedData);
                };
                merged.extend_from_slice(chunk);
            }
            self.received_fragments.remove(&msg_id);
            self.received_ids.insert(msg_id);
            self.received_buffer.insert(msg_id, merged);
            self.flush_ordered(out);
        }
        Ok(())
    }

    fn flush_ordered(&mut self, out: &mut CoreOutput) {
        while let Some(ordered_payload) = self.received_buffer.remove(&(self.last_ordered_rx + 1)) {
            self.last_ordered_rx += 1;
            out.events.push(Event::MessageDelivered { msg_id: self.last_ordered_rx });
            out.events.push(Event::MessageReceived {
                msg_id: self.last_ordered_rx,
                payload: ordered_payload,
            });
        }
    }

    fn max_pending_id(&self) -> u64 {
        self.outbound_messages.keys().next_back().copied().unwrap_or(0)
    }

    fn touch_disconnect_deadline(&mut self, now_ms: u64) {
        self.disconnect_deadline_ms = Some(now_ms + self.cfg.disconnect_window_ms);
    }

    fn schedule_server_timers(&mut self, now_ms: u64, out: &mut CoreOutput) {
        self.touch_disconnect_deadline(now_ms);
        out.timer_actions.push(TimerAction::Schedule {
            id: TimerId::ServerMsr,
            deadline_ms: now_ms + self.cfg.msr_interval_ms,
        });
        out.timer_actions.push(TimerAction::Schedule {
            id: TimerId::Disconnect,
            deadline_ms: now_ms + self.cfg.disconnect_window_ms,
        });
    }

    fn schedule_client_timers(&mut self, now_ms: u64, out: &mut CoreOutput) {
        self.last_server_msr_at_ms = Some(now_ms);
        self.client_server_msr_rx_since_last_keepalive = 0;
        self.touch_disconnect_deadline(now_ms);
        out.timer_actions.push(TimerAction::Schedule {
            id: TimerId::ClientProbe,
            deadline_ms: now_ms + self.cfg.msr_interval_ms,
        });
        out.timer_actions.push(TimerAction::Schedule {
            id: TimerId::Disconnect,
            deadline_ms: now_ms + self.cfg.disconnect_window_ms,
        });
    }

    fn on_valid_peer_datagram(&mut self, now_ms: u64) {
        if self.state != LifecycleState::Established {
            return;
        }
        if self.role == Role::Client {
            self.last_server_msr_at_ms = Some(now_ms);
        } else if self.role == Role::Server {
            self.server_consecutive_msr_without_client_msr = 0;
        }
    }
}

fn encode_frame(message_type: MessageType, cid: u64, payload: &[u8]) -> Result<Vec<u8>, CoreError> {
    WireFrame::new(message_type, 0, cid, payload.to_vec(), Vec::new())
        .encode()
        .map_err(CoreError::Wire)
}

fn encode_frame_with_tlvs(
    message_type: MessageType,
    cid: u64,
    payload: &[u8],
    tlvs: Vec<Tlv>,
) -> Result<Vec<u8>, CoreError> {
    WireFrame::new(message_type, 0, cid, payload.to_vec(), tlvs).encode().map_err(CoreError::Wire)
}

fn encode_msr_payload(acked: u64, next_pending: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&acked.to_be_bytes());
    out.extend_from_slice(&next_pending.to_be_bytes());
    out
}

fn decode_msr_payload(payload: &[u8]) -> Option<(u64, u64)> {
    if payload.len() < 16 {
        return None;
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&payload[..8]);
    let mut b = [0u8; 8];
    b.copy_from_slice(&payload[8..16]);
    Some((u64::from_be_bytes(a), u64::from_be_bytes(b)))
}

fn encode_msr_signal_tlv(acked: u64, next_pending: u64) -> Tlv {
    Tlv { kind: TLV_MSR_SIGNAL, value: encode_msr_payload(acked, next_pending) }
}

fn find_msr_signal_tlv(tlvs: &[Tlv]) -> Option<(u64, u64)> {
    tlvs.iter()
        .find(|tlv| tlv.kind == TLV_MSR_SIGNAL)
        .and_then(|tlv| decode_msr_payload(&tlv.value))
}

fn encode_range(start: u64, end: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&end.to_be_bytes());
    out
}

fn decode_range(payload: &[u8]) -> Option<(u64, u64)> {
    if payload.len() < 16 {
        return None;
    }
    let mut s = [0u8; 8];
    s.copy_from_slice(&payload[..8]);
    let mut e = [0u8; 8];
    e.copy_from_slice(&payload[8..16]);
    Some((u64::from_be_bytes(s), u64::from_be_bytes(e)))
}

fn encode_timer_proposal_tlv(proposal_id: u32, proposal: TimerProposal) -> Tlv {
    let mut value = Vec::with_capacity(4 + 8 + 8 + 8);
    value.extend_from_slice(&proposal_id.to_be_bytes());
    value.extend_from_slice(&proposal.msr_interval_ms.to_be_bytes());
    value.extend_from_slice(&proposal.miss_window_count.to_be_bytes());
    value.extend_from_slice(&proposal.disconnect_window_ms.to_be_bytes());
    Tlv { kind: TLV_TIMER_PROPOSAL, value }
}

fn decode_timer_proposal_tlv(tlv: &Tlv) -> Option<(u32, TimerProposal)> {
    if tlv.kind != TLV_TIMER_PROPOSAL || tlv.value.len() != 28 {
        return None;
    }
    let mut id = [0u8; 4];
    id.copy_from_slice(&tlv.value[..4]);
    let mut msr = [0u8; 8];
    msr.copy_from_slice(&tlv.value[4..12]);
    let mut miss = [0u8; 8];
    miss.copy_from_slice(&tlv.value[12..20]);
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&tlv.value[20..28]);
    Some((
        u32::from_be_bytes(id),
        TimerProposal {
            msr_interval_ms: u64::from_be_bytes(msr),
            miss_window_count: u64::from_be_bytes(miss),
            disconnect_window_ms: u64::from_be_bytes(disc),
        },
    ))
}

fn encode_timer_ack_tlv(proposal_id: u32, accepted: bool) -> Tlv {
    let mut value = Vec::with_capacity(5);
    value.extend_from_slice(&proposal_id.to_be_bytes());
    value.push(u8::from(accepted));
    Tlv { kind: TLV_TIMER_ACK, value }
}

fn decode_timer_ack_tlv(tlv: &Tlv) -> Option<(u32, bool)> {
    if tlv.kind != TLV_TIMER_ACK || tlv.value.len() != 5 {
        return None;
    }
    let mut id = [0u8; 4];
    id.copy_from_slice(&tlv.value[..4]);
    let accepted = match tlv.value[4] {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some((u32::from_be_bytes(id), accepted))
}

fn validate_timer_proposal(current: CoreConfig, proposal: TimerProposal) -> Result<(), CoreError> {
    const MIN_MSR_INTERVAL_MS: u64 = 1_000;
    const MAX_MSR_INTERVAL_MS: u64 = 120_000;
    const MIN_MISS_WINDOW_COUNT: u64 = 1;
    const MAX_MISS_WINDOW_STEP: u64 = 2;
    const MIN_DISCONNECT_WINDOW_MS: u64 = 5_000;
    const MAX_DISCONNECT_WINDOW_MS: u64 = 900_000;

    if !(MIN_MSR_INTERVAL_MS..=MAX_MSR_INTERVAL_MS).contains(&proposal.msr_interval_ms) {
        return Err(CoreError::InvalidTimerProposal("msr_interval_ms out of bounds"));
    }
    if !(MIN_MISS_WINDOW_COUNT..=64).contains(&proposal.miss_window_count) {
        return Err(CoreError::InvalidTimerProposal("miss_window_count out of bounds"));
    }
    if proposal.miss_window_count > current.miss_window_count.saturating_add(MAX_MISS_WINDOW_STEP) {
        return Err(CoreError::InvalidTimerProposal("miss_window_count step too large"));
    }
    if !(MIN_DISCONNECT_WINDOW_MS..=MAX_DISCONNECT_WINDOW_MS)
        .contains(&proposal.disconnect_window_ms)
    {
        return Err(CoreError::InvalidTimerProposal("disconnect_window_ms out of bounds"));
    }
    if proposal.disconnect_window_ms
        < proposal.msr_interval_ms.saturating_mul(proposal.miss_window_count)
    {
        return Err(CoreError::InvalidTimerProposal(
            "disconnect_window_ms smaller than probe window",
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("wire error: {0}")]
    Wire(#[from] v3_wire::WireError),
    #[error("cid mismatch expected={expected} got={got}")]
    CidMismatch { expected: u64, got: u64 },
    #[error("malformed MSR payload")]
    MalformedMsr,
    #[error("malformed request range payload")]
    MalformedRequestRange,
    #[error("malformed data payload")]
    MalformedData,
    #[error("invalid state for renegotiation")]
    InvalidStateForRenegotiation,
    #[error("invalid timer proposal: {0}")]
    InvalidTimerProposal(&'static str),
    #[error("malformed timer proposal tlv")]
    MalformedTimerProposal,
    #[error("malformed timer ack tlv")]
    MalformedTimerAck,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> CoreConfig {
        CoreConfig { msr_interval_ms: 100, miss_window_count: 2, disconnect_window_ms: 1000 }
    }

    #[test]
    fn basic_connect_and_steady_state() {
        let cid = 42;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());

        let start = client.begin_handshake(0).expect("client begin");
        assert!(start.events.contains(&Event::HandshakeStarted));
        assert_eq!(start.outbound_datagrams.len(), 1);

        let server_step =
            server.on_datagram(1, 7, &start.outbound_datagrams[0].bytes).expect("server datagram");
        assert_eq!(server.state(), LifecycleState::Established);
        assert!(server_step.events.contains(&Event::HandshakeEstablished));

        let client_step = client
            .on_datagram(2, 7, &server_step.outbound_datagrams[0].bytes)
            .expect("client datagram");
        assert_eq!(client.state(), LifecycleState::Established);
        assert!(client_step.events.contains(&Event::HandshakeEstablished));
    }

    #[test]
    fn missed_msr_triggers_probe_then_reactivation() {
        let cid = 7;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        client.state = LifecycleState::Established;
        client.last_server_msr_at_ms = Some(0);
        client.disconnect_deadline_ms = Some(5_000);

        let probe = client.on_timer(250, TimerId::ClientProbe).expect("probe timer");
        assert_eq!(client.state(), LifecycleState::Reactivating);
        assert!(probe.events.contains(&Event::ReactivationProbeSent));

        let msr =
            encode_frame(MessageType::Msr, cid, &encode_msr_payload(0, 0)).expect("msr frame");
        let after = client.on_datagram(260, 3, &msr).expect("reactivation msr");
        assert_eq!(client.state(), LifecycleState::Established);
        assert!(after.events.contains(&Event::Reactivated));
    }

    #[test]
    fn disconnect_convergence_marks_lost() {
        let cid = 9;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        client.state = LifecycleState::Established;
        client.disconnect_deadline_ms = Some(1000);

        let out = client.on_timer(1001, TimerId::Disconnect).expect("disconnect timer");
        assert_eq!(client.state(), LifecycleState::Lost);
        assert!(out.events.contains(&Event::Disconnected));
    }

    #[test]
    fn pull_flow_handles_repeated_request_and_ordered_delivery() {
        let cid = 99;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let enqueue = client.on_api_send(b"hello".to_vec()).expect("enqueue");
        let advertise = &enqueue.outbound_datagrams[0].bytes;

        let req1 = server.on_datagram(10, 1, advertise).expect("server request 1");
        assert!(req1
            .events
            .iter()
            .any(|e| matches!(e, Event::RequestRangeSent { start: 1, end: 1 })));

        // Repeated MSR advertisement should not advance requested range beyond pending end.
        let req2 = server.on_datagram(11, 1, advertise).expect("server request 2");
        assert!(req2.outbound_datagrams.is_empty());

        // Client responds to request with DATA.
        let data_out =
            client.on_datagram(12, 1, &req1.outbound_datagrams[0].bytes).expect("client data send");
        assert_eq!(data_out.outbound_datagrams.len(), 1);

        // Server receives DATA and advances ordered delivery.
        let delivered = server
            .on_datagram(13, 1, &data_out.outbound_datagrams[0].bytes)
            .expect("server data recv");
        assert_eq!(server.last_ordered_rx(), 1);
        assert!(delivered
            .events
            .iter()
            .any(|e| matches!(e, Event::MessageDelivered { msg_id: 1 })));
    }

    #[test]
    fn timer_renegotiation_commits_on_ack() {
        let cid = 101;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let proposal = TimerProposal {
            msr_interval_ms: 2_000,
            miss_window_count: 3,
            disconnect_window_ms: 20_000,
        };

        let propose = client.on_api_propose_timers(1_000, proposal).expect("proposal");
        assert!(propose
            .events
            .iter()
            .any(|e| matches!(e, Event::TimerRenegotiationProposed { .. })));

        let server_seen = server
            .on_datagram(1_001, 9, &propose.outbound_datagrams[0].bytes)
            .expect("server handles proposal");
        assert!(server_seen
            .events
            .iter()
            .any(|e| matches!(e, Event::TimerRenegotiationCommitted { .. })));

        let client_ack = client
            .on_datagram(1_002, 9, &server_seen.outbound_datagrams[0].bytes)
            .expect("client handles ack");
        assert!(client_ack
            .events
            .iter()
            .any(|e| matches!(e, Event::TimerRenegotiationCommitted { .. })));
        assert_eq!(client.cfg.msr_interval_ms, 2_000);
        assert_eq!(client.cfg.miss_window_count, 3);
    }

    #[test]
    fn timer_renegotiation_rejects_large_miss_window_step() {
        let cid = 202;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let mut value = Vec::with_capacity(28);
        value.extend_from_slice(&1_u32.to_be_bytes());
        value.extend_from_slice(&2_000_u64.to_be_bytes());
        value.extend_from_slice(&10_u64.to_be_bytes());
        value.extend_from_slice(&20_000_u64.to_be_bytes());
        let frame = WireFrame::new(
            MessageType::Msr,
            0,
            cid,
            encode_msr_payload(0, 0),
            vec![Tlv { kind: TLV_TIMER_PROPOSAL, value }],
        )
        .encode()
        .expect("encode");

        let out = server.on_datagram(10, 1, &frame).expect("server datagram");
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, Event::TimerRenegotiationRejected { proposal_id: 1 })));
    }

    #[test]
    fn timer_renegotiation_rolls_back_on_timeout() {
        let cid = 303;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        client.state = LifecycleState::Established;

        let proposal = TimerProposal {
            msr_interval_ms: 2_000,
            miss_window_count: 3,
            disconnect_window_ms: 20_000,
        };

        let propose = client.on_api_propose_timers(1_000, proposal).expect("proposal");
        assert!(propose
            .timer_actions
            .iter()
            .any(|a| matches!(a, TimerAction::Schedule { id: TimerId::RenegotiationTimeout, .. })));

        let timed_out =
            client.on_timer(1_500, TimerId::RenegotiationTimeout).expect("reneg timeout");
        assert!(timed_out
            .events
            .iter()
            .any(|e| matches!(e, Event::TimerRenegotiationRolledBack { .. })));
        assert_eq!(client.cfg.msr_interval_ms, 100);
    }

    #[test]
    fn pull_flow_tolerates_duplicate_and_reordered_data() {
        let cid = 404;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let _ = client.on_api_send(b"m1".to_vec()).expect("enqueue 1");
        let adv2 = client.on_api_send(b"m2".to_vec()).expect("enqueue 2");
        let advertise = &adv2.outbound_datagrams[0].bytes;

        let req = server.on_datagram(10, 1, advertise).expect("request");
        assert!(req
            .events
            .iter()
            .any(|e| matches!(e, Event::RequestRangeSent { start: 1, end: 2 })));

        let data =
            client.on_datagram(11, 1, &req.outbound_datagrams[0].bytes).expect("produce data");
        assert_eq!(data.outbound_datagrams.len(), 2);

        let recv2 =
            server.on_datagram(12, 1, &data.outbound_datagrams[1].bytes).expect("recv msg2 first");
        assert_eq!(server.last_ordered_rx(), 0);
        assert!(recv2.events.is_empty());

        let _dup =
            server.on_datagram(13, 1, &data.outbound_datagrams[1].bytes).expect("recv msg2 dup");
        assert_eq!(server.last_ordered_rx(), 0);

        let recv1 =
            server.on_datagram(14, 1, &data.outbound_datagrams[0].bytes).expect("recv msg1");
        assert_eq!(server.last_ordered_rx(), 2);
        assert_eq!(
            recv1.events.iter().filter(|e| matches!(e, Event::MessageDelivered { .. })).count(),
            2
        );
    }

    #[test]
    fn server_push_anytime_reaches_client_via_urgent_msr_and_pull() {
        let cid = 505;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let push = server.on_api_send(b"from-server".to_vec()).expect("server enqueue");
        assert_eq!(push.outbound_datagrams.len(), 1);

        // Client sees server MSR with pending=1 and requests the range.
        let req = client
            .on_datagram(10, 1, &push.outbound_datagrams[0].bytes)
            .expect("client handles urgent msr");
        assert!(req
            .events
            .iter()
            .any(|e| matches!(e, Event::RequestRangeSent { start: 1, end: 1 })));
        assert_eq!(req.outbound_datagrams.len(), 1);

        // Server responds with DATA.
        let data = server
            .on_datagram(11, 1, &req.outbound_datagrams[0].bytes)
            .expect("server handles request");
        assert_eq!(data.outbound_datagrams.len(), 1);

        // Client receives ordered payload.
        let delivered = client
            .on_datagram(12, 1, &data.outbound_datagrams[0].bytes)
            .expect("client handles data");
        assert!(delivered
            .events
            .iter()
            .any(|e| matches!(e, Event::MessageDelivered { msg_id: 1 })));
        assert!(delivered.events.iter().any(
            |e| matches!(e, Event::MessageReceived { msg_id: 1, payload } if payload == b"from-server")
        ));
    }

    #[test]
    fn client_close_api_emits_close_and_server_drops_session() {
        let cid = 808;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let close = client.on_api_close().expect("close");
        assert_eq!(client.state(), LifecycleState::Lost);
        assert!(close.events.iter().any(|e| matches!(e, Event::Disconnected)));
        assert_eq!(close.outbound_datagrams.len(), 1);
        let frame = WireFrame::decode(&close.outbound_datagrams[0].bytes).expect("decode close");
        assert_eq!(frame.message_type, MessageType::Close);

        let srv = server
            .on_datagram(10, 1, &close.outbound_datagrams[0].bytes)
            .expect("server close datagram");
        assert_eq!(server.state(), LifecycleState::Lost);
        assert!(srv.events.iter().any(|e| matches!(e, Event::Disconnected)));
    }

    #[test]
    fn client_sends_periodic_keepalive_msr_every_four_server_msrs() {
        let cid = 606;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        client.state = LifecycleState::Established;

        let server_msr =
            encode_frame(MessageType::Msr, cid, &encode_msr_payload(0, 0)).expect("msr frame");
        for now in [1_u64, 2, 3] {
            let out = client.on_datagram(now, 1, &server_msr).expect("client msr");
            assert!(out.outbound_datagrams.iter().all(|d| WireFrame::decode(&d.bytes)
                .map(|f| f.message_type != MessageType::Msr)
                .unwrap_or(true)));
        }
        let out = client.on_datagram(4, 1, &server_msr).expect("client msr keepalive");
        assert!(out.outbound_datagrams.iter().any(|d| {
            WireFrame::decode(&d.bytes).map(|f| f.message_type == MessageType::Msr).unwrap_or(false)
        }));
    }

    #[test]
    fn server_disconnects_after_consecutive_msr_without_client_keepalive() {
        let cid = 707;
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        server.state = LifecycleState::Established;
        server.current_path_id = 1;

        let mut saw_disconnect = false;
        for tick in 1..=10_u64 {
            let out = server
                .on_timer(tick * default_cfg().msr_interval_ms, TimerId::ServerMsr)
                .expect("server timer");
            if out.events.iter().any(|e| matches!(e, Event::Disconnected)) {
                saw_disconnect = true;
                break;
            }
        }
        assert!(saw_disconnect);
        assert_eq!(server.state(), LifecycleState::Lost);
    }

    #[test]
    fn request_range_and_data_frames_carry_msr_piggyback_tlv() {
        let cid = 8081;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        let mut server = CoreMachine::new(Role::Server, cid, default_cfg());
        client.state = LifecycleState::Established;
        server.state = LifecycleState::Established;

        let enqueue = client.on_api_send(b"hello".to_vec()).expect("enqueue");
        let advertise = &enqueue.outbound_datagrams[0].bytes;
        let req = server.on_datagram(10, 1, advertise).expect("server request");
        assert_eq!(req.outbound_datagrams.len(), 1);
        let req_frame =
            WireFrame::decode(&req.outbound_datagrams[0].bytes).expect("decode request");
        assert_eq!(req_frame.message_type, MessageType::RequestRange);
        assert!(req_frame.tlvs.iter().any(|t| t.kind == TLV_MSR_SIGNAL));

        let data =
            client.on_datagram(11, 1, &req.outbound_datagrams[0].bytes).expect("client data");
        assert!(!data.outbound_datagrams.is_empty());
        let data_frame = WireFrame::decode(&data.outbound_datagrams[0].bytes).expect("decode data");
        assert!(matches!(data_frame.message_type, MessageType::Data | MessageType::Fragment));
        assert!(data_frame.tlvs.iter().any(|t| t.kind == TLV_MSR_SIGNAL));
    }

    #[test]
    fn piggyback_msr_tlv_on_data_triggers_pull_request_without_control_msr_frame() {
        let cid = 8082;
        let mut client = CoreMachine::new(Role::Client, cid, default_cfg());
        client.state = LifecycleState::Established;

        let mut data_payload = Vec::new();
        data_payload.extend_from_slice(&99_u64.to_be_bytes());
        data_payload.extend_from_slice(b"x");
        let piggyback = encode_msr_signal_tlv(0, 1);
        let frame = WireFrame::new(MessageType::Data, 0, cid, data_payload, vec![piggyback])
            .encode()
            .expect("encode data+msr tlv");

        let out = client.on_datagram(20, 1, &frame).expect("client datagram");
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, Event::RequestRangeSent { start: 1, end: 1 })));
    }
}
