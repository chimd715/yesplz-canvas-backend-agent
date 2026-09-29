use aionui_db::{
    ConversationArtifactRow, ConversationAssistantSnapshotRow, DbError, FrozenConversationImport,
    FrozenNativeImportBinding, IConversationRepository, SqliteConversationRepository, init_database_memory,
    models::{ConversationRow, MessageRow},
};

const OWNER: &str = "system_default_user";

fn frozen() -> FrozenConversationImport {
    FrozenConversationImport {
        native_binding: FrozenNativeImportBinding {
            owner_id: OWNER.into(),
            runtime_build_sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
            session_sha256: "c".repeat(64),
        },
        source: ConversationRow {
            id: "source".into(),
            user_id: OWNER.into(),
            name: "Frozen title".into(),
            r#type: "aionrs".into(),
            extra: serde_json::json!({"workspace":"/work/original", "fork":{"parent_conversation_id":"older"},
                "cronJobId":"old-job", "channel_chat_id":"old-channel", "custom_setting":true})
            .to_string(),
            model: Some(r#"{"providerId":"owned-provider","model":"frozen-model"}"#.into()),
            status: Some("finished".into()),
            source: Some("telegram".into()),
            channel_chat_id: Some("chat".into()),
            pinned: true,
            pinned_at: Some(1),
            created_at: 1,
            updated_at: 10,
            project_id: Some("old-project".into()),
            folder_id: Some("old-folder".into()),
            name_source: Some("user".into()),
        },
        assistant: ConversationAssistantSnapshotRow {
            conversation_id: "source".into(),
            assistant_definition_id: "frozen-definition".into(),
            assistant_id: "frozen-assistant".into(),
            assistant_source: "user".into(),
            agent_id: "frozen-agent".into(),
            rules_content: "Frozen rules".into(),
            default_model_mode: "fixed".into(),
            resolved_model_id: Some("frozen-model".into()),
            default_permission_mode: "fixed".into(),
            resolved_permission_value: Some("default".into()),
            default_thought_level_mode: "fixed".into(),
            resolved_thought_level_value: Some("high".into()),
            default_skills_mode: "fixed".into(),
            resolved_skill_ids: r#"["frozen-skill"]"#.into(),
            resolved_disabled_builtin_skill_ids: r#"["disabled-skill"]"#.into(),
            default_mcps_mode: "fixed".into(),
            resolved_mcp_ids: r#"["frozen-mcp"]"#.into(),
            created_at: 1,
            updated_at: 2,
        },
        messages: vec![
            message("m1", "Earlier turn", 5, "turn-earlier"),
            message("m2", "Selected turn", 5, "turn-selected"),
        ],
        artifacts: vec![ConversationArtifactRow {
            id: "artifact".into(),
            conversation_id: "source".into(),
            cron_job_id: Some("old-job".into()),
            kind: "skill_suggest".into(),
            status: "pending".into(),
            payload: r#"{"name":"Frozen artifact"}"#.into(),
            created_at: 5,
            updated_at: 6,
        }],
        target_id: "continuation".into(),
        target_workspace: "/work/continuation".into(),
        original_turn_id: "turn-selected".into(),
        boundary_message_id: "m2".into(),
    }
}

fn message(id: &str, text: &str, created_at: i64, turn: &str) -> MessageRow {
    MessageRow {
        id: id.into(),
        conversation_id: "source".into(),
        msg_id: Some(format!("stream-{id}")),
        r#type: "text".into(),
        content: serde_json::json!({"content":text}).to_string(),
        position: Some("left".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at,
        backend_turn_id: Some(turn.into()),
    }
}

async fn seed(pool: &aionui_db::SqlitePool) {
    sqlx::query("INSERT INTO agent_metadata (id, agent_id, user_id, name, agent_type, agent_source, created_at, updated_at) VALUES ('frozen-agent-row', 'frozen-agent', ?, 'Fixture', 'aionrs', 'user', 1, 1)")
        .bind(OWNER).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO assistant_definitions (id, user_id, assistant_id, source, owner_type, name, agent_id, rule_resource_type, default_model_mode, default_permission_mode, default_skills_mode, default_mcps_mode, created_at, updated_at) VALUES ('frozen-definition', ?, 'frozen-assistant', 'user', 'user', 'Current definition', 'frozen-agent', 'none', 'auto', 'auto', 'auto', 'auto', 1, 1)")
        .bind(OWNER).execute(pool).await.unwrap();
}

async fn counts(pool: &aionui_db::SqlitePool) -> (i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM conversations), (SELECT count(*) FROM conversation_assistant_snapshots), (SELECT count(*) FROM messages), (SELECT count(*) FROM conversation_artifacts), (SELECT count(*) FROM conversation_outbox)")
        .fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn frozen_import_preserves_snapshot_and_original_anchors_without_reading_live_parent() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    let params = frozen();
    let mut live = params.source.clone();
    live.name = "Changed after checkpoint".into();
    live.project_id = None;
    live.folder_id = None;
    live.extra = r#"{"workspace":"/other/current-workspace"}"#.into();
    repo.create(&live).await.unwrap();
    let later = message("m3", "Preserve later history", 20, "turn-later");
    repo.insert_message(OWNER, &later).await.unwrap();
    let before = counts(db.pool()).await;
    repo.validate_frozen_continuation(OWNER, &params).await.unwrap();
    assert_eq!(counts(db.pool()).await, before);

    let imported = repo.import_frozen_continuation(OWNER, &params).await.unwrap();
    let target = repo.get(OWNER, &imported.conversation_id).await.unwrap().unwrap();
    assert_eq!(target.name, "Frozen title");
    assert_eq!(target.model, params.source.model);
    assert_eq!(target.name_source.as_deref(), Some("user"));
    assert!(target.channel_chat_id.is_none() && target.project_id.is_none() && target.folder_id.is_none());
    assert!(!target.pinned);
    let extra: serde_json::Value = serde_json::from_str(&target.extra).unwrap();
    assert_eq!(extra["workspace"], "/work/continuation");
    assert_eq!(extra["custom_setting"], true);
    assert_eq!(
        extra["restoredFrom"],
        serde_json::json!({"conversationId":"source", "messageId":"m2", "turnId":"turn-selected"})
    );
    for key in ["fork", "cronJobId", "channel_chat_id"] {
        assert!(extra.get(key).is_none());
    }
    let snapshot = repo.get_assistant_snapshot(OWNER, &target.id).await.unwrap().unwrap();
    let mut expected = params.assistant.clone();
    expected.conversation_id = target.id.clone();
    assert_eq!(
        serde_json::to_value(snapshot).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let messages: Vec<MessageRow> =
        sqlx::query_as("SELECT * FROM messages WHERE conversation_id = ? ORDER BY created_at, id")
            .bind(&target.id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(messages.len(), 2);
    for ((actual, original), mapping) in messages.iter().zip(&params.messages).zip(&imported.copied_messages) {
        assert_ne!(actual.id, original.id);
        assert_eq!(actual.id, mapping.message_id);
        assert_eq!(mapping.source_message_id, original.id);
        assert_eq!(mapping.original_turn_id, original.backend_turn_id);
        assert_eq!(actual.content, original.content);
        assert_eq!(actual.msg_id, original.msg_id);
        assert_eq!(actual.created_at, original.created_at);
        assert!(actual.backend_turn_id.is_none());
    }
    let artifacts: Vec<ConversationArtifactRow> =
        sqlx::query_as("SELECT * FROM conversation_artifacts WHERE conversation_id = ?")
            .bind(&target.id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(artifacts[0].payload, params.artifacts[0].payload);
    assert_eq!(artifacts[0].id, imported.copied_artifacts[0].artifact_id);
    assert_eq!(imported.copied_artifacts[0].source_artifact_id, "artifact");
    assert!(artifacts[0].cron_job_id.is_none());
    assert_eq!(
        repo.get(OWNER, "source").await.unwrap().unwrap().name,
        "Changed after checkpoint"
    );
    assert_eq!(repo.get_message(OWNER, "source", "m3").await.unwrap(), Some(later));
    assert!(repo.get("other", &target.id).await.unwrap().is_none());
    let committed = counts(db.pool()).await;
    // A new repository instance can recover an exact lost response, read-only.
    let restarted = SqliteConversationRepository::new(db.pool().clone());
    assert_eq!(
        restarted.get_frozen_continuation(OWNER, &params).await.unwrap(),
        Some(imported.clone())
    );
    let mut changed_native = params.clone();
    changed_native.native_binding.session_sha256 = "d".repeat(64);
    assert!(
        matches!(restarted.get_frozen_continuation(OWNER, &changed_native).await,
        Err(DbError::Conflict(reason)) if reason == "Frozen continuation request does not match committed import")
    );
    let mut changed = params.clone();
    changed.messages[0].content = r#"{"content":"Changed input"}"#.into();
    assert!(matches!(restarted.get_frozen_continuation(OWNER, &changed).await,
        Err(DbError::Conflict(reason)) if reason == "Frozen continuation request does not match committed import"));
    let mut foreign = params.clone();
    foreign.source.user_id = "other".into();
    foreign.native_binding.owner_id = "other".into();
    assert!(
        restarted
            .get_frozen_continuation("other", &foreign)
            .await
            .unwrap()
            .is_none()
    );
    let immutable =
        sqlx::query("UPDATE frozen_continuation_imports SET result_json = '{}' WHERE conversation_id = 'continuation'")
            .execute(db.pool())
            .await
            .unwrap_err();
    assert!(
        immutable
            .to_string()
            .contains("Frozen continuation import receipts are immutable")
    );

    assert!(matches!(repo.import_frozen_continuation(OWNER, &params).await,
        Err(DbError::Conflict(reason)) if reason == "Frozen continuation destination already exists"));
    assert_eq!(counts(db.pool()).await, committed);
}

#[tokio::test]
async fn malformed_frozen_inputs_reject_before_writes() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    type Mutation = fn(&mut FrozenConversationImport);
    let mutations: Vec<(Mutation, &str)> = vec![
        (
            |p| p.native_binding.owner_id = "other".into(),
            "Database initialization failed: Invalid frozen continuation: native archive binding does not match",
        ),
        (
            |p| p.native_binding.manifest_sha256 = "F".repeat(64),
            "Database initialization failed: Invalid frozen continuation: native archive binding does not match",
        ),
        (
            |p| p.source.user_id = "other".into(),
            "Record not found: Frozen conversation owner does not match",
        ),
        (
            |p| p.source.extra = "[]".into(),
            "Database initialization failed: Invalid frozen continuation: expected JSON object",
        ),
        (
            |p| p.source.extra = "{".into(),
            "Database initialization failed: Invalid frozen continuation: malformed JSON",
        ),
        (
            |p| p.source.extra = r#"{"teamId":"private-team"}"#.into(),
            "Database initialization failed: Invalid frozen continuation: team conversations are unsupported",
        ),
        (
            |p| p.assistant.conversation_id = "other".into(),
            "Database initialization failed: Invalid frozen continuation: assistant snapshot does not match",
        ),
        (
            |p| p.messages[0].conversation_id = "other".into(),
            "Database initialization failed: Invalid frozen continuation: invalid or duplicate frozen message",
        ),
        (
            |p| p.messages[0].id = p.messages[1].id.clone(),
            "Database initialization failed: Invalid frozen continuation: invalid or duplicate frozen message",
        ),
        (
            |p| p.messages.swap(0, 1),
            "Database initialization failed: Invalid frozen continuation: messages must be in strict timeline order",
        ),
        (
            |p| p.original_turn_id = "wrong-turn".into(),
            "Database initialization failed: Invalid frozen continuation: turn boundary does not match",
        ),
        (
            |p| p.artifacts.push(p.artifacts[0].clone()),
            "Database initialization failed: Invalid frozen continuation: invalid or duplicate frozen artifact",
        ),
        (
            |p| p.target_workspace = "/work/../original".into(),
            "Database initialization failed: Invalid frozen continuation: target workspace must be a new absolute path",
        ),
    ];
    let before = counts(db.pool()).await;
    for (change, expected) in mutations {
        let mut params = frozen();
        change(&mut params);
        assert_eq!(params.validate(OWNER).unwrap_err().to_string(), expected);
        assert_eq!(
            repo.import_frozen_continuation(OWNER, &params)
                .await
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(counts(db.pool()).await, before);
    }
    let mut oversized = frozen();
    oversized.assistant.rules_content = "x".repeat(16 * 1024 * 1024);
    assert_eq!(
        oversized.validate(OWNER).unwrap_err().to_string(),
        "Database initialization failed: Invalid frozen continuation: frozen record exceeds export limit"
    );
    assert!(matches!(repo.import_frozen_continuation(OWNER, &oversized).await,
        Err(DbError::Init(reason)) if reason == "Invalid frozen continuation: frozen record exceeds export limit"));
    assert_eq!(counts(db.pool()).await, before);
    let mut encoded = serde_json::to_value(frozen()).unwrap();
    encoded["unexpected"] = true.into();
    assert!(
        serde_json::from_value::<FrozenConversationImport>(encoded)
            .unwrap_err()
            .to_string()
            .contains("unknown field `unexpected`")
    );
}

#[tokio::test]
async fn destination_dependency_ownership_and_preflight_races_are_checked_atomically() {
    let db = init_database_memory().await.unwrap();
    let repo = SqliteConversationRepository::new(db.pool().clone());
    let params = frozen();
    assert!(matches!(repo.validate_frozen_continuation(OWNER, &params).await,
        Err(DbError::NotFound(reason)) if reason == "Frozen continuation dependencies unavailable"));
    seed(db.pool()).await;
    sqlx::query("INSERT INTO users (id, password_hash, created_at, updated_at) VALUES ('other', 'fixture', 1, 1)")
        .execute(db.pool())
        .await
        .unwrap();
    for table in ["assistant_definitions", "agent_metadata"] {
        sqlx::query(&format!("UPDATE {table} SET user_id = 'other' WHERE user_id = ?"))
            .bind(OWNER)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(matches!(repo.validate_frozen_continuation(OWNER, &params).await,
            Err(DbError::NotFound(reason)) if reason == "Frozen continuation dependencies unavailable"));
        assert!(matches!(repo.import_frozen_continuation(OWNER, &params).await,
            Err(DbError::NotFound(reason)) if reason == "Frozen continuation dependencies unavailable"));
        assert_eq!(counts(db.pool()).await, (0, 0, 0, 0, 0));
        sqlx::query(&format!("UPDATE {table} SET user_id = ? WHERE user_id = 'other'"))
            .bind(OWNER)
            .execute(db.pool())
            .await
            .unwrap();
    }
    repo.validate_frozen_continuation(OWNER, &params).await.unwrap();
    sqlx::query("INSERT INTO conversations (id, user_id, name, type, created_at, updated_at) VALUES ('continuation', 'other', 'Foreign destination', 'aionrs', 1, 1)")
        .execute(db.pool()).await.unwrap();
    let before = counts(db.pool()).await;
    assert!(matches!(repo.import_frozen_continuation(OWNER, &params).await,
        Err(DbError::Conflict(reason)) if reason == "Frozen continuation destination already exists"));
    assert_eq!(counts(db.pool()).await, before);
}

#[tokio::test]
async fn late_import_failure_rolls_back_every_row_and_outbox_event() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    sqlx::query("CREATE TRIGGER reject_frozen_artifact BEFORE INSERT ON conversation_artifacts BEGIN SELECT RAISE(ABORT, 'fixture rejects frozen artifact'); END")
        .execute(db.pool()).await.unwrap();
    let error = repo.import_frozen_continuation(OWNER, &frozen()).await.unwrap_err();
    assert!(
        matches!(error, DbError::Query(ref error) if error.to_string().contains("fixture rejects frozen artifact"))
    );
    assert_eq!(counts(db.pool()).await, (0, 0, 0, 0, 0));
    sqlx::query("DROP TRIGGER reject_frozen_artifact")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_import_receipt BEFORE INSERT ON frozen_continuation_imports BEGIN SELECT RAISE(ABORT, 'fixture rejects import receipt'); END")
        .execute(db.pool()).await.unwrap();
    assert!(matches!(repo.import_frozen_continuation(OWNER, &frozen()).await,
        Err(DbError::Query(ref error)) if error.to_string().contains("fixture rejects import receipt")));
    assert_eq!(counts(db.pool()).await, (0, 0, 0, 0, 0));
    assert!(repo.get_frozen_continuation(OWNER, &frozen()).await.unwrap().is_none());
    sqlx::query("DROP TRIGGER reject_import_receipt")
        .execute(db.pool())
        .await
        .unwrap();
    // Retry is safe because the failed transaction left no destination at all.
    let params = frozen();
    let (first, second) = tokio::join!(
        repo.import_frozen_continuation(OWNER, &params),
        repo.import_frozen_continuation(OWNER, &params)
    );
    let (success, failure) = if first.is_ok() {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(success.unwrap().copied_messages.len(), 2);
    assert!(
        matches!(failure, Err(DbError::Conflict(reason)) if reason == "Frozen continuation destination already exists")
    );
    assert_eq!(counts(db.pool()).await, (1, 1, 2, 1, 5));
}
