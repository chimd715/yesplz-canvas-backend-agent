-- Capture rows in the SAME transaction as every conversation-domain write,
-- including direct SQL from sidebar, teams and identity adoption.
-- subject_id deliberately is not named user_id/owner_user_id: the adoption
-- sweep must not rewrite immutable historical ownership.
-- No foreign keys: deletion must retain tombstones for external recovery.
CREATE TABLE IF NOT EXISTS conversation_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK (sequence <= 9007199254740991),
    subject_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    entity TEXT NOT NULL CHECK (entity IN ('conversation', 'message', 'artifact', 'assistant_snapshot')),
    entity_id TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
    payload TEXT CHECK (payload IS NULL OR json_valid(payload)),
    CHECK ((operation = 'delete' AND payload IS NULL) OR (operation = 'upsert' AND payload IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_conversation_outbox_subject
    ON conversation_outbox(subject_id, sequence);

-- ACK is a delivery watermark, never permission to remove recovery history.
-- ponytail: retain history until checkpoint-aware pruning is verified against external restore.
CREATE TABLE IF NOT EXISTS conversation_outbox_delivery (
    subject_id TEXT PRIMARY KEY NOT NULL,
    acknowledged_sequence INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged_sequence >= 0),
    exported_sequence INTEGER NOT NULL DEFAULT 0 CHECK (exported_sequence >= acknowledged_sequence)
);

-- Initial backfill is atomic with installing the triggers. Existing IDs,
-- ownership, JSON strings and hidden/archive state are preserved verbatim.

INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
SELECT r.user_id, r.id, 'conversation', r.id, 'upsert',
    json_object(
        'id', r.id,
        'user_id', r.user_id,
        'name', r.name,
        'type', r.type,
        'extra', r.extra,
        'model', r.model,
        'status', r.status,
        'source', r.source,
        'channel_chat_id', r.channel_chat_id,
        'pinned', r.pinned,
        'pinned_at', r.pinned_at,
        'created_at', r.created_at,
        'updated_at', r.updated_at,
        'project_id', r.project_id,
        'folder_id', r.folder_id,
        'name_source', r.name_source,
        'archived_at', r.archived_at
    )
FROM conversations r
ORDER BY r.created_at, r.id;

INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
SELECT c.user_id, r.conversation_id, 'message', r.id, 'upsert',
    json_object(
        'id', r.id,
        'conversation_id', r.conversation_id,
        'msg_id', r.msg_id,
        'type', r.type,
        'content', r.content,
        'position', r.position,
        'status', r.status,
        'hidden', r.hidden,
        'created_at', r.created_at,
        'backend_turn_id', r.backend_turn_id
    )
FROM messages r JOIN conversations c ON c.id = r.conversation_id
ORDER BY r.conversation_id, r.id;

INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
SELECT c.user_id, r.conversation_id, 'artifact', r.id, 'upsert',
    json_object(
        'id', r.id,
        'conversation_id', r.conversation_id,
        'cron_job_id', r.cron_job_id,
        'kind', r.kind,
        'status', r.status,
        'payload', r.payload,
        'created_at', r.created_at,
        'updated_at', r.updated_at
    )
FROM conversation_artifacts r JOIN conversations c ON c.id = r.conversation_id
ORDER BY r.conversation_id, r.id;

INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
SELECT c.user_id, r.conversation_id, 'assistant_snapshot', r.conversation_id, 'upsert',
    json_object(
        'conversation_id', r.conversation_id,
        'assistant_definition_id', r.assistant_definition_id,
        'assistant_id', r.assistant_id,
        'assistant_source', r.assistant_source,
        'agent_id', r.agent_id,
        'rules_content', r.rules_content,
        'default_model_mode', r.default_model_mode,
        'resolved_model_id', r.resolved_model_id,
        'default_permission_mode', r.default_permission_mode,
        'resolved_permission_value', r.resolved_permission_value,
        'default_skills_mode', r.default_skills_mode,
        'resolved_skill_ids', r.resolved_skill_ids,
        'resolved_disabled_builtin_skill_ids', r.resolved_disabled_builtin_skill_ids,
        'default_mcps_mode', r.default_mcps_mode,
        'resolved_mcp_ids', r.resolved_mcp_ids,
        'created_at', r.created_at,
        'updated_at', r.updated_at,
        'default_thought_level_mode', r.default_thought_level_mode,
        'resolved_thought_level_value', r.resolved_thought_level_value
    )
FROM conversation_assistant_snapshots r JOIN conversations c ON c.id = r.conversation_id
ORDER BY r.conversation_id;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversations_insert AFTER INSERT ON conversations
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    VALUES (NEW.user_id, NEW.id, 'conversation', NEW.id, 'upsert', json_object(
        'id', NEW.id,
        'user_id', NEW.user_id,
        'name', NEW.name,
        'type', NEW.type,
        'extra', NEW.extra,
        'model', NEW.model,
        'status', NEW.status,
        'source', NEW.source,
        'channel_chat_id', NEW.channel_chat_id,
        'pinned', NEW.pinned,
        'pinned_at', NEW.pinned_at,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at,
        'project_id', NEW.project_id,
        'folder_id', NEW.folder_id,
        'name_source', NEW.name_source,
        'archived_at', NEW.archived_at
    ));
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversations_update AFTER UPDATE ON conversations
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT OLD.user_id, OLD.id, 'conversation', OLD.id, 'delete', NULL
    WHERE OLD.user_id != NEW.user_id OR OLD.id != NEW.id;
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    VALUES (NEW.user_id, NEW.id, 'conversation', NEW.id, 'upsert', json_object(
        'id', NEW.id,
        'user_id', NEW.user_id,
        'name', NEW.name,
        'type', NEW.type,
        'extra', NEW.extra,
        'model', NEW.model,
        'status', NEW.status,
        'source', NEW.source,
        'channel_chat_id', NEW.channel_chat_id,
        'pinned', NEW.pinned,
        'pinned_at', NEW.pinned_at,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at,
        'project_id', NEW.project_id,
        'folder_id', NEW.folder_id,
        'name_source', NEW.name_source,
        'archived_at', NEW.archived_at
    ));

    -- An ownership adoption changes the parent without updating child rows.
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT NEW.user_id, NEW.id, 'message', r.id, 'upsert',
        json_object(
        'id', r.id,
        'conversation_id', r.conversation_id,
        'msg_id', r.msg_id,
        'type', r.type,
        'content', r.content,
        'position', r.position,
        'status', r.status,
        'hidden', r.hidden,
        'created_at', r.created_at,
        'backend_turn_id', r.backend_turn_id
    )
    FROM messages r
    WHERE r.conversation_id = NEW.id AND OLD.user_id != NEW.user_id;

    -- An ownership adoption changes the parent without updating child rows.
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT NEW.user_id, NEW.id, 'artifact', r.id, 'upsert',
        json_object(
        'id', r.id,
        'conversation_id', r.conversation_id,
        'cron_job_id', r.cron_job_id,
        'kind', r.kind,
        'status', r.status,
        'payload', r.payload,
        'created_at', r.created_at,
        'updated_at', r.updated_at
    )
    FROM conversation_artifacts r
    WHERE r.conversation_id = NEW.id AND OLD.user_id != NEW.user_id;

    -- An ownership adoption changes the parent without updating child rows.
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT NEW.user_id, NEW.id, 'assistant_snapshot', r.conversation_id, 'upsert',
        json_object(
        'conversation_id', r.conversation_id,
        'assistant_definition_id', r.assistant_definition_id,
        'assistant_id', r.assistant_id,
        'assistant_source', r.assistant_source,
        'agent_id', r.agent_id,
        'rules_content', r.rules_content,
        'default_model_mode', r.default_model_mode,
        'resolved_model_id', r.resolved_model_id,
        'default_permission_mode', r.default_permission_mode,
        'resolved_permission_value', r.resolved_permission_value,
        'default_skills_mode', r.default_skills_mode,
        'resolved_skill_ids', r.resolved_skill_ids,
        'resolved_disabled_builtin_skill_ids', r.resolved_disabled_builtin_skill_ids,
        'default_mcps_mode', r.default_mcps_mode,
        'resolved_mcp_ids', r.resolved_mcp_ids,
        'created_at', r.created_at,
        'updated_at', r.updated_at,
        'default_thought_level_mode', r.default_thought_level_mode,
        'resolved_thought_level_value', r.resolved_thought_level_value
    )
    FROM conversation_assistant_snapshots r
    WHERE r.conversation_id = NEW.id AND OLD.user_id != NEW.user_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversations_delete BEFORE DELETE ON conversations
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    VALUES (OLD.user_id, OLD.id, 'conversation', OLD.id, 'delete', NULL);
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_messages_insert AFTER INSERT ON messages
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'message', NEW.id, 'upsert',
        json_object(
        'id', NEW.id,
        'conversation_id', NEW.conversation_id,
        'msg_id', NEW.msg_id,
        'type', NEW.type,
        'content', NEW.content,
        'position', NEW.position,
        'status', NEW.status,
        'hidden', NEW.hidden,
        'created_at', NEW.created_at,
        'backend_turn_id', NEW.backend_turn_id
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_messages_update AFTER UPDATE ON messages
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'message', OLD.id, 'delete', NULL
    FROM conversations c WHERE c.id = OLD.conversation_id
      AND (OLD.conversation_id != NEW.conversation_id OR OLD.id != NEW.id);
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'message', NEW.id, 'upsert',
        json_object(
        'id', NEW.id,
        'conversation_id', NEW.conversation_id,
        'msg_id', NEW.msg_id,
        'type', NEW.type,
        'content', NEW.content,
        'position', NEW.position,
        'status', NEW.status,
        'hidden', NEW.hidden,
        'created_at', NEW.created_at,
        'backend_turn_id', NEW.backend_turn_id
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_messages_delete AFTER DELETE ON messages
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'message', OLD.id, 'delete',
        NULL
    FROM conversations c WHERE c.id = OLD.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_artifacts_insert AFTER INSERT ON conversation_artifacts
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'artifact', NEW.id, 'upsert',
        json_object(
        'id', NEW.id,
        'conversation_id', NEW.conversation_id,
        'cron_job_id', NEW.cron_job_id,
        'kind', NEW.kind,
        'status', NEW.status,
        'payload', NEW.payload,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_artifacts_update AFTER UPDATE ON conversation_artifacts
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'artifact', OLD.id, 'delete', NULL
    FROM conversations c WHERE c.id = OLD.conversation_id
      AND (OLD.conversation_id != NEW.conversation_id OR OLD.id != NEW.id);
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'artifact', NEW.id, 'upsert',
        json_object(
        'id', NEW.id,
        'conversation_id', NEW.conversation_id,
        'cron_job_id', NEW.cron_job_id,
        'kind', NEW.kind,
        'status', NEW.status,
        'payload', NEW.payload,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_artifacts_delete AFTER DELETE ON conversation_artifacts
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'artifact', OLD.id, 'delete',
        NULL
    FROM conversations c WHERE c.id = OLD.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_assistant_snapshots_insert AFTER INSERT ON conversation_assistant_snapshots
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'assistant_snapshot', NEW.conversation_id, 'upsert',
        json_object(
        'conversation_id', NEW.conversation_id,
        'assistant_definition_id', NEW.assistant_definition_id,
        'assistant_id', NEW.assistant_id,
        'assistant_source', NEW.assistant_source,
        'agent_id', NEW.agent_id,
        'rules_content', NEW.rules_content,
        'default_model_mode', NEW.default_model_mode,
        'resolved_model_id', NEW.resolved_model_id,
        'default_permission_mode', NEW.default_permission_mode,
        'resolved_permission_value', NEW.resolved_permission_value,
        'default_skills_mode', NEW.default_skills_mode,
        'resolved_skill_ids', NEW.resolved_skill_ids,
        'resolved_disabled_builtin_skill_ids', NEW.resolved_disabled_builtin_skill_ids,
        'default_mcps_mode', NEW.default_mcps_mode,
        'resolved_mcp_ids', NEW.resolved_mcp_ids,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at,
        'default_thought_level_mode', NEW.default_thought_level_mode,
        'resolved_thought_level_value', NEW.resolved_thought_level_value
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_assistant_snapshots_update AFTER UPDATE ON conversation_assistant_snapshots
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'assistant_snapshot', OLD.conversation_id, 'delete', NULL
    FROM conversations c WHERE c.id = OLD.conversation_id
      AND OLD.conversation_id != NEW.conversation_id;
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, NEW.conversation_id, 'assistant_snapshot', NEW.conversation_id, 'upsert',
        json_object(
        'conversation_id', NEW.conversation_id,
        'assistant_definition_id', NEW.assistant_definition_id,
        'assistant_id', NEW.assistant_id,
        'assistant_source', NEW.assistant_source,
        'agent_id', NEW.agent_id,
        'rules_content', NEW.rules_content,
        'default_model_mode', NEW.default_model_mode,
        'resolved_model_id', NEW.resolved_model_id,
        'default_permission_mode', NEW.default_permission_mode,
        'resolved_permission_value', NEW.resolved_permission_value,
        'default_skills_mode', NEW.default_skills_mode,
        'resolved_skill_ids', NEW.resolved_skill_ids,
        'resolved_disabled_builtin_skill_ids', NEW.resolved_disabled_builtin_skill_ids,
        'default_mcps_mode', NEW.default_mcps_mode,
        'resolved_mcp_ids', NEW.resolved_mcp_ids,
        'created_at', NEW.created_at,
        'updated_at', NEW.updated_at,
        'default_thought_level_mode', NEW.default_thought_level_mode,
        'resolved_thought_level_value', NEW.resolved_thought_level_value
    )
    FROM conversations c WHERE c.id = NEW.conversation_id;
END;

CREATE TRIGGER IF NOT EXISTS conversation_outbox_conversation_assistant_snapshots_delete AFTER DELETE ON conversation_assistant_snapshots
BEGIN
    INSERT INTO conversation_outbox (subject_id, conversation_id, entity, entity_id, operation, payload)
    SELECT c.user_id, OLD.conversation_id, 'assistant_snapshot', OLD.conversation_id, 'delete',
        NULL
    FROM conversations c WHERE c.id = OLD.conversation_id;
END;
