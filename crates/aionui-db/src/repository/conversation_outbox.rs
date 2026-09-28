use sqlx::SqlitePool;

use crate::DbError;
use crate::models::{ConversationOutboxBatch, ConversationOutboxEvent};

const MAX_SEQUENCE: i64 = 9_007_199_254_740_991;
const MAX_EXPORT_BYTES: i64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConversationOutboxError {
    #[error("limit must be between 1 and 100")]
    InvalidLimit,
    #[error("invalid conversation outbox cursor")]
    InvalidCursor,
    #[error("cannot skip unacknowledged conversation changes")]
    UnacknowledgedCursor,
    #[error("cannot acknowledge changes that have not been exported")]
    UnexportedAck,
    #[error("conversation outbox record exceeds the export size limit")]
    RecordTooLarge,
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for ConversationOutboxError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(DbError::Query(error))
    }
}

#[async_trait::async_trait]
pub trait IConversationOutboxRepository: Send + Sync {
    /// A missing cursor starts at the ACK watermark. Earlier cursors replay
    /// retained history; later cursors cannot skip unacknowledged writes.
    async fn export(
        &self,
        user_id: &str,
        after_sequence: Option<i64>,
        limit: u32,
    ) -> Result<ConversationOutboxBatch, ConversationOutboxError>;

    /// Advances only delivery metadata. Never deletes captured history.
    async fn acknowledge(&self, user_id: &str, through_sequence: i64) -> Result<i64, ConversationOutboxError>;
}

pub struct SqliteConversationOutboxRepository {
    pool: SqlitePool,
}

impl SqliteConversationOutboxRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IConversationOutboxRepository for SqliteConversationOutboxRepository {
    async fn export(
        &self,
        user_id: &str,
        after_sequence: Option<i64>,
        limit: u32,
    ) -> Result<ConversationOutboxBatch, ConversationOutboxError> {
        if !(1..=100).contains(&limit) {
            return Err(ConversationOutboxError::InvalidLimit);
        }
        if after_sequence.is_some_and(|value| !(0..=MAX_SEQUENCE).contains(&value)) {
            return Err(ConversationOutboxError::InvalidCursor);
        }

        let mut tx = self.pool.begin().await?;
        // Take the writer lock before reading; a snapshot + exported watermark
        // must be atomic even when a turn writes during an export.
        sqlx::query("INSERT INTO conversation_outbox_delivery (subject_id) VALUES (?) ON CONFLICT DO NOTHING")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let acknowledged_sequence: i64 = sqlx::query_scalar(
            "SELECT acknowledged_sequence FROM conversation_outbox_delivery WHERE subject_id = ?",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        let after_sequence = after_sequence.unwrap_or(acknowledged_sequence);
        if after_sequence > acknowledged_sequence {
            return Err(ConversationOutboxError::UnacknowledgedCursor);
        }
        let latest_sequence: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sequence), 0) FROM conversation_outbox WHERE subject_id = ?")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
        // Bound rows AND payload bytes in SQLite, before allocating payloads.
        // An oversized first record fails explicitly; it is never skipped.
        let rows = sqlx::query_as::<_, (i64, String, String, String, String, Option<String>)>(
            "SELECT sequence, conversation_id, entity, entity_id, operation, payload FROM ( \
                 SELECT *, SUM(COALESCE(length(CAST(payload AS BLOB)), 0) + 1024) \
                     OVER (ORDER BY sequence) AS page_bytes FROM ( \
                         SELECT * FROM conversation_outbox WHERE subject_id = ? AND sequence > ? \
                         ORDER BY sequence LIMIT ? \
                     ) \
             ) WHERE page_bytes <= ? ORDER BY sequence",
        )
        .bind(user_id)
        .bind(after_sequence)
        .bind(i64::from(limit))
        .bind(MAX_EXPORT_BYTES)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() && latest_sequence > after_sequence {
            return Err(ConversationOutboxError::RecordTooLarge);
        }
        let mut events = Vec::with_capacity(rows.len());
        for (sequence, conversation_id, entity, entity_id, operation, payload) in rows {
            let payload = payload
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|_| DbError::Init("Invalid conversation outbox payload".into()))?;
            events.push(ConversationOutboxEvent {
                sequence,
                user_id: user_id.to_owned(),
                conversation_id,
                entity,
                entity_id,
                operation,
                payload,
            });
        }
        let through_sequence = events.last().map_or(after_sequence, |event| event.sequence);
        sqlx::query(
            "UPDATE conversation_outbox_delivery SET exported_sequence = MAX(exported_sequence, ?) WHERE subject_id = ?",
        )
        .bind(through_sequence)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(ConversationOutboxBatch {
            after_sequence,
            through_sequence,
            acknowledged_sequence,
            latest_sequence,
            has_more: latest_sequence > through_sequence,
            events,
        })
    }

    async fn acknowledge(&self, user_id: &str, through_sequence: i64) -> Result<i64, ConversationOutboxError> {
        if !(0..=MAX_SEQUENCE).contains(&through_sequence) {
            return Err(ConversationOutboxError::InvalidCursor);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO conversation_outbox_delivery (subject_id) VALUES (?) ON CONFLICT DO NOTHING")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let (acknowledged, exported): (i64, i64) = sqlx::query_as(
            "SELECT acknowledged_sequence, exported_sequence FROM conversation_outbox_delivery WHERE subject_id = ?",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if through_sequence > acknowledged {
            let belongs_to_user: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM conversation_outbox WHERE subject_id = ? AND sequence = ?)",
            )
            .bind(user_id)
            .bind(through_sequence)
            .fetch_one(&mut *tx)
            .await?;
            if through_sequence > exported || !belongs_to_user {
                return Err(ConversationOutboxError::UnexportedAck);
            }
            sqlx::query("UPDATE conversation_outbox_delivery SET acknowledged_sequence = ? WHERE subject_id = ?")
                .bind(through_sequence)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(acknowledged.max(through_sequence))
    }
}
