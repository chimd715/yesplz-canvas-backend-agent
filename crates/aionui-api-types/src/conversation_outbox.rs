use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationOutboxQuery {
    pub after_sequence: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeConversationOutboxRequest {
    pub through_sequence: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AcknowledgeConversationOutboxResponse {
    pub acknowledged_sequence: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConversationOutboxEvent {
    pub sequence: i64,
    pub user_id: String,
    pub conversation_id: String,
    pub entity: String,
    pub entity_id: String,
    pub operation: String,
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConversationOutboxResponse {
    pub schema_version: u32,
    pub after_sequence: i64,
    pub through_sequence: i64,
    pub acknowledged_sequence: i64,
    pub latest_sequence: i64,
    pub has_more: bool,
    pub events: Vec<ConversationOutboxEvent>,
}
