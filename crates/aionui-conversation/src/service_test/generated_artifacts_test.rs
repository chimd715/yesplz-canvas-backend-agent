use super::*;
use crate::generated_artifacts::{GeneratedFileCalls, publish_generated_file};

fn tool(id: &str, name: &str, status: ToolCallStatus, args: serde_json::Value) -> ToolCallEventData {
    ToolCallEventData {
        call_id: id.into(),
        name: name.into(),
        input: Some(args.clone()),
        args,
        status,
        output: None,
        description: None,
        parent_call_id: None,
    }
}

#[test]
fn generated_candidates_require_matching_successful_native_file_tools() {
    let mut calls = GeneratedFileCalls::default();
    assert!(
        calls
            .observe(&tool(
                "aionrs-write",
                "Write",
                ToolCallStatus::Running,
                json!({"file_path":"report.md"})
            ))
            .is_none()
    );
    assert_eq!(
        calls.observe(&tool(
            "aionrs-write",
            "Write",
            ToolCallStatus::Completed,
            serde_json::Value::Null
        )),
        Some("report.md".into())
    );
    assert!(
        calls
            .observe(&tool(
                "aionrs-write",
                "Write",
                ToolCallStatus::Completed,
                serde_json::Value::Null
            ))
            .is_none()
    );
    for (id, name) in [("cli-write", "Write"), ("aionrs-read", "Read"), ("aionrs-bash", "Bash")] {
        calls.observe(&tool(
            id,
            name,
            ToolCallStatus::Running,
            json!({"file_path":"report.md"}),
        ));
        assert!(
            calls
                .observe(&tool(id, name, ToolCallStatus::Completed, serde_json::Value::Null))
                .is_none()
        );
    }
    calls.observe(&tool(
        "aionrs-failed",
        "Write",
        ToolCallStatus::Running,
        json!({"file_path":"report.md"}),
    ));
    assert!(
        calls
            .observe(&tool(
                "aionrs-failed",
                "Write",
                ToolCallStatus::Error,
                serde_json::Value::Null
            ))
            .is_none()
    );
    assert!(
        calls
            .observe(&tool(
                "aionrs-failed",
                "Write",
                ToolCallStatus::Completed,
                serde_json::Value::Null
            ))
            .is_none()
    );
    calls.observe(&tool(
        "aionrs-publish",
        "PublishArtifact",
        ToolCallStatus::Running,
        json!({"file_path":"report.pdf"}),
    ));
    assert_eq!(
        calls.observe(&tool(
            "aionrs-publish",
            "PublishArtifact",
            ToolCallStatus::Completed,
            serde_json::Value::Null
        )),
        Some("report.pdf".into())
    );
    calls.observe(&tool(
        "aionrs-mismatch",
        "Write",
        ToolCallStatus::Running,
        json!({"file_path":"report.md"}),
    ));
    assert!(
        calls
            .observe(&tool(
                "aionrs-mismatch",
                "Read",
                ToolCallStatus::Completed,
                serde_json::Value::Null
            ))
            .is_none()
    );
}

async fn bound_conversation() -> (
    tempfile::TempDir,
    ConversationService,
    Arc<MockBroadcaster>,
    Arc<MockRepo>,
    String,
) {
    let root = tempfile::tempdir().unwrap();
    let (service, broadcaster, repo, _) = make_service_with_workspace_root(root.path().to_owned());
    service.with_project_service(make_injected_project_service(root.path()).await);
    let conversation = service
        .create(
            "user_1",
            serde_json::from_value(json!({"type":"aionrs", "extra":{"workspace":root.path().to_string_lossy()}}))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        repo.get("user_1", &conversation.id)
            .await
            .unwrap()
            .unwrap()
            .project_id
            .is_some()
    );
    broadcaster.take_events();
    (root, service, broadcaster, repo, conversation.id)
}

