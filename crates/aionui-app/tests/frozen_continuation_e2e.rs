//! Real router, current session/CSRF, native Session and SQLite; no LLM/network calls.
#![cfg(target_os = "linux")]
mod common;

use aion_agent::session::Session;
use aionui_ai_agent::{RuntimeTokenScope, TEAM_RUNTIME_TOKEN_SESSION_GENERATION, manager::aionrs::FrozenNativeSession};
use aionui_app::{AppConfig, AppServices, IdentityMode, build_module_states, create_router_with_states};
use aionui_conversation::FrozenContinuationPreparation;
use aionui_db::{
    ConversationArtifactRow, ConversationAssistantSnapshotRow, FrozenConversationImport, FrozenNativeImportBinding,
    init_database_memory,
    models::{ConversationRow, MessageRow},
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::{body_json, json_with_token, setup_and_login};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

const PREPARE: &str = "/api/conversations/internal/frozen-continuations/prepare";
const RECOVER: &str = "/api/conversations/internal/frozen-continuations/recover";
const TURN_FREEZE: &str = "/api/conversations/internal/backup/turn-freeze";
const BOOTSTRAP: &str = "fixture-bootstrap-credential";

async fn fixture() -> (Router, AppServices, tempfile::TempDir, String, String) {
    let directory = tempfile::tempdir().unwrap();
    let config = AppConfig {
        data_dir: directory.path().join("data"),
        work_dir: directory.path().join("work"),
        bootstrap_secret: Some(BOOTSTRAP.into()),
        ..Default::default()
    };
    let services = AppServices::from_config(init_database_memory().await.unwrap(), &config)
        .await
        .unwrap();
    let (states, _) = build_module_states(&services).await.unwrap();
    let mut app = create_router_with_states(&services, states);
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    (app, services, directory, token, csrf)
}

fn request(path: &str, body: Value, token: &str, csrf: &str) -> Request<Body> {
    let mut request = json_with_token("POST", path, body, token, csrf);
    request
        .headers_mut()
        .insert("x-aioncore-bootstrap-secret", BOOTSTRAP.parse().unwrap());
    request
}

async fn assert_status(app: &Router, request: Request<Body>, status: StatusCode) -> Value {
    let response = app.clone().oneshot(request).await.unwrap();
    let actual = response.status();
    let body = body_json(response).await;
    assert_eq!(actual, status, "{body}");
    body
}

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

fn input(source: &Path, workspace: &Path, build: &str) -> FrozenContinuationPreparation {
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
        runtime_build_sha256: build.into(),
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

#[tokio::test]
async fn frozen_continuation_control_requires_bootstrap_session_and_csrf_before_body() {
    let (app, services, _directory, token, csrf) = fixture().await;
    for path in [PREPARE, RECOVER] {
        let mut no_bootstrap = request(path, json!({}), &token, &csrf);
        no_bootstrap.headers_mut().remove("x-aioncore-bootstrap-secret");
        *no_bootstrap.body_mut() = Body::from("not JSON");
        let rejected = assert_status(&app, no_bootstrap, StatusCode::UNAUTHORIZED).await;
        assert_eq!(rejected["code"], "BOOTSTRAP_SECRET_REQUIRED");
        let mut wrong_bootstrap = request(path, json!({}), &token, &csrf);
        wrong_bootstrap
            .headers_mut()
            .insert("x-aioncore-bootstrap-secret", "wrong".parse().unwrap());
        let rejected = assert_status(&app, wrong_bootstrap, StatusCode::UNAUTHORIZED).await;
        assert_eq!(rejected["code"], "INVALID_BOOTSTRAP_SECRET");

        let polled = Arc::new(AtomicUsize::new(0));
        let body_polled = polled.clone();
        let mut anonymous = request(path, json!({}), &token, &csrf);
        anonymous.headers_mut().remove("authorization");
        *anonymous.body_mut() = Body::from_stream(futures_util::stream::once(async move {
            body_polled.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"private body must not be read"))
        }));
        assert_status(&app, anonymous, StatusCode::UNAUTHORIZED).await;
        assert_eq!(polled.load(Ordering::SeqCst), 0);

        let mut no_csrf = request(path, json!({}), &token, &csrf);
        no_csrf.headers_mut().remove("x-csrf-token");
        assert_status(&app, no_csrf, StatusCode::FORBIDDEN).await;
        // This is an actually issued helper credential accepted by ordinary
        // Core routes. It has no authority to import archived conversations.
        let helper = services.runtime_token_service.issue(
            OWNER,
            "source",
            TEAM_RUNTIME_TOKEN_SESSION_GENERATION,
            [RuntimeTokenScope::ConversationHelper],
        );
        let mut runtime = request(path, json!({}), &token, &csrf);
        runtime
            .headers_mut()
            .insert("x-aionui-runtime-token", helper.token.parse().unwrap());
        runtime.headers_mut().insert("x-aionui-user-id", OWNER.parse().unwrap());
        runtime
            .headers_mut()
            .insert("x-aionui-conversation-id", "source".parse().unwrap());
        runtime.headers_mut().remove("authorization");
        runtime.headers_mut().remove("x-csrf-token");
        assert_status(&app, runtime, StatusCode::UNAUTHORIZED).await;
        let mut mixed = request(path, json!({}), &token, &csrf);
        mixed
            .headers_mut()
            .insert("x-aionui-runtime-token", helper.token.parse().unwrap());
        mixed.headers_mut().remove("x-csrf-token");
        assert_status(&app, mixed, StatusCode::FORBIDDEN).await;

        assert_status(
            &app,
            request(path, json!({"extra":"unknown"}), &token, &csrf),
            StatusCode::BAD_REQUEST,
        )
        .await;
        // The internal archive route must override the application's 10 MiB
        // default. Reject the schema, not the size, without preparing any data.
        let oversized_default = json!({"extra": "x".repeat(10 * 1024 * 1024 + 1)});
        let rejected = assert_status(
            &app,
            request(path, oversized_default, &token, &csrf),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(rejected["code"], "BAD_REQUEST");
        let mut too_large = request(path, json!({}), &token, &csrf);
        too_large
            .headers_mut()
            .insert("content-length", (128 * 1024 * 1024 + 1).to_string().parse().unwrap());
        assert_status(&app, too_large, StatusCode::PAYLOAD_TOO_LARGE).await;
    }
    services.user_repo.increment_session_generation(OWNER).await.unwrap();
    assert_status(
        &app,
        request(PREPARE, json!({}), &token, &csrf),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert!(!services.data_dir.join("aionrs-sessions/sessions/continuation").exists());
}

#[tokio::test]
async fn frozen_continuation_control_prepares_and_recovers_exact_native_receipt_without_activation() {
    let (app, services, directory, token, csrf) = fixture().await;
    seed(services.database.pool()).await;
    let source = directory.path().join("source");
    let restored = directory.path().join("restored");
    std::fs::create_dir(&restored).unwrap();
    std::fs::set_permissions(&restored, std::fs::Permissions::from_mode(0o700)).unwrap();
    let build = services.frozen_continuation_build_sha256.as_deref().unwrap();
    assert_eq!(build.len(), 64);
    let frozen = input(&source, &restored, build);
    let body = serde_json::to_value(&frozen).unwrap();
    let native_root = services.data_dir.join("aionrs-sessions");
    let native_path = services
        .data_dir
        .join("aionrs-sessions/sessions/continuation/state.json");
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_outbox")
        .fetch_one(services.database.pool())
        .await
        .unwrap();
    assert_status(
        &app,
        request(RECOVER, body.clone(), &token, &csrf),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(!native_path.exists());
    assert!(
        !native_root.exists(),
        "read-only recovery cannot initialize native storage"
    );
    assert!(
        services
            .conversation_repo
            .get(OWNER, "continuation")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_outbox")
            .fetch_one(services.database.pool())
            .await
            .unwrap(),
        before
    );

    let mut wrong_owner = body.clone();
    wrong_owner["rows"]["source"]["user_id"] = json!("someone-else");
    assert_status(
        &app,
        request(PREPARE, wrong_owner, &token, &csrf),
        StatusCode::NOT_FOUND,
    )
    .await;
    let mut wrong_build = body.clone();
    wrong_build["native"]["runtime_build_sha256"] = json!("0".repeat(64));
    wrong_build["rows"]["native_binding"]["runtime_build_sha256"] = json!("0".repeat(64));
    assert_status(
        &app,
        request(PREPARE, wrong_build, &token, &csrf),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(!native_path.exists());
    assert!(!native_root.exists(), "rejected input cannot initialize native storage");

    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &native_root).unwrap();
    assert_status(
        &app,
        request(PREPARE, body.clone(), &token, &csrf),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    std::fs::remove_file(&native_root).unwrap();

    let prepared = assert_status(&app, request(PREPARE, body.clone(), &token, &csrf), StatusCode::OK).await;
    for directory in [&native_root, &native_root.join("sessions")] {
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(prepared["data"]["core_user_id"], OWNER);
    assert_eq!(prepared["data"]["runtime_build_sha256"], build);
    assert_eq!(prepared["data"]["imported"]["conversation_id"], "continuation");
    assert_eq!(
        prepared["data"]["imported"]["copied_messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let bytes = std::fs::read(&native_path).unwrap();
    let native: Session = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(native.id, "continuation");
    assert_eq!(native.cwd, restored.to_str().unwrap());
    assert_eq!(native.forked_from.as_deref(), Some("source"));
    assert!(!source.exists(), "no mutable source workspace is required");
    assert!(services.conversation_repo.get(OWNER, "source").await.unwrap().is_none());
    assert_eq!(services.worker_task_manager.active_conversation_ids().len(), 0);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_outbox")
        .fetch_one(services.database.pool())
        .await
        .unwrap();
    for path in [RECOVER, PREPARE] {
        let replay = assert_status(&app, request(path, body.clone(), &token, &csrf), StatusCode::OK).await;
        assert_eq!(replay, prepared);
        assert_eq!(std::fs::read(&native_path).unwrap(), bytes);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_outbox")
                .fetch_one(services.database.pool())
                .await
                .unwrap(),
            after
        );
    }
    let mut changed = body.clone();
    changed["rows"]["source"]["name"] = json!("Different immutable input");
    assert_status(&app, request(RECOVER, changed, &token, &csrf), StatusCode::CONFLICT).await;
    std::fs::remove_file(&native_path).unwrap();
    assert_status(&app, request(RECOVER, body, &token, &csrf), StatusCode::BAD_REQUEST).await;
    assert!(
        !native_path.exists(),
        "read-only recovery cannot reconstruct missing native state"
    );
}

#[tokio::test]
async fn frozen_continuation_control_is_disabled_without_build_identity_and_in_local_mode() {
    let (_app, mut services, _directory, token, csrf) = fixture().await;
    services.frozen_continuation_build_sha256 = None;
    let (states, _) = build_module_states(&services).await.unwrap();
    let app = create_router_with_states(&services, states);
    let unavailable = assert_status(
        &app,
        request(PREPARE, json!({}), &token, &csrf),
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(unavailable["code"], "NATIVE_BUILD_UNAVAILABLE");
    services.identity_mode = IdentityMode::Local;
    let (states, _) = build_module_states(&services).await.unwrap();
    let app = create_router_with_states(&services, states);
    let mut local = request(PREPARE, json!({}), &token, &csrf);
    local.headers_mut().remove("authorization");
    assert_status(&app, local, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn backup_turn_freeze_requires_private_authority_and_reports_released_claims() {
    let (app, services, _directory, token, csrf) = fixture().await;
    let runtime = services.conversation_runtime_state.clone();
    let claim = runtime.try_claim_turn("conversation-a", "turn-a").unwrap();
    let mut unauthorized = request(TURN_FREEZE, json!({}), &token, &csrf);
    unauthorized.headers_mut().remove("x-aioncore-bootstrap-secret");
    assert_status(&app, unauthorized, StatusCode::UNAUTHORIZED).await;
    assert_eq!(runtime.backup_turn_status().unwrap(), (false, 1));

    let frozen = assert_status(&app, request(TURN_FREEZE, json!({}), &token, &csrf), StatusCode::OK).await;
    assert_eq!(frozen["data"]["frozen"], true);
    assert_eq!(frozen["data"]["activeTurnCount"], 1);
    assert!(runtime.try_claim_turn("conversation-b", "turn-b").is_err());
    drop(claim);
    let drained = assert_status(&app, request(TURN_FREEZE, json!({}), &token, &csrf), StatusCode::OK).await;
    assert_eq!(drained["data"]["activeTurnCount"], 0);
}
