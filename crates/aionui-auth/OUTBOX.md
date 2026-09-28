# Conversation outbox contract

Migration 044 captures changes to conversations, messages, conversation artifacts,
and assistant snapshots in the same SQLite transaction as the source write. SQL
triggers also capture direct writes outside the normal repositories. Installing
the migration backfills existing rows before new writes can occur. Local source
tables remain intact.

These endpoints are for a trusted platform server. They require
`x-aioncore-bootstrap-secret` and an active Core user ID. Ordinary Core session
cookies and bearer tokens do not authorize export or acknowledgment. The platform
must verify its project-to-Core-user mapping before choosing the path; the
bootstrap credential itself is instance-wide. Never place a platform-wide
Supabase credential in the runtime. Requests and logs must not contain credentials
in URLs. Logs record cursor/user metadata, never row payloads.

## Export and acknowledgment

`GET /api/auth/internal/conversation-outbox/{core_user_id}?after_sequence=0&limit=100`
returns an `ApiResponse` whose `data` has:

```json
{
  "schema_version": 1,
  "after_sequence": 0,
  "through_sequence": 12,
  "acknowledged_sequence": 0,
  "latest_sequence": 12,
  "has_more": false,
  "events": [
    {
      "sequence": 12,
      "user_id": "core-user-id",
      "conversation_id": "conversation-id",
      "entity": "conversation",
      "entity_id": "conversation-id",
      "operation": "delete",
      "payload": null
    }
  ]
}
```

- Sequences are increasing SQLite database-wide integers, bounded to JavaScript's
  safe integer range. Gaps for a user's stream are valid; compare the previous
  `after_sequence` to the platform cursor rather than requiring `sequence + 1`.
- Omitted `after_sequence` starts at the acknowledged cursor. Explicit cursors
  from zero through that cursor replay retained history. A later cursor fails
  with 409 so callers cannot skip unacknowledged changes.
- `limit` is 1–100 (default 100). SQL bounds the page to 16 MiB of stored JSON
  payload plus 1 KiB per event before loading payloads. An oversized first record
  fails with 413 / `OUTBOX_RECORD_TOO_LARGE`; it is never silently skipped.
- `entity` is `conversation`, `message`, `artifact`, or `assistant_snapshot`.
  An upsert payload is the full native SQLite row. JSON-valued TEXT columns stay
  strings; nullable columns and integer booleans retain their native values.
- Deletes have null payload. A conversation delete must also remove its child
  entities in the central projection. Ownership changes emit an old-owner
  conversation tombstone and then the parent plus all children for the new owner.
  Earlier history retains its original owner.

After committing the complete exported page durably outside the runtime, call
`POST /api/auth/internal/conversation-outbox/{core_user_id}/ack` with
`{"through_sequence":12}`. Its `data` is `{"acknowledged_sequence":12}`.
ACK is monotonic and idempotent. It cannot advance beyond a previously exported
event belonging to that user. Export alone does not acknowledge anything. ACK
never deletes events. Retry an interrupted transfer from the acknowledged cursor;
deduplicate centrally by runtime generation plus event sequence.

An absent/incorrect bootstrap secret returns 401, unavailable user 404, invalid
query/cursor 400, and skipped cursor/unexported acknowledgment 409. Existing
binaries without these routes return 404; verify the runtime build and successful
schema-version response before enabling synchronization. This source change does
not update an installed binary or any live Sprite.

## Completion and recovery boundary

This contract supplies durable local capture and transport. It does not gate turn
acceptance on external persistence, change UI Saved/Ready semantics, flush native
agent session files, or restore rows. Those operations require a coordinated
platform persistence/restore implementation before central durability can be
claimed. A captured message transcript is not a native resumable session.

The conversation relay now carries local SQLite write failures to the turn
orchestrator, prevents automatic replay after such failures, and emits an error
instead of successful turn completion. The orchestrator keeps its turn claim
through the final conversation-status write. This is a local failure boundary;
it does not wait for the platform outbox acknowledgment.

Two integration gaps must be closed before accepting hosted turns as durable:

1. Core currently generates message and turn IDs for every send request. Add a
   durable platform request ID to Core message/turn mapping and a verified
   accepted-request receipt. A retry after a lost Core response must return the
   existing mapping rather than execute again. Ordinary, mid-turn, and internal
   agent-turn entry paths must all honor the hosted acceptance gate. Persisting
   input only at the platform without this mapping is insufficient.
2. The pinned aionrs v0.2.11 engine has no public error-returning flush/checkpoint
   method. Its private `save_session` emits save errors without returning them;
   `SessionManager` writes a temporary file and renames it without `sync_all`.
   Verified against commit `8e61a90329fa9f67c4fdf7e97fe02c24dba33f75`,
   [engine.rs](https://github.com/iOfficeAI/aionrs/blob/8e61a90329fa9f67c4fdf7e97fe02c24dba33f75/crates/aion-agent/src/engine.rs#L1512)
   and [session.rs](https://github.com/iOfficeAI/aionrs/blob/8e61a90329fa9f67c4fdf7e97fe02c24dba33f75/crates/aion-agent/src/session.rs#L448).
   The opt-in Linux [native checkpoint producer](../aionui-ai-agent/NATIVE_CHECKPOINTS.md)
   now observes that synchronous save failure and validates/copies a successfully
   completed normal turn under the engine mutex. It syncs an immutable private
   copy and returns errors before successful completion. It does not cover
   cancellation/slash commands or capture the complete Core/source/settings
   boundary. A coordinator must bind and externally commit these artifacts;
   neither a streamed Finish nor a local native checkpoint proves that happened.

History currently has no pruning policy. Repeated message updates append complete
rows and can grow the database substantially. Add checkpoint-aware retention only
after external recovery is verified. Back up the complete data directory before
upgrading: migration 044 advances the SQLite schema, and older Core builds may
reject a newer database.

## Focused validation

`python3 scripts/migration/check-conversation-outbox.py` executes the real SQLite
migrations and checks backfill, all-column payloads, direct writes, transaction
rollback/capture failure, adoption, tombstones, and history retention.

With the repository Rust toolchain available, also run:

```sh
cargo test -p aionui-db --test conversation_outbox
cargo test -p aionui-auth --test route_tests outbox_
cargo test -p aionui-db --test adoption_coverage
cargo fmt --all -- --check
cargo clippy -p aionui-db -p aionui-auth -p aionui-api-types -p aionui-app -- -D warnings
```