async fn bound_sqlite_conversation() -> (
    tempfile::TempDir,
    ConversationService,
    Arc<MockBroadcaster>,
    Arc<aionui_db::SqliteConversationRepository>,
    String,
    aionui_db::Database,
) {
    let root = tempfile::tempdir().unwrap();
    let db = init_database_memory().await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at) \
         VALUES ('user_1', 'artifact-owner', 'hash', 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let repo = Arc::new(aionui_db::SqliteConversationRepository::new(db.pool().clone()));
    let broadcaster = Arc::new(MockBroadcaster::new());
    let service = ConversationService::new(
        root.path().to_owned(),
        broadcaster.clone(),
        Arc::new(FixedSkillResolver { names: vec![] }),
        Arc::new(MockTaskManager::new()),
        repo.clone(),
        Arc::new(aionui_db::SqliteAgentMetadataRepository::new(db.pool().clone())),
        Arc::new(aionui_db::SqliteAcpSessionRepository::new(db.pool().clone())),
    );
    service.with_project_service(Arc::new(aionui_project::ProjectService::new(
        Arc::new(aionui_db::SqliteProjectStore::new(db.pool().clone())),
        root.path().join("conversations"),
    )));
    let conversation = service
        .create(
            "user_1",
            serde_json::from_value(json!({"type":"aionrs", "extra":{"workspace":root.path().to_string_lossy()}}))
                .unwrap(),
        )
        .await
        .unwrap();
    // Creation returns its original DTO; the best-effort project binding is
    // persisted separately. Verify the actual owner-scoped stored binding.
    assert!(
        repo.get("user_1", &conversation.id)
            .await
            .unwrap()
            .unwrap()
            .project_id
            .is_some()
    );
    broadcaster.take_events();
    (root, service, broadcaster, repo, conversation.id, db)
}

#[tokio::test]
async fn generated_publication_is_owner_scoped_durable_and_contains_only_project_reference() {
    // Use the production SQLite schema/repositories, not the HashMap mock:
    // a generated_file CHECK mismatch previously passed this publication fixture.
    let (root, service, broadcaster, repo, cid, _db) = bound_sqlite_conversation().await;
    std::fs::write(root.path().join("report.html"), "PRIVATE_DOCUMENT_CONTENT").unwrap();
    let projects = service.project_service().unwrap();
    publish_generated_file(
        service.conversation_repo(),
        &projects,
        service.broadcaster(),
        "user_1",
        &cid,
        "artifact-1".into(),
        "report.html",
    )
    .await
    .unwrap();
    let artifacts = service.list_artifacts("user_1", &cid).await.unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        artifacts[0].kind,
        aionui_api_types::ConversationArtifactKind::GeneratedFile
    );
    assert_eq!(artifacts[0].payload["file"]["kind"], "project");
    assert_eq!(artifacts[0].payload["file"]["relative_path"], "report.html");
    let events = broadcaster.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "conversation.artifact");
    assert_eq!(events[0].data["user_id"], "user_1");
    let wire = serde_json::to_string(&events[0]).unwrap();
    assert!(!wire.contains("PRIVATE_DOCUMENT_CONTENT"));
    assert!(!wire.contains(root.path().to_str().unwrap()));
    assert!(service.list_artifacts("user_2", &cid).await.is_err());
    assert!(
        publish_generated_file(
            service.conversation_repo(),
            &projects,
            service.broadcaster(),
            "user_2",
            &cid,
            "foreign".into(),
            "report.html"
        )
        .await
        .is_err()
    );
    assert_eq!(repo.list_artifacts("user_1", &cid).await.unwrap().len(), 1);
    assert!(broadcaster.take_events().is_empty());
}

#[tokio::test]
async fn generated_publication_rejects_mismatched_workspace_missing_and_unsafe_files() {
    let (root, service, broadcaster, repo, cid) = bound_conversation().await;
    let projects = service.project_service().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.html"), "private").unwrap();
    std::fs::create_dir(root.path().join("directory.html")).unwrap();
    let outside_path = outside.path().join("private.html");
    for file in [
        "missing.html",
        "../private.html",
        "file.exe",
        "directory.html",
        outside_path.to_str().unwrap(),
    ] {
        assert!(
            publish_generated_file(
                service.conversation_repo(),
                &projects,
                service.broadcaster(),
                "user_1",
                &cid,
                "invalid".into(),
                file
            )
            .await
            .is_err()
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_path, root.path().join("escape.html")).unwrap();
        assert!(
            publish_generated_file(
                service.conversation_repo(),
                &projects,
                service.broadcaster(),
                "user_1",
                &cid,
                "escape".into(),
                "escape.html"
            )
            .await
            .is_err()
        );
    }
    std::fs::write(root.path().join("report.md"), "# Document").unwrap();
    repo.rows
        .lock()
        .unwrap()
        .iter_mut()
        .find(|row| row.id == cid)
        .unwrap()
        .extra = json!({"workspace":outside.path().to_string_lossy()}).to_string();
    assert!(
        publish_generated_file(
            service.conversation_repo(),
            &projects,
            service.broadcaster(),
            "user_1",
            &cid,
            "mismatch".into(),
            "report.md"
        )
        .await
        .is_err()
    );
    assert!(repo.artifacts.lock().unwrap().is_empty());
    assert!(broadcaster.take_events().is_empty());
}

