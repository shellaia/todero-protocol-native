//! v3-store
//!
//! Session storage abstractions for Todero V3.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SESSION_SCHEMA_VERSION: u16 = 1;
const DEFAULT_SHARD_COUNT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub cid: u64,
    pub peer_id: String,
    pub negotiated_msr_interval_ms: u64,
    pub disconnect_window_ms: u64,
    pub last_ordered_rx: u64,
    pub last_ordered_tx: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedRecord {
    pub version: u64,
    pub record: SessionRecord,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    schema_version: u16,
    record: SessionRecord,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("CAS conflict: expected={expected:?}, actual={actual:?}")]
    CasConflict { expected: Option<u64>, actual: Option<u64> },
    #[error("invalid shard count")]
    InvalidShardCount,
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("unsupported schema version: {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("corrupt log entry")]
    CorruptLogEntry,
    #[error("checksum mismatch")]
    ChecksumMismatch,
}

pub trait SessionStore: Send + Sync {
    fn load(&self, cid: u64, now_ms: u64) -> Result<Option<VersionedRecord>, StoreError>;

    fn compare_and_swap(
        &self,
        cid: u64,
        expected_version: Option<u64>,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError>;

    fn upsert(
        &self,
        cid: u64,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError>;

    fn delete(&self, cid: u64) -> Result<(), StoreError>;

    /// Scans and removes expired records.
    fn scan_expired(&self, now_ms: u64, limit: usize) -> Result<Vec<u64>, StoreError>;
}

#[derive(Debug)]
pub struct InMemorySessionStore {
    shards: Vec<RwLock<HashMap<u64, Entry>>>,
}

#[derive(Debug, Clone)]
struct Entry {
    version: u64,
    record: SessionRecord,
    expires_at_ms: u64,
}

impl InMemorySessionStore {
    pub fn new(shard_count: usize) -> Result<Self, StoreError> {
        if shard_count == 0 {
            return Err(StoreError::InvalidShardCount);
        }
        let mut shards = Vec::with_capacity(shard_count);
        for _ in 0..shard_count {
            shards.push(RwLock::new(HashMap::new()));
        }
        Ok(Self { shards })
    }

    pub fn with_default_shards() -> Self {
        Self::new(DEFAULT_SHARD_COUNT).expect("default shard count must be valid")
    }

    fn shard_index(&self, cid: u64) -> usize {
        (cid as usize) % self.shards.len()
    }

    fn effective_entry(map: &mut HashMap<u64, Entry>, cid: u64, now_ms: u64) -> Option<Entry> {
        match map.get(&cid) {
            Some(entry) if entry.expires_at_ms > now_ms => Some(entry.clone()),
            Some(_) => {
                map.remove(&cid);
                None
            }
            None => None,
        }
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::with_default_shards()
    }
}

impl SessionStore for InMemorySessionStore {
    fn load(&self, cid: u64, now_ms: u64) -> Result<Option<VersionedRecord>, StoreError> {
        let shard = &self.shards[self.shard_index(cid)];
        let mut guard = shard.write().expect("rwlock poisoned");
        let Some(entry) = Self::effective_entry(&mut guard, cid, now_ms) else {
            return Ok(None);
        };

        Ok(Some(VersionedRecord {
            version: entry.version,
            record: entry.record,
            expires_at_ms: entry.expires_at_ms,
        }))
    }

    fn compare_and_swap(
        &self,
        cid: u64,
        expected_version: Option<u64>,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let shard = &self.shards[self.shard_index(cid)];
        let mut guard = shard.write().expect("rwlock poisoned");
        let current = Self::effective_entry(&mut guard, cid, now_ms);
        let current_version = current.as_ref().map(|e| e.version);

        if current_version != expected_version {
            return Err(StoreError::CasConflict {
                expected: expected_version,
                actual: current_version,
            });
        }

        let next_version = current_version.unwrap_or(0).saturating_add(1);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);

        let entry = Entry { version: next_version, record: new_record.clone(), expires_at_ms };

        guard.insert(cid, entry);

        Ok(VersionedRecord { version: next_version, record: new_record, expires_at_ms })
    }

    fn upsert(
        &self,
        cid: u64,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let shard = &self.shards[self.shard_index(cid)];
        let mut guard = shard.write().expect("rwlock poisoned");
        let current = Self::effective_entry(&mut guard, cid, now_ms);

        let next_version = current.map(|e| e.version).unwrap_or(0).saturating_add(1);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);

        let entry = Entry { version: next_version, record: new_record.clone(), expires_at_ms };

        guard.insert(cid, entry);

        Ok(VersionedRecord { version: next_version, record: new_record, expires_at_ms })
    }

