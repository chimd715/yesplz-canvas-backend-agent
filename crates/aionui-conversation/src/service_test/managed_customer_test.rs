use super::*;

fn native_request(mode: &str) -> CreateConversationRequest {
    serde_json::from_value(json!({
        "type": "aionrs", "name": "Retain this conversation",
        "model": { "provider_id": "local-development-mock", "model": "mock-local", "use_model": "mock-local" },
        "extra": { "workspace": ensure_test_workspace_path(), "session_mode": mode }
    }))
    .unwrap()
}

fn select_model(model: &str) -> UpdateConversationRequest {
    serde_json::from_value(json!({ "model": {
        "provider_id": "configured-provider", "model": model, "use_model": model
    }}))
    .unwrap()
}

#[tokio::test]
async fn managed_creation_persists_yolo_after_customer_overrides() {
    let (service, _, repo) = make_service_with_mock_task_manager(Arc::new(MockTaskManager::new()));
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    let stored = repo.get("user_1", &created.id).await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored.extra).unwrap()["session_mode"],
        "yolo"
    );
    let options = service.build_task_options(&stored).await.unwrap();
    let AgentSessionKind::Aionrs(native) = options.context.kind else {
        panic!("must remain native")
    };
    assert_eq!(native.config.session_mode.as_deref(), Some("yolo"));
}

#[tokio::test]
async fn managed_legacy_recovery_ignores_stored_permission_and_preserves_model_workspace() {
    let (service, _, repo) = make_service_with_mock_task_manager(Arc::new(MockTaskManager::new()));
    let created = service.create("user_1", native_request("auto_edit")).await.unwrap();
    let service = service.with_managed_customer_mode(true);
    let stored = repo.get("user_1", &created.id).await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored.extra).unwrap()["session_mode"],
        "auto_edit"
    );
    for options in [
        service.build_task_options(&stored).await.unwrap(),
        service.build_task_options_for_runtime(&stored, None).await.unwrap(),
    ] {
        assert_eq!(options.context.model.use_model.as_deref(), Some("mock-local"));
        assert_eq!(options.context.workspace.path, ensure_test_workspace_path());
        let AgentSessionKind::Aionrs(native) = options.context.kind else {
            panic!("must remain native")
        };
        assert_eq!(native.config.session_mode.as_deref(), Some("yolo"));
    }
}

#[tokio::test]
async fn managed_permissions_are_immutable_and_cross_owner_calls_stay_not_found() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, _) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    let patch: UpdateConversationRequest =
        serde_json::from_value(json!({ "extra": { "session_mode": "default" } })).unwrap();
    assert!(matches!(
        service
            .update(
                "user_1",
                &created.id,
                patch,
                &(manager.clone() as Arc<dyn IWorkerTaskManager>)
            )
            .await,
        Err(ConversationError::BadRequest { .. })
    ));
    assert!(matches!(
        service
            .set_config_option(
                "user_1",
                &created.id,
                "mode",
                SetConfigOptionRequest {
                    value: "default".into()
                }
            )
            .await,
        Err(ConversationError::BadRequest { .. })
    ));
    assert!(matches!(
        service
            .set_config_option(
                "other_user",
                &created.id,
                "mode",
                SetConfigOptionRequest {
                    value: "default".into()
                }
            )
            .await,
        Err(ConversationError::NotFound { .. })
    ));
    assert!(matches!(
        service
            .update(
                "other_user",
                &created.id,
                select_model("z-ai/glm-5.3"),
                &(manager.clone() as Arc<dyn IWorkerTaskManager>)
            )
            .await,
        Err(ConversationError::NotFound { .. })
    ));
}

#[tokio::test]
async fn managed_model_change_refuses_an_active_turn_without_eviction_or_persisting() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, repo) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    let state = service.runtime_state();
    let _turn = state.try_claim_turn(&created.id, "active-turn").unwrap();
    let error = service
        .update(
            "user_1",
            &created.id,
            select_model("z-ai/glm-5.3"),
            &(manager.clone() as Arc<dyn IWorkerTaskManager>),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ConversationError::Busy { .. }));
    assert_eq!(state.active_turn_id_for(&created.id).as_deref(), Some("active-turn"));
    assert!(!state.is_restarting(&created.id));
    assert_eq!(manager.kill_count(), 0);
    assert!(
        repo.get("user_1", &created.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .unwrap()
            .contains("mock-local")
    );
}

