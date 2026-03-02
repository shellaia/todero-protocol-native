//! v3-orchestrator-api
//!
//! Shared DTOs/contracts for V3 signaling/discovery API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TraceContext {
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterEndpointRequest {
    pub endpoint_id: String,
    pub vhost: String,
    pub ttl_ms: u64,
    #[serde(default)]
    pub trace: TraceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishCandidatesRequest {
    pub endpoint_id: String,
    pub vhost: String,
    pub generation: u64,
    pub ttl_ms: u64,
    pub payload: Value,
    #[serde(default)]
    pub trace: TraceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverCandidatesRequest {
    pub endpoint_id: String,
    pub vhost: String,
    #[serde(default)]
    pub trace: TraceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverCandidatesResponse {
    pub found: bool,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCredentialsRequest {
    pub vhost: String,
    pub purpose: String,
    pub ttl_seconds: u64,
    #[serde(default)]
    pub trace: TraceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCredentialsResponse {
    pub username: String,
    pub credential: String,
    pub ttl_seconds: u64,
    pub realm: String,
    pub not_before_epoch: u64,
    pub not_after_epoch: u64,
    pub rotation_epoch: u64,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSelectedRequest {
    pub source_endpoint_id: String,
    pub target_endpoint_id: String,
    pub vhost: String,
    pub selected: Value,
    #[serde(default)]
    pub trace: TraceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryBudget {
    pub max_retries: u32,
    pub base_backoff_ms: u64,
    pub jitter_ratio_bps: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutageBehaviorContract {
    pub cached_candidate_fallback: bool,
    pub retry_budget: RetryBudget,
    pub safe_failure_mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_json_roundtrip() {
        let contract = OutageBehaviorContract {
            cached_candidate_fallback: true,
            retry_budget: RetryBudget {
                max_retries: 5,
                base_backoff_ms: 250,
                jitter_ratio_bps: 1500,
            },
            safe_failure_mode: "relay-only".to_string(),
        };
        let bytes = serde_json::to_vec(&contract).expect("serialize");
        let decoded: OutageBehaviorContract = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded, contract);
    }
}
