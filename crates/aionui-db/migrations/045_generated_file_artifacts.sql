-- Preserve existing conversation artifacts while allowing trusted generated
-- documents. SQLite requires rebuilding the table to extend its CHECK clause.
CREATE TABLE IF NOT EXISTS _conversation_artifacts_generated_file (
    id              TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT    NOT NULL,
    cron_job_id     TEXT,
    kind            TEXT    NOT NULL
                            CHECK(kind IN ('cron_trigger', 'skill_suggest', 'generated_file')),
    status          TEXT    NOT NULL DEFAULT 'active'
                            CHECK(status IN ('active', 'pending', 'dismissed', 'saved')),
    payload         TEXT    NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

INSERT INTO _conversation_artifacts_generated_file
    (id, conversation_id, cron_job_id, kind, status, payload, created_at, updated_at)
SELECT id, conversation_id, cron_job_id, kind, status, payload, created_at, updated_at
FROM conversation_artifacts;

DROP TABLE conversation_artifacts;
ALTER TABLE _conversation_artifacts_generated_file RENAME TO conversation_artifacts;

CREATE INDEX IF NOT EXISTS idx_conversation_artifacts_conversation_id ON conversation_artifacts(conversation_id);
CREATE INDEX IF NOT EXISTS idx_conversation_artifacts_created_at ON conversation_artifacts(created_at);
CREATE INDEX IF NOT EXISTS idx_conversation_artifacts_conversation_created ON conversation_artifacts(conversation_id, created_at);
CREATE INDEX IF NOT EXISTS idx_conversation_artifacts_cron_job ON conversation_artifacts(cron_job_id);
CREATE INDEX IF NOT EXISTS idx_conversation_artifacts_kind_status ON conversation_artifacts(kind, status);
