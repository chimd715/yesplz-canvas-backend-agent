//! Local native-session checkpoint at a verified v0.2.11 normal-turn boundary.
//! This is not a central persistence acknowledgement or a whole-engine snapshot.

use std::io;
use std::path::Path;

use aion_agent::context_usage::ContextStatus;
use aion_agent::engine::{AgentEngine, AgentResult};
#[cfg(target_os = "linux")]
use aion_agent::session::Session;
#[cfg(target_os = "linux")]
use aion_types::message::Role;
use aion_types::message::{StopReason, TokenUsage};
#[cfg(target_os = "linux")]
use aionui_common::now_ms;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

#[cfg(target_os = "linux")]
#[path = "checkpoint_linux.rs"]
mod filesystem;

#[path = "continuation.rs"]
mod continuation;
pub(crate) use continuation::load_prepared_native_continuation;
pub use continuation::{
    FrozenNativeSession, NativeContinuationTarget, PreparedNativeContinuation, prepare_frozen_native_continuation,
    recover_frozen_native_continuation,
};

#[cfg(target_os = "linux")]
const MAX_SESSION_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointManifest {
    pub schema_version: u32,
    pub backend: String,
    pub backend_version: String,
    pub conversation_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub session_sha256: String,
    pub session_bytes: usize,
    pub created_at_ms: i64,
    pub local_only: bool,
}

pub(super) struct NativeCheckpointStore {
    #[cfg(target_os = "linux")]
    files: filesystem::CheckpointFiles,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 160 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl NativeCheckpointStore {
    pub(super) fn configured(session_directory: &Path) -> io::Result<Option<Self>> {
        match std::env::var("AIONCORE_NATIVE_CHECKPOINTS").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("0") => Ok(None),
            Ok("1") => Self::new(session_directory).map(Some),
            _ => Err(invalid("AIONCORE_NATIVE_CHECKPOINTS must be 0 or 1")),
        }
    }

    pub(super) fn new(session_directory: &Path) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                files: filesystem::CheckpointFiles::new(session_directory)?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = session_directory;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Native checkpoints require Linux",
            ))
        }
    }

    /// Caller retains the engine mutex until this finishes. The pinned engine
    /// saves synchronously before a normal successful return; the caller must
    /// first reject its synchronous save-error latch. Do not call after abort.
    pub(super) fn capture(
        &self,
        engine: &AgentEngine,
        result: &AgentResult,
        conversation_id: &str,
        turn_id: &str,
    ) -> io::Result<Option<CheckpointManifest>> {
        if result.turns == 0 || result.stop_reason != StopReason::EndTurn {
            return Ok(None); // Slash commands and incomplete turns have no new proof.
        }
        if engine.current_session_id().as_deref() != Some(conversation_id) || engine.current_turn_id() != Some(turn_id)
        {
            return Err(invalid("Native checkpoint identity does not match the completed turn"));
        }
        self.capture_verified(conversation_id, turn_id, &result.usage, &engine.context_status())
            .map(Some)
    }

    fn capture_verified(
        &self,
        conversation_id: &str,
        turn_id: &str,
        usage: &TokenUsage,
        context: &ContextStatus,
    ) -> io::Result<CheckpointManifest> {
        if !valid_id(conversation_id) || !valid_id(turn_id) {
            return Err(invalid("Invalid native checkpoint identity"));
        }
        #[cfg(target_os = "linux")]
        {
            let bytes = self.files.read_session(conversation_id, MAX_SESSION_BYTES)?;
            validate_session(&bytes, conversation_id, turn_id, usage, context)?;
            let manifest = CheckpointManifest {
                schema_version: 1,
                backend: "aionrs".into(),
                backend_version: "0.2.11".into(),
                conversation_id: conversation_id.into(),
                session_id: conversation_id.into(),
                turn_id: turn_id.into(),
                session_sha256: hex::encode(Sha256::digest(&bytes)),
                session_bytes: bytes.len(),
                created_at_ms: now_ms(),
                local_only: true,
            };
            let metadata = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
            self.files.publish(conversation_id, turn_id, &bytes, &metadata)?;
            Ok(manifest)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (usage, context);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Native checkpoints require Linux",
            ))
        }
    }
}

#[cfg(target_os = "linux")]
fn validate_session(bytes: &[u8], id: &str, turn: &str, usage: &TokenUsage, context: &ContextStatus) -> io::Result<()> {
    let session: Session = serde_json::from_slice(bytes).map_err(|_| invalid("Invalid native session JSON"))?;
    let last = session
        .messages
        .last()
        .ok_or_else(|| invalid("Native session has no completed message"))?;
    let state = &session.context_state;
    if session.id != id
        || last.role != Role::Assistant
        || last.turn_id.as_deref() != Some(turn)
        || session.total_usage.input_tokens != usage.input_tokens
        || session.total_usage.output_tokens != usage.output_tokens
        || session.total_usage.cache_creation_tokens != usage.cache_creation_tokens
        || session.total_usage.cache_read_tokens != usage.cache_read_tokens
        || state.schema_version != context.schema_version
        || state.context_usage != context.context_usage
        || state.source != context.source
        || state.compact_count != context.compact_count
        || state.microcompact_count != context.microcompact_count
        || state.updated_at != context.updated_at
    {
        return Err(invalid("Native session does not match the completed turn"));
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "checkpoint_test.rs"]
mod tests;
