//! v3-ffi
//!
//! Stable C ABI wrapper over V3 runtimes.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use jni::objects::{JByteArray, JClass};
use jni::sys::{jint, jlong};
use jni::JNIEnv;
use serde_json::json;
use v3_client_runtime::{ClientRuntime, ClientRuntimeEvent, ClientRuntimeOutput};
use v3_core::{CoreConfig, Event};
use v3_crypto::{
    AuthProfile, CipherPolicy, HandshakeConfig, HandshakeProvider, OpenSslDtlsAdapter, TrustConfig,
};
use v3_nat::RelayPolicy;
use v3_server_runtime::{RuntimeEvent, ServerRuntime, ServerRuntimeOutput};
use v3_store::InMemorySessionStore;
use v3_wire::WireFrame;

pub const V3_FFI_ABI_VERSION: u32 = 1;

enum EngineKind {
    Client(Box<ClientRuntime>),
    Server(Box<ServerRuntime>),
}

#[derive(Default)]
enum TlsMode {
    #[default]
    Disabled,
    Client(ClientTlsState),
    Server(ServerTlsState),
}

struct ClientTlsState {
    adapter: OpenSslDtlsAdapter,
    cfg: HandshakeConfig,
    established: bool,
    core_started: bool,
}

struct ServerTlsState {
    server_name: String,
    cert_path: String,
    key_path: String,
    sessions_by_path: HashMap<u32, ServerPathTlsState>,
}

struct ServerPathTlsState {
    adapter: OpenSslDtlsAdapter,
    established: bool,
}

#[derive(Debug, Default)]
struct OutputBuffer {
    datagrams: Vec<(u32, Vec<u8>)>,
    events: Vec<String>,
}

struct EngineHandle {
    kind: EngineKind,
    tls: TlsMode,
    output: OutputBuffer,
    last_error: Option<String>,
}

