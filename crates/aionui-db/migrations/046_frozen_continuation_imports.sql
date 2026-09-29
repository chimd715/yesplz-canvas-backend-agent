-- Preserve the exact committed copy mapping across a lost preparation response.
-- The immutable subject follows receipt/outbox ownership, not legacy adoption.
CREATE TABLE IF NOT EXISTS frozen_continuation_imports (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    subject_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_sha256 TEXT NOT NULL CHECK(length(request_sha256) = 64 AND request_sha256 NOT GLOB '*[^0-9a-f]*'),
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    created_at INTEGER NOT NULL
);

CREATE TRIGGER IF NOT EXISTS frozen_continuation_imports_immutable
BEFORE UPDATE ON frozen_continuation_imports
BEGIN
    SELECT RAISE(ABORT, 'Frozen continuation import receipts are immutable');
END;