#[tokio::test]
async fn managed_idle_model_change_preserves_identity_extra_and_recovery_context() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, repo) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    let before = repo.get("user_1", &created.id).await.unwrap().unwrap();
    let message = MessageRow {
        id: "history-before-model-change".into(),
        conversation_id: created.id.clone(),
        msg_id: Some("history-before-model-change".into()),
        r#type: "user".into(),
        content: json!({ "text": "Keep this history" }).to_string(),
        position: Some("right".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at: 1234,
        backend_turn_id: None,
    };
    repo.insert_message("user_1", &message).await.unwrap();
    manager.insert_agent(&created.id, AgentInstance::Mock(Arc::new(MockAgent::new(&created.id))));
    let updated = service
        .update(
            "user_1",
            &created.id,
            select_model("z-ai/glm-5.3"),
            &(manager.clone() as Arc<dyn IWorkerTaskManager>),
        )
        .await
        .unwrap();
    let after = repo.get("user_1", &created.id).await.unwrap().unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(before.extra, after.extra);
    assert_eq!(before.name, after.name);
    assert_eq!(before.created_at, after.created_at);
    let history = repo_messages_asc(&repo, &created.id, 10).await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, message.content);
    assert_eq!(manager.kill_count(), 1);
    assert!(manager.get_task(&created.id).is_none());
    assert!(!service.runtime_state().is_restarting(&created.id));
    let options = service.build_task_options(&after).await.unwrap();
    assert_eq!(options.context.model.use_model.as_deref(), Some("z-ai/glm-5.3"));
}

#[tokio::test]
async fn managed_failed_model_write_releases_admission_and_retains_old_configuration() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, repo) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    repo.fail_next_update.store(true, Ordering::SeqCst);
    assert!(
        service
            .update(
                "user_1",
                &created.id,
                select_model("z-ai/glm-5.3"),
                &(manager.clone() as Arc<dyn IWorkerTaskManager>)
            )
            .await
            .is_err()
    );
    assert_eq!(manager.kill_count(), 0);
    assert!(
        repo.get("user_1", &created.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .unwrap()
            .contains("mock-local")
    );
    assert!(!service.runtime_state().is_restarting(&created.id));
    assert!(
        service
            .runtime_state()
            .try_claim_turn(&created.id, "retry-turn")
            .is_ok()
    );
}

#[tokio::test]
async fn managed_existing_task_reopen_is_idempotent_during_an_active_turn() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, _) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    manager.insert_agent(&created.id, AgentInstance::Mock(Arc::new(MockAgent::new(&created.id))));
    let state = service.runtime_state();
    let _turn = state.try_claim_turn(&created.id, "active-turn").unwrap();
    for _ in 0..2 {
        let result = service
            .ensure_runtime("user_1", &created.id, &(manager.clone() as Arc<dyn IWorkerTaskManager>))
            .await
            .unwrap();
        assert!(!result.recovered);
        assert!(result.runtime.has_task);
        assert_eq!(result.runtime.turn_id.as_deref(), Some("active-turn"));
    }
    assert_eq!(manager.kill_count(), 0);
}

#[tokio::test]
async fn managed_apparent_noop_model_patch_cannot_bypass_active_turn_admission() {
    let manager = Arc::new(MockTaskManager::new());
    let (service, _, repo) = make_service_with_mock_task_manager(manager.clone());
    let service = service.with_managed_customer_mode(true);
    let created = service.create("user_1", native_request("default")).await.unwrap();
    let before = repo.get("user_1", &created.id).await.unwrap().unwrap();
    let request: UpdateConversationRequest = serde_json::from_value(
        json!({ "model": serde_json::from_str::<serde_json::Value>(before.model.as_deref().unwrap()).unwrap() }),
    )
    .unwrap();
    let state = service.runtime_state();
    let _turn = state.try_claim_turn(&created.id, "active-turn").unwrap();
    assert!(matches!(
        service
            .update(
                "user_1",
                &created.id,
                request,
                &(manager.clone() as Arc<dyn IWorkerTaskManager>)
            )
            .await,
        Err(ConversationError::Busy { .. })
    ));
    assert_eq!(manager.kill_count(), 0);
    assert_eq!(
        repo.get("user_1", &created.id).await.unwrap().unwrap().model,
        before.model
    );
}
