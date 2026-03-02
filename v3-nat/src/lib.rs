//! v3-nat
//!
//! Deterministic NAT traversal helpers for V3 runtime integration.
//! This crate intentionally keeps socket I/O outside and models policy/state.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateType {
    Host,
    Srflx,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidate {
    pub foundation: String,
    pub component: u16,
    pub transport: String,
    pub priority: u32,
    pub address: IpAddr,
    pub port: u16,
    pub typ: CandidateType,
    pub related_address: Option<IpAddr>,
    pub related_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidatePairState {
    Frozen,
    InProgress,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidatePair {
    pub local_index: usize,
    pub remote_index: usize,
    pub priority: u64,
    pub nominated: bool,
    pub state: CandidatePairState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceChecklist {
    pub local: Vec<IceCandidate>,
    pub remote: Vec<IceCandidate>,
    pub pairs: Vec<CandidatePair>,
}

impl IceChecklist {
    pub fn new(local: Vec<IceCandidate>, remote: Vec<IceCandidate>) -> Self {
        let mut pairs = Vec::new();
        for (li, l) in local.iter().enumerate() {
            for (ri, r) in remote.iter().enumerate() {
                if l.component != r.component || l.transport != r.transport {
                    continue;
                }
                let pair_priority = pair_priority(l.priority, r.priority);
                pairs.push(CandidatePair {
                    local_index: li,
                    remote_index: ri,
                    priority: pair_priority,
                    nominated: false,
                    state: CandidatePairState::Frozen,
                });
            }
        }
        pairs.sort_by(|a, b| b.priority.cmp(&a.priority));
        Self { local, remote, pairs }
    }

    pub fn next_check(&mut self) -> Option<(usize, usize)> {
        let next = self.pairs.iter_mut().find(|p| p.state == CandidatePairState::Frozen)?;
        next.state = CandidatePairState::InProgress;
        Some((next.local_index, next.remote_index))
    }

    pub fn mark_check_result(&mut self, local_index: usize, remote_index: usize, ok: bool) {
        if let Some(pair) = self
            .pairs
            .iter_mut()
            .find(|p| p.local_index == local_index && p.remote_index == remote_index)
        {
            pair.state =
                if ok { CandidatePairState::Succeeded } else { CandidatePairState::Failed };
        }
    }

    pub fn nominate_best(&mut self) -> Option<CandidatePair> {
        let best = self
            .pairs
            .iter_mut()
            .filter(|p| p.state == CandidatePairState::Succeeded)
            .max_by(|a, b| a.priority.cmp(&b.priority))?;
        best.nominated = true;
        Some(best.clone())
    }
}

fn pair_priority(local_priority: u32, remote_priority: u32) -> u64 {
    let g = u64::from(local_priority.max(remote_priority));
    let l = u64::from(local_priority.min(remote_priority));
    (1u64 << 32) * l + 2 * g + u64::from(local_priority > remote_priority)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentConfig {
    pub interval_ms: u64,
    pub max_missed: u8,
}

impl Default for ConsentConfig {
    fn default() -> Self {
        Self { interval_ms: 15_000, max_missed: 3 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentState {
    pub next_deadline_ms: u64,
    pub missed_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentAction {
    SendKeepalive,
    PathLost,
    Noop,
}

pub struct ConsentManager {
    cfg: ConsentConfig,
    state: ConsentState,
}

impl ConsentManager {
    pub fn new(start_ms: u64, cfg: ConsentConfig) -> Self {
        Self {
            cfg,
            state: ConsentState {
                next_deadline_ms: start_ms.saturating_add(cfg.interval_ms),
                missed_count: 0,
            },
        }
    }

    pub fn on_tick(&mut self, now_ms: u64) -> ConsentAction {
        if now_ms < self.state.next_deadline_ms {
            return ConsentAction::Noop;
        }
        self.state.next_deadline_ms = now_ms.saturating_add(self.cfg.interval_ms);
        self.state.missed_count = self.state.missed_count.saturating_add(1);
        if self.state.missed_count > self.cfg.max_missed {
            ConsentAction::PathLost
        } else {
            ConsentAction::SendKeepalive
        }
    }

    pub fn on_response(&mut self, now_ms: u64) {
        self.state.missed_count = 0;
        self.state.next_deadline_ms = now_ms.saturating_add(self.cfg.interval_ms);
    }

    pub fn state(&self) -> ConsentState {
        self.state
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NatError {
    #[error("invalid stun packet")]
    InvalidStunPacket,
    #[error("unsupported stun family")]
    UnsupportedStunFamily,
}

pub fn stun_build_binding_request(tx_id: [u8; 12]) -> Vec<u8> {
    // RFC 5389 header for Binding Request with no attributes.
    let mut out = vec![0u8; 20];
    out[0] = 0x00;
    out[1] = 0x01;
    out[2] = 0x00;
    out[3] = 0x00;
    out[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    out[8..20].copy_from_slice(&tx_id);
    out
}

pub fn stun_parse_binding_success(packet: &[u8]) -> Result<(SocketAddr, [u8; 12]), NatError> {
    if packet.len() < 20 {
        return Err(NatError::InvalidStunPacket);
    }
    if packet[0] != 0x01 || packet[1] != 0x01 {
        return Err(NatError::InvalidStunPacket);
    }
    let len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if packet.len() < 20 + len {
        return Err(NatError::InvalidStunPacket);
    }
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if cookie != STUN_MAGIC_COOKIE {
        return Err(NatError::InvalidStunPacket);
    }
    let mut tx_id = [0u8; 12];
    tx_id.copy_from_slice(&packet[8..20]);

    let mut pos = 20;
    while pos + 4 <= packet.len() {
        let attr_type = u16::from_be_bytes([packet[pos], packet[pos + 1]]);
        let attr_len = u16::from_be_bytes([packet[pos + 2], packet[pos + 3]]) as usize;
        pos += 4;
        if pos + attr_len > packet.len() {
            return Err(NatError::InvalidStunPacket);
        }

        if attr_type == 0x0020 {
            if attr_len < 8 {
                return Err(NatError::InvalidStunPacket);
            }
            let family = packet[pos + 1];
            if family != 0x01 {
                return Err(NatError::UnsupportedStunFamily);
            }
            let x_port = u16::from_be_bytes([packet[pos + 2], packet[pos + 3]]);
            let port = x_port ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
            let cookie_bytes = STUN_MAGIC_COOKIE.to_be_bytes();
            let ip = Ipv4Addr::new(
                packet[pos + 4] ^ cookie_bytes[0],
                packet[pos + 5] ^ cookie_bytes[1],
                packet[pos + 6] ^ cookie_bytes[2],
                packet[pos + 7] ^ cookie_bytes[3],
            );
            return Ok((SocketAddr::new(IpAddr::V4(ip), port), tx_id));
        }

        let padded = (attr_len + 3) & !3;
        pos += padded;
    }

    Err(NatError::InvalidStunPacket)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnAuthMode {
    LongTerm,
    RestSecret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Idle,
    Allocating,
    Allocated,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEvent {
    AllocationOk { lifetime_secs: u32 },
    PermissionOk,
    ChannelBindOk,
    Unauthorized,
    StaleNonce,
    TransportFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnAction {
    SendAllocate,
    SendAllocateWithAuth,
    SendRefresh,
    SendCreatePermission,
    SendChannelBind,
    RetryAfter { delay_ms: u64 },
    MarkFailed,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayPolicy {
    PreferDirect,
    RelayOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnConfig {
    pub auth_mode: TurnAuthMode,
    pub refresh_before_expiry_secs: u32,
    pub retry_backoff_ms: u64,
    pub permission_refresh_ms: u64,
    pub channel_refresh_ms: u64,
    pub relay_policy: RelayPolicy,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            auth_mode: TurnAuthMode::RestSecret,
            refresh_before_expiry_secs: 60,
            retry_backoff_ms: 1_000,
            permission_refresh_ms: 240_000,
            channel_refresh_ms: 540_000,
            relay_policy: RelayPolicy::PreferDirect,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnManager {
    pub state: TurnState,
    pub allocation_expiry_ms: Option<u64>,
    pub permission_expiry_ms: Option<u64>,
    pub channel_expiry_ms: Option<u64>,
    pub retries: u32,
    cfg: TurnConfig,
}

impl TurnManager {
    pub fn new(cfg: TurnConfig) -> Self {
        Self {
            state: TurnState::Idle,
            allocation_expiry_ms: None,
            permission_expiry_ms: None,
            channel_expiry_ms: None,
            retries: 0,
            cfg,
        }
    }

    pub fn start_allocate(&mut self) -> TurnAction {
        self.state = TurnState::Allocating;
        TurnAction::SendAllocate
    }

    pub fn on_tick(&mut self, now_ms: u64) -> TurnAction {
        if self.state != TurnState::Allocated {
            return TurnAction::Noop;
        }

        if let Some(expiry) = self.allocation_expiry_ms {
            let refresh_at =
                expiry.saturating_sub(u64::from(self.cfg.refresh_before_expiry_secs) * 1000);
            if now_ms >= refresh_at {
                return TurnAction::SendRefresh;
            }
        }

        if self.permission_expiry_ms.is_some_and(|v| now_ms >= v) {
            return TurnAction::SendCreatePermission;
        }

        if self.channel_expiry_ms.is_some_and(|v| now_ms >= v) {
            return TurnAction::SendChannelBind;
        }

        TurnAction::Noop
    }

    pub fn on_event(&mut self, now_ms: u64, event: TurnEvent) -> TurnAction {
        match event {
            TurnEvent::AllocationOk { lifetime_secs } => {
                self.state = TurnState::Allocated;
                self.retries = 0;
                let life_ms = u64::from(lifetime_secs) * 1000;
                self.allocation_expiry_ms = Some(now_ms.saturating_add(life_ms));
                self.permission_expiry_ms =
                    Some(now_ms.saturating_add(self.cfg.permission_refresh_ms));
                self.channel_expiry_ms = Some(now_ms.saturating_add(self.cfg.channel_refresh_ms));
                TurnAction::SendCreatePermission
            }
            TurnEvent::PermissionOk => {
                self.permission_expiry_ms =
                    Some(now_ms.saturating_add(self.cfg.permission_refresh_ms));
                TurnAction::SendChannelBind
            }
            TurnEvent::ChannelBindOk => {
                self.channel_expiry_ms = Some(now_ms.saturating_add(self.cfg.channel_refresh_ms));
                TurnAction::Noop
            }
            TurnEvent::Unauthorized | TurnEvent::StaleNonce => TurnAction::SendAllocateWithAuth,
            TurnEvent::TransportFailure => {
                self.retries = self.retries.saturating_add(1);
                if self.retries > 5 {
                    self.state = TurnState::Failed;
                    TurnAction::MarkFailed
                } else {
                    TurnAction::RetryAfter {
                        delay_ms: self
                            .cfg
                            .retry_backoff_ms
                            .saturating_mul(1u64 << (self.retries - 1)),
                    }
                }
            }
        }
    }
}

pub fn tune_turn_refresh_for_disconnect_window(
    mut cfg: TurnConfig,
    disconnect_window_ms: u64,
) -> TurnConfig {
    // Keep TURN periodic refreshes inside the negotiated disconnect window.
    let guard_ms = disconnect_window_ms / 10;
    let target_ms = disconnect_window_ms.saturating_sub(guard_ms).max(30_000);
    cfg.permission_refresh_ms = cfg.permission_refresh_ms.min(target_ms);
    cfg.channel_refresh_ms = cfg.channel_refresh_ms.min(target_ms);
    cfg
}

pub fn relay_path_mtu(candidate_type: CandidateType) -> usize {
    match candidate_type {
        CandidateType::Relay => 1200,
        CandidateType::Srflx => 1232,
        CandidateType::Host => 1400,
    }
}

pub fn select_path_candidates<'a>(
    local: &'a [IceCandidate],
    remote: &'a [IceCandidate],
    policy: RelayPolicy,
) -> (Vec<&'a IceCandidate>, Vec<&'a IceCandidate>) {
    let allow = |c: &IceCandidate| match policy {
        RelayPolicy::PreferDirect => true,
        RelayPolicy::RelayOnly => c.typ == CandidateType::Relay,
    };
    (local.iter().filter(|c| allow(c)).collect(), remote.iter().filter(|c| allow(c)).collect())
}

pub fn gather_host_candidates(
    sockets: &[(IpAddr, u16)],
    component: u16,
    transport: &str,
) -> Vec<IceCandidate> {
    sockets
        .iter()
        .enumerate()
        .map(|(idx, (ip, port))| IceCandidate {
            foundation: format!("host-{idx}"),
            component,
            transport: transport.to_string(),
            priority: 100u32.saturating_add(idx as u32),
            address: *ip,
            port: *port,
            typ: CandidateType::Host,
            related_address: None,
            related_port: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ip: [u8; 4], port: u16, priority: u32) -> IceCandidate {
        IceCandidate {
            foundation: "f".to_string(),
            component: 1,
            transport: "udp".to_string(),
            priority,
            address: IpAddr::V4(Ipv4Addr::from(ip)),
            port,
            typ: CandidateType::Host,
            related_address: None,
            related_port: None,
        }
    }

    #[test]
    fn checklist_nominates_best_succeeded_pair() {
        let local = vec![host([10, 0, 0, 1], 5000, 100), host([10, 0, 0, 1], 5001, 200)];
        let remote = vec![host([10, 0, 0, 2], 6000, 90), host([10, 0, 0, 2], 6001, 250)];
        let mut list = IceChecklist::new(local, remote);

        while let Some((li, ri)) = list.next_check() {
            // Mark only two pairs as succeeded, with different priorities.
            let ok = (li == 0 && ri == 1) || (li == 1 && ri == 0);
            list.mark_check_result(li, ri, ok);
        }

        let nominated = list.nominate_best().expect("must nominate");
        assert!(nominated.nominated);
        assert_eq!(nominated.state, CandidatePairState::Succeeded);
    }

    #[test]
    fn consent_manager_transitions_to_path_lost() {
        let mut consent = ConsentManager::new(0, ConsentConfig { interval_ms: 100, max_missed: 2 });
        assert_eq!(consent.on_tick(100), ConsentAction::SendKeepalive);
        assert_eq!(consent.on_tick(200), ConsentAction::SendKeepalive);
        assert_eq!(consent.on_tick(300), ConsentAction::PathLost);

        consent.on_response(301);
        assert_eq!(consent.state().missed_count, 0);
    }

    #[test]
    fn stun_binding_success_parsing_ipv4() {
        let tx = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let req = stun_build_binding_request(tx);
        assert_eq!(req.len(), 20);

        let ip = Ipv4Addr::new(1, 2, 3, 4);
        let port = 3478;
        let xport = port ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
        let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
        let xip = [
            ip.octets()[0] ^ cookie[0],
            ip.octets()[1] ^ cookie[1],
            ip.octets()[2] ^ cookie[2],
            ip.octets()[3] ^ cookie[3],
        ];

        let mut resp = vec![0u8; 20 + 12];
        resp[0] = 0x01;
        resp[1] = 0x01;
        resp[2..4].copy_from_slice(&(12u16).to_be_bytes());
        resp[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp[8..20].copy_from_slice(&tx);
        // XOR-MAPPED-ADDRESS
        resp[20..22].copy_from_slice(&(0x0020u16).to_be_bytes());
        resp[22..24].copy_from_slice(&(8u16).to_be_bytes());
        resp[24] = 0;
        resp[25] = 0x01;
        resp[26..28].copy_from_slice(&xport.to_be_bytes());
        resp[28..32].copy_from_slice(&xip);

        let (mapped, tx_out) = stun_parse_binding_success(&resp).expect("parse");
        assert_eq!(mapped, SocketAddr::new(IpAddr::V4(ip), port));
        assert_eq!(tx_out, tx);
    }

    #[test]
    fn turn_manager_allocate_refresh_and_retries() {
        let mut turn = TurnManager::new(TurnConfig::default());
        assert_eq!(turn.start_allocate(), TurnAction::SendAllocate);
        assert_eq!(turn.on_event(1_000, TurnEvent::Unauthorized), TurnAction::SendAllocateWithAuth);
        assert_eq!(
            turn.on_event(1_500, TurnEvent::AllocationOk { lifetime_secs: 600 }),
            TurnAction::SendCreatePermission
        );
        assert_eq!(turn.state, TurnState::Allocated);

        // Near expiry should trigger refresh.
        let refresh_at = 1_500 + (600 - 60) * 1000;
        assert_eq!(turn.on_tick(refresh_at), TurnAction::SendRefresh);

        let mut failures = 0;
        loop {
            let action = turn.on_event(2_000, TurnEvent::TransportFailure);
            if action == TurnAction::MarkFailed {
                break;
            }
            failures += 1;
        }
        assert!(failures >= 5);
        assert_eq!(turn.state, TurnState::Failed);
    }

    #[test]
    fn turn_manager_handles_stale_nonce_as_reauth() {
        let mut turn = TurnManager::new(TurnConfig::default());
        turn.start_allocate();
        assert_eq!(turn.on_event(10, TurnEvent::StaleNonce), TurnAction::SendAllocateWithAuth);
    }

    #[test]
    fn turn_refresh_tuned_to_disconnect_window() {
        let tuned = tune_turn_refresh_for_disconnect_window(TurnConfig::default(), 90_000);
        assert!(tuned.permission_refresh_ms <= 81_000);
        assert!(tuned.channel_refresh_ms <= 81_000);
    }

    #[test]
    fn relay_only_policy_filters_candidates() {
        let local = vec![
            host([10, 0, 0, 1], 5000, 100),
            IceCandidate { typ: CandidateType::Relay, ..host([203, 0, 113, 10], 5001, 200) },
        ];
        let remote = vec![
            host([10, 0, 0, 2], 6000, 100),
            IceCandidate { typ: CandidateType::Relay, ..host([203, 0, 113, 20], 6001, 200) },
        ];
        let (ll, rr) = select_path_candidates(&local, &remote, RelayPolicy::RelayOnly);
        assert_eq!(ll.len(), 1);
        assert_eq!(rr.len(), 1);
        assert_eq!(ll[0].typ, CandidateType::Relay);
        assert_eq!(rr[0].typ, CandidateType::Relay);
        assert_eq!(relay_path_mtu(CandidateType::Relay), 1200);
    }

    #[test]
    fn host_candidate_gathering_from_bound_sockets() {
        let gathered = gather_host_candidates(
            &[
                (IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 5000),
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 5001),
            ],
            1,
            "udp",
        );
        assert_eq!(gathered.len(), 2);
        assert_eq!(gathered[0].typ, CandidateType::Host);
        assert_eq!(gathered[0].transport, "udp");
    }
}
