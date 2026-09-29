//! Server-only archive preparation transport. These inputs contain private history.
#![allow(clippy::disallowed_types)]
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenContinuationRequest {
    /// Strictly decoded into the database snapshot type by the domain boundary.
    pub rows: serde_json::Value,
    pub native: FrozenNativeSessionRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenNativeSessionRequest {
    pub owner_id: String,
    pub runtime_build_sha256: String,
    pub manifest_json: String,
    pub session_json: String,
}

#[derive(Serialize)]
pub struct FrozenContinuationResponse {
    pub imported: FrozenContinuationImportResponse,
    pub native: PreparedNativeContinuationResponse,
    pub core_user_id: String,
    pub runtime_build_sha256: String,
}

#[derive(Serialize)]
pub struct FrozenContinuationImportResponse {
    pub conversation_id: String,
    pub copied_messages: Vec<FrozenMessageMappingResponse>,
    pub copied_artifacts: Vec<FrozenArtifactMappingResponse>,
}

#[derive(Serialize)]
pub struct FrozenMessageMappingResponse {
    pub source_message_id: String,
    pub message_id: String,
    pub original_turn_id: Option<String>,
}

#[derive(Serialize)]
pub struct FrozenArtifactMappingResponse {
    pub source_artifact_id: String,
    pub artifact_id: String,
}

#[derive(Serialize)]
pub struct PreparedNativeContinuationResponse {
    pub conversation_id: String,
    pub source_conversation_id: String,
    pub source_turn_id: String,
    pub native_state_sha256: String,
    pub session_bytes: usize,
}
