use std::sync::Arc;

use aionui_api_types::{
    AgentErrorCode, AgentErrorOwnership, AgentStreamErrorData, ConversationRuntimeSummary, WebSocketMessage,
};
use aionui_common::now_ms;
use aionui_db::{ConversationRowUpdate, DbError, IConversationRepository};
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use tracing::debug;

use crate::runtime_persistence::{RuntimePersistenceCoordinator, RuntimeWriteKind};

pub(crate) fn local_persistence_error() -> AgentStreamErrorData {
    AgentStreamErrorData::classified(
        "Conversation history could not be saved locally. Reload and check the saved history before retrying.",
        AgentErrorCode::AionuiInternalError,
        AgentErrorOwnership::Aionui,
        None,
        false,
        true,
        None,
    )
}

#[derive(Clone)]
pub struct RuntimeCompletionPublisher {
    user_id: String,
    repo: Arc<dyn IConversationRepository>,
    broadcaster: Arc<dyn EventBroadcaster>,
    persistence: RuntimePersistenceCoordinator,
}

impl RuntimeCompletionPublisher {
    pub fn new(
        user_id: String,
        repo: Arc<dyn IConversationRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
        persistence: RuntimePersistenceCoordinator,
    ) -> Self {
        Self {
            user_id,
            repo,
            broadcaster,
            persistence,
        }
    }

    #[tracing::instrument(skip_all, fields(conversation_id = %conversation_id, turn_id = %turn_id))]
    pub async fn persist_finished(&self, conversation_id: &str, turn_id: &str) -> Result<bool, DbError> {
        if !self
            .persistence
            .allows(conversation_id, RuntimeWriteKind::ConversationFinished)
        {
            debug!(
                conversation_id,
                turn_id, "turn completion skipped by runtime persistence policy"
            );
            return Ok(false);
        }

        match self.repo.get(&self.user_id, conversation_id).await? {
            Some(_) => {}
            None => {
                debug!(
                    conversation_id,
                    turn_id, "turn completion skipped because conversation row is missing"
                );
                return Ok(false);
            }
        }

        let update = ConversationRowUpdate {
            status: Some("finished".to_owned()),
            updated_at: Some(now_ms()),
            ..Default::default()
        };
        self.repo.update(&self.user_id, conversation_id, &update).await?;
        Ok(true)
    }

    /// Called only after local writes succeeded. Orchestrated turns release
    /// their claim before publishing the resulting runtime summary.
    pub fn publish_persisted(
        &self,
        conversation_id: &str,
        turn_id: &str,
        runtime: Option<ConversationRuntimeSummary>,
    ) {
        let payload = json!({
            "user_id": self.user_id,
            "conversation_id": conversation_id,
            "session_id": conversation_id,
            "turn_id": turn_id,
            "status": "finished",
            "canSendMessage": true,
            "runtime": runtime,
        });
        self.broadcaster
            .broadcast(WebSocketMessage::new("turn.completed", payload));

        debug!(conversation_id, turn_id, status = "finished", "Turn completed");
    }

    pub async fn publish(
        &self,
        conversation_id: &str,
        turn_id: &str,
        runtime: Option<ConversationRuntimeSummary>,
    ) -> Result<(), DbError> {
        if self.persist_finished(conversation_id, turn_id).await? {
            self.publish_persisted(conversation_id, turn_id, runtime);
        }
        Ok(())
    }

    pub(crate) fn publish_failure(&self, conversation_id: &str, turn_id: &str, msg_id: &str) {
        self.broadcaster.broadcast(WebSocketMessage::new(
            "message.stream",
            json!({
                "user_id": self.user_id,
                "conversation_id": conversation_id,
                "msg_id": msg_id,
                "turn_id": turn_id,
                "type": "error",
                "data": local_persistence_error(),
                "hidden": false,
            }),
        ));
    }
}