    fn delete(&self, cid: u64) -> Result<(), StoreError> {
        let shard = &self.shards[self.shard_index(cid)];
        let mut guard = shard.write().expect("rwlock poisoned");
        guard.remove(&cid);
        Ok(())
    }

    fn scan_expired(&self, now_ms: u64, limit: usize) -> Result<Vec<u64>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut removed = Vec::with_capacity(limit.min(1024));
        for shard in &self.shards {
            if removed.len() >= limit {
                break;
            }
            let mut guard = shard.write().expect("rwlock poisoned");
            let mut expired_keys = Vec::new();
            for (&cid, entry) in guard.iter() {
                if entry.expires_at_ms <= now_ms {
                    expired_keys.push(cid);
                    if removed.len() + expired_keys.len() >= limit {
                        break;
                    }
                }
            }

            for cid in expired_keys {
                guard.remove(&cid);
                removed.push(cid);
                if removed.len() >= limit {
                    break;
                }
            }
        }

        Ok(removed)
    }
}

pub fn encode_session_record(record: &SessionRecord) -> Result<Vec<u8>, StoreError> {
    let payload = StoredSession { schema_version: SESSION_SCHEMA_VERSION, record: record.clone() };
    serde_json::to_vec(&payload).map_err(|e| StoreError::Serialization(e.to_string()))
}

pub fn decode_session_record(bytes: &[u8]) -> Result<SessionRecord, StoreError> {
    let payload: StoredSession =
        serde_json::from_slice(bytes).map_err(|e| StoreError::Serialization(e.to_string()))?;
    if payload.schema_version != SESSION_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion(payload.schema_version));
    }
    Ok(payload.record)
}

#[cfg(feature = "redis-store")]
#[derive(Debug, Clone)]
pub struct RedisSessionStore {
    client: redis::Client,
    key_prefix: String,
}

#[cfg(feature = "redis-store")]
impl RedisSessionStore {
    pub fn new(redis_url: &str, key_prefix: impl Into<String>) -> Result<Self, StoreError> {
        let client =
            redis::Client::open(redis_url).map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(Self { client, key_prefix: key_prefix.into() })
    }

    fn session_key(&self, cid: u64) -> String {
        format!("{}:session:{cid}", self.key_prefix)
    }

    fn expiry_index_key(&self) -> String {
        format!("{}:session_expiry", self.key_prefix)
    }

    fn conn(&self) -> Result<redis::Connection, StoreError> {
        self.client.get_connection().map_err(|e| StoreError::Backend(e.to_string()))
    }
}

