//! Outbound ciphertext relay wire contract. Routing identity is not attestation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const RELAY_VERSION: u8 = 1;
pub const RELAY_PATH: &str = "/v1/relay";
pub const MAX_RELAY_HTTP_BYTES: usize = 768 * 1024;
pub const MAX_RELAY_CHUNK_BYTES: usize = 16 * 1024;
pub const MAX_RELAY_BUFFER_BYTES: usize = 256 * 1024;
pub const MAX_RELAY_STREAM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RELAY_STREAM_SECONDS: u64 = 300;
pub const MAX_RELAY_OPERATIONS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayRequest {
    pub version: u8,
    pub session_id: Uuid,
    pub operation: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayResponse {
    pub version: u8,
    pub result: Value,
}

/// The operator's immutable allocation claim, subsequently checked against
/// vendor-verified boot identities by node, coordinator and buyer. No secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayMetadata {
    pub schema_version: u8,
    pub job_id: Uuid,
    pub app_id: String,
    pub instance_id: Option<String>,
    pub relay_endpoint: String,
    pub expires_at_ms: u64,
}

pub fn relay_metadata_path(job: Uuid) -> String {
    format!("/federation/hardware/{job}/relay")
}
