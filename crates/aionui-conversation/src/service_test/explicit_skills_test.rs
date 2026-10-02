use super::*;
use crate::skill_resolver::LoadedAgentSkill;

struct ExplicitSkillResolver;
#[async_trait::async_trait]
impl SkillResolver for ExplicitSkillResolver {
    async fn auto_inject_names(&self) -> Vec<String> {
        vec![]
    }
    async fn resolve_skills(&self, _: &[String]) -> Vec<ResolvedAgentSkill> {
        vec![]
    }
    async fn load_skill_bodies_for_user(&self, user_id: &str, names: &[String]) -> Vec<LoadedAgentSkill> {
        if user_id != "user_1" || !names.iter().any(|name| name == "report") {
            return vec![];
        }
        vec![LoadedAgentSkill {
            name: "report".into(),
            body: "EXACT_TRUSTED_REPORT_INSTRUCTIONS".into(),
            source_path: "/trusted/skills/report".into(),
        }]
    }
}

async fn native_conversation(service: &ConversationService) -> aionui_api_types::ConversationResponse {
    service
        .create(
            "user_1",
            serde_json::from_value(json!({ "type": "aionrs", "extra": { "workspace": ensure_test_workspace_path() } }))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_skill_applies_actual_instructions_without_polluting_user_history() {
    let (service, _, repo, _) = make_service_with_resolver(Arc::new(ExplicitSkillResolver));
    let conversation = native_conversation(&service).await;
    let agent = Arc::new(ScriptedAgent::new(&conversation.id, vec![]).with_agent_type(AgentType::Aionrs));
    let manager: Arc<dyn IWorkerTaskManager> = Arc::new(RebuildingScriptedTaskManager::new(vec![AgentInstance::Mock(
        agent.clone(),
    )]));
    service
        .send_message(
            "user_1",
            &conversation.id,
            serde_json::from_value(json!({ "content": "$report create an overview" })).unwrap(),
            &manager,
        )
        .await
        .unwrap();
    wait_for_turn_released(&service, &conversation.id).await;
    let sent = agent.sent_contents();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("EXACT_TRUSTED_REPORT_INSTRUCTIONS"));
    assert!(sent[0].contains("/trusted/skills/report"));
    assert!(sent[0].ends_with("$report create an overview"));
    let messages = repo.messages.lock().unwrap();
    let user = messages
        .iter()
        .find(|row| row.position.as_deref() == Some("right"))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&user.content).unwrap()["content"],
        "$report create an overview"
    );
}

#[tokio::test]
async fn unknown_explicit_skill_rejects_before_message_claim_or_agent_build() {
    let (service, _, repo, _) = make_service_with_resolver(Arc::new(ExplicitSkillResolver));
    let conversation = native_conversation(&service).await;
    let manager = Arc::new(RebuildingScriptedTaskManager::new(vec![]));
    let port: Arc<dyn IWorkerTaskManager> = manager.clone();
    let error = service
        .send_message(
            "user_1",
            &conversation.id,
            serde_json::from_value(json!({ "content": "$reports create an overview" })).unwrap(),
            &port,
        )
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), "BAD_REQUEST");
    assert_eq!(
        error.to_string(),
        "Bad request: An explicitly requested skill is unavailable"
    );
    assert!(repo.messages.lock().unwrap().is_empty());
    assert!(!service.runtime_state().is_claimed(&conversation.id));
    assert_eq!(manager.build_count(), 0);
}

#[tokio::test]
async fn explicit_skill_cannot_bypass_conversation_ownership() {
    let (service, _, repo, _) = make_service_with_resolver(Arc::new(ExplicitSkillResolver));
    let conversation = native_conversation(&service).await;
    let manager: Arc<dyn IWorkerTaskManager> = Arc::new(RebuildingScriptedTaskManager::new(vec![]));
    let error = service
        .send_message(
            "user_2",
            &conversation.id,
            serde_json::from_value(json!({ "content": "$report inspect" })).unwrap(),
            &manager,
        )
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), "NOT_FOUND");
    assert!(repo.messages.lock().unwrap().is_empty());
}