#[derive(Default)]
struct Registry {
    handles: HashMap<u64, EngineHandle>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static PACKET_DEDUP: OnceLock<Mutex<HashMap<String, (u128, u32)>>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn cfg(msr_interval_ms: u64, miss_window_count: u64, disconnect_window_ms: u64) -> CoreConfig {
    CoreConfig { msr_interval_ms, miss_window_count, disconnect_window_ms }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn packet_hash64(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn dedup_note(
    side: &str,
    dir: &str,
    path_id: u32,
    kind: &str,
    cid: u64,
    bytes: &[u8],
    ts: u128,
) -> String {
    let key = format!("{side}|{dir}|{path_id}|{kind}|{cid}|{:016x}", packet_hash64(bytes));
    let map = PACKET_DEDUP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("packet dedup mutex poisoned");
    let mut out = String::from("dup=false");
    if let Some((last_ts, count)) = guard.get_mut(&key) {
        let delta = ts.saturating_sub(*last_ts);
        *last_ts = ts;
        *count = count.saturating_add(1);
        out = format!("dup=true dup_count={} dup_delta_ms={}", *count, delta);
    } else {
        guard.insert(key, (ts, 1));
    }
    if guard.len() > 20_000 {
        let cutoff = ts.saturating_sub(60_000);
        guard.retain(|_, (last_ts, _)| *last_ts >= cutoff);
    }
    out
}

fn log_proto_packet(side: &str, dir: &str, path_id: u32, bytes: &[u8]) {
    let ts = now_ms();
    if is_dtls_packet(bytes) {
        let note = dedup_note(side, dir, path_id, "DTLS", 0, bytes, ts);
        eprintln!(
            "[V3PROTO] ts_ms={ts} side={side} dir={dir} path={path_id} type=DTLS len={} {note}",
            bytes.len(),
        );
        return;
    }
    match WireFrame::decode(bytes) {
        Ok(frame) => {
            let kind = format!("{:?}", frame.message_type);
            let note = dedup_note(side, dir, path_id, &kind, frame.cid, bytes, ts);
            eprintln!(
                "[V3PROTO] ts_ms={ts} side={side} dir={dir} path={path_id} type={:?} cid={} payload_len={} tlv_count={} {note}",
                frame.message_type,
                frame.cid,
                frame.payload.len(),
                frame.tlvs.len()
            );
        }
        Err(err) => {
            let note = dedup_note(side, dir, path_id, "UNKNOWN", 0, bytes, ts);
            eprintln!(
                "[V3PROTO] ts_ms={ts} side={side} dir={dir} path={path_id} type=UNKNOWN len={} err={err} {note}",
                bytes.len(),
            );
        }
    }
}

fn normalize_client_output(out: ClientRuntimeOutput, buffer: &mut OutputBuffer) {
    let mut datagrams = Vec::with_capacity(out.outbound_datagrams.len());
    for d in out.outbound_datagrams {
        log_proto_packet("client", "send", d.path_id, &d.bytes);
        datagrams.push((d.path_id, d.bytes));
    }
    buffer.datagrams = datagrams;
    buffer.events = out
        .events
        .into_iter()
        .map(|e| match e {
            ClientRuntimeEvent::Core(inner) => match inner {
                Event::MessageReceived { msg_id, payload } => json!({
                    "kind":"client_deliver",
                    "msg_id":msg_id,
                    "payload_b64":base64::engine::general_purpose::STANDARD.encode(payload)
                })
                .to_string(),
                _ => json!({"kind": "client_core", "event": format!("{:?}", inner)}).to_string(),
            },
            ClientRuntimeEvent::NatPairNominated { local_index, remote_index, path_id } => json!({
                "kind":"client_nat_nomination",
                "local_index":local_index,
                "remote_index":remote_index,
                "path_id":path_id
            })
            .to_string(),
            ClientRuntimeEvent::TurnAction(action) => {
                json!({"kind":"client_turn","action": format!("{:?}", action)}).to_string()
            }
            ClientRuntimeEvent::ConsentAction(action) => {
                json!({"kind":"client_consent","action": format!("{:?}", action)}).to_string()
            }
        })
        .collect();
}

fn normalize_server_output(out: ServerRuntimeOutput, buffer: &mut OutputBuffer) {
    let mut datagrams = Vec::with_capacity(out.outbound_datagrams.len());
    for d in out.outbound_datagrams {
        log_proto_packet("server", "send", d.path_id, &d.bytes);
        datagrams.push((d.path_id, d.bytes));
    }
    buffer.datagrams = datagrams;
    buffer.events = out
        .events
        .into_iter()
        .map(|e| match e {
            RuntimeEvent::ParseError => json!({"kind":"server_parse_error"}).to_string(),
            RuntimeEvent::CoreEvent { cid, event } => {
                json!({"kind":"server_core","cid":cid,"event":format!("{:?}", event)}).to_string()
            }
            RuntimeEvent::NatPairNominated { cid, local_index, remote_index } => json!({
                "kind":"server_nat_nomination",
                "cid":cid,
                "local_index":local_index,
                "remote_index":remote_index
            })
            .to_string(),
            RuntimeEvent::TurnAction { cid, action } => {
                json!({"kind":"server_turn","cid":cid,"action": format!("{:?}", action)})
                    .to_string()
            }
            RuntimeEvent::VhostPathBound { cid, vhost, path_id } => {
                json!({"kind":"server_vhost_path","cid":cid,"vhost":vhost,"path_id":path_id})
                    .to_string()
            }
            RuntimeEvent::MessageReceived { cid, msg_id, payload_b64 } => json!({
                "kind":"server_deliver",
                "cid":cid,
                "msg_id":msg_id,
                "payload_b64":payload_b64
            })
            .to_string(),
        })
        .collect();
}

unsafe fn bytes_from_ptr(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller promises pointer validity for `len` bytes.
    unsafe { slice::from_raw_parts(ptr, len) }
}

fn set_error(handle: &mut EngineHandle, err: String) -> i32 {
    handle.last_error = Some(err);
    -1
}

fn is_dtls_packet(bytes: &[u8]) -> bool {
    bytes.starts_with(b"DTLS_")
}

fn build_client_tls_cfg(
    expected_server_name: &str,
    trust_anchor_path: Option<&str>,
) -> Result<HandshakeConfig, String> {
    if expected_server_name.trim().is_empty() {
        return Err("expected_server_name is required for TLS".to_string());
    }
    let mut trust_anchor_ids = Vec::new();
    if let Some(path) = trust_anchor_path.map(str::trim).filter(|s| !s.is_empty()) {
        if !std::path::Path::new(path).exists() {
            return Err(format!("trust anchor path does not exist: {path}"));
        }
        trust_anchor_ids.push(path.to_string());
    } else {
        return Err("trust anchor path is required for V3 TLS mode".to_string());
    }

    Ok(HandshakeConfig {
        profile: AuthProfile::ServerAuth,
        cipher_policy: CipherPolicy::default(),
        trust: TrustConfig { trust_anchor_ids, require_hostname_verification: true },
        client_identity: None,
        expected_server_name: Some(expected_server_name.to_string()),
    })
}

fn build_server_tls_cfg(
    server_name: &str,
    cert_path: &str,
    key_path: &str,
) -> Result<HandshakeConfig, String> {
    if server_name.trim().is_empty() {
        return Err("server_name is required for TLS".to_string());
    }
    if cert_path.trim().is_empty() || key_path.trim().is_empty() {
        return Err("server TLS cert/key paths are required".to_string());
    }
    if !std::path::Path::new(cert_path).exists() {
        return Err(format!("server cert path does not exist: {cert_path}"));
    }
    if !std::path::Path::new(key_path).exists() {
        return Err(format!("server key path does not exist: {key_path}"));
    }
    Ok(HandshakeConfig {
        profile: AuthProfile::ServerAuth,
        cipher_policy: CipherPolicy::default(),
        trust: TrustConfig { trust_anchor_ids: Vec::new(), require_hostname_verification: false },
        client_identity: Some(v3_crypto::ClientIdentityRef {
            key_id: key_path.trim().to_string(),
            cert_chain_ids: vec![cert_path.trim().to_string()],
        }),
        expected_server_name: Some(server_name.to_string()),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_abi_version() -> u32 {
    V3_FFI_ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_new_client(
    cid: u64,
    msr_interval_ms: u64,
    miss_window_count: u64,
    disconnect_window_ms: u64,
) -> u64 {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let mut reg = registry().lock().expect("registry mutex poisoned");
    reg.handles.insert(
        id,
        EngineHandle {
            kind: EngineKind::Client(
                ClientRuntime::new(
                    cid,
                    cfg(msr_interval_ms, miss_window_count, disconnect_window_ms),
                )
                .into(),
            ),
            tls: TlsMode::Disabled,
            output: OutputBuffer::default(),
            last_error: None,
        },
    );
    id
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_new_server(
    msr_interval_ms: u64,
    miss_window_count: u64,
    disconnect_window_ms: u64,
) -> u64 {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let store = Arc::new(InMemorySessionStore::with_default_shards());
    reg.handles.insert(
        id,
        EngineHandle {
            kind: EngineKind::Server(
                ServerRuntime::new(
                    cfg(msr_interval_ms, miss_window_count, disconnect_window_ms),
                    store,
                )
                .into(),
            ),
            tls: TlsMode::Disabled,
            output: OutputBuffer::default(),
            last_error: None,
        },
    );
    id
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_free(handle_id: u64) {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let _ = reg.handles.remove(&handle_id);
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_begin_handshake(handle_id: u64, now_ms: u64) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    match &mut handle.kind {
        EngineKind::Client(runtime) => match &mut handle.tls {
            TlsMode::Client(tls) if !tls.established => match tls.adapter.start_client(&tls.cfg) {
                Ok(hs) => {
                    handle.output.datagrams =
                        hs.outbound_datagrams.into_iter().map(|d| (0, d.bytes)).collect();
                    handle.output.events = vec![json!({
                        "kind":"client_tls",
                        "event":"handshake_started"
                    })
                    .to_string()];
                    0
                }
                Err(e) => set_error(handle, format!("tls start failed: {e}")),
            },
            _ => match runtime.begin_handshake(now_ms) {
                Ok(out) => {
                    normalize_client_output(out, &mut handle.output);
                    0
                }
                Err(e) => set_error(handle, e.to_string()),
            },
        },
        EngineKind::Server(_) => 0,
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_on_datagram(
    handle_id: u64,
    now_ms: u64,
    path_id: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    // SAFETY: FFI caller provides pointer/len.
    let bytes = unsafe { bytes_from_ptr(data_ptr, data_len) };
    let side = match &handle.kind {
        EngineKind::Client(_) => "client",
        EngineKind::Server(_) => "server",
    };
    log_proto_packet(side, "recv", path_id, bytes);
    match &mut handle.kind {
        EngineKind::Client(runtime) => {
            if let TlsMode::Client(tls) = &mut handle.tls {
                if !tls.established {
                    let hs = tls.adapter.on_datagram(bytes);
                    match hs {
                        Ok(out) => {
                            handle.output.datagrams = out
                                .outbound_datagrams
                                .into_iter()
                                .map(|d| {
                                    log_proto_packet("client", "send", path_id, &d.bytes);
                                    (path_id, d.bytes)
                                })
                                .collect();
                            handle.output.events = Vec::new();
                            if out.established {
                                tls.established = true;
                                handle.output.events.push(
                                    json!({"kind":"client_tls","event":"handshake_established"})
                                        .to_string(),
                                );
                                if !tls.core_started {
                                    tls.core_started = true;
                                    match runtime.begin_handshake(now_ms) {
                                        Ok(core_out) => {
                                            handle.output.datagrams.extend(
                                                core_out
                                                    .outbound_datagrams
                                                    .into_iter()
                                                    .map(|d| (d.path_id, d.bytes)),
                                            );
                                            handle.output.events.extend(
                                                core_out.events.into_iter().map(|e| {
                                                    json!({"kind":"client_core","event": format!("{:?}", e)})
                                                        .to_string()
                                                }),
                                            );
                                        }
                                        Err(e) => return set_error(handle, e.to_string()),
                                    }
                                }
                            }
                            return 0;
                        }
                        Err(e) => return set_error(handle, format!("tls datagram failed: {e}")),
                    }
                }
                if is_dtls_packet(bytes) {
                    handle.output = OutputBuffer::default();
                    return 0;
                }
            }
            match runtime.on_datagram(now_ms, path_id, bytes) {
                Ok(out) => {
                    normalize_client_output(out, &mut handle.output);
                    0
                }
                Err(e) => set_error(handle, e.to_string()),
            }
        }
        EngineKind::Server(runtime) => {
            if let TlsMode::Server(tls) = &mut handle.tls {
                let session = tls.sessions_by_path.entry(path_id).or_insert_with(|| {
                    let cfg = build_server_tls_cfg(&tls.server_name, &tls.cert_path, &tls.key_path)
                        .expect("validated tls config");
                    let mut adapter = OpenSslDtlsAdapter::new();
                    let _ = adapter.start_server(&cfg);
                    ServerPathTlsState { adapter, established: false }
                });
                if !session.established || is_dtls_packet(bytes) {
                    if is_dtls_packet(bytes) {
                        match session.adapter.on_datagram(bytes) {
                            Ok(out) => {
                                handle.output.datagrams = out
                                    .outbound_datagrams
                                    .into_iter()
                                    .map(|d| {
                                        log_proto_packet("server", "send", path_id, &d.bytes);
                                        (path_id, d.bytes)
                                    })
                                    .collect();
                                handle.output.events = Vec::new();
                                if out.established {
                                    session.established = true;
                                    handle.output.events.push(
                                        json!({"kind":"server_tls","event":"handshake_established","path_id":path_id})
                                            .to_string(),
                                    );
                                }
                                return 0;
                            }
                            Err(e) => {
                                return set_error(
                                    handle,
                                    format!("server tls datagram failed: {e}"),
                                );
                            }
                        }
                    }
                    handle.output = OutputBuffer::default();
                    return 0;
                }
            }
            match runtime.on_datagram(now_ms, path_id, bytes) {
                Ok(out) => {
                    normalize_server_output(out, &mut handle.output);
                    0
                }
                Err(e) => set_error(handle, e.to_string()),
            }
        }
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_send_message(handle_id: u64, data_ptr: *const u8, data_len: usize) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    // SAFETY: FFI caller provides pointer/len.
    let bytes = unsafe { bytes_from_ptr(data_ptr, data_len) }.to_vec();
    match &mut handle.kind {
        EngineKind::Client(runtime) => match runtime.send_message(bytes) {
            Ok(out) => {
                normalize_client_output(out, &mut handle.output);
                0
            }
            Err(e) => set_error(handle, e.to_string()),
        },
        EngineKind::Server(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_close_session(handle_id: u64) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    match &mut handle.kind {
        EngineKind::Client(runtime) => match runtime.close_session() {
            Ok(out) => {
                normalize_client_output(out, &mut handle.output);
                0
            }
            Err(e) => set_error(handle, e.to_string()),
        },
        EngineKind::Server(_) => -1,
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_configure_nat(
    handle_id: u64,
    relay_policy: i32,
    stun_uri_ptr: *const u8,
    stun_uri_len: usize,
    turn_uri_ptr: *const u8,
    turn_uri_len: usize,
    turn_realm_ptr: *const u8,
    turn_realm_len: usize,
    turn_username_ptr: *const u8,
    turn_username_len: usize,
    turn_password_ptr: *const u8,
    turn_password_len: usize,
) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };

    let stun_uri = unsafe { bytes_from_ptr(stun_uri_ptr, stun_uri_len) };
    let turn_uri = unsafe { bytes_from_ptr(turn_uri_ptr, turn_uri_len) };
    let turn_realm = unsafe { bytes_from_ptr(turn_realm_ptr, turn_realm_len) };
    let turn_username = unsafe { bytes_from_ptr(turn_username_ptr, turn_username_len) };
    let turn_password = unsafe { bytes_from_ptr(turn_password_ptr, turn_password_len) };

    let mut to_str = |b: &[u8], what: &str| -> Result<String, i32> {
        std::str::from_utf8(b)
            .map(|v| v.to_string())
            .map_err(|e| set_error(handle, format!("invalid {what} utf8: {e}")))
    };
    let stun_uri = match to_str(stun_uri, "stun_uri") {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    let turn_uri = match to_str(turn_uri, "turn_uri") {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    let turn_realm = match to_str(turn_realm, "turn_realm") {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    let turn_username = match to_str(turn_username, "turn_username") {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    let turn_password = match to_str(turn_password, "turn_password") {
        Ok(v) => v,
        Err(rc) => return rc,
    };

    let policy = if relay_policy == 1 { RelayPolicy::RelayOnly } else { RelayPolicy::PreferDirect };

    match &mut handle.kind {
        EngineKind::Client(runtime) => runtime.set_relay_policy(policy),
        EngineKind::Server(runtime) => runtime.set_relay_policy(policy),
    }

    handle.output.events = vec![json!({
        "kind":"nat_config",
        "relay_policy": if relay_policy == 1 { "relay_only" } else { "prefer_direct" },
        "stun_uri": stun_uri,
        "turn_uri": turn_uri,
        "turn_realm": turn_realm,
        "turn_username": turn_username,
        "turn_password_set": !turn_password.is_empty()
    })
    .to_string()];
    handle.output.datagrams.clear();
    0
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_server_send_message(
    handle_id: u64,
    cid: u64,
    data_ptr: *const u8,
    data_len: usize,
    now_ms: u64,
) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    // SAFETY: FFI caller provides pointer/len.
    let bytes = unsafe { bytes_from_ptr(data_ptr, data_len) }.to_vec();
    match &mut handle.kind {
        EngineKind::Client(_) => -1,
        EngineKind::Server(runtime) => match runtime.send_message(cid, bytes, now_ms) {
            Ok(out) => {
                normalize_server_output(out, &mut handle.output);
                0
            }
            Err(e) => set_error(handle, e.to_string()),
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_poll_timers(handle_id: u64, now_ms: u64, limit: usize) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    match &mut handle.kind {
        EngineKind::Client(runtime) => match runtime.poll_timers(now_ms, limit) {
            Ok(out) => {
                normalize_client_output(out, &mut handle.output);
                0
            }
            Err(e) => set_error(handle, e.to_string()),
        },
        EngineKind::Server(runtime) => match runtime.poll_timers(now_ms, limit) {
            Ok(out) => {
                normalize_server_output(out, &mut handle.output);
                0
            }
            Err(e) => set_error(handle, e.to_string()),
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_poll_nat(handle_id: u64, now_ms: u64) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    match &mut handle.kind {
        EngineKind::Client(runtime) => {
            normalize_client_output(runtime.on_nat_tick(now_ms), &mut handle.output);
            0
        }
        EngineKind::Server(runtime) => {
            normalize_server_output(runtime.on_nat_tick(now_ms), &mut handle.output);
            0
        }
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_configure_client_tls(
    handle_id: u64,
    expected_server_name_ptr: *const u8,
    expected_server_name_len: usize,
    trust_anchor_path_ptr: *const u8,
    trust_anchor_path_len: usize,
) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    let expected = unsafe { bytes_from_ptr(expected_server_name_ptr, expected_server_name_len) };
    let trust = unsafe { bytes_from_ptr(trust_anchor_path_ptr, trust_anchor_path_len) };
    let expected = match std::str::from_utf8(expected) {
        Ok(v) => v,
        Err(e) => return set_error(handle, format!("invalid expected_server_name utf8: {e}")),
    };
    let trust = match std::str::from_utf8(trust) {
        Ok(v) => Some(v),
        Err(e) => return set_error(handle, format!("invalid trust_anchor_path utf8: {e}")),
    };
    let cfg = match build_client_tls_cfg(expected, trust) {
        Ok(v) => v,
        Err(e) => return set_error(handle, e),
    };
    match &handle.kind {
        EngineKind::Client(_) => {
            handle.tls = TlsMode::Client(ClientTlsState {
                adapter: OpenSslDtlsAdapter::new(),
                cfg,
                established: false,
                core_started: false,
            });
            0
        }
        EngineKind::Server(_) => {
            set_error(handle, "client tls can only be configured on client handle".to_string())
        }
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_configure_server_tls(
    handle_id: u64,
    server_name_ptr: *const u8,
    server_name_len: usize,
    cert_path_ptr: *const u8,
    cert_path_len: usize,
    key_path_ptr: *const u8,
    key_path_len: usize,
) -> i32 {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    let Some(handle) = reg.handles.get_mut(&handle_id) else {
        return -1;
    };
    let server_name = unsafe { bytes_from_ptr(server_name_ptr, server_name_len) };
    let cert_path = unsafe { bytes_from_ptr(cert_path_ptr, cert_path_len) };
    let key_path = unsafe { bytes_from_ptr(key_path_ptr, key_path_len) };
    let server_name = match std::str::from_utf8(server_name) {
        Ok(v) => v.to_string(),
        Err(e) => return set_error(handle, format!("invalid server_name utf8: {e}")),
    };
    let cert_path = match std::str::from_utf8(cert_path) {
        Ok(v) => v.to_string(),
        Err(e) => return set_error(handle, format!("invalid cert_path utf8: {e}")),
    };
    let key_path = match std::str::from_utf8(key_path) {
        Ok(v) => v.to_string(),
        Err(e) => return set_error(handle, format!("invalid key_path utf8: {e}")),
    };
    if let Err(e) = build_server_tls_cfg(&server_name, &cert_path, &key_path) {
        return set_error(handle, e);
    }
    match &handle.kind {
        EngineKind::Server(_) => {
            handle.tls = TlsMode::Server(ServerTlsState {
                server_name,
                cert_path,
                key_path,
                sessions_by_path: HashMap::new(),
            });
            0
        }
        EngineKind::Client(_) => {
            set_error(handle, "server tls can only be configured on server handle".to_string())
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_clear_output(handle_id: u64) {
    let mut reg = registry().lock().expect("registry mutex poisoned");
    if let Some(handle) = reg.handles.get_mut(&handle_id) {
        handle.output = OutputBuffer::default();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_out_datagram_count(handle_id: u64) -> usize {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles.get(&handle_id).map(|h| h.output.datagrams.len()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_out_datagram_len(handle_id: u64, index: usize) -> usize {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles
        .get(&handle_id)
        .and_then(|h| h.output.datagrams.get(index))
        .map(|(_, v)| v.len())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_out_datagram_path_id(handle_id: u64, index: usize) -> u32 {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles
        .get(&handle_id)
        .and_then(|h| h.output.datagrams.get(index))
        .map(|(path_id, _)| *path_id)
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_out_datagram_copy(
    handle_id: u64,
    index: usize,
    out_ptr: *mut u8,
    out_len: usize,
) -> usize {
    if out_ptr.is_null() || out_len == 0 {
        return 0;
    }
    let reg = registry().lock().expect("registry mutex poisoned");
    let Some((_, data)) = reg.handles.get(&handle_id).and_then(|h| h.output.datagrams.get(index))
    else {
        return 0;
    };
    let n = data.len().min(out_len);
    // SAFETY: caller provides valid output buffer of length out_len.
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), out_ptr, n);
    }
    n
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_out_event_count(handle_id: u64) -> usize {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles.get(&handle_id).map(|h| h.output.events.len()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_out_event_len(handle_id: u64, index: usize) -> usize {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles
        .get(&handle_id)
        .and_then(|h| h.output.events.get(index))
        .map(|v| v.len())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_out_event_copy(
    handle_id: u64,
    index: usize,
    out_ptr: *mut u8,
    out_len: usize,
) -> usize {
    if out_ptr.is_null() || out_len == 0 {
        return 0;
    }
    let reg = registry().lock().expect("registry mutex poisoned");
    let Some(data) =
        reg.handles.get(&handle_id).and_then(|h| h.output.events.get(index)).map(|s| s.as_bytes())
    else {
        return 0;
    };
    let n = data.len().min(out_len);
    // SAFETY: caller provides valid output buffer of length out_len.
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), out_ptr, n);
    }
    n
}

#[unsafe(no_mangle)]
pub extern "C" fn v3_ffi_last_error_len(handle_id: u64) -> usize {
    let reg = registry().lock().expect("registry mutex poisoned");
    reg.handles.get(&handle_id).and_then(|h| h.last_error.as_ref()).map(|e| e.len()).unwrap_or(0)
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn v3_ffi_last_error_copy(
    handle_id: u64,
    out_ptr: *mut u8,
    out_len: usize,
) -> usize {
    if out_ptr.is_null() || out_len == 0 {
        return 0;
    }
    let reg = registry().lock().expect("registry mutex poisoned");
    let Some(data) =
        reg.handles.get(&handle_id).and_then(|h| h.last_error.as_ref()).map(|s| s.as_bytes())
    else {
        return 0;
    };
    let n = data.len().min(out_len);
    // SAFETY: caller provides valid output buffer of length out_len.
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), out_ptr, n);
    }
    n
}

fn jbyte_array_to_vec(env: JNIEnv, arr: JByteArray) -> Option<Vec<u8>> {
    env.convert_byte_array(&arr).ok()
}

fn copy_bytes_to_jbyte_array(env: JNIEnv, out: JByteArray, bytes: &[u8]) -> jint {
    let signed: Vec<i8> = bytes.iter().map(|b| *b as i8).collect();
    match env.set_byte_array_region(&out, 0, &signed) {
        Ok(()) => bytes.len() as jint,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiAbiVersion(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    v3_ffi_abi_version() as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiNewClient(
    _env: JNIEnv,
    _class: JClass,
    cid: jlong,
    msr_interval_ms: jlong,
    miss_window_count: jlong,
    disconnect_window_ms: jlong,
) -> jlong {
    if cid < 0 || msr_interval_ms < 0 || miss_window_count < 0 || disconnect_window_ms < 0 {
        return 0;
    }
    v3_ffi_new_client(
        cid as u64,
        msr_interval_ms as u64,
        miss_window_count as u64,
        disconnect_window_ms as u64,
    ) as jlong
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiNewServer(
    _env: JNIEnv,
    _class: JClass,
    msr_interval_ms: jlong,
    miss_window_count: jlong,
    disconnect_window_ms: jlong,
) -> jlong {
    if msr_interval_ms < 0 || miss_window_count < 0 || disconnect_window_ms < 0 {
        return 0;
    }
    v3_ffi_new_server(msr_interval_ms as u64, miss_window_count as u64, disconnect_window_ms as u64)
        as jlong
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiFree(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle >= 0 {
        v3_ffi_free(handle as u64);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiBeginHandshake(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    now_ms: jlong,
) -> jint {
    if handle < 0 || now_ms < 0 {
        return -1;
    }
    v3_ffi_begin_handshake(handle as u64, now_ms as u64)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOnDatagram(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    now_ms: jlong,
    path_id: jint,
    data: JByteArray,
) -> jint {
    if handle < 0 || now_ms < 0 || path_id < 0 {
        return -1;
    }
    let Some(bytes) = jbyte_array_to_vec(env, data) else {
        return -1;
    };
    v3_ffi_on_datagram(handle as u64, now_ms as u64, path_id as u32, bytes.as_ptr(), bytes.len())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiSendMessage(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    data: JByteArray,
) -> jint {
    if handle < 0 {
        return -1;
    }
    let Some(bytes) = jbyte_array_to_vec(env, data) else {
        return -1;
    };
    v3_ffi_send_message(handle as u64, bytes.as_ptr(), bytes.len())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiCloseSession(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle < 0 {
        return -1;
    }
    v3_ffi_close_session(handle as u64)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiConfigureNat(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    relay_policy: jint,
    stun_uri: JByteArray,
    turn_uri: JByteArray,
    turn_realm: JByteArray,
    turn_username: JByteArray,
    turn_password: JByteArray,
) -> jint {
    if handle < 0 {
        return -1;
    }
    // SAFETY: same thread/frame, immediate reuse.
    let env2 = unsafe { env.unsafe_clone() };
    // SAFETY: same thread/frame, immediate reuse.
    let env3 = unsafe { env.unsafe_clone() };
    // SAFETY: same thread/frame, immediate reuse.
    let env4 = unsafe { env.unsafe_clone() };
    // SAFETY: same thread/frame, immediate reuse.
    let env5 = unsafe { env.unsafe_clone() };
    let Some(stun) = jbyte_array_to_vec(env, stun_uri) else {
        return -1;
    };
    let Some(turn) = jbyte_array_to_vec(env2, turn_uri) else {
        return -1;
    };
    let Some(realm) = jbyte_array_to_vec(env3, turn_realm) else {
        return -1;
    };
    let Some(user) = jbyte_array_to_vec(env4, turn_username) else {
        return -1;
    };
    let Some(pass) = jbyte_array_to_vec(env5, turn_password) else {
        return -1;
    };

    v3_ffi_configure_nat(
        handle as u64,
        relay_policy as i32,
        stun.as_ptr(),
        stun.len(),
        turn.as_ptr(),
        turn.len(),
        realm.as_ptr(),
        realm.len(),
        user.as_ptr(),
        user.len(),
        pass.as_ptr(),
        pass.len(),
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiConfigureClientTls(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    expected_server_name: JByteArray,
    trust_anchor_path: JByteArray,
) -> jint {
    if handle < 0 {
        return -1;
    }
    // SAFETY: same thread and frame; cloning JNIEnv handle is valid for immediate reuse.
    let env2 = unsafe { env.unsafe_clone() };
    let expected = match jbyte_array_to_vec(env, expected_server_name) {
        Some(v) => v,
        None => return -1,
    };
    let trust = match jbyte_array_to_vec(env2, trust_anchor_path) {
        Some(v) => v,
        None => return -1,
    };
    v3_ffi_configure_client_tls(
        handle as u64,
        expected.as_ptr(),
        expected.len(),
        trust.as_ptr(),
        trust.len(),
    ) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiConfigureServerTls(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    server_name: JByteArray,
    cert_path: JByteArray,
    key_path: JByteArray,
) -> jint {
    if handle < 0 {
        return -1;
    }
    // SAFETY: same thread and frame; cloning JNIEnv handle is valid for immediate reuse.
    let env2 = unsafe { env.unsafe_clone() };
    // SAFETY: same thread and frame; cloning JNIEnv handle is valid for immediate reuse.
    let env3 = unsafe { env.unsafe_clone() };
    let server_name = match jbyte_array_to_vec(env, server_name) {
        Some(v) => v,
        None => return -1,
    };
    let cert_path = match jbyte_array_to_vec(env2, cert_path) {
        Some(v) => v,
        None => return -1,
    };
    let key_path = match jbyte_array_to_vec(env3, key_path) {
        Some(v) => v,
        None => return -1,
    };
    v3_ffi_configure_server_tls(
        handle as u64,
        server_name.as_ptr(),
        server_name.len(),
        cert_path.as_ptr(),
        cert_path.len(),
        key_path.as_ptr(),
        key_path.len(),
    ) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiServerSendMessage(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    cid: jlong,
    data: JByteArray,
    now_ms: jlong,
) -> jint {
    if handle < 0 || cid < 0 || now_ms < 0 {
        return -1;
    }
    let Some(bytes) = jbyte_array_to_vec(env, data) else {
        return -1;
    };
    v3_ffi_server_send_message(
        handle as u64,
        cid as u64,
        bytes.as_ptr(),
        bytes.len(),
        now_ms as u64,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiPollTimers(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    now_ms: jlong,
    limit: jint,
) -> jint {
    if handle < 0 || now_ms < 0 || limit < 0 {
        return -1;
    }
    v3_ffi_poll_timers(handle as u64, now_ms as u64, limit as usize)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiPollNat(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    now_ms: jlong,
) -> jint {
    if handle < 0 || now_ms < 0 {
        return -1;
    }
    v3_ffi_poll_nat(handle as u64, now_ms as u64)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiClearOutput(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle >= 0 {
        v3_ffi_clear_output(handle as u64);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutDatagramCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle < 0 {
        return 0;
    }
    v3_ffi_out_datagram_count(handle as u64) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutDatagramLen(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jint {
    if handle < 0 || index < 0 {
        return 0;
    }
    v3_ffi_out_datagram_len(handle as u64, index as usize) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutDatagramPathId(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jint {
    if handle < 0 || index < 0 {
        return 0;
    }
    v3_ffi_out_datagram_path_id(handle as u64, index as usize) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutDatagramCopy(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    out: JByteArray,
) -> jint {
    if handle < 0 || index < 0 {
        return -1;
    }
    let len = v3_ffi_out_datagram_len(handle as u64, index as usize);
    if len == 0 {
        return 0;
    }
    let mut bytes = vec![0_u8; len];
    let copied =
        v3_ffi_out_datagram_copy(handle as u64, index as usize, bytes.as_mut_ptr(), bytes.len());
    if copied == 0 {
        return 0;
    }
    copy_bytes_to_jbyte_array(env, out, &bytes[..copied])
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutEventCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle < 0 {
        return 0;
    }
    v3_ffi_out_event_count(handle as u64) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutEventLen(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jint {
    if handle < 0 || index < 0 {
        return 0;
    }
    v3_ffi_out_event_len(handle as u64, index as usize) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiOutEventCopy(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    out: JByteArray,
) -> jint {
    if handle < 0 || index < 0 {
        return -1;
    }
    let len = v3_ffi_out_event_len(handle as u64, index as usize);
    if len == 0 {
        return 0;
    }
    let mut bytes = vec![0_u8; len];
    let copied =
        v3_ffi_out_event_copy(handle as u64, index as usize, bytes.as_mut_ptr(), bytes.len());
    if copied == 0 {
        return 0;
    }
    copy_bytes_to_jbyte_array(env, out, &bytes[..copied])
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiLastErrorLen(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle < 0 {
        return 0;
    }
    v3_ffi_last_error_len(handle as u64) as jint
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_todero_v3jni_V3NativeBindings_v3FfiLastErrorCopy(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    out: JByteArray,
) -> jint {
    if handle < 0 {
        return -1;
    }
    let len = v3_ffi_last_error_len(handle as u64);
    if len == 0 {
        return 0;
    }
    let mut bytes = vec![0_u8; len];
    let copied = v3_ffi_last_error_copy(handle as u64, bytes.as_mut_ptr(), bytes.len());
    if copied == 0 {
        return 0;
    }
    copy_bytes_to_jbyte_array(env, out, &bytes[..copied])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use v3_wire::{MessageType, Tlv, WireFrame};

    const TLV_MSR_SIGNAL: u8 = 0x32;

    fn take_datagrams(handle: u64) -> Vec<(u32, Vec<u8>)> {
        let count = v3_ffi_out_datagram_count(handle);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let path_id = v3_ffi_out_datagram_path_id(handle, i);
            let len = v3_ffi_out_datagram_len(handle, i);
            let mut buf = vec![0u8; len];
            let copied = v3_ffi_out_datagram_copy(handle, i, buf.as_mut_ptr(), buf.len());
            buf.truncate(copied);
            out.push((path_id, buf));
        }
        out
    }

    fn take_events(handle: u64) -> Vec<String> {
        let count = v3_ffi_out_event_count(handle);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let len = v3_ffi_out_event_len(handle, i);
            let mut buf = vec![0u8; len];
            let copied = v3_ffi_out_event_copy(handle, i, buf.as_mut_ptr(), buf.len());
            buf.truncate(copied);
            out.push(String::from_utf8(buf).expect("utf8 event"));
        }
        out
    }

    fn pump(
        client: u64,
        server: u64,
        mut now_ms: u64,
        rounds: usize,
    ) -> (Vec<String>, Vec<String>) {
        let mut client_events = Vec::new();
        let mut server_events = Vec::new();
        for _ in 0..rounds {
            let start_client_events = client_events.len();
            let start_server_events = server_events.len();
            for _ in 0..64 {
                let mut progressed = false;
                for (path_id, bytes) in take_datagrams(client) {
                    assert_eq!(
                        v3_ffi_on_datagram(server, now_ms, path_id, bytes.as_ptr(), bytes.len()),
                        0
                    );
                    server_events.extend(take_events(server));
                    now_ms = now_ms.saturating_add(1);
                    progressed = true;
                }
                for (path_id, bytes) in take_datagrams(server) {
                    assert_eq!(
                        v3_ffi_on_datagram(client, now_ms, path_id, bytes.as_ptr(), bytes.len()),
                        0
                    );
                    client_events.extend(take_events(client));
                    now_ms = now_ms.saturating_add(1);
                    progressed = true;
                }
                if !progressed {
                    break;
                }
            }
            let _ = v3_ffi_poll_timers(client, now_ms, 16);
            client_events.extend(take_events(client));
            let _ = v3_ffi_poll_timers(server, now_ms, 16);
            server_events.extend(take_events(server));
            let idle = v3_ffi_out_datagram_count(client) == 0
                && v3_ffi_out_datagram_count(server) == 0
                && client_events.len() == start_client_events
                && server_events.len() == start_server_events;
            if idle {
                break;
            }
            now_ms = now_ms.saturating_add(50);
        }
        (client_events, server_events)
    }

    #[test]
    fn abi_version_is_stable() {
        assert_eq!(v3_ffi_abi_version(), 1);
    }

    #[test]
    fn client_handle_lifecycle_and_output_copy() {
        let h = v3_ffi_new_client(55, 100, 2, 1000);
        assert!(h > 0);
        assert_eq!(v3_ffi_begin_handshake(h, 0), 0);
        assert_eq!(v3_ffi_out_datagram_count(h), 1);

        let len = v3_ffi_out_datagram_len(h, 0);
        let mut buf = vec![0u8; len];
        let copied = v3_ffi_out_datagram_copy(h, 0, buf.as_mut_ptr(), buf.len());
        assert_eq!(copied, len);

        let decoded = WireFrame::decode(&buf).expect("decode");
        assert_eq!(decoded.message_type, MessageType::ClientHello);

        v3_ffi_free(h);
    }

    #[test]
    fn server_handle_accepts_datagram() {
        let server = v3_ffi_new_server(100, 2, 1000);
        let client = v3_ffi_new_client(99, 100, 2, 1000);
        assert_eq!(v3_ffi_begin_handshake(client, 0), 0);

        let len = v3_ffi_out_datagram_len(client, 0);
        let mut buf = vec![0u8; len];
        let _ = v3_ffi_out_datagram_copy(client, 0, buf.as_mut_ptr(), buf.len());

        assert_eq!(v3_ffi_on_datagram(server, 1, 7, buf.as_ptr(), buf.len()), 0);
        assert!(v3_ffi_out_datagram_count(server) >= 1);

        v3_ffi_free(client);
        v3_ffi_free(server);
    }

    #[test]
    fn client_and_server_exchange_messages_over_v3() {
        let cid = 4242;
        let server = v3_ffi_new_server(100, 2, 2_000);
        let client = v3_ffi_new_client(cid, 100, 2, 2_000);

        // Handshake
        assert_eq!(v3_ffi_begin_handshake(client, 0), 0);
        let (_client_events, _server_events) = pump(client, server, 1, 16);

        // Client -> Server delivery (pull-based flow)
        let client_payload = b"hello-from-client";
        assert_eq!(v3_ffi_send_message(client, client_payload.as_ptr(), client_payload.len()), 0);
        let (_client_events, server_events) = pump(client, server, 100, 32);
        let server_deliver = server_events
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(e).ok())
            .find(|v| v.get("kind").and_then(Value::as_str) == Some("server_deliver"))
            .expect("server receive event");
        assert_eq!(server_deliver.get("cid").and_then(Value::as_u64), Some(cid));

        // Server -> Client delivery (server can trigger/send any time)
        let server_payload = b"hello-from-server";
        assert_eq!(
            v3_ffi_server_send_message(
                server,
                cid,
                server_payload.as_ptr(),
                server_payload.len(),
                500
            ),
            0
        );
        assert!(v3_ffi_out_datagram_count(server) > 0, "server_send_message produced no datagrams");
        let (client_events, _server_events) = pump(client, server, 501, 32);
        let client_deliver = client_events
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(e).ok())
            .find(|v| v.get("kind").and_then(Value::as_str) == Some("client_deliver"))
            .expect("client receive event");
        assert_eq!(client_deliver.get("msg_id").and_then(Value::as_u64), Some(1));
        let payload_b64 =
            client_deliver.get("payload_b64").and_then(Value::as_str).expect("payload b64");
        let decoded =
            base64::engine::general_purpose::STANDARD.decode(payload_b64).expect("decode payload");
        assert_eq!(decoded, server_payload);

        v3_ffi_free(client);
        v3_ffi_free(server);
    }

    #[test]
    fn ffi_request_and_data_frames_include_msr_piggyback_tlv() {
        let cid = 9001;
        let server = v3_ffi_new_server(100, 2, 2_000);
        let client = v3_ffi_new_client(cid, 100, 2, 2_000);

        assert_eq!(v3_ffi_begin_handshake(client, 0), 0);
        let _ = pump(client, server, 1, 16);
        v3_ffi_clear_output(client);
        v3_ffi_clear_output(server);

        let payload = b"ffi-piggyback-check";
        assert_eq!(v3_ffi_send_message(client, payload.as_ptr(), payload.len()), 0);
        let client_msr = take_datagrams(client);
        assert!(!client_msr.is_empty());
        for (path_id, bytes) in client_msr {
            assert_eq!(v3_ffi_on_datagram(server, 100, path_id, bytes.as_ptr(), bytes.len()), 0);
        }

        let server_out = take_datagrams(server);
        let req = server_out
            .iter()
            .find_map(|(_, b)| WireFrame::decode(b).ok())
            .filter(|f| f.message_type == MessageType::RequestRange)
            .expect("server request_range");
        assert!(req.tlvs.iter().any(|t| t.kind == TLV_MSR_SIGNAL));

        for (path_id, bytes) in server_out {
            assert_eq!(v3_ffi_on_datagram(client, 101, path_id, bytes.as_ptr(), bytes.len()), 0);
        }
        let client_data = take_datagrams(client);
        let data = client_data
            .iter()
            .find_map(|(_, b)| WireFrame::decode(b).ok())
            .filter(|f| matches!(f.message_type, MessageType::Data | MessageType::Fragment))
            .expect("client data/fragment");
        assert!(data.tlvs.iter().any(|t| t.kind == TLV_MSR_SIGNAL));

        v3_ffi_free(client);
        v3_ffi_free(server);
    }

    #[test]
    fn ffi_piggyback_msr_on_data_triggers_pull_without_control_msr() {
        let cid = 9002;
        let server = v3_ffi_new_server(100, 2, 2_000);
        let client = v3_ffi_new_client(cid, 100, 2, 2_000);

        assert_eq!(v3_ffi_begin_handshake(client, 0), 0);
        let _ = pump(client, server, 1, 16);
        v3_ffi_clear_output(client);
        v3_ffi_clear_output(server);

        let mut msr_value = Vec::with_capacity(16);
        msr_value.extend_from_slice(&0_u64.to_be_bytes());
        msr_value.extend_from_slice(&1_u64.to_be_bytes());
        let mut data_payload = Vec::new();
        data_payload.extend_from_slice(&99_u64.to_be_bytes());
        data_payload.extend_from_slice(b"x");
        let frame = WireFrame::new(
            MessageType::Data,
            0,
            cid,
            data_payload,
            vec![Tlv { kind: TLV_MSR_SIGNAL, value: msr_value }],
        )
        .encode()
        .expect("encode");

        assert_eq!(v3_ffi_on_datagram(client, 200, 1, frame.as_ptr(), frame.len()), 0);
        let out = take_datagrams(client);
        assert!(out.iter().any(|(_, b)| {
            WireFrame::decode(b)
                .map(|f| f.message_type == MessageType::RequestRange)
                .unwrap_or(false)
        }));

        v3_ffi_free(client);
        v3_ffi_free(server);
    }
}
