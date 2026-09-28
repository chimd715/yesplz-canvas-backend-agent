use serde_json::Value;

/// Immutable after-image or tombstone captured by a SQLite trigger.
#[derive(Debug, Clone)]
pub struct ConversationOutboxEvent {
    pub sequence: i64,
    pub user_id: String,
    pub conversation_id: String,
    pub entity: String,
    pub entity_id: String,
    pub operation: String,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ConversationOutboxBatch {
    pub after_sequence: i64,
    pub through_sequence: i64,
    pub acknowledged_sequence: i64,
    pub latest_sequence: i64,
    pub has_more: bool,
    pub events: Vec<ConversationOutboxEvent>,
}
