use super::*;
use aionui_db::SqliteConversationRepository;

const OWNER: &str = "system_default_user";

fn request() -> SendMessageRequest {
    serde_json::from_value(json!({ "content": "Hello", "client_request_id": "send-1" })).unwrap()
}

fn service(
    workspace: &Path,
    repo: Arc<SqliteConversationRepository>,
    broadcaster: Arc<MockBroadcaster>,
    task_manager: Arc<dyn IWorkerTaskManager>,
) -> ConversationService {
    ConversationService::new(
        workspace.into(),
        broadcaster,
        Arc::new(FixedSkillResolver { names: vec![] }),
        task_manager,
        repo,
        Arc::new(StubAgentMetadataRepo),
        Arc::new(StubAcpSessionRepo::default()),
    )
}

#[tokio::test]
async fn send_receipts_concurrent_retries_run_once_and_survive_restart() {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let workspace = tempfile::tempdir().unwrap();
    let broadcaster = Arc::new(MockBroadcaster::new());
    let task_manager = Arc::new(MockTaskManager::new());
    let manager: Arc<dyn IWorkerTaskManager> = task_manager.clone();
    let svc = service(workspace.path(), repo.clone(), broadcaster.clone(), manager.clone());
    let conversation = svc
        .create(
            OWNER,
            serde_json::from_value(json!({
                "type": "aionrs", "extra": { "workspace": workspace.path() }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let agent = Arc::new(
        ScriptedAgent::new(
            &conversation.id,
            vec![vec![AgentStreamEvent::Finish(FinishEventData::default())]],
        )
        .with_agent_type(AgentType::Aionrs),
    );
    task_manager.insert_agent(&conversation.id, AgentInstance::Mock(agent.clone()));
    broadcaster.take_events();
    let (a, b) = tokio::join!(
        svc.send_message(OWNER, &conversation.id, request(), &manager),
        svc.send_message(OWNER, &conversation.id, request(), &manager)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!((&a.msg_id, &a.turn_id), (&b.msg_id, &b.turn_id));
    assert_eq!(a.client_request_id.as_deref(), Some("send-1"));
    assert_eq!(b.client_request_id, a.client_request_id);
    wait_for_turn_released(&svc, &conversation.id).await;
    assert_eq!(agent.sent_contents.lock().unwrap().len(), 1);
    assert_eq!(
        broadcaster
            .take_events()
            .iter()
            .filter(|event| event.name == "message.userCreated")
            .count(),
        1
    );
    let accepted = svc.get_send_request(OWNER, &conversation.id, "send-1").await.unwrap();
    assert_eq!(accepted.msg_id, a.msg_id);
    assert_eq!(accepted.turn_id, a.turn_id);

    // A different turn being active cannot hide or re-execute the old receipt.
    let claim = svc
        .runtime_state()
        .try_claim_turn(&conversation.id, "turn_new")
        .unwrap();
    let replay = svc
        .send_message(OWNER, &conversation.id, request(), &manager)
        .await
        .unwrap();
    assert_eq!(replay.turn_id, a.turn_id);
    assert_eq!(replay.runtime.turn_id.as_deref(), Some("turn_new"));
    drop(claim);

    // Fresh service/task manager over the same SQLite data models a restart.
    // Reset messages too: receipts do not depend on a message still existing.
    repo.delete_messages_by_conversation(OWNER, &conversation.id)
        .await
        .unwrap();
    let restarted_manager: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());
    let restarted = service(workspace.path(), repo, broadcaster.clone(), restarted_manager.clone());
    let replay = restarted
        .send_message(OWNER, &conversation.id, request(), &restarted_manager)
        .await
        .unwrap();
    assert_eq!((replay.msg_id, replay.turn_id), (a.msg_id, a.turn_id));
    assert_eq!(
        replay.runtime.state,
        aionui_api_types::ConversationRuntimeStateKind::Idle
    );
    assert_eq!(restarted_manager.active_count(), 0);
    assert!(broadcaster.take_events().is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn send_receipts_validate_payload_owner_backend_and_unavailable_references() {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let workspace = tempfile::tempdir().unwrap();
    let broadcaster = Arc::new(MockBroadcaster::new());
    let manager: Arc<dyn IWorkerTaskManager> = Arc::new(MockTaskManager::new());
    let svc = service(workspace.path(), repo.clone(), broadcaster.clone(), manager.clone());
    let conversation = svc
        .create(
            OWNER,
            serde_json::from_value(json!({
                "type": "aionrs", "extra": { "workspace": workspace.path() }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    svc.with_project_service(make_injected_project_service(workspace.path()).await);
    let file = workspace.path().join("attachment.txt");
    std::fs::write(&file, "original attachment").unwrap();
    let body = json!({ "content": "Hello", "client_request_id": "send-1", "files": [{ "kind": "local", "path": file }], "sessions": [{ "id": conversation.id }] });
    let first = svc
        .send_message(
            OWNER,
            &conversation.id,
            serde_json::from_value(body.clone()).unwrap(),
            &manager,
        )
        .await
        .unwrap();
    wait_for_turn_released(&svc, &conversation.id).await;
    std::fs::remove_file(&file).unwrap();
    broadcaster.take_events();
    let replay = svc
        .send_message(
            OWNER,
            &conversation.id,
            serde_json::from_value(body.clone()).unwrap(),
            &manager,
        )
        .await
        .unwrap();
    assert_eq!(
        replay.msg_id, first.msg_id,
        "retry must not resolve the deleted attachment"
    );
    assert!(broadcaster.take_events().is_empty());
    for (field, value) in [
        ("content", json!("")),
        ("content", json!("changed")),
        ("files", json!([])),
        ("sessions", json!([])),
        ("inject_skills", json!(["other"])),
        ("hidden", json!(true)),
    ] {
        let mut changed = body.clone();
        changed[field] = value;
        let error = svc
            .send_message(
                OWNER,
                &conversation.id,
                serde_json::from_value(changed).unwrap(),
                &manager,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ConversationError::Busy { reason } if reason == "client_request_id already accepted with another payload")
        );
    }
    assert!(matches!(
        svc.send_message("foreign", &conversation.id, request(), &manager).await,
        Err(ConversationError::NotFound { .. })
    ));
    assert!(matches!(
        svc.get_send_request("foreign", &conversation.id, "send-1").await,
        Err(ConversationError::NotFound { .. })
    ));
    assert!(
        matches!(svc.get_send_request(OWNER, &conversation.id, "missing").await, Err(ConversationError::NotFoundReason { reason }) if reason == "Send request not found")
    );
    for invalid in ["".to_owned(), " ".into(), "../x".into(), "é".into(), "x".repeat(129)] {
        let mut req = request();
        req.client_request_id = Some(invalid);
        assert!(
            matches!(svc.send_message(OWNER, &conversation.id, req, &manager).await, Err(ConversationError::BadRequest { reason }) if reason == "Invalid client_request_id")
        );
    }
    let acp = svc.create(OWNER, make_create_req()).await.unwrap();
    assert!(matches!(
        svc.send_message(OWNER, &acp.id, request(), &manager).await,
        Err(ConversationError::Unprocessable { .. })
    ));
    assert!(matches!(
        svc.get_send_request(OWNER, &acp.id, "send-1").await,
        Err(ConversationError::Unprocessable { .. })
    ));
    let mut empty = request();
    empty.client_request_id = Some("unknown".into());
    empty.content.clear();
    assert!(
        matches!(svc.send_message(OWNER, &conversation.id, empty, &manager).await, Err(ConversationError::BadRequest { reason }) if reason == "Message content must not be empty")
    );
}

#[tokio::test]
async fn send_receipts_fail_closed_for_repositories_without_atomic_support() {
    let (svc, broadcaster, repo, manager) = make_service();
    let conversation = svc
        .create(
            OWNER,
            serde_json::from_value(json!({ "type": "aionrs", "extra": { "workspace": ensure_test_workspace_path() } }))
                .unwrap(),
        )
        .await
        .unwrap();
    broadcaster.take_events();
    assert!(matches!(
        svc.send_message(OWNER, &conversation.id, request(), &manager).await,
        Err(ConversationError::Internal { .. })
    ));
    assert!(repo.messages.lock().unwrap().is_empty());
    assert!(broadcaster.take_events().is_empty());
    assert_eq!(manager.active_count(), 0);
}
