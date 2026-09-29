use super::*;
use crate::FrozenContinuationPreparation;
use aion_agent::session::Session;
use aionui_ai_agent::manager::aionrs::FrozenNativeSession;
use aionui_db::{FrozenConversationImport, FrozenNativeImportBinding, SqliteConversationRepository};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

const OWNER: &str = "system_default_user";

fn rows() -> FrozenConversationImport {
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

fn input(source: &Path, workspace: &Path) -> FrozenContinuationPreparation {
    let now = chrono::Utc::now();
    let session = Session {
        id: "source".into(), forked_from: None, root_id: None, created_at: now, updated_at: now,
        provider: "anthropic".into(), model: "frozen-model".into(), cwd: source.to_str().unwrap().into(),
        total_usage: Default::default(), context_state: Default::default(),
        messages: vec![serde_json::from_value(json!({"role":"assistant", "content":[{"type":"text", "text":"Selected turn"}], "turn_id":"turn-selected"})).unwrap()],
    };
    let session_json = serde_json::to_string(&session).unwrap();
    let session_sha256 = format!("{:x}", Sha256::digest(session_json.as_bytes()));
    let manifest_json = json!({"schema_version":1, "backend":"aionrs", "backend_version":"0.2.11",
        "conversation_id":"source", "session_id":"source", "turn_id":"turn-selected",
        "session_sha256":session_sha256, "session_bytes":session_json.len(), "created_at_ms":1, "local_only":true})
    .to_string();
    let native = FrozenNativeSession {
        owner_id: OWNER.into(),
        runtime_build_sha256: "a".repeat(64),
        manifest_json,
        session_json,
    };
    let mut rows = rows();
    rows.source.extra = json!({"workspace":source}).to_string();
    rows.target_workspace = workspace.to_str().unwrap().into();
    rows.native_binding = FrozenNativeImportBinding {
        owner_id: OWNER.into(),
        runtime_build_sha256: native.runtime_build_sha256.clone(),
        manifest_sha256: format!("{:x}", Sha256::digest(native.manifest_json.as_bytes())),
        session_sha256,
    };
    FrozenContinuationPreparation { rows, native }
}

fn service(
    workspace: &Path,
    repo: Arc<SqliteConversationRepository>,
    events: Arc<MockBroadcaster>,
) -> ConversationService {
    ConversationService::new(
        workspace.into(),
        events,
        Arc::new(FixedSkillResolver { names: vec![] }),
        Arc::new(MockTaskManager::new()),
        repo,
        Arc::new(StubAgentMetadataRepo),
        Arc::new(StubAcpSessionRepo::default()),
    )
}

#[tokio::test]
async fn frozen_continuation_prepares_real_native_and_sqlite_then_recovers_exact_result_read_only() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let source = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::set_permissions(workspace.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::create_dir(sessions.path().join("sessions")).unwrap();
    let events = Arc::new(MockBroadcaster::new());
    let svc = service(workspace.path(), repo.clone(), events.clone());
    let request = input(source.path(), workspace.path());
    // There is deliberately no mutable parent conversation or session to read.
    let prepared = svc
        .prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), request.clone())
        .await
        .unwrap();
    assert_eq!(prepared.imported.copied_messages.len(), 2);
    assert_eq!(prepared.imported.copied_artifacts.len(), 1);
    assert_eq!(prepared.native.source_turn_id, "turn-selected");
    let target = repo.get(OWNER, "continuation").await.unwrap().unwrap();
    let options = svc.build_task_options(&target).await.unwrap();
    assert!(
        matches!(options.context.kind, aionui_ai_agent::session_context::AgentSessionKind::Aionrs(ref context)
        if context.config.restored_from.is_some())
    );
    let mut malformed = target.clone();
    let mut extra: serde_json::Value = serde_json::from_str(&malformed.extra).unwrap();
    extra["system_prompt"] = json!(123);
    malformed.extra = extra.to_string();
    assert!(
        svc.build_task_options(&malformed).await.is_err(),
        "malformed restored extra must not fall back to an empty fresh-session configuration"
    );
    let state = sessions.path().join("sessions/continuation/state.json");
    let bytes = std::fs::read(&state).unwrap();
    let saved: Session = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved.id, "continuation");
    assert_eq!(saved.cwd, workspace.path().to_str().unwrap());
    assert_eq!(saved.forked_from.as_deref(), Some("source"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_outbox")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let restarted = service(workspace.path(), repo.clone(), events.clone());
    let recovered = restarted
        .prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), request.clone())
        .await
        .unwrap();
    assert_eq!(recovered.imported, prepared.imported);
    assert_eq!(recovered.native, prepared.native);
    assert_eq!(std::fs::read(&state).unwrap(), bytes);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_outbox")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        count
    );
    assert!(
        events.take_events().is_empty(),
        "preparation must not announce activation or start a turn"
    );
    let mut changed = request.clone();
    changed.native.session_json.push(' ');
    assert!(
        restarted
            .prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), changed)
            .await
            .is_err()
    );
    assert!(
        restarted
            .prepare_frozen_continuation("other", &"a".repeat(64), sessions.path(), request.clone())
            .await
            .is_err()
    );
    // Even valid alternate archive bytes must conflict with the committed binding.
    let mut changed = request.clone();
    changed.native.manifest_json.push(' ');
    changed.rows.native_binding.manifest_sha256 =
        format!("{:x}", Sha256::digest(changed.native.manifest_json.as_bytes()));
    assert!(
        restarted
            .prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), changed)
            .await
            .is_err()
    );
    std::fs::remove_file(&state).unwrap();
    assert!(
        restarted
            .prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), request)
            .await
            .is_err()
    );
    assert!(!state.exists(), "recovery must not fork again after native file loss");
}

#[tokio::test]
async fn frozen_continuation_rejects_dependencies_and_native_identity_before_any_install() {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let source = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::set_permissions(workspace.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::create_dir(sessions.path().join("sessions")).unwrap();
    let events = Arc::new(MockBroadcaster::new());
    let svc = service(workspace.path(), repo.clone(), events);
    let request = input(source.path(), workspace.path());
    assert!(
        svc.prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), request.clone())
            .await
            .is_err()
    );
    assert!(
        !sessions.path().join("continuations").exists(),
        "dependency checks must precede native writes"
    );
    seed(db.pool()).await;
    assert!(
        svc.prepare_frozen_continuation(OWNER, &"b".repeat(64), sessions.path(), request.clone())
            .await
            .is_err()
    );
    let mut changed = request.clone();
    let mut manifest: serde_json::Value = serde_json::from_str(&changed.native.manifest_json).unwrap();
    manifest["turn_id"] = json!("other-turn");
    changed.native.manifest_json = manifest.to_string();
    changed.rows.native_binding.manifest_sha256 =
        format!("{:x}", Sha256::digest(changed.native.manifest_json.as_bytes()));
    assert!(
        svc.prepare_frozen_continuation(OWNER, &"a".repeat(64), sessions.path(), changed)
            .await
            .is_err()
    );
    assert!(!sessions.path().join("continuations").exists());
    assert!(repo.get(OWNER, "continuation").await.unwrap().is_none());
}
