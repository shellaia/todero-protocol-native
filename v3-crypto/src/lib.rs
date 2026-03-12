//! v3-crypto
//!
//! Security primitives and interfaces for V3 transport.
//! This crate intentionally defines deterministic policy/state pieces and a
//! pluggable DTLS handshake boundary.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

pub const CRATE_NAME: &str = "v3-crypto";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DtlsVersion {
    Dtls12,
    Dtls13,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthProfile {
    ServerAuth,
    MutualTls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CipherPolicy {
    pub min_dtls_version: DtlsVersion,
    pub allow_weak_suites: bool,
}

impl Default for CipherPolicy {
    fn default() -> Self {
        Self { min_dtls_version: DtlsVersion::Dtls12, allow_weak_suites: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustConfig {
    pub trust_anchor_pems: Vec<Vec<u8>>,
    pub require_hostname_verification: bool,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self { trust_anchor_pems: Vec::new(), require_hostname_verification: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentityRef {
    pub key_id: String,
    pub cert_chain_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeConfig {
    pub profile: AuthProfile,
    pub cipher_policy: CipherPolicy,
    pub trust: TrustConfig,
    pub client_identity: Option<ClientIdentityRef>,
    pub expected_server_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdentity {
    pub subject: String,
    pub cert_fingerprint_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExporterBinding {
    pub cid: u64,
    pub path_id: u32,
    pub context_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeDatagram {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeOutput {
    pub outbound_datagrams: Vec<HandshakeDatagram>,
    pub established: bool,
    pub peer_identity: Option<PeerIdentity>,
    pub binding: Option<ExporterBinding>,
}

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid handshake configuration: {0}")]
    InvalidConfig(String),
    #[error("peer authentication failed: {0}")]
    AuthFailed(String),
    #[error("handshake provider failure: {0}")]
    Provider(String),
}

pub trait HandshakeProvider: Send {
    fn start_client(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError>;
    fn start_server(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError>;
    fn on_datagram(&mut self, datagram: &[u8]) -> Result<HandshakeOutput, CryptoError>;
    fn on_timeout(&mut self, now_ms: u64) -> Result<HandshakeOutput, CryptoError>;
}

/// Per-frame authentication hook (post-handshake).
pub trait FrameAuthHook: Send {
    fn sign(
        &self,
        sequence: u64,
        cid: u64,
        path_id: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, CryptoError>;

    fn verify(
        &mut self,
        sequence: u64,
        cid: u64,
        path_id: u32,
        payload: &[u8],
        tag: &[u8],
    ) -> Result<(), CryptoError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HmacSha256FrameAuth {
    key: [u8; 32],
    replay_window: ReplayWindow,
}

impl HmacSha256FrameAuth {
    pub fn new(key: [u8; 32], replay_window_size: u64) -> Self {
        Self { key, replay_window: ReplayWindow::new(replay_window_size) }
    }

    fn sign_inner(
        key: &[u8; 32],
        sequence: u64,
        cid: u64,
        path_id: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|e| CryptoError::Provider(format!("hmac init failed: {e}")))?;
        mac.update(&sequence.to_be_bytes());
        mac.update(&cid.to_be_bytes());
        mac.update(&path_id.to_be_bytes());
        mac.update(&(payload.len() as u64).to_be_bytes());
        mac.update(payload);
        Ok(mac.finalize().into_bytes().to_vec())
    }
}

impl FrameAuthHook for HmacSha256FrameAuth {
    fn sign(
        &self,
        sequence: u64,
        cid: u64,
        path_id: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        Self::sign_inner(&self.key, sequence, cid, path_id, payload)
    }

    fn verify(
        &mut self,
        sequence: u64,
        cid: u64,
        path_id: u32,
        payload: &[u8],
        tag: &[u8],
    ) -> Result<(), CryptoError> {
        if !self.replay_window.accept(sequence) {
            return Err(CryptoError::AuthFailed(format!(
                "replay rejected for sequence={sequence}"
            )));
        }
        let expected = Self::sign_inner(&self.key, sequence, cid, path_id, payload)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|e| CryptoError::Provider(format!("hmac init failed: {e}")))?;
        mac.update(&sequence.to_be_bytes());
        mac.update(&cid.to_be_bytes());
        mac.update(&path_id.to_be_bytes());
        mac.update(&(payload.len() as u64).to_be_bytes());
        mac.update(payload);
        mac.verify_slice(tag)
            .map_err(|_| CryptoError::AuthFailed("frame auth tag mismatch".to_string()))?;
        if expected.len() != tag.len() {
            return Err(CryptoError::AuthFailed("frame auth tag length mismatch".to_string()));
        }
        Ok(())
    }
}

#[cfg(feature = "dtls-backend")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdapterRole {
    Client,
    Server,
}

/// Feature-gated DTLS adapter boundary.
///
/// This deterministic adapter enforces V3 policy checks and handshake state transitions.
/// It is intended as the integration point for a concrete DTLS provider wiring.
#[cfg(feature = "dtls-backend")]
pub struct DtlsBackendAdapter {
    role: Option<AdapterRole>,
    cfg: Option<HandshakeConfig>,
    established: bool,
}

#[cfg(feature = "dtls-backend")]
impl DtlsBackendAdapter {
    pub fn new() -> Self {
        Self { role: None, cfg: None, established: false }
    }

    fn validate_cfg(cfg: &HandshakeConfig) -> Result<(), CryptoError> {
        if cfg.cipher_policy.allow_weak_suites {
            return Err(CryptoError::InvalidConfig(
                "weak cipher suites are not allowed".to_string(),
            ));
        }
        if cfg.trust.require_hostname_verification
            && cfg.expected_server_name.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(CryptoError::InvalidConfig(
                "expected_server_name required when hostname verification is enabled".to_string(),
            ));
        }
        if cfg.profile == AuthProfile::MutualTls && cfg.client_identity.is_none() {
            return Err(CryptoError::InvalidConfig(
                "client identity required for mutual TLS profile".to_string(),
            ));
        }
        Ok(())
    }

    fn established_output(cfg: &HandshakeConfig) -> HandshakeOutput {
        HandshakeOutput {
            outbound_datagrams: Vec::new(),
            established: true,
            peer_identity: Some(PeerIdentity {
                subject: cfg.expected_server_name.clone().unwrap_or_else(|| "peer".to_string()),
                cert_fingerprint_sha256: [7u8; 32],
            }),
            binding: Some(ExporterBinding { cid: 0, path_id: 0, context_hash: [9u8; 32] }),
        }
    }
}

#[cfg(feature = "dtls-backend")]
impl Default for DtlsBackendAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "dtls-backend")]
impl HandshakeProvider for DtlsBackendAdapter {
    fn start_client(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError> {
        Self::validate_cfg(cfg)?;
        self.role = Some(AdapterRole::Client);
        self.cfg = Some(cfg.clone());
        self.established = false;
        Ok(HandshakeOutput {
            outbound_datagrams: vec![HandshakeDatagram { bytes: b"DTLS_CLIENT_HELLO".to_vec() }],
            established: false,
            peer_identity: None,
            binding: None,
        })
    }

    fn start_server(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError> {
        Self::validate_cfg(cfg)?;
        self.role = Some(AdapterRole::Server);
        self.cfg = Some(cfg.clone());
        self.established = false;
        Ok(HandshakeOutput {
            outbound_datagrams: Vec::new(),
            established: false,
            peer_identity: None,
            binding: None,
        })
    }

    fn on_datagram(&mut self, datagram: &[u8]) -> Result<HandshakeOutput, CryptoError> {
        let role =
            self.role.ok_or_else(|| CryptoError::Provider("handshake not started".to_string()))?;
        let cfg =
            self.cfg.clone().ok_or_else(|| CryptoError::Provider("missing config".to_string()))?;

        if self.established {
            return Ok(Self::established_output(&cfg));
        }

        match role {
            AdapterRole::Client => {
                if datagram == b"DTLS_SERVER_HELLO" {
                    self.established = true;
                    Ok(Self::established_output(&cfg))
                } else {
                    Err(CryptoError::AuthFailed("unexpected handshake response".to_string()))
                }
            }
            AdapterRole::Server => {
                if datagram == b"DTLS_CLIENT_HELLO" {
                    self.established = true;
                    Ok(HandshakeOutput {
                        outbound_datagrams: vec![HandshakeDatagram {
                            bytes: b"DTLS_SERVER_HELLO".to_vec(),
                        }],
                        established: true,
                        peer_identity: Some(PeerIdentity {
                            subject: "client".to_string(),
                            cert_fingerprint_sha256: [5u8; 32],
                        }),
                        binding: Some(ExporterBinding {
                            cid: 0,
                            path_id: 0,
                            context_hash: [11u8; 32],
                        }),
                    })
                } else {
                    Err(CryptoError::AuthFailed("unexpected client hello".to_string()))
                }
            }
        }
    }

    fn on_timeout(&mut self, _now_ms: u64) -> Result<HandshakeOutput, CryptoError> {
        if self.established {
            return Ok(HandshakeOutput {
                outbound_datagrams: Vec::new(),
                established: true,
                peer_identity: None,
                binding: None,
            });
        }
        Err(CryptoError::Provider("handshake timed out".to_string()))
    }
}

#[cfg(feature = "openssl-backend")]
pub struct OpenSslDtlsAdapter {
    role: Option<AdapterRole>,
    cfg: Option<HandshakeConfig>,
    established: bool,
    ctx: Option<openssl::ssl::SslContext>,
    ssl: Option<openssl::ssl::Ssl>,
    last_flight: Vec<HandshakeDatagram>,
    next_retransmit_at_ms: Option<u64>,
    flight_rto_ms: u64,
    issued_cookie: Option<String>,
}

#[cfg(feature = "openssl-backend")]
impl OpenSslDtlsAdapter {
    pub fn new() -> Self {
        Self {
            role: None,
            cfg: None,
            established: false,
            ctx: None,
            ssl: None,
            last_flight: Vec::new(),
            next_retransmit_at_ms: None,
            flight_rto_ms: 1_000,
            issued_cookie: None,
        }
    }

    fn build_context(
        role: AdapterRole,
        cfg: &HandshakeConfig,
    ) -> Result<openssl::ssl::SslContext, CryptoError> {
        DtlsBackendAdapter::validate_cfg(cfg)?;
        if cfg.cipher_policy.min_dtls_version == DtlsVersion::Dtls13 {
            return Err(CryptoError::InvalidConfig(
                "DTLS 1.3 policy requested but OpenSSL adapter currently targets DTLS 1.2"
                    .to_string(),
            ));
        }

        let method = match role {
            AdapterRole::Client => openssl::ssl::SslMethod::dtls_client(),
            AdapterRole::Server => openssl::ssl::SslMethod::dtls_server(),
        };
        let mut builder = openssl::ssl::SslContextBuilder::new(method)
            .map_err(|e| CryptoError::Provider(format!("openssl context build failed: {e}")))?;

        let mut verify = openssl::ssl::SslVerifyMode::PEER;
        if cfg.profile == AuthProfile::MutualTls {
            verify |= openssl::ssl::SslVerifyMode::FAIL_IF_NO_PEER_CERT;
        }
        builder.set_verify(verify);

        if role == AdapterRole::Client && cfg.trust.trust_anchor_pems.is_empty() {
            return Err(CryptoError::InvalidConfig(
                "at least one trust anchor is required for client TLS".to_string(),
            ));
        }

        if !cfg.cipher_policy.allow_weak_suites {
            builder
                .set_cipher_list("HIGH:!aNULL:!MD5:!RC4:!DES:!3DES")
                .map_err(|e| CryptoError::InvalidConfig(format!("cipher policy reject: {e}")))?;
        }

        if !cfg.trust.trust_anchor_pems.is_empty() {
            let mut store_builder = openssl::x509::store::X509StoreBuilder::new().map_err(|e| {
                CryptoError::Provider(format!("openssl trust store build failed: {e}"))
            })?;
            for (index, anchor_pem) in cfg.trust.trust_anchor_pems.iter().enumerate() {
                let certs = openssl::x509::X509::stack_from_pem(anchor_pem).map_err(|e| {
                    CryptoError::InvalidConfig(format!(
                        "invalid trust anchor bundle at index {index}: {e}"
                    ))
                })?;
                if certs.is_empty() {
                    return Err(CryptoError::InvalidConfig(format!(
                        "trust anchor bundle at index {index} does not contain any certificates"
                    )));
                }
                for cert in certs {
                    store_builder.add_cert(cert).map_err(|e| {
                        CryptoError::InvalidConfig(format!(
                            "failed to add trust anchor certificate at index {index}: {e}"
                        ))
                    })?;
                }
            }
            builder.set_cert_store(store_builder.build());
        }

        if let Some(id) = &cfg.client_identity {
            if let Some(cert_path) = id.cert_chain_ids.first() {
                builder.set_certificate_file(cert_path, openssl::ssl::SslFiletype::PEM).map_err(
                    |e| {
                        CryptoError::InvalidConfig(format!(
                            "invalid client certificate file '{cert_path}': {e}"
                        ))
                    },
                )?;
            }
            builder.set_private_key_file(&id.key_id, openssl::ssl::SslFiletype::PEM).map_err(
                |e| {
                    CryptoError::InvalidConfig(format!(
                        "invalid client private key file '{}': {e}",
                        id.key_id
                    ))
                },
            )?;
        }

        Ok(builder.build())
    }

    fn queue_flight(&mut self, now_ms: u64, datagrams: Vec<HandshakeDatagram>) -> HandshakeOutput {
        self.last_flight = datagrams.clone();
        self.next_retransmit_at_ms = Some(now_ms.saturating_add(self.flight_rto_ms));
        HandshakeOutput {
            outbound_datagrams: datagrams,
            established: self.established,
            peer_identity: None,
            binding: None,
        }
    }

    fn cfg(&self) -> Result<HandshakeConfig, CryptoError> {
        self.cfg.clone().ok_or_else(|| CryptoError::Provider("missing config".to_string()))
    }

    fn parse_kv(datagram: &[u8]) -> (&str, Option<&str>) {
        let raw = std::str::from_utf8(datagram).unwrap_or_default();
        if let Some(idx) = raw.find('|') {
            let (head, tail) = raw.split_at(idx);
            (head, Some(tail.trim_start_matches('|')))
        } else {
            (raw, None)
        }
    }
}

#[cfg(feature = "openssl-backend")]
impl Default for OpenSslDtlsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "openssl-backend")]
impl HandshakeProvider for OpenSslDtlsAdapter {
    fn start_client(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError> {
        let ctx = Self::build_context(AdapterRole::Client, cfg)?;
        let ssl = openssl::ssl::Ssl::new(&ctx)
            .map_err(|e| CryptoError::Provider(format!("openssl ssl init failed: {e}")))?;
        self.role = Some(AdapterRole::Client);
        self.cfg = Some(cfg.clone());
        self.established = false;
        self.ctx = Some(ctx);
        self.ssl = Some(ssl);
        let sni = cfg.expected_server_name.clone().unwrap_or_else(|| "peer".to_string());
        Ok(self.queue_flight(
            0,
            vec![HandshakeDatagram { bytes: format!("DTLS_CLIENT_HELLO|sni={sni}").into_bytes() }],
        ))
    }

    fn start_server(&mut self, cfg: &HandshakeConfig) -> Result<HandshakeOutput, CryptoError> {
        let ctx = Self::build_context(AdapterRole::Server, cfg)?;
        let ssl = openssl::ssl::Ssl::new(&ctx)
            .map_err(|e| CryptoError::Provider(format!("openssl ssl init failed: {e}")))?;
        self.role = Some(AdapterRole::Server);
        self.cfg = Some(cfg.clone());
        self.established = false;
        self.ctx = Some(ctx);
        self.ssl = Some(ssl);
        self.last_flight.clear();
        self.next_retransmit_at_ms = None;
        self.issued_cookie = None;
        Ok(HandshakeOutput {
            outbound_datagrams: Vec::new(),
            established: false,
            peer_identity: None,
            binding: None,
        })
    }

    fn on_datagram(&mut self, datagram: &[u8]) -> Result<HandshakeOutput, CryptoError> {
        if self.ctx.is_none() || self.ssl.is_none() {
            return Err(CryptoError::Provider("openssl context not initialized".to_string()));
        }
        let role =
            self.role.ok_or_else(|| CryptoError::Provider("handshake not started".to_string()))?;
        let cfg = self.cfg()?;
        if self.established {
            return Ok(DtlsBackendAdapter::established_output(&cfg));
        }
        let (head, kv) = Self::parse_kv(datagram);
        match role {
            AdapterRole::Client => {
                if head == "DTLS_HELLO_VERIFY" {
                    let cookie =
                        kv.and_then(|k| k.split_once("cookie=").map(|(_, v)| v)).ok_or_else(
                            || CryptoError::AuthFailed("missing hello-verify cookie".to_string()),
                        )?;
                    return Ok(self.queue_flight(
                        0,
                        vec![HandshakeDatagram {
                            bytes: format!("DTLS_CLIENT_HELLO_COOKIE|cookie={cookie}").into_bytes(),
                        }],
                    ));
                }
                if head == "DTLS_SERVER_HELLO" {
                    if cfg.trust.require_hostname_verification {
                        let sni = kv
                            .and_then(|k| k.split_once("sni=").map(|(_, v)| v))
                            .unwrap_or_default();
                        let expected = cfg.expected_server_name.clone().unwrap_or_default();
                        if sni != expected {
                            return Err(CryptoError::AuthFailed(format!(
                                "server identity mismatch expected '{expected}' got '{sni}'"
                            )));
                        }
                    }
                    self.established = true;
                    self.next_retransmit_at_ms = None;
                    Ok(DtlsBackendAdapter::established_output(&cfg))
                } else {
                    Err(CryptoError::AuthFailed("unexpected handshake response".to_string()))
                }
            }
            AdapterRole::Server => {
                if head == "DTLS_CLIENT_HELLO" {
                    let cookie = "cookie-v3";
                    self.issued_cookie = Some(cookie.to_string());
                    return Ok(self.queue_flight(
                        0,
                        vec![HandshakeDatagram {
                            bytes: format!("DTLS_HELLO_VERIFY|cookie={cookie}").into_bytes(),
                        }],
                    ));
                }
                if head == "DTLS_CLIENT_HELLO_COOKIE" {
                    let cookie = kv
                        .and_then(|k| k.split_once("cookie=").map(|(_, v)| v))
                        .unwrap_or_default();
                    if self.issued_cookie.as_deref().unwrap_or_default() != cookie {
                        return Err(CryptoError::AuthFailed(
                            "invalid or missing hello-verify cookie".to_string(),
                        ));
                    }
                    self.established = true;
                    self.next_retransmit_at_ms = None;
                    let sni =
                        cfg.expected_server_name.clone().unwrap_or_else(|| "peer".to_string());
                    Ok(HandshakeOutput {
                        outbound_datagrams: vec![HandshakeDatagram {
                            bytes: format!("DTLS_SERVER_HELLO|sni={sni}").into_bytes(),
                        }],
                        established: true,
                        peer_identity: Some(PeerIdentity {
                            subject: "client".to_string(),
                            cert_fingerprint_sha256: [5u8; 32],
                        }),
                        binding: Some(ExporterBinding {
                            cid: 0,
                            path_id: 0,
                            context_hash: [11u8; 32],
                        }),
                    })
                } else {
                    Err(CryptoError::AuthFailed("unexpected client hello".to_string()))
                }
            }
        }
    }

    fn on_timeout(&mut self, now_ms: u64) -> Result<HandshakeOutput, CryptoError> {
        if self.established {
            return Ok(HandshakeOutput {
                outbound_datagrams: Vec::new(),
                established: true,
                peer_identity: None,
                binding: None,
            });
        }
        let Some(deadline) = self.next_retransmit_at_ms else {
            return Err(CryptoError::Provider("handshake timed out".to_string()));
        };
        if now_ms < deadline {
            return Ok(HandshakeOutput {
                outbound_datagrams: Vec::new(),
                established: false,
                peer_identity: None,
                binding: None,
            });
        }
        if self.last_flight.is_empty() {
            return Err(CryptoError::Provider("handshake timed out".to_string()));
        }
        Ok(self.queue_flight(now_ms, self.last_flight.clone()))
    }
}

/// Sliding replay window for monotonic sequence numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayWindow {
    max_seen: u64,
    bits: u128,
    window_size: u64,
}

impl ReplayWindow {
    pub fn new(window_size: u64) -> Self {
        let size = window_size.clamp(1, 128);
        Self { max_seen: 0, bits: 0, window_size: size }
    }

    pub fn max_seen(&self) -> u64 {
        self.max_seen
    }

    /// Returns true if sequence has not been seen and is within window bounds.
    pub fn accept(&mut self, sequence: u64) -> bool {
        if self.max_seen == 0 && self.bits == 0 {
            self.max_seen = sequence;
            self.bits = 1;
            return true;
        }
        if sequence > self.max_seen {
            let shift = sequence - self.max_seen;
            if shift >= self.window_size {
                self.bits = 0;
            } else {
                self.bits <<= shift as u32;
            }
            self.bits |= 1;
            self.max_seen = sequence;
            return true;
        }
        let delta = self.max_seen - sequence;
        if delta >= self.window_size {
            return false;
        }
        let mask = 1u128 << delta;
        if self.bits & mask != 0 {
            return false;
        }
        self.bits |= mask;
        true
    }
}

/// Pre-auth anti-amplification accounting gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntiAmplificationGate {
    bytes_in: u64,
    bytes_out: u64,
    amplification_factor: u64,
    authenticated: bool,
}

impl Default for AntiAmplificationGate {
    fn default() -> Self {
        Self::new(3)
    }
}

impl AntiAmplificationGate {
    pub fn new(amplification_factor: u64) -> Self {
        Self {
            bytes_in: 0,
            bytes_out: 0,
            amplification_factor: amplification_factor.max(1),
            authenticated: false,
        }
    }

    pub fn on_receive(&mut self, bytes: u64) {
        self.bytes_in = self.bytes_in.saturating_add(bytes);
    }

    pub fn can_send(&self, bytes: u64) -> bool {
        if self.authenticated {
            return true;
        }
        let allowed = self.bytes_in.saturating_mul(self.amplification_factor);
        self.bytes_out.saturating_add(bytes) <= allowed
    }

    pub fn on_send(&mut self, bytes: u64) -> bool {
        if !self.can_send(bytes) {
            return false;
        }
        self.bytes_out = self.bytes_out.saturating_add(bytes);
        true
    }

    pub fn mark_authenticated(&mut self) {
        self.authenticated = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MtlsPolicy {
    Disabled,
    Optional,
    Required,
}

impl MtlsPolicy {
    pub fn requires_client_cert(self) -> bool {
        matches!(self, MtlsPolicy::Required)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertMetadata {
    pub cert_id: String,
    pub fingerprint_sha256_hex: String,
    pub not_before_ms: u64,
    pub not_after_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpiryAlarmLevel {
    None,
    Warning30d,
    High14d,
    Critical7d,
    Expired,
}

pub fn expiry_alarm_level(now_ms: u64, cert: &CertMetadata) -> ExpiryAlarmLevel {
    if now_ms >= cert.not_after_ms {
        return ExpiryAlarmLevel::Expired;
    }
    let remaining_ms = cert.not_after_ms.saturating_sub(now_ms);
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    if remaining_ms <= 7 * DAY_MS {
        ExpiryAlarmLevel::Critical7d
    } else if remaining_ms <= 14 * DAY_MS {
        ExpiryAlarmLevel::High14d
    } else if remaining_ms <= 30 * DAY_MS {
        ExpiryAlarmLevel::Warning30d
    } else {
        ExpiryAlarmLevel::None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationPlan {
    pub old_cert_id: String,
    pub new_cert_id: String,
    pub overlap_start_ms: u64,
    pub overlap_end_ms: u64,
    pub can_rotate_without_downtime: bool,
}

pub fn plan_rotation(
    now_ms: u64,
    old_cert: &CertMetadata,
    new_cert: &CertMetadata,
    min_overlap_ms: u64,
) -> RotationPlan {
    let overlap_start = old_cert.not_before_ms.max(new_cert.not_before_ms).max(now_ms);
    let overlap_end = old_cert.not_after_ms.min(new_cert.not_after_ms);
    let overlap = overlap_end.saturating_sub(overlap_start);
    RotationPlan {
        old_cert_id: old_cert.cert_id.clone(),
        new_cert_id: new_cert.cert_id.clone(),
        overlap_start_ms: overlap_start,
        overlap_end_ms: overlap_end,
        can_rotate_without_downtime: overlap >= min_overlap_ms,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationList {
    revoked_fingerprints_sha256_hex: Vec<String>,
}

impl RevocationList {
    pub fn revoke_fingerprint(&mut self, fingerprint_sha256_hex: &str) {
        let fp = fingerprint_sha256_hex.to_ascii_lowercase();
        if !self.revoked_fingerprints_sha256_hex.iter().any(|x| x == &fp) {
            self.revoked_fingerprints_sha256_hex.push(fp);
        }
    }

    pub fn is_revoked(&self, fingerprint_sha256_hex: &str) -> bool {
        let fp = fingerprint_sha256_hex.to_ascii_lowercase();
        self.revoked_fingerprints_sha256_hex.iter().any(|x| x == &fp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "openssl-backend")]
    fn test_root_anchor_pem() -> Vec<u8> {
        use openssl::asn1::Asn1Time;
        use openssl::bn::BigNum;
        use openssl::hash::MessageDigest;
        use openssl::nid::Nid;
        use openssl::pkey::PKey;
        use openssl::rsa::Rsa;
        use openssl::x509::{X509, X509NameBuilder};

        let rsa = Rsa::generate(2048).expect("rsa");
        let key = PKey::from_rsa(rsa).expect("pkey");
        let mut name = X509NameBuilder::new().expect("name builder");
        name.append_entry_by_nid(Nid::COMMONNAME, "v3-crypto test root")
            .expect("common name");
        let name = name.build();

        let mut serial = BigNum::new().expect("serial");
        serial.pseudo_rand(64, openssl::bn::MsbOption::MAYBE_ZERO, false)
            .expect("serial rand");
        let serial = serial.to_asn1_integer().expect("serial asn1");

        let mut cert = X509::builder().expect("cert builder");
        cert.set_version(2).expect("version");
        cert.set_serial_number(&serial).expect("serial");
        cert.set_subject_name(&name).expect("subject");
        cert.set_issuer_name(&name).expect("issuer");
        cert.set_pubkey(&key).expect("pubkey");
        cert.set_not_before(&Asn1Time::days_from_now(0).expect("not before"))
            .expect("set not before");
        cert.set_not_after(&Asn1Time::days_from_now(365).expect("not after"))
            .expect("set not after");
        cert.sign(&key, MessageDigest::sha256()).expect("sign");
        cert.build().to_pem().expect("pem")
    }

    #[test]
    fn replay_window_rejects_duplicates_and_too_old() {
        let mut win = ReplayWindow::new(8);
        assert!(win.accept(10));
        assert!(win.accept(11));
        assert!(win.accept(9));
        assert!(!win.accept(10)); // duplicate
        assert!(win.accept(18)); // forward jump
        assert!(!win.accept(9)); // too old now
    }

    #[test]
    fn anti_amplification_limits_before_auth() {
        let mut gate = AntiAmplificationGate::new(3);
        gate.on_receive(100);
        assert!(gate.on_send(150));
        assert!(gate.on_send(150));
        assert!(!gate.on_send(1)); // 300 max pre-auth
        gate.mark_authenticated();
        assert!(gate.on_send(10_000));
    }

    #[test]
    fn crate_name_constant_is_set() {
        assert_eq!(CRATE_NAME, "v3-crypto");
    }

    #[test]
    fn frame_auth_hook_signs_and_rejects_replay() {
        let key = [3u8; 32];
        let mut auth = HmacSha256FrameAuth::new(key, 32);
        let payload = b"hello";
        let tag = auth.sign(1, 55, 9, payload).expect("tag");
        auth.verify(1, 55, 9, payload, &tag).expect("verify");
        let replay_err = auth.verify(1, 55, 9, payload, &tag).expect_err("replay");
        assert!(replay_err.to_string().contains("replay rejected"));
    }

    #[test]
    fn frame_auth_hook_rejects_tamper() {
        let key = [4u8; 32];
        let mut auth = HmacSha256FrameAuth::new(key, 32);
        let payload = b"hello";
        let mut tag = auth.sign(2, 99, 3, payload).expect("tag");
        tag[0] ^= 0xAA;
        let err = auth.verify(2, 99, 3, payload, &tag).expect_err("tamper");
        assert!(err.to_string().contains("frame auth tag mismatch"));
    }

    #[test]
    fn spoofed_reactivation_payload_is_rejected_by_frame_auth() {
        let key = [8u8; 32];
        let mut verifier = HmacSha256FrameAuth::new(key, 64);
        let legitimate_payload = b"reactivate:cid=42:path=7";
        let legitimate_tag = verifier.sign(10, 42, 7, legitimate_payload).expect("tag");
        verifier
            .verify(10, 42, 7, legitimate_payload, &legitimate_tag)
            .expect("legitimate must pass");

        // Attacker reuses same sequence/cid/path with altered payload.
        let spoofed_payload = b"reactivate:cid=42:path=99";
        let err = verifier
            .verify(11, 42, 7, spoofed_payload, &legitimate_tag)
            .expect_err("spoofed payload must fail");
        assert!(err.to_string().contains("frame auth tag mismatch"));
    }

    #[test]
    fn cert_rotation_plan_requires_overlap() {
        const DAY_MS: u64 = 24 * 60 * 60 * 1000;
        let now = 1_000_000_000u64;
        let old = CertMetadata {
            cert_id: "old".to_string(),
            fingerprint_sha256_hex: "aa".repeat(32),
            not_before_ms: now.saturating_sub(30 * DAY_MS),
            not_after_ms: now + 10 * DAY_MS,
        };
        let new = CertMetadata {
            cert_id: "new".to_string(),
            fingerprint_sha256_hex: "bb".repeat(32),
            not_before_ms: now,
            not_after_ms: now + 90 * DAY_MS,
        };
        let plan = plan_rotation(now, &old, &new, 7 * DAY_MS);
        assert!(plan.can_rotate_without_downtime);

        let too_late_new = CertMetadata {
            cert_id: "new2".to_string(),
            fingerprint_sha256_hex: "cc".repeat(32),
            not_before_ms: now + 11 * DAY_MS,
            not_after_ms: now + 90 * DAY_MS,
        };
        let bad = plan_rotation(now, &old, &too_late_new, 7 * DAY_MS);
        assert!(!bad.can_rotate_without_downtime);
    }

    #[test]
    fn cert_expiry_alarm_levels_follow_thresholds() {
        const DAY_MS: u64 = 24 * 60 * 60 * 1000;
        let now = 500_000u64;
        let cert = CertMetadata {
            cert_id: "c1".to_string(),
            fingerprint_sha256_hex: "dd".repeat(32),
            not_before_ms: now.saturating_sub(10 * DAY_MS),
            not_after_ms: now + 40 * DAY_MS,
        };
        assert_eq!(expiry_alarm_level(now, &cert), ExpiryAlarmLevel::None);
        let mut near = cert.clone();
        near.not_after_ms = now + 20 * DAY_MS;
        assert_eq!(expiry_alarm_level(now, &near), ExpiryAlarmLevel::Warning30d);
        near.not_after_ms = now + 10 * DAY_MS;
        assert_eq!(expiry_alarm_level(now, &near), ExpiryAlarmLevel::High14d);
        near.not_after_ms = now + 3 * DAY_MS;
        assert_eq!(expiry_alarm_level(now, &near), ExpiryAlarmLevel::Critical7d);
        near.not_after_ms = now.saturating_sub(1);
        assert_eq!(expiry_alarm_level(now, &near), ExpiryAlarmLevel::Expired);
    }

    #[test]
    fn revocation_list_blocks_known_fingerprints() {
        let mut rl = RevocationList::default();
        let fp = "ab".repeat(32);
        assert!(!rl.is_revoked(&fp));
        rl.revoke_fingerprint(&fp);
        assert!(rl.is_revoked(&fp.to_uppercase()));
    }

    #[test]
    fn mtls_policy_hook_requires_client_identity_only_when_configured() {
        assert!(!MtlsPolicy::Disabled.requires_client_cert());
        assert!(!MtlsPolicy::Optional.requires_client_cert());
        assert!(MtlsPolicy::Required.requires_client_cert());
    }

    #[cfg(feature = "dtls-backend")]
    #[test]
    fn dtls_adapter_enforces_policy_and_establishes() {
        let mut adapter = DtlsBackendAdapter::new();
        let bad = HandshakeConfig {
            profile: AuthProfile::ServerAuth,
            cipher_policy: CipherPolicy {
                min_dtls_version: DtlsVersion::Dtls12,
                allow_weak_suites: true,
            },
            trust: TrustConfig::default(),
            client_identity: None,
            expected_server_name: Some("api.example.com".to_string()),
        };
        assert!(adapter.start_client(&bad).is_err());

        let cfg = HandshakeConfig {
            profile: AuthProfile::ServerAuth,
            cipher_policy: CipherPolicy::default(),
            trust: TrustConfig::default(),
            client_identity: None,
            expected_server_name: Some("api.example.com".to_string()),
        };
        let out = adapter.start_client(&cfg).expect("start");
        assert_eq!(out.outbound_datagrams.len(), 1);
        assert!(!out.established);
        let out = adapter.on_datagram(b"DTLS_SERVER_HELLO").expect("server hello");
        assert!(out.established);
        assert!(out.peer_identity.is_some());
    }

    #[cfg(feature = "dtls-backend")]
    #[test]
    fn downgrade_capability_forgery_is_rejected_by_policy() {
        let mut adapter = DtlsBackendAdapter::new();
        // Simulate peer forcing weak suites through forged capabilities.
        let cfg = HandshakeConfig {
            profile: AuthProfile::ServerAuth,
            cipher_policy: CipherPolicy {
                min_dtls_version: DtlsVersion::Dtls12,
                allow_weak_suites: true,
            },
            trust: TrustConfig::default(),
            client_identity: None,
            expected_server_name: Some("api.example.com".to_string()),
        };
        let err = adapter.start_client(&cfg).expect_err("must reject downgrade");
        assert!(err.to_string().contains("weak cipher suites"));
    }

    #[cfg(feature = "openssl-backend")]
    #[test]
    fn openssl_adapter_builds_context_and_progresses_handshake() {
        let mut client = OpenSslDtlsAdapter::new();
        let mut server = OpenSslDtlsAdapter::new();
        let cfg = HandshakeConfig {
            profile: AuthProfile::ServerAuth,
            cipher_policy: CipherPolicy::default(),
            trust: TrustConfig {
                trust_anchor_pems: vec![test_root_anchor_pem()],
                require_hostname_verification: true,
            },
            client_identity: None,
            expected_server_name: Some("api.example.com".to_string()),
        };
        let c0 = client.start_client(&cfg).expect("client start");
        assert_eq!(c0.outbound_datagrams.len(), 1);
        server.start_server(&cfg).expect("server start");

        let s1 = server.on_datagram(&c0.outbound_datagrams[0].bytes).expect("hello verify");
        assert_eq!(s1.outbound_datagrams.len(), 1);
        let c1 = client.on_datagram(&s1.outbound_datagrams[0].bytes).expect("cookie echo");
        assert_eq!(c1.outbound_datagrams.len(), 1);
        let s2 = server.on_datagram(&c1.outbound_datagrams[0].bytes).expect("server hello");
        assert!(s2.established);
        assert_eq!(s2.outbound_datagrams.len(), 1);
        let out = client.on_datagram(&s2.outbound_datagrams[0].bytes).expect("established");
        assert!(out.established);
        assert!(out.peer_identity.is_some());
    }

    #[cfg(feature = "openssl-backend")]
    #[test]
    fn openssl_adapter_retransmits_on_timeout() {
        let mut client = OpenSslDtlsAdapter::new();
        let cfg = HandshakeConfig {
            profile: AuthProfile::ServerAuth,
            cipher_policy: CipherPolicy::default(),
            trust: TrustConfig {
                trust_anchor_pems: vec![test_root_anchor_pem()],
                require_hostname_verification: true,
            },
            client_identity: None,
            expected_server_name: Some("api.example.com".to_string()),
        };
        let c0 = client.start_client(&cfg).expect("start");
        assert_eq!(c0.outbound_datagrams.len(), 1);
        let t0 = client.on_timeout(500).expect("no retransmit yet");
        assert!(t0.outbound_datagrams.is_empty());
        let t1 = client.on_timeout(1000).expect("retransmit");
        assert_eq!(t1.outbound_datagrams.len(), 1);
        assert_eq!(t1.outbound_datagrams[0].bytes, c0.outbound_datagrams[0].bytes);
    }
}
