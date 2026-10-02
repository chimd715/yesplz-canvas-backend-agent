-- Customer deletion hides a conversation while retaining its row, messages,
-- tool history, sessions, artifacts and workspace for trusted internal analytics.
-- Ordinary repositories must always exclude tombstoned conversations.
ALTER TABLE conversations ADD COLUMN deleted_at INTEGER;
CREATE INDEX idx_conversations_customer_visible
    ON conversations(user_id, updated_at DESC, id DESC)
    WHERE deleted_at IS NULL;
