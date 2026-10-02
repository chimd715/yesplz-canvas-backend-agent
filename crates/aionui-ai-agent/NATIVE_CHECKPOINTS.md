# Native session checkpoints

Set `AIONCORE_NATIVE_CHECKPOINTS=1` only on a verified Linux custom Core build.
The default is disabled; `0` also disables it. Other values fail initialization.
This feature does not change the official binary deployed in existing Sprites.

After a successful normal aionrs turn (`EndTurn`, at least one provider turn),
Core holds the engine mutex, checks its synchronous native-save failure latch,
and validates the persisted session ID, last assistant turn anchor, token usage,
and persisted context accounting against the engine's public status/result.
It then commits a private immutable copy before emitting its successful Finish.
Checkpoint failure is non-retryable: do not automatically repeat tool effects.
Native session initialization failures are fatal even with checkpoints disabled.

This relies on the pinned aionrs v0.2.11 implementation: successful normal final
branches call `save_session` before returning; that method copies messages, usage
and context then synchronously calls `OutputSink::emit_error` with
`Failed to save session: ` on failure. The latch observes that exact prefix;
ordinary tool-result errors and recoverable compaction errors do not set it.
Review this contract and run the real-engine failure-injection fixtures whenever
the native dependency changes. Public live history/export APIs are unavailable.

## Recoverable compaction diagnostics

In pinned aionrs v0.2.11 (`crates/aion-agent/src/engine.rs`, commit
`8e61a90329fa9f67c4fdf7e97fe02c24dba33f75`), an automatic compaction failure
emits `Autocompact failed: ` through `OutputSink::emit_error` and continues the
native turn. Core maps only that diagnostic to a nonterminal Warning tip so
the relay keeps receiving subsequent text, tools and the actual Finish/Error.
The warning is retained in conversation history; a payload-free warning log
records the classification. Other errors, including native session-save
failures, retain their existing terminal behavior.

This fixes premature relay termination, not compaction latency or provider
timeouts. It does not alter context thresholds, retries, models, persisted
sessions or checkpoint eligibility. Existing workspaces require a rebuilt Core
binary and a controlled idle-runtime upgrade; a management-server deployment
alone does not install this change.

## Private coordinator contract

Relative to Core's configured native session directory (normally
`<data_dir>/aionrs-sessions`), completed checkpoints are:

```
checkpoints/<conversation_id>/<original_core_turn_id>/state.json
checkpoints/<conversation_id>/<original_core_turn_id>/manifest.json
```

The manifest is JSON with these fields:

```
schema_version: 1
backend: "aionrs"
backend_version: "0.2.11"
conversation_id, session_id, turn_id: original Core identifiers
session_sha256: lowercase SHA-256 of the exact state.json bytes
session_bytes: byte length (maximum 32 MiB)
created_at_ms: checkpoint creation time
local_only: true
```

Directory access uses pinned descriptors, no-follow opens and single path
components. Files are mode 0600, directories 0700. Files and staging directory
are synced before an atomic no-replace rename; the parent is synced afterward.
Existing turn IDs cannot be overwritten. Partial `pending_*` directories may
remain after failure and are never completed checkpoints. No cleanup deletes
previous checkpoints or native session data.

There is no browser endpoint. A trusted external coordinator must select an
explicit conversation/turn after checking ownership, validate manifest identity,
hash and byte length, and collect these exact bytes. It must also bind a consistent
Core SQLite/outbox sequence, runtime build/generation, matching Git revision,
attachments and configuration. The coordinator must enforce idle/writer fencing
across that larger capture; this native file alone does not provide it. Directory
permissions do not isolate processes running under the same OS user.

The manifest proves a local native-session boundary, not Supabase/object-storage
acknowledgment or a Saved/Ready state. Do not restore from mutable live state.json
when a revision references this immutable checkpoint. No central credentials,
payloads or session contents are logged by this producer.

## Limits and verification

Canceled/error turns and slash commands create no new checkpoint proof. Their
ordinary local behavior remains separate. This does not persist all process
memory: native Session omits plan state, reasoning effort and some runtime-only
compaction state; restore must follow native resume semantics and restore Core
configuration separately. Existing missing/corrupt-session fallback on ordinary
reopen still requires strict handling in a future recovery coordinator.

Focused Linux fixtures exercise an actual pinned engine with an in-process
provider: successful turn with a recovered tool error, native file round-trip,
immutable preservation, wrong identity/accounting, symlink rejection, native
save failure without any broadcast receiver, and slash-command exclusion.
Run `cargo test -p aionui-ai-agent checkpoint` and the sink/init selectors before
enabling. Tests require Rust and the pinned dependencies; source edits alone are
not build or deployment verification.
