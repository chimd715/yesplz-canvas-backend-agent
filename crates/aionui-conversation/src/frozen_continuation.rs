//! Trusted archive import without activating the restored conversation.
use super::*;
use aionui_ai_agent::manager::aionrs::{
    FrozenNativeSession, NativeContinuationTarget, PreparedNativeContinuation, prepare_frozen_native_continuation,
    recover_frozen_native_continuation,
};
use aionui_db::{FrozenContinuationImportResult, FrozenConversationImport};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenContinuationPreparation {
    pub rows: FrozenConversationImport,
    pub native: FrozenNativeSession,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct PreparedFrozenContinuation {
    pub imported: FrozenContinuationImportResult,
    pub native: PreparedNativeContinuation,
}

impl ConversationService {
    /// Caller verifies the outer archive registration, owner, build and frozen
    /// workspace before invoking this service. `native_session_directory` is
    /// trusted application configuration, never deserialized archive input.
    /// No mutable parent is read and no runtime, head or active writer is moved.
    pub async fn prepare_frozen_continuation(
        &self,
        user_id: &str,
        expected_runtime_build_sha256: &str,
        native_session_directory: &Path,
        input: FrozenContinuationPreparation,
    ) -> Result<PreparedFrozenContinuation, ConversationError> {
        self.frozen_continuation(
            user_id,
            expected_runtime_build_sha256,
            native_session_directory,
            input,
            false,
        )
        .await
    }

    /// Read only: a missing committed import never falls back to preparation.
    pub async fn recover_frozen_continuation(
        &self,
        user_id: &str,
        expected_runtime_build_sha256: &str,
        native_session_directory: &Path,
        input: FrozenContinuationPreparation,
    ) -> Result<PreparedFrozenContinuation, ConversationError> {
        self.frozen_continuation(
            user_id,
            expected_runtime_build_sha256,
            native_session_directory,
            input,
            true,
        )
        .await
    }

    async fn frozen_continuation(
        &self,
        user_id: &str,
        expected_runtime_build_sha256: &str,
        native_session_directory: &Path,
        input: FrozenContinuationPreparation,
        read_only: bool,
    ) -> Result<PreparedFrozenContinuation, ConversationError> {
        let rows = input.rows;
        rows.validate(user_id).map_err(|error| match error {
            aionui_db::DbError::Init(_) => ConversationError::BadRequest {
                reason: "Invalid frozen continuation rows".into(),
            },
            error => error.into(),
        })?;
        use sha2::{Digest, Sha256};
        if serde_json::to_vec(&rows)
            .map_err(|_| ConversationError::internal("Frozen import serialization failed"))?
            .len()
            > 32 * 1024 * 1024
        {
            return Err(ConversationError::BadRequest {
                reason: "Frozen import exceeds size limit".into(),
            });
        }
        let binding = &rows.native_binding;
        if binding.owner_id != input.native.owner_id
            || binding.runtime_build_sha256 != expected_runtime_build_sha256
            || input.native.runtime_build_sha256 != expected_runtime_build_sha256
            || binding.manifest_sha256 != format!("{:x}", Sha256::digest(input.native.manifest_json.as_bytes()))
            || binding.session_sha256 != format!("{:x}", Sha256::digest(input.native.session_json.as_bytes()))
        {
            return Err(ConversationError::BadRequest {
                reason: "Frozen native import binding mismatch".into(),
            });
        }
        let committed = self.conversation_repo.get_frozen_continuation(user_id, &rows).await?;
        if read_only && committed.is_none() {
            return Err(ConversationError::NotFoundReason {
                reason: "Frozen continuation has not been prepared".into(),
            });
        }
        // Authorization/dependency checks precede filesystem preparation and
        // repeat inside the final SQLite transaction to reject races.
        if committed.is_none() {
            self.conversation_repo
                .validate_frozen_continuation(user_id, &rows)
                .await?;
        }
        let extra: serde_json::Value =
            serde_json::from_str(&rows.source.extra).map_err(|_| ConversationError::BadRequest {
                reason: "Invalid frozen conversation extra".into(),
            })?;
        serde_json::from_value::<aionui_api_types::AionrsBuildExtra>(extra.clone()).map_err(|_| {
            ConversationError::BadRequest {
                reason: "Invalid frozen native build options".into(),
            }
        })?;
        let source_workspace = extra
            .get("workspace")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ConversationError::BadRequest {
                reason: "Frozen source workspace is required".into(),
            })?;
        let owner = user_id.to_owned();
        let expected_build = expected_runtime_build_sha256.to_owned();
        let session_directory = native_session_directory.to_path_buf();
        let source_workspace = PathBuf::from(source_workspace);
        let restored_workspace = PathBuf::from(&rows.target_workspace);
        let source_id = rows.source.id.clone();
        let turn = rows.original_turn_id.clone();
        let new_id = rows.target_id.clone();
        let recovering = committed.is_some();
        let native = tokio::task::spawn_blocking(move || {
            let target = NativeContinuationTarget {
                owner_id: &owner,
                expected_runtime_build_sha256: &expected_build,
                source_conversation_id: &source_id,
                source_turn_id: &turn,
                conversation_id: &new_id,
                source_workspace: &source_workspace,
                restored_workspace: &restored_workspace,
                session_directory: &session_directory,
            };
            if recovering {
                recover_frozen_native_continuation(&input.native, &target)
            } else {
                prepare_frozen_native_continuation(&input.native, &target)
            }
        })
        .await
        .map_err(|_| ConversationError::internal("Native continuation preparation interrupted"))?
        .map_err(|error| {
            warn!(error_kind = ?error.kind(), "Frozen native continuation preparation failed");
            ConversationError::BadRequest {
                reason: "Frozen native continuation validation or preparation failed".into(),
            }
        })?;
        // Native installation precedes row visibility. Failure leaves an
        // unreferenced private session; never overwrite it or reuse its ID.
        let imported = match committed {
            Some(imported) => imported,
            None => {
                self.conversation_repo
                    .import_frozen_continuation(user_id, &rows)
                    .await?
            }
        };
        info!(conversation_id = %imported.conversation_id, source_conversation_id = %rows.source.id,
            copied_messages = imported.copied_messages.len(), "Frozen continuation prepared; head unchanged");
        Ok(PreparedFrozenContinuation { imported, native })
    }
}