#[tokio::test]
async fn generated_native_engine_write_creates_file_and_relay_publishes_authenticated_preview() {
    use aionui_ai_agent::{manager::aionrs::AionrsAgentManager, types::AionrsResolvedConfig};
    use axum::{Json, Router, extract::State, routing::post};
    #[derive(Clone)]
    struct Fixture {
        path: String,
        calls: Arc<AtomicUsize>,
    }
    async fn model(
        State(state): State<Fixture>,
        Json(body): Json<serde_json::Value>,
    ) -> ([(&'static str, &'static str); 1], String) {
        state.calls.fetch_add(1, Ordering::SeqCst);
        let last_role = body["messages"].as_array().unwrap().last().unwrap()["role"]
            .as_str()
            .unwrap_or("");
        let delta = if last_role == "user" {
            assert!(
                body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["function"]["name"] == "PublishArtifact")
            );
            json!({"role":"assistant","tool_calls":[{"index":0,"id":"native-write-call","type":"function","function":{"name":"Write","arguments":json!({"file_path":state.path,"content":"<!doctype html><html><body><h1>Generated document</h1></body></html>"}).to_string()}}]})
        } else {
            json!({"role":"assistant","content":"Created your document."})
        };
        let chunk = json!({"id":"fixture-response","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":delta,"finish_reason":if last_role=="user" {"tool_calls"} else {"stop"}}]});
        (
            [("content-type", "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
    }
    // Exercise actual native Write through the real persistence schema as well
    // as project ownership, so a rejected artifact cannot hide behind a mock.
    let (root, service, broadcaster, repo, cid, _db) = bound_sqlite_conversation().await;
    let state = Fixture {
        path: root.path().join("native-output.html").to_string_lossy().into_owned(),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = AionrsResolvedConfig {
        managed_customer_mode: true,
        context_limit: None,
        provider: "openai".into(),
        api_key: "local-fixture".into(),
        model: "z-ai/glm-5.3-flash".into(),
        base_url: Some(format!("http://{address}/v1")),
        system_prompt: None,
        max_tokens: None,
        max_turns: Some(4),
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        compat_overrides: Default::default(),
        session_directory: root.path().join("sessions"),
        session_mode: Some("yolo".into()),
        skills: vec![],
        extra_mcp_servers: Default::default(),
        bedrock_config: None,
        runtime_env: vec![],
        prompt_dump_dir: None,
    };
    let agent = AionrsAgentManager::new(cid.clone(), root.path().to_string_lossy().into_owned(), config, None)
        .await
        .unwrap();
    let manager: Arc<dyn IWorkerTaskManager> =
        Arc::new(RebuildingScriptedTaskManager::new(vec![AgentInstance::Aionrs(
            Arc::new(agent),
        )]));
    service
        .send_message(
            "user_1",
            &cid,
            serde_json::from_value(json!({"content":"Create an HTML document"})).unwrap(),
            &manager,
        )
        .await
        .unwrap();
    wait_for_turn_released(&service, &cid).await;
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert!(
        std::fs::read_to_string(root.path().join("native-output.html"))
            .unwrap()
            .contains("Generated document")
    );
    assert_eq!(repo.list_artifacts("user_1", &cid).await.unwrap().len(), 1);
    let events = broadcaster.take_events();
    let artifact = events
        .iter()
        .find(|event| event.name == "conversation.artifact")
        .expect("actual Write completion must open preview");
    assert_eq!(artifact.data["payload"]["file"]["relative_path"], "native-output.html");
    assert_eq!(artifact.data["conversation_id"], cid);
    assert_eq!(artifact.data["user_id"], "user_1");
    assert!(!artifact.data.to_string().contains(root.path().to_str().unwrap()));
    server.abort();
}
