#!/usr/bin/env python3
"""Exercise the shipped SQLite migration without a Rust toolchain."""

import json
import pathlib
import sqlite3


ROOT = pathlib.Path(__file__).resolve().parents[2]
MIGRATIONS = ROOT / "crates/aionui-db/migrations"
TABLES = {
    "conversation": "conversations",
    "message": "messages",
    "artifact": "conversation_artifacts",
    "assistant_snapshot": "conversation_assistant_snapshots",
}


def insert(db, table, **values):
    columns = ", ".join(values)
    placeholders = ", ".join("?" for _ in values)
    db.execute(f"INSERT INTO {table} ({columns}) VALUES ({placeholders})", tuple(values.values()))


def events(db):
    return [dict(row) for row in db.execute("SELECT * FROM conversation_outbox ORDER BY sequence")]


def main():
    db = sqlite3.connect(":memory:", isolation_level=None)
    db.row_factory = sqlite3.Row
    # Match database.rs::run_migrations_staged, including transactional PRAGMAs.
    db.execute("PRAGMA foreign_keys = OFF")
    db.execute("PRAGMA legacy_alter_table = ON")
    for migration in sorted(MIGRATIONS.glob("*.sql")):
        if migration.name >= "044":
            break
        db.executescript("BEGIN;\n" + migration.read_text() + "\nCOMMIT;")
    db.execute("PRAGMA foreign_keys = ON")
    db.execute("PRAGMA legacy_alter_table = OFF")

    for user in ("owner", "next_owner"):
        insert(db, "users", id=user, username=user, password_hash="fixture", created_at=1, updated_at=1)
    insert(db, "assistant_definitions", id="definition", assistant_id="fixture", source="builtin",
           owner_type="system", name="Fixture", agent_id="fixture", rule_resource_type="none",
           default_model_mode="auto", default_permission_mode="auto", default_skills_mode="auto",
           default_mcps_mode="auto", created_at=1, updated_at=1)
    insert(db, "conversations", id="conversation", user_id="owner", name="Fixture", type="acp",
           extra='{"workspace":"/fixture"}', created_at=1, updated_at=1)
    insert(db, "messages", id="message", conversation_id="conversation", type="text",
           content='{"text":"existing"}', position="left", created_at=1)
    insert(db, "conversation_artifacts", id="artifact", conversation_id="conversation", kind="skill_suggest",
           status="active", payload='{"path":"fixture"}', created_at=1, updated_at=1)
    insert(db, "conversation_assistant_snapshots", conversation_id="conversation", assistant_definition_id="definition",
           assistant_id="fixture", assistant_source="builtin", agent_id="fixture", default_model_mode="auto",
           default_permission_mode="auto", default_skills_mode="auto", default_mcps_mode="auto", created_at=1, updated_at=1)

    migration = (MIGRATIONS / "044_conversation_outbox.sql").read_text()
    db.executescript("BEGIN;\n" + migration + "\nCOMMIT;")
    initial = events(db)
    assert len(initial) == 4, initial
    for event in initial:
        # Compare all live columns, not a hand-picked subset. Future schema
        # changes must update the snapshot contract deliberately.
        expected = dict(db.execute(f"SELECT * FROM {TABLES[event['entity']]}").fetchone())
        assert json.loads(event["payload"]) == expected, event["entity"]
    assert not db.execute("PRAGMA foreign_key_list(conversation_outbox)").fetchall()

    # Direct writes bypassing repositories (sidebar/archive and message state).
    updates = [
        ("conversation", "UPDATE conversations SET name='Renamed', archived_at=2"),
        ("message", "UPDATE messages SET hidden=1, content='{\"text\":\"final\"}'"),
        ("artifact", "UPDATE conversation_artifacts SET payload='{\"path\":\"changed\"}'"),
        ("assistant_snapshot", "UPDATE conversation_assistant_snapshots SET rules_content='Changed'"),
    ]
    for entity, sql in updates:
        before = len(events(db))
        db.execute(sql)
        after = events(db)
        assert len(after) == before + 1
        assert after[-1]["entity"] == entity and after[-1]["operation"] == "upsert"
        expected = dict(db.execute(f"SELECT * FROM {TABLES[entity]}").fetchone())
        assert json.loads(after[-1]["payload"]) == expected

    # Both explicit rollback and capture failure must preserve source + history.
    before = events(db)
    db.execute("BEGIN")
    db.execute("UPDATE conversations SET name='rolled back'")
    db.execute("ROLLBACK")
    assert events(db) == before
    assert db.execute("SELECT name FROM conversations").fetchone()[0] == "Renamed"
    db.execute("CREATE TRIGGER fail_capture BEFORE INSERT ON conversation_outbox BEGIN SELECT RAISE(ABORT, 'fixture'); END")
    try:
        db.execute("UPDATE conversations SET name='lost capture'")
        raise AssertionError("source write succeeded after capture failure")
    except sqlite3.IntegrityError:
        pass
    db.execute("DROP TRIGGER fail_capture")
    assert events(db) == before
    assert db.execute("SELECT name FROM conversations").fetchone()[0] == "Renamed"

    # Identity adoption preserves old history, emits an old-owner tombstone,
    # then publishes the complete parent/child state for the new owner.
    db.execute("UPDATE conversations SET user_id='next_owner'")
    after = events(db)
    assert after[:len(before)] == before
    adopted = after[len(before):]
    assert len(adopted) == 5
    assert (adopted[0]["subject_id"], adopted[0]["operation"]) == ("owner", "delete")
    assert adopted[0]["payload"] is None
    assert adopted[1]["entity"] == "conversation"
    assert all(e["subject_id"] == "next_owner" and e["operation"] == "upsert" for e in adopted[1:])
    assert {e["entity"] for e in adopted[1:]} == set(TABLES)

    # Moving a child across owners removes the old projection before exporting
    # its new parent-scoped state. No change is attributed to the wrong user.
    insert(db, "conversations", id="other_conversation", user_id="owner", name="Other", type="acp",
           created_at=1, updated_at=1)
    db.execute("UPDATE messages SET conversation_id='other_conversation'")
    moved = events(db)[-2:]
    assert [(e["subject_id"], e["operation"]) for e in moved] == [("next_owner", "delete"), ("owner", "upsert")]
    assert json.loads(moved[1]["payload"])["conversation_id"] == "other_conversation"
    db.execute("UPDATE messages SET conversation_id='conversation'")
    db.execute("DELETE FROM conversations WHERE id='other_conversation'")

    # Direct child deletes produce scoped tombstones; reinserts are captured.
    for entity in ("message", "artifact", "assistant_snapshot"):
        table = TABLES[entity]
        row = dict(db.execute(f"SELECT * FROM {table}").fetchone())
        db.execute(f"DELETE FROM {table}")
        last = events(db)[-1]
        assert (last["entity"], last["subject_id"], last["operation"], last["payload"]) == (
            entity, "next_owner", "delete", None)
        insert(db, table, **row)
        assert events(db)[-1]["operation"] == "upsert"

    # A parent tombstone covers FK-cascaded children even after the parent row
    # is unavailable to a child delete trigger.
    before_delete = len(events(db))
    db.execute("DELETE FROM conversations")
    deleted = events(db)[before_delete:]
    assert any(e["entity"] == "conversation" and e["operation"] == "delete" for e in deleted)
    for table in TABLES.values():
        assert db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] == 0
    db.execute("DELETE FROM users WHERE id IN ('owner', 'next_owner')")
    assert len(events(db)) >= before_delete + 1
    sequences = [e["sequence"] for e in events(db)]
    assert sequences == sorted(set(sequences))
    assert db.execute("PRAGMA foreign_key_check").fetchall() == []
    print("PASS: backfill/all columns, direct writes, rollback/failure atomicity, adoption, tombstones and retention")


if __name__ == "__main__":
    main()
