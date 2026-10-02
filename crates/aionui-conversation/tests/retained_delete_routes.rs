//! Customer deletion must preserve analytics data and deny every conversation API.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use aionui_ai_agent::{ActiveLeaseRegistry, AgentError, IWorkerTaskManager};
use aionui_api_types::{CreateConversationRequest, WebSocketMessage};
use aionui_auth::{AuthIdentityMode, AuthState, CookieConfig, JwtService, auth_middleware, csrf_middleware};
use aionui_common::{AgentKillReason, TimestampMs};
use aionui_conversation::{
    ConversationRouterState, ConversationService, conversation_routes, skill_resolver::SkillResolver,
};
use aionui_db::{IConversationRepository, SqliteConversationRepository, init_database_memory};
use aionui_realtime::EventBroadcaster;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    middleware,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const OWNER: &str = "system_default_user";
const OTHER: &str = "other-user";
struct Noop;
impl EventBroadcaster for Noop {
    fn broadcast(&self, _: WebSocketMessage<Value>) {}
}
#[async_trait::async_trait]
impl SkillResolver for Noop {
    async fn auto_inject_names(&self) -> Vec<String> {
        vec![]
    }
    async fn resolve_skills(&self, _: &[String]) -> Vec<aionui_extension::ResolvedAgentSkill> {
        vec![]
    }
}
#[derive(Default)]
struct Tasks(AtomicUsize);
#[async_trait::async_trait]
impl IWorkerTaskManager for Tasks {
    fn get_task(&self, _: &str) -> Option<aionui_ai_agent::AgentInstance> {
        None
    }
    async fn get_or_build_task(
        &self,
        _: &str,
        _: aionui_ai_agent::types::BuildTaskOptions,
    ) -> Result<aionui_ai_agent::AgentInstance, AgentError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::internal(
            "no runtime must be built for a deleted conversation",
        ))
    }
    fn kill(&self, _: &str, _: Option<AgentKillReason>) -> Result<(), AgentError> {
        Ok(())
    }
    fn kill_and_wait(
        &self,
        _: &str,
        _: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(std::future::ready(()))
    }
    async fn clear(&self) {}
    fn active_count(&self) -> usize {
        0
    }
    fn collect_idle(&self, _: TimestampMs) -> Vec<String> {
        vec![]
    }
}
struct Fixture {
    router: Router,
    db: aionui_db::Database,
    id: String,
    owner_token: String,
    other_token: String,
    tasks: Arc<Tasks>,
    workspace: tempfile::TempDir,
}
async fn setup() -> Fixture {
    let db = init_database_memory().await.unwrap();
    sqlx::query("INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES (?, ?, 'test', 1, 1)")
        .bind(OTHER)
        .bind(OTHER)
        .execute(db.pool())
        .await
        .unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let tasks = Arc::new(Tasks::default());
    let workspace = tempfile::tempdir().unwrap();
    let service = ConversationService::new(
        workspace.path().to_path_buf(),
        Arc::new(Noop),
        Arc::new(Noop),
        tasks.clone(),
        repo.clone(),
        Arc::new(aionui_db::SqliteAgentMetadataRepository::new(db.pool().clone())),
        Arc::new(aionui_db::SqliteAcpSessionRepository::new(db.pool().clone())),
    );
    let request: CreateConversationRequest =
        serde_json::from_value(json!({"type":"acp", "extra":{ "workspace": workspace.path().to_str().unwrap() }}))
            .unwrap();
    let conversation = service.create(OWNER, request).await.unwrap();
    std::fs::write(workspace.path().join("published.html"), "<p>retained document</p>").unwrap();
    let id = conversation.id;
    sqlx::query("INSERT INTO messages (id, conversation_id, type, content, position, status, hidden, created_at) VALUES ('retained-msg', ?, 'tool_call', '{\"content\":\"retained analytics content\"}', 'left', 'finish', 0, 2)")
        .bind(&id).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO conversation_artifacts (id, conversation_id, kind, status, payload, created_at, updated_at) VALUES ('retained-artifact', ?, 'skill_suggest', 'pending', '{}', 2, 2)")
        .bind(&id).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE acp_session SET session_id = 'retained-session' WHERE conversation_id = ?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    let jwt = Arc::new(JwtService::new("retained-delete-test-signing-key".into()));
    let owner_token = jwt.sign(OWNER, OWNER).unwrap();
    let other_token = jwt.sign(OTHER, OTHER).unwrap();
    let auth = AuthState {
        jwt_service: jwt,
        user_repo: Arc::new(aionui_db::SqliteUserRepository::new(db.pool().clone())),
        identity_mode: AuthIdentityMode::UserSession,
        runtime_token_verifier: None,
    };
    let router = conversation_routes(ConversationRouterState {
        service,
        task_manager: tasks.clone(),
        active_leases: Arc::new(ActiveLeaseRegistry::new()),
    })
    .layer(middleware::from_fn_with_state(auth, auth_middleware))
    .layer(middleware::from_fn_with_state(
        Arc::new(CookieConfig {
            secure: false,
            same_site: "Lax",
        }),
        csrf_middleware,
    ));
    Fixture {
        router,
        db,
        id,
        owner_token,
        other_token,
        tasks,
        workspace,
    }
}
async fn call(
    f: &Fixture,
    method: Method,
    path: &str,
    token: Option<&str>,
    csrf: bool,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if csrf {
        builder = builder
            .header("cookie", "aionui-csrf-token=test-csrf")
            .header("x-csrf-token", "test-csrf");
    }
    let response = f
        .router
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("{path} {status}: {error}, {}", String::from_utf8_lossy(&bytes))),
    )
}
#[tokio::test]
async fn deletion_auth_and_csrf_are_required_and_cross_owner_is_not_found() {
    let f = setup().await;
    let path = format!("/api/conversations/{}", f.id);
    for (token, csrf, status, code, error) in [
        (
            None,
            true,
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Authentication required".to_owned(),
        ),
        (
            Some(f.owner_token.as_str()),
            false,
            StatusCode::FORBIDDEN,
            "CSRF_INVALID",
            "CSRF token validation failed".to_owned(),
        ),
        (
            Some(f.other_token.as_str()),
            true,
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("Conversation {} not found", f.id),
        ),
    ] {
        let (actual, json) = call(&f, Method::DELETE, &path, token, csrf, Value::Null).await;
        assert_eq!(actual, status, "{json}");
        assert_eq!(json["code"], code);
        assert_eq!(json["error"], error);
    }
    let repo = SqliteConversationRepository::new(f.db.pool().clone());
    assert!(repo.get(OWNER, &f.id).await.unwrap().is_some());
}
#[tokio::test]
async fn deleted_conversation_cannot_reopen_or_start_work_but_all_data_remains() {
    let f = setup().await;
    let base = format!("/api/conversations/{}", f.id);
    let (status, response) = call(&f, Method::DELETE, &base, Some(&f.owner_token), true, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({"success":true}));
    for (method, suffix, body) in [
        (Method::GET, "", Value::Null),
        (Method::DELETE, "", Value::Null),
        (Method::PATCH, "", json!({"name":"reopen"})),
        (Method::GET, "/messages", Value::Null),
        (Method::GET, "/messages/latest?type=tool_call", Value::Null),
        (Method::GET, "/messages/retained-msg", Value::Null),
        (Method::POST, "/messages", json!({"content":"start work"})),
        (Method::GET, "/artifacts", Value::Null),
        (Method::PATCH, "/artifacts/retained-artifact", json!({"status":"saved"})),
        (Method::POST, "/runtime/ensure", json!({})),
        (Method::POST, "/runtime/restart", json!({})),
        (Method::POST, "/active-lease", json!({"active":true})),
        (Method::POST, "/cancel", json!({"turn_id":"retained-turn"})),
        (Method::GET, "/confirmations", Value::Null),
        (Method::GET, "/associated", Value::Null),
        (Method::GET, "/approvals/check?action=read_file", Value::Null),
        (Method::POST, "/asks/request/answer", json!({"decline":true})),
    ] {
        let path = format!("{base}{suffix}");
        let (status, response) = call(&f, method, &path, Some(&f.owner_token), true, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {response}");
        assert_eq!(response["code"], "NOT_FOUND", "{path}: {response}");
        assert_eq!(response["error"], format!("Conversation {} not found", f.id), "{path}");
    }
    let (_, list) = call(
        &f,
        Method::GET,
        "/api/conversations",
        Some(&f.owner_token),
        true,
        Value::Null,
    )
    .await;
    assert!(list["data"]["items"].as_array().unwrap().is_empty());
    let (_, search) = call(
        &f,
        Method::GET,
        "/api/messages/search?keyword=retained",
        Some(&f.owner_token),
        true,
        Value::Null,
    )
    .await;
    assert!(search["data"]["items"].as_array().unwrap().is_empty());
    assert_eq!(f.tasks.0.load(Ordering::SeqCst), 0);
    use aionui_db::IAcpSessionRepository;
    assert!(
        aionui_db::SqliteAcpSessionRepository::new(f.db.pool().clone())
            .get_for_user(OWNER, &f.id)
            .await
            .unwrap()
            .is_none()
    );
    let retained: (String, Option<i64>) = sqlx::query_as("SELECT name, deleted_at FROM conversations WHERE id = ?")
        .bind(&f.id)
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert!(retained.1.unwrap() > 0);
    for table in ["messages", "conversation_artifacts", "acp_session"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE conversation_id = ?"))
            .bind(&f.id)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} must remain for analytics");
    }
    assert_eq!(
        std::fs::read_to_string(f.workspace.path().join("published.html")).unwrap(),
        "<p>retained document</p>"
    );
}