#[cfg(feature = "redis-store")]
impl SessionStore for RedisSessionStore {
    fn load(&self, cid: u64, now_ms: u64) -> Result<Option<VersionedRecord>, StoreError> {
        let script = redis::Script::new(
            r#"
local key = KEYS[1]
local zkey = KEYS[2]
local member = ARGV[1]
local now = tonumber(ARGV[2])
if redis.call('EXISTS', key) == 0 then
  return {}
end
local expires = tonumber(redis.call('HGET', key, 'expires_at_ms') or '0')
if expires <= now then
  redis.call('DEL', key)
  redis.call('ZREM', zkey, member)
  return {}
end
local version = tonumber(redis.call('HGET', key, 'version') or '0')
local data = redis.call('HGET', key, 'data') or ''
return {version, expires, data}
"#,
        );
        let mut conn = self.conn()?;
        let key = self.session_key(cid);
        let zkey = self.expiry_index_key();
        let rows: Vec<redis::Value> = script
            .key(key)
            .key(zkey)
            .arg(cid.to_string())
            .arg(now_ms)
            .invoke(&mut conn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if rows.is_empty() {
            return Ok(None);
        }

        let version: u64 = redis::from_redis_value(&rows[0])
            .map_err(|e| StoreError::Backend(format!("invalid redis version: {e}")))?;
        let expires_at_ms: u64 = redis::from_redis_value(&rows[1])
            .map_err(|e| StoreError::Backend(format!("invalid redis expiry: {e}")))?;
        let bytes: Vec<u8> = redis::from_redis_value(&rows[2])
            .map_err(|e| StoreError::Backend(format!("invalid redis payload: {e}")))?;
        let record = decode_session_record(&bytes)?;
        Ok(Some(VersionedRecord { version, record, expires_at_ms }))
    }

    fn compare_and_swap(
        &self,
        cid: u64,
        expected_version: Option<u64>,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let script = redis::Script::new(
            r#"
local key = KEYS[1]
local zkey = KEYS[2]
local member = ARGV[1]
local expected = ARGV[2]
local payload = ARGV[3]
local now = tonumber(ARGV[4])
local ttl = tonumber(ARGV[5])
local expires = tonumber(ARGV[6])

local exists = redis.call('EXISTS', key)
local cur_version = nil
if exists == 1 then
  local cur_expires = tonumber(redis.call('HGET', key, 'expires_at_ms') or '0')
  if cur_expires <= now then
    redis.call('DEL', key)
    redis.call('ZREM', zkey, member)
    exists = 0
  else
    cur_version = tonumber(redis.call('HGET', key, 'version') or '0')
  end
end

if expected == '' then
  if exists == 1 then
    return {0, cur_version}
  end
else
  local expected_num = tonumber(expected)
  if exists == 0 then
    return {0, -1}
  end
  if cur_version ~= expected_num then
    return {0, cur_version}
  end
end

local next_version = (cur_version or 0) + 1
redis.call('HSET', key, 'version', next_version, 'expires_at_ms', expires, 'data', payload)
redis.call('PEXPIRE', key, ttl)
redis.call('ZADD', zkey, expires, member)
return {1, next_version}
"#,
        );

        let payload = encode_session_record(&new_record)?;
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let mut conn = self.conn()?;
        let key = self.session_key(cid);
        let zkey = self.expiry_index_key();
        let expected = expected_version.map(|v| v.to_string()).unwrap_or_default();
        let result: Vec<i64> = script
            .key(key)
            .key(zkey)
            .arg(cid.to_string())
            .arg(expected)
            .arg(payload)
            .arg(now_ms)
            .arg(ttl_ms)
            .arg(expires_at_ms)
            .invoke(&mut conn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if result.len() != 2 {
            return Err(StoreError::Backend("invalid CAS response from redis script".to_string()));
        }
        if result[0] == 1 {
            return Ok(VersionedRecord {
                version: result[1] as u64,
                record: new_record,
                expires_at_ms,
            });
        }

        let actual = if result[1] < 0 { None } else { Some(result[1] as u64) };
        Err(StoreError::CasConflict { expected: expected_version, actual })
    }

    fn upsert(
        &self,
        cid: u64,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let script = redis::Script::new(
            r#"
local key = KEYS[1]
local zkey = KEYS[2]
local member = ARGV[1]
local payload = ARGV[2]
local now = tonumber(ARGV[3])
local ttl = tonumber(ARGV[4])
local expires = tonumber(ARGV[5])

local cur_version = 0
if redis.call('EXISTS', key) == 1 then
  local cur_expires = tonumber(redis.call('HGET', key, 'expires_at_ms') or '0')
  if cur_expires > now then
    cur_version = tonumber(redis.call('HGET', key, 'version') or '0')
  end
end
local next_version = cur_version + 1
redis.call('HSET', key, 'version', next_version, 'expires_at_ms', expires, 'data', payload)
redis.call('PEXPIRE', key, ttl)
redis.call('ZADD', zkey, expires, member)
return next_version
"#,
        );

        let payload = encode_session_record(&new_record)?;
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let mut conn = self.conn()?;
        let key = self.session_key(cid);
        let zkey = self.expiry_index_key();
        let next_version: u64 = script
            .key(key)
            .key(zkey)
            .arg(cid.to_string())
            .arg(payload)
            .arg(now_ms)
            .arg(ttl_ms)
            .arg(expires_at_ms)
            .invoke(&mut conn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(VersionedRecord { version: next_version, record: new_record, expires_at_ms })
    }

    fn delete(&self, cid: u64) -> Result<(), StoreError> {
        let mut conn = self.conn()?;
        let key = self.session_key(cid);
        let zkey = self.expiry_index_key();
        let member = cid.to_string();
        let _: () = redis::pipe()
            .del(key)
            .ignore()
            .cmd("ZREM")
            .arg(zkey)
            .arg(member)
            .ignore()
            .query(&mut conn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    fn scan_expired(&self, now_ms: u64, limit: usize) -> Result<Vec<u64>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut conn = self.conn()?;
        let zkey = self.expiry_index_key();
        let members: Vec<String> = redis::cmd("ZRANGEBYSCORE")
            .arg(&zkey)
            .arg("-inf")
            .arg(now_ms)
            .arg("LIMIT")
            .arg(0)
            .arg(limit)
            .query(&mut conn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if members.is_empty() {
            return Ok(Vec::new());
        }

        let cleanup_script = redis::Script::new(
            r#"
local key = KEYS[1]
local zkey = KEYS[2]
local member = ARGV[1]
local now = tonumber(ARGV[2])
if redis.call('EXISTS', key) == 0 then
  redis.call('ZREM', zkey, member)
  return 0
end
local expires = tonumber(redis.call('HGET', key, 'expires_at_ms') or '0')
if expires <= now then
  redis.call('DEL', key)
  redis.call('ZREM', zkey, member)
  return 1
end
return 0
"#,
        );

        let mut removed = Vec::new();
        for member in members {
            let Ok(cid) = member.parse::<u64>() else {
                continue;
            };
            let key = self.session_key(cid);
            let deleted: i32 = cleanup_script
                .key(key)
                .key(&zkey)
                .arg(member)
                .arg(now_ms)
                .invoke(&mut conn)
                .map_err(|e| StoreError::Backend(e.to_string()))?;
            if deleted == 1 {
                removed.push(cid);
            }
        }
        Ok(removed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskLogEntry {
    op: DiskOp,
    checksum: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
enum DiskOp {
    Upsert { cid: u64, version: u64, expires_at_ms: u64, data: Vec<u8> },
    Delete { cid: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskSnapshot {
    schema_version: u16,
    entries: Vec<DiskSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskSnapshotEntry {
    cid: u64,
    version: u64,
    expires_at_ms: u64,
    data: Vec<u8>,
}

#[derive(Debug)]
pub struct DiskSessionStore {
    state: Mutex<DiskState>,
    root: PathBuf,
    log_path: PathBuf,
    snapshot_path: PathBuf,
}

#[derive(Debug, Default)]
struct DiskState {
    entries: HashMap<u64, Entry>,
}

impl DiskSessionStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| StoreError::Io(e.to_string()))?;
        let log_path = root.join("sessions.log");
        let snapshot_path = root.join("sessions.snapshot.json");
        if !log_path.exists() {
            let _ = File::create(&log_path).map_err(|e| StoreError::Io(e.to_string()))?;
        }

        let mut state = DiskState::default();
        if snapshot_path.exists() {
            Self::load_snapshot(&snapshot_path, &mut state)?;
        }
        Self::replay_log(&log_path, &mut state)?;

        Ok(Self { state: Mutex::new(state), root, log_path, snapshot_path })
    }

    pub fn compact(&self) -> Result<(), StoreError> {
        let guard = self.state.lock().expect("mutex poisoned");
        let mut entries = Vec::with_capacity(guard.entries.len());
        for (&cid, entry) in &guard.entries {
            entries.push(DiskSnapshotEntry {
                cid,
                version: entry.version,
                expires_at_ms: entry.expires_at_ms,
                data: encode_session_record(&entry.record)?,
            });
        }
        drop(guard);

        let snapshot = DiskSnapshot { schema_version: SESSION_SCHEMA_VERSION, entries };
        let tmp_snapshot = self.root.join("sessions.snapshot.tmp");
        let payload =
            serde_json::to_vec(&snapshot).map_err(|e| StoreError::Serialization(e.to_string()))?;
        fs::write(&tmp_snapshot, payload).map_err(|e| StoreError::Io(e.to_string()))?;
        fs::rename(&tmp_snapshot, &self.snapshot_path)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        fs::write(&self.log_path, b"").map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    fn append_op(&self, op: DiskOp) -> Result<(), StoreError> {
        let op_bytes =
            serde_json::to_vec(&op).map_err(|e| StoreError::Serialization(e.to_string()))?;
        let checksum = calc_checksum(&op_bytes);
        let entry = DiskLogEntry { op, checksum };
        let line =
            serde_json::to_vec(&entry).map_err(|e| StoreError::Serialization(e.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        file.write_all(&line)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|e| StoreError::Io(e.to_string()))?;
        file.flush().map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    fn load_snapshot(path: &Path, state: &mut DiskState) -> Result<(), StoreError> {
        let bytes = fs::read(path).map_err(|e| StoreError::Io(e.to_string()))?;
        if bytes.is_empty() {
            return Ok(());
        }
        let snap: DiskSnapshot =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Serialization(e.to_string()))?;
        if snap.schema_version != SESSION_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(snap.schema_version));
        }
        for item in snap.entries {
            let record = decode_session_record(&item.data)?;
            state.entries.insert(
                item.cid,
                Entry { version: item.version, record, expires_at_ms: item.expires_at_ms },
            );
        }
        Ok(())
    }

    fn replay_log(path: &Path, state: &mut DiskState) -> Result<(), StoreError> {
        let file =
            OpenOptions::new().read(true).open(path).map_err(|e| StoreError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(|e| StoreError::Io(e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: DiskLogEntry =
                serde_json::from_str(&line).map_err(|_| StoreError::CorruptLogEntry)?;
            let op_bytes = serde_json::to_vec(&entry.op)
                .map_err(|e| StoreError::Serialization(e.to_string()))?;
            if calc_checksum(&op_bytes) != entry.checksum {
                return Err(StoreError::ChecksumMismatch);
            }
            match entry.op {
                DiskOp::Upsert { cid, version, expires_at_ms, data } => {
                    let record = decode_session_record_migrating(&data)?;
                    state.entries.insert(cid, Entry { version, record, expires_at_ms });
                }
                DiskOp::Delete { cid } => {
                    state.entries.remove(&cid);
                }
            }
        }
        Ok(())
    }
}

impl SessionStore for DiskSessionStore {
    fn load(&self, cid: u64, now_ms: u64) -> Result<Option<VersionedRecord>, StoreError> {
        let mut guard = self.state.lock().expect("mutex poisoned");
        let current = guard.entries.get(&cid).cloned();
        match current {
            Some(entry) if entry.expires_at_ms > now_ms => Ok(Some(VersionedRecord {
                version: entry.version,
                record: entry.record,
                expires_at_ms: entry.expires_at_ms,
            })),
            Some(_) => {
                guard.entries.remove(&cid);
                self.append_op(DiskOp::Delete { cid })?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn compare_and_swap(
        &self,
        cid: u64,
        expected_version: Option<u64>,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let mut guard = self.state.lock().expect("mutex poisoned");
        let current = guard.entries.get(&cid).cloned().filter(|v| v.expires_at_ms > now_ms);
        let current_version = current.as_ref().map(|v| v.version);

        if current_version != expected_version {
            return Err(StoreError::CasConflict {
                expected: expected_version,
                actual: current_version,
            });
        }

        let next_version = current_version.unwrap_or(0).saturating_add(1);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let encoded = encode_session_record(&new_record)?;
        let entry = Entry { version: next_version, record: new_record.clone(), expires_at_ms };
        guard.entries.insert(cid, entry);
        drop(guard);

        self.append_op(DiskOp::Upsert {
            cid,
            version: next_version,
            expires_at_ms,
            data: encoded,
        })?;

        Ok(VersionedRecord { version: next_version, record: new_record, expires_at_ms })
    }

    fn upsert(
        &self,
        cid: u64,
        new_record: SessionRecord,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<VersionedRecord, StoreError> {
        let mut guard = self.state.lock().expect("mutex poisoned");
        let current = guard.entries.get(&cid).cloned().filter(|v| v.expires_at_ms > now_ms);
        let next_version = current.map(|v| v.version).unwrap_or(0).saturating_add(1);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let encoded = encode_session_record(&new_record)?;
        guard.entries.insert(
            cid,
            Entry { version: next_version, record: new_record.clone(), expires_at_ms },
        );
        drop(guard);

        self.append_op(DiskOp::Upsert {
            cid,
            version: next_version,
            expires_at_ms,
            data: encoded,
        })?;

        Ok(VersionedRecord { version: next_version, record: new_record, expires_at_ms })
    }

    fn delete(&self, cid: u64) -> Result<(), StoreError> {
        let mut guard = self.state.lock().expect("mutex poisoned");
        guard.entries.remove(&cid);
        drop(guard);
        self.append_op(DiskOp::Delete { cid })?;
        Ok(())
    }

    fn scan_expired(&self, now_ms: u64, limit: usize) -> Result<Vec<u64>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut guard = self.state.lock().expect("mutex poisoned");
        let mut expired = Vec::new();
        for (&cid, entry) in &guard.entries {
            if entry.expires_at_ms <= now_ms {
                expired.push(cid);
                if expired.len() >= limit {
                    break;
                }
            }
        }
        for cid in &expired {
            guard.entries.remove(cid);
        }
        drop(guard);
        for cid in &expired {
            self.append_op(DiskOp::Delete { cid: *cid })?;
        }
        Ok(expired)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyStoredSessionV0 {
    record: SessionRecord,
}

fn decode_session_record_migrating(bytes: &[u8]) -> Result<SessionRecord, StoreError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| StoreError::Serialization(e.to_string()))?;
    let schema = value.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    match schema {
        SESSION_SCHEMA_VERSION => decode_session_record(bytes),
        0 => {
            let legacy: LegacyStoredSessionV0 = serde_json::from_value(value)
                .map_err(|e| StoreError::Serialization(e.to_string()))?;
            Ok(legacy.record)
        }
        other => Err(StoreError::UnsupportedSchemaVersion(other)),
    }
}

fn calc_checksum(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::{fs, path::PathBuf};

    use super::*;

    fn sample_record(cid: u64, peer: &str) -> SessionRecord {
        SessionRecord {
            cid,
            peer_id: peer.to_string(),
            negotiated_msr_interval_ms: 15_000,
            disconnect_window_ms: 90_000,
            last_ordered_rx: 10,
            last_ordered_tx: 12,
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn cas_collision_allows_single_winner() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let cid = 42;
        let now = 10_000;

        let created = store
            .compare_and_swap(cid, None, sample_record(cid, "init"), 10_000, now)
            .expect("create with CAS");
        assert_eq!(created.version, 1);

        let mut handles = Vec::new();
        for i in 0..8 {
            let st = store.clone();
            handles.push(thread::spawn(move || {
                st.compare_and_swap(
                    cid,
                    Some(1),
                    sample_record(cid, &format!("peer-{i}")),
                    10_000,
                    now + 1,
                )
            }));
        }

        let mut success = 0;
        let mut conflict = 0;
        for h in handles {
            match h.join().expect("thread join") {
                Ok(v) => {
                    success += 1;
                    assert_eq!(v.version, 2);
                }
                Err(StoreError::CasConflict { .. }) => {
                    conflict += 1;
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }

        assert_eq!(success, 1);
        assert_eq!(conflict, 7);
    }

    #[test]
    fn expiry_under_contention_and_scan_cleanup() {
        let store = Arc::new(InMemorySessionStore::with_default_shards());
        let now = 1_000;

        for cid in 1..=64u64 {
            store.upsert(cid, sample_record(cid, "peer"), 5, now).expect("upsert");
        }

        let mut loaders = Vec::new();
        for _ in 0..4 {
            let st = store.clone();
            loaders.push(thread::spawn(move || {
                for cid in 1..=64u64 {
                    let _ = st.load(cid, now + 2);
                }
            }));
        }
        for h in loaders {
            h.join().expect("join");
        }

        let removed = store.scan_expired(now + 10, 1000).expect("scan expired");
        assert_eq!(removed.len(), 64);

        for cid in 1..=64u64 {
            let loaded = store.load(cid, now + 10).expect("load post-expiry");
            assert!(loaded.is_none());
        }
    }

    #[test]
    fn serialization_roundtrip_is_stable_for_schema_v1() {
        let record = sample_record(7, "peer-a");
        let bytes = encode_session_record(&record).expect("encode");
        let decoded = decode_session_record(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn decode_rejects_unsupported_schema_version() {
        let payload = serde_json::json!({
            "schema_version": SESSION_SCHEMA_VERSION + 1,
            "record": sample_record(1, "x")
        });
        let bytes = serde_json::to_vec(&payload).expect("json serialize");
        let err = decode_session_record(&bytes).expect_err("must fail");
        assert_eq!(err, StoreError::UnsupportedSchemaVersion(SESSION_SCHEMA_VERSION + 1));
    }

    #[test]
    fn disk_backend_append_log_and_recovery() {
        let dir = unique_temp_dir("v3-store-disk-recovery");
        let cid = 7001;
        {
            let store = DiskSessionStore::new(&dir).expect("new");
            let rec = sample_record(cid, "disk-peer");
            let written = store.upsert(cid, rec.clone(), 5_000, 100).expect("upsert");
            assert_eq!(written.version, 1);
            let loaded = store.load(cid, 101).expect("load");
            assert!(loaded.is_some());
        }

        // Re-open and recover from persisted log.
        let recovered = DiskSessionStore::new(&dir).expect("reopen");
        let loaded = recovered.load(cid, 102).expect("load recovered");
        assert!(loaded.is_some());
    }

    #[test]
    fn disk_backend_compaction_keeps_state_and_clears_log() {
        let dir = unique_temp_dir("v3-store-disk-compact");
        let log = dir.join("sessions.log");
        let store = DiskSessionStore::new(&dir).expect("new");
        store.upsert(1, sample_record(1, "a"), 5_000, 1_000).expect("upsert 1");
        store.upsert(2, sample_record(2, "b"), 5_000, 1_000).expect("upsert 2");
        let before = fs::read(&log).expect("read log before");
        assert!(!before.is_empty());

        store.compact().expect("compact");
        let after = fs::read(&log).expect("read log after");
        assert!(after.is_empty());

        let reopened = DiskSessionStore::new(&dir).expect("reopen");
        assert!(reopened.load(1, 1_001).expect("load 1").is_some());
        assert!(reopened.load(2, 1_001).expect("load 2").is_some());
    }

    #[test]
    fn disk_backend_detects_checksum_corruption() {
        let dir = unique_temp_dir("v3-store-disk-corrupt");
        let store = DiskSessionStore::new(&dir).expect("new");
        store.upsert(9, sample_record(9, "x"), 5_000, 10).expect("upsert");

        let log_path = dir.join("sessions.log");
        let mut bytes = fs::read(&log_path).expect("read log");
        // Flip one byte in serialized line to invalidate checksum.
        if let Some(first) = bytes.iter_mut().find(|b| **b != b'\n') {
            *first ^= 0x01;
        }
        fs::write(&log_path, bytes).expect("rewrite log");

        let err = DiskSessionStore::new(&dir).expect_err("must fail checksum");
        assert!(matches!(err, StoreError::ChecksumMismatch | StoreError::CorruptLogEntry));
    }

    #[test]
    fn disk_backend_supports_schema_migration_from_v0() {
        let dir = unique_temp_dir("v3-store-disk-migrate");
        let log_path = dir.join("sessions.log");
        let record = sample_record(55, "legacy");
        let legacy = serde_json::json!({ "record": record });
        let bytes = serde_json::to_vec(&legacy).expect("serialize legacy");
        let op = DiskOp::Upsert { cid: 55, version: 1, expires_at_ms: 50_000, data: bytes };
        let op_bytes = serde_json::to_vec(&op).expect("serialize op");
        let entry = DiskLogEntry { op, checksum: calc_checksum(&op_bytes) };
        let line = serde_json::to_vec(&entry).expect("serialize entry");
        fs::write(&log_path, [line, b"\n".to_vec()].concat()).expect("write log");

        let store = DiskSessionStore::new(&dir).expect("load migrated");
        let loaded = store.load(55, 40_000).expect("load");
        assert!(loaded.is_some());
    }

    #[cfg(feature = "redis-store")]
    fn redis_test_url() -> String {
        std::env::var("V3_REDIS_URL").unwrap_or_else(|_| "redis://10.0.0.143:6379/".to_string())
    }

    #[cfg(feature = "redis-store")]
    fn should_run_redis_tests() -> bool {
        matches!(std::env::var("V3_RUN_REDIS_TESTS").ok().as_deref(), Some("1"))
    }

    #[cfg(feature = "redis-store")]
    fn unique_prefix() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        format!("v3test:{nanos}")
    }

    #[cfg(feature = "redis-store")]
    #[test]
    fn redis_store_cas_and_ttl_behaves_like_contract() {
        if !should_run_redis_tests() {
            return;
        }
        let now = 50_000;
        let cid = 501;
        let store =
            RedisSessionStore::new(&redis_test_url(), unique_prefix()).expect("redis store");

        let created = store
            .compare_and_swap(cid, None, sample_record(cid, "r1"), 10_000, now)
            .expect("redis create");
        assert_eq!(created.version, 1);

        let updated = store
            .compare_and_swap(cid, Some(1), sample_record(cid, "r2"), 10_000, now + 1)
            .expect("redis update");
        assert_eq!(updated.version, 2);

        let conflict = store
            .compare_and_swap(cid, Some(1), sample_record(cid, "r3"), 10_000, now + 2)
            .expect_err("must conflict");
        assert!(matches!(conflict, StoreError::CasConflict { .. }));

        let loaded = store.load(cid, now + 3).expect("redis load");
        assert!(loaded.is_some());

        let expired = store.load(cid, now + 20_000).expect("redis expired load");
        assert!(expired.is_none());
    }

    #[cfg(feature = "redis-store")]
    #[test]
    fn redis_scan_expired_respects_limit() {
        if !should_run_redis_tests() {
            return;
        }
        let now = 80_000;
        let store =
            RedisSessionStore::new(&redis_test_url(), unique_prefix()).expect("redis store");

        for cid in 1..=6 {
            store.upsert(cid, sample_record(cid, "peer"), 10_000, now).expect("upsert");
        }
        let removed = store.scan_expired(now + 20_000, 3).expect("scan expired");
        assert_eq!(removed.len(), 3);

        let removed2 = store.scan_expired(now + 20_000, 10).expect("scan expired second");
        assert_eq!(removed2.len(), 3);
    }
}
