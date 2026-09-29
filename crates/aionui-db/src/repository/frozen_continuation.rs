//! Import an authenticated, frozen native conversation without reading its live parent.

use std::{
    collections::HashSet,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    DbError,
    models::{ConversationArtifactRow, ConversationAssistantSnapshotRow, ConversationRow, MessageRow},
};

/// Frozen archive rows. The caller must authenticate the archive and prepare the
/// native session/workspace separately before committing this fresh continuation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenConversationImport {
    pub source: ConversationRow,
    pub native_binding: FrozenNativeImportBinding,
    pub assistant: ConversationAssistantSnapshotRow,
    pub messages: Vec<MessageRow>,
    pub artifacts: Vec<ConversationArtifactRow>,
    pub target_id: String,
    pub target_workspace: String,
    pub original_turn_id: String,
    pub boundary_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenNativeImportBinding {
    pub owner_id: String,
    pub runtime_build_sha256: String,
    pub manifest_sha256: String,
    pub session_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenMessageMapping {
    pub source_message_id: String,
    pub message_id: String,
    pub original_turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenArtifactMapping {
    pub source_artifact_id: String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenContinuationImportResult {
    pub conversation_id: String,
    pub copied_messages: Vec<FrozenMessageMapping>,
    pub copied_artifacts: Vec<FrozenArtifactMapping>,
}

fn invalid(reason: &str) -> DbError {
    DbError::Init(format!("Invalid frozen continuation: {reason}"))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn json_object(value: &str) -> Result<Value, DbError> {
    let parsed: Value = serde_json::from_str(value).map_err(|_| invalid("malformed JSON"))?;
    if !parsed.is_object() {
        return Err(invalid("expected JSON object"));
    }
    Ok(parsed)
}

fn timestamp(value: i64) -> bool {
    (0..=9_007_199_254_740_991).contains(&value)
}

fn exportable(value: &impl Serialize) -> Result<(), DbError> {
    // Matches the outbox record ceiling. A committed import must remain exportable.
    if serde_json::to_vec(value)
        .map_err(|_| invalid("record serialization failed"))?
        .len()
        > 16 * 1024 * 1024
    {
        return Err(invalid("frozen record exceeds export limit"));
    }
    Ok(())
}

fn fingerprint(params: &FrozenConversationImport) -> Result<String, DbError> {
    let bytes = serde_json::to_vec(params).map_err(|_| invalid("request serialization failed"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

impl FrozenConversationImport {
    /// Pure shape/scope validation, safe before native filesystem preparation.
    /// Target dependency authorization/freshness additionally requires repository
    /// preflight and is repeated within the final write transaction.
    pub fn validate(&self, user_id: &str) -> Result<(), DbError> {
        exportable(&self.source)?;
        exportable(&self.assistant)?;
        if self.source.user_id != user_id || !identifier(user_id) {
            return Err(DbError::NotFound("Frozen conversation owner does not match".into()));
        }
        if self.native_binding.owner_id != user_id
            || [
                &self.native_binding.runtime_build_sha256,
                &self.native_binding.manifest_sha256,
                &self.native_binding.session_sha256,
            ]
            .iter()
            .any(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(invalid("native archive binding does not match"));
        }
        if !identifier(&self.source.id)
            || !identifier(&self.target_id)
            || self.source.id == self.target_id
            || !identifier(&self.original_turn_id)
            || !identifier(&self.boundary_message_id)
        {
            return Err(invalid("invalid identity"));
        }
        if self.source.r#type != "aionrs" || self.source.status.as_deref() != Some("finished") {
            return Err(invalid("source must be a completed native conversation"));
        }
        let extra = json_object(&self.source.extra)?;
        if ["teamId", "team_id"]
            .iter()
            .any(|key| extra.get(key).is_some_and(|value| !value.is_null() && value != ""))
        {
            return Err(invalid("team conversations are unsupported"));
        }
        let workspace = Path::new(&self.target_workspace);
        if self.target_workspace.contains('\0')
            || self.target_workspace.len() > 4096
            || !workspace.is_absolute()
            || workspace.parent().is_none()
            || workspace.components().any(|part| matches!(part, Component::ParentDir))
            || extra
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(|source| Path::new(source) == workspace)
        {
            return Err(invalid("target workspace must be a new absolute path"));
        }
        if let Some(model) = &self.source.model {
            json_object(model)?;
        }
        if !timestamp(self.source.created_at) || !timestamp(self.source.updated_at) {
            return Err(invalid("invalid conversation timestamp"));
        }
        let snapshot = &self.assistant;
        if snapshot.conversation_id != self.source.id
            || [
                &snapshot.assistant_definition_id,
                &snapshot.assistant_id,
                &snapshot.agent_id,
            ]
            .iter()
            .any(|id| id.trim().is_empty())
            || !["builtin", "user", "generated"].contains(&snapshot.assistant_source.as_str())
            || [
                &snapshot.default_model_mode,
                &snapshot.default_permission_mode,
                &snapshot.default_thought_level_mode,
                &snapshot.default_skills_mode,
                &snapshot.default_mcps_mode,
            ]
            .iter()
            .any(|mode| !["auto", "fixed"].contains(&mode.as_str()))
            || !timestamp(snapshot.created_at)
            || !timestamp(snapshot.updated_at)
        {
            return Err(invalid("assistant snapshot does not match"));
        }
        for ids in [
            &snapshot.resolved_skill_ids,
            &snapshot.resolved_disabled_builtin_skill_ids,
            &snapshot.resolved_mcp_ids,
        ] {
            let ids: Vec<String> =
                serde_json::from_str(ids).map_err(|_| invalid("invalid assistant reference list"))?;
            if ids.iter().any(|id| id.trim().is_empty()) {
                return Err(invalid("invalid assistant reference list"));
            }
        }
        let mut message_ids = HashSet::new();
        for message in &self.messages {
            exportable(message)?;
            if message.conversation_id != self.source.id
                || !identifier(&message.id)
                || !message_ids.insert(&message.id)
                || message.r#type.is_empty()
                || !timestamp(message.created_at)
                || message
                    .position
                    .as_deref()
                    .is_some_and(|value| !["left", "right", "center", "pop"].contains(&value))
                || message
                    .status
                    .as_deref()
                    .is_some_and(|value| !["finish", "error"].contains(&value))
                || message
                    .backend_turn_id
                    .as_deref()
                    .is_some_and(|value| !identifier(value))
            {
                return Err(invalid("invalid or duplicate frozen message"));
            }
            json_object(&message.content)?;
        }
        if self
            .messages
            .windows(2)
            .any(|pair| (pair[0].created_at, &pair[0].id) >= (pair[1].created_at, &pair[1].id))
        {
            return Err(invalid("messages must be in strict timeline order"));
        }
        let boundary = self.messages.last().ok_or_else(|| invalid("missing turn boundary"))?;
        if boundary.id != self.boundary_message_id
            || boundary.status.as_deref() != Some("finish")
            || boundary.backend_turn_id.as_deref() != Some(self.original_turn_id.as_str())
        {
            return Err(invalid("turn boundary does not match"));
        }
        let mut artifact_ids = HashSet::new();
        for artifact in &self.artifacts {
            exportable(artifact)?;
            if artifact.conversation_id != self.source.id
                || !identifier(&artifact.id)
                || !artifact_ids.insert(&artifact.id)
                || !["cron_trigger", "skill_suggest"].contains(&artifact.kind.as_str())
                || !["active", "pending", "dismissed", "saved"].contains(&artifact.status.as_str())
                || !timestamp(artifact.created_at)
                || !timestamp(artifact.updated_at)
            {
                return Err(invalid("invalid or duplicate frozen artifact"));
            }
            json_object(&artifact.payload)?;
        }
        Ok(())
    }

    fn target_extra(&self) -> Result<String, DbError> {
        let mut extra = json_object(&self.source.extra)?;
        let object = extra.as_object_mut().expect("validated object");
        for key in [
            "fork",
            "cronJobId",
            "cron_job_id",
            "channel_chat_id",
            "channelChatId",
            "teamId",
            "team_id",
            "team_mcp_stdio_config",
            "slot_id",
            "role",
        ] {
            object.remove(key);
        }
        object.insert("workspace".into(), Value::String(self.target_workspace.clone()));
        object.insert("restoredFrom".into(), serde_json::json!({
            "conversationId": self.source.id, "messageId": self.boundary_message_id, "turnId": self.original_turn_id,
        }));
        serde_json::to_string(&extra).map_err(|_| invalid("extra serialization failed"))
    }
}

async fn validate_target(
    connection: &mut SqliteConnection,
    user_id: &str,
    params: &FrozenConversationImport,
) -> Result<(), DbError> {
    params.validate(user_id)?;
    let user_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND status = 'active')")
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
    if !user_exists {
        return Err(DbError::NotFound("Frozen continuation owner unavailable".into()));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ? UNION ALL SELECT 1 FROM acp_session WHERE conversation_id = ?)")
        .bind(&params.target_id).bind(&params.target_id).fetch_one(&mut *connection).await?;
    if exists {
        return Err(DbError::Conflict(
            "Frozen continuation destination already exists".into(),
        ));
    }
    // Check identity/visibility only. Live defaults must not override frozen settings.
    let definition: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_definitions WHERE id = ? AND assistant_id = ? AND source = ? AND deleted_at IS NULL AND (user_id = ? OR (user_id IS NULL AND source = 'builtin' AND owner_type = 'system')))")
        .bind(&params.assistant.assistant_definition_id).bind(&params.assistant.assistant_id)
        .bind(&params.assistant.assistant_source).bind(user_id).fetch_one(&mut *connection).await?;
    let agent: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_metadata WHERE agent_id = ? AND (user_id = ? OR (user_id IS NULL AND agent_source = 'builtin')) AND COALESCE(NULLIF(TRIM(backend), ''), agent_type) = 'aionrs')")
        .bind(&params.assistant.agent_id).bind(user_id).fetch_one(&mut *connection).await?;
    if !definition || !agent {
        return Err(DbError::NotFound("Frozen continuation dependencies unavailable".into()));
    }
    Ok(())
}

pub(super) async fn preflight(
    pool: &SqlitePool,
    user_id: &str,
    params: &FrozenConversationImport,
) -> Result<(), DbError> {
    let mut transaction = pool.begin().await?;
    validate_target(&mut transaction, user_id, params).await?;
    transaction.rollback().await?;
    Ok(())
}

/// Read the committed import identity only. This never claims that the target
/// has not subsequently progressed; native/file readiness is checked separately.
pub(super) async fn lookup(
    pool: &SqlitePool,
    user_id: &str,
    params: &FrozenConversationImport,
) -> Result<Option<FrozenContinuationImportResult>, DbError> {
    params.validate(user_id)?;
    let row: Option<(String, String)> = sqlx::query_as("SELECT r.request_sha256, r.result_json FROM frozen_continuation_imports r JOIN conversations c ON c.id = r.conversation_id AND c.user_id = r.subject_id WHERE r.conversation_id = ? AND r.subject_id = ?")
        .bind(&params.target_id).bind(user_id).fetch_optional(pool).await?;
    let Some((digest, result)) = row else {
        return Ok(None);
    };
    if digest != fingerprint(params)? {
        return Err(DbError::Conflict(
            "Frozen continuation request does not match committed import".into(),
        ));
    }
    let result: FrozenContinuationImportResult =
        serde_json::from_str(&result).map_err(|_| invalid("committed import result is invalid"))?;
    if result.conversation_id != params.target_id {
        return Err(invalid("committed import target does not match"));
    }
    Ok(Some(result))
}

pub(super) async fn import(
    pool: &SqlitePool,
    user_id: &str,
    params: &FrozenConversationImport,
) -> Result<FrozenContinuationImportResult, DbError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    validate_target(&mut transaction, user_id, params).await?;
    let extra = params.target_extra()?;
    let source = &params.source;
    let now = aionui_common::now_ms();
    sqlx::query("INSERT INTO conversations (id, user_id, name, type, extra, model, status, source, channel_chat_id, pinned, pinned_at, created_at, updated_at, project_id, folder_id, name_source) VALUES (?, ?, ?, 'aionrs', ?, ?, 'pending', 'aionui', NULL, 0, NULL, ?, ?, NULL, NULL, ?)")
        .bind(&params.target_id).bind(user_id).bind(&source.name).bind(extra).bind(&source.model)
        .bind(now).bind(now).bind(&source.name_source).execute(&mut *transaction).await?;
    let snapshot = &params.assistant;
    sqlx::query("INSERT INTO conversation_assistant_snapshots (conversation_id, assistant_definition_id, assistant_id, assistant_source, agent_id, rules_content, default_model_mode, resolved_model_id, default_permission_mode, resolved_permission_value, default_thought_level_mode, resolved_thought_level_value, default_skills_mode, resolved_skill_ids, resolved_disabled_builtin_skill_ids, default_mcps_mode, resolved_mcp_ids, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&params.target_id).bind(&snapshot.assistant_definition_id).bind(&snapshot.assistant_id)
        .bind(&snapshot.assistant_source).bind(&snapshot.agent_id).bind(&snapshot.rules_content)
        .bind(&snapshot.default_model_mode).bind(&snapshot.resolved_model_id).bind(&snapshot.default_permission_mode)
        .bind(&snapshot.resolved_permission_value).bind(&snapshot.default_thought_level_mode).bind(&snapshot.resolved_thought_level_value)
        .bind(&snapshot.default_skills_mode).bind(&snapshot.resolved_skill_ids).bind(&snapshot.resolved_disabled_builtin_skill_ids)
        .bind(&snapshot.default_mcps_mode).bind(&snapshot.resolved_mcp_ids).bind(snapshot.created_at).bind(snapshot.updated_at)
        .execute(&mut *transaction).await?;
    let mut copied_messages = Vec::with_capacity(params.messages.len());
    for message in &params.messages {
        let message_id = aionui_common::generate_id();
        sqlx::query("INSERT INTO messages (id, conversation_id, msg_id, type, content, position, status, hidden, created_at, backend_turn_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)")
            .bind(&message_id).bind(&params.target_id).bind(&message.msg_id).bind(&message.r#type).bind(&message.content)
            .bind(&message.position).bind(&message.status).bind(message.hidden).bind(message.created_at)
            .execute(&mut *transaction).await?;
        copied_messages.push(FrozenMessageMapping {
            source_message_id: message.id.clone(),
            message_id,
            original_turn_id: message.backend_turn_id.clone(),
        });
    }
    let mut copied_artifacts = Vec::with_capacity(params.artifacts.len());
    for artifact in &params.artifacts {
        let artifact_id = aionui_common::generate_id();
        sqlx::query("INSERT INTO conversation_artifacts (id, conversation_id, cron_job_id, kind, status, payload, created_at, updated_at) VALUES (?, ?, NULL, ?, ?, ?, ?, ?)")
            .bind(&artifact_id).bind(&params.target_id).bind(&artifact.kind).bind(&artifact.status).bind(&artifact.payload)
            .bind(artifact.created_at).bind(artifact.updated_at).execute(&mut *transaction).await?;
        copied_artifacts.push(FrozenArtifactMapping {
            source_artifact_id: artifact.id.clone(),
            artifact_id,
        });
    }
    // Validate actual trigger payloads, including the outbox's framing allowance
    // and transformed identifiers/extra, before making the import visible.
    let unexportable: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_outbox WHERE conversation_id = ? AND length(CAST(payload AS BLOB)) + 1024 > 16777216)")
        .bind(&params.target_id).fetch_one(&mut *transaction).await?;
    if unexportable {
        return Err(invalid("frozen record exceeds export limit"));
    }
    let result = FrozenContinuationImportResult {
        conversation_id: params.target_id.clone(),
        copied_messages,
        copied_artifacts,
    };
    sqlx::query("INSERT INTO frozen_continuation_imports (conversation_id, subject_id, request_sha256, result_json, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&params.target_id).bind(user_id).bind(fingerprint(params)?)
        .bind(serde_json::to_string(&result).map_err(|_| invalid("result serialization failed"))?).bind(now)
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(result)
}
