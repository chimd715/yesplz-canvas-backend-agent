-- Local acceptance receipts are committed with the first user message. They
-- survive message resets so retrying an accepted ID cannot repeat tool effects.
-- subject_id stays immutable across legacy ownership adoption, like the outbox.
CREATE TABLE IF NOT EXISTS conversation_send_receipts (
    subject_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    client_request_id TEXT NOT NULL CHECK(length(client_request_id) BETWEEN 1 AND 128),
    payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256) = 64),
    msg_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(subject_id, conversation_id, client_request_id)
);
