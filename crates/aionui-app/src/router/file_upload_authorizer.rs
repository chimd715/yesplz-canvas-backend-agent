//! Conversation ownership adapter for browser uploads.

use aionui_db::IConversationRepository;
use aionui_file::{FileError, IUploadAuthorizer};
use std::sync::Arc;

pub(super) struct ConversationUploadAuthorizer {
    pub conversations: Arc<dyn IConversationRepository>,
}

#[async_trait::async_trait]
impl IUploadAuthorizer for ConversationUploadAuthorizer {
    async fn authorize(&self, user_id: &str, conversation_id: &str) -> Result<(), FileError> {
        let conversation = self
            .conversations
            .get(user_id, conversation_id)
            .await
            .map_err(|_| FileError::Internal("could not authorize attachment".to_owned()))?;
        if conversation.is_none() {
            return Err(FileError::NotFound("conversation not found".to_owned()));
        }
        Ok(())
    }
}
