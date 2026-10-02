//! Persist and publish native document outputs only after owner/project validation.
use std::{collections::HashMap, path::Path, sync::Arc};

use aionui_ai_agent::{
    capability::generated_artifacts::{PUBLISH_ARTIFACT_TOOL, validate_generated_file},
    protocol::events::{ToolCallEventData, ToolCallStatus},
};
use aionui_api_types::{ChatFileRef, GeneratedFileArtifactPayload, WebSocketMessage};
use aionui_common::now_ms;
use aionui_db::{IConversationRepository, models::ConversationArtifactRow};
use aionui_project::{FileOp, ProjectService, ReferenceInput};
use aionui_realtime::EventBroadcaster;

use crate::{ConversationError, convert::row_to_artifact_response};

/// Only matching successful native tool completions may become artifact candidates.
#[derive(Default)]
pub(crate) struct GeneratedFileCalls(HashMap<String, (String, String)>);
impl GeneratedFileCalls {
    pub(crate) fn observe(&mut self, event: &ToolCallEventData) -> Option<String> {
        if !event.call_id.starts_with("aionrs-") {
            return None;
        }
        match event.status {
            ToolCallStatus::Running if event.name == "Write" || event.name == PUBLISH_ARTIFACT_TOOL => {
                if self.0.len() < 128
                    && let Some(path) = event
                        .input
                        .as_ref()
                        .unwrap_or(&event.args)
                        .get("file_path")
                        .and_then(serde_json::Value::as_str)
                        .filter(|path| path.len() <= 4096)
                {
                    self.0
                        .insert(event.call_id.clone(), (event.name.clone(), path.to_owned()));
                }
                None
            }
            ToolCallStatus::Completed => self
                .0
                .remove(&event.call_id)
                .filter(|(name, _)| name == &event.name)
                .map(|(_, path)| path),
            ToolCallStatus::Error | ToolCallStatus::Canceled => {
                self.0.remove(&event.call_id);
                None
            }
            ToolCallStatus::Running => {
                self.0.remove(&event.call_id);
                None
            }
        }
    }
}

pub(crate) async fn publish_generated_file(
    repo: &Arc<dyn IConversationRepository>,
    project_service: &ProjectService,
    broadcaster: &Arc<dyn EventBroadcaster>,
    owner: &str,
    conversation_id: &str,
    artifact_id: String,
    file_path: &str,
) -> Result<(), ConversationError> {
    // Active owner lookup also hides retained, customer-deleted conversations.
    let conversation = repo
        .get(owner, conversation_id)
        .await?
        .ok_or_else(|| ConversationError::NotFound {
            id: conversation_id.to_owned(),
        })?;
    let project_id = conversation
        .project_id
        .as_deref()
        .ok_or_else(|| ConversationError::bad_request("Generated document has no project binding"))?;
    let project = project_service
        .get_project(owner, project_id)
        .await
        .map_err(|_| ConversationError::bad_request("Generated document project is unavailable"))?;
    let pe_id = project.explorer.workspace_pe_id;
    let workspace = project_service
        .resolve_reference(
            owner,
            ReferenceInput {
                pe_id: pe_id.clone(),
                relative_path: String::new(),
                op: FileOp::Read,
            },
        )
        .await
        .map_err(|_| ConversationError::bad_request("Generated document workspace is unavailable"))?;
    let root = workspace
        .absolute_path
        .ok_or_else(|| ConversationError::bad_request("Generated document workspace is unavailable"))?;
    let extra: serde_json::Value = serde_json::from_str(&conversation.extra)
        .map_err(|_| ConversationError::bad_request("Generated document scope is unavailable"))?;
    let conversation_workspace = extra
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ConversationError::bad_request("Generated document scope is unavailable"))?;
    let (canonical_root, relative_path) = validate_generated_file(Path::new(&root), file_path)
        .await
        .map_err(ConversationError::bad_request)?;
    if tokio::fs::canonicalize(conversation_workspace).await.ok().as_ref() != Some(&canonical_root) {
        return Err(ConversationError::bad_request(
            "Generated document workspace does not match its conversation",
        ));
    }
    let resolved = project_service
        .resolve_reference(
            owner,
            ReferenceInput {
                pe_id: pe_id.clone(),
                relative_path: relative_path.clone(),
                op: FileOp::Read,
            },
        )
        .await
        .map_err(|_| ConversationError::bad_request("Generated document is unavailable"))?;
    if resolved.project_id != project_id {
        return Err(ConversationError::bad_request(
            "Generated document project does not match its conversation",
        ));
    }
    let file_name = Path::new(&relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ConversationError::bad_request("Invalid generated document name"))?
        .to_owned();
    let payload = GeneratedFileArtifactPayload {
        project_id: project_id.to_owned(),
        file: ChatFileRef::Project { pe_id, relative_path },
        file_name,
    };
    let timestamp = now_ms();
    let row = ConversationArtifactRow {
        id: artifact_id,
        conversation_id: conversation_id.to_owned(),
        cron_job_id: None,
        kind: "generated_file".into(),
        status: "active".into(),
        payload: serde_json::to_string(&payload)
            .map_err(|_| ConversationError::bad_request("Invalid generated document metadata"))?,
        created_at: timestamp,
        updated_at: timestamp,
    };
    let stored = repo.upsert_artifact(owner, &row).await?;
    // Recheck after persistence; a concurrent delete must never emit a customer artifact.
    if repo.get(owner, conversation_id).await?.is_none() {
        return Ok(());
    }
    let mut event = serde_json::to_value(row_to_artifact_response(stored)?)
        .map_err(|_| ConversationError::bad_request("Invalid generated document metadata"))?;
    event["user_id"] = owner.into();
    broadcaster.broadcast(WebSocketMessage::new("conversation.artifact", event));
    Ok(())
}
