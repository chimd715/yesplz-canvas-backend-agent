use super::*;
use aion_types::message::{ContentBlock, Message, Role};
use aion_types::tool::ToolDef;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    output_cap: AtomicUsize,
}
#[async_trait]
impl LlmProvider for Probe {
    async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
        assert!(request.max_tokens.is_some());
        self.output_cap
            .store(request.max_tokens.unwrap() as usize, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (_, receiver) = mpsc::channel(1);
        Ok(receiver)
    }
}

fn request() -> LlmRequest {
    LlmRequest {
        model: "z-ai/glm-5.3-flash".into(),
        system: "System instructions".into(),
        messages: vec![Message::new(
            Role::User,
            vec![ContentBlock::Text { text: "Hello".into() }],
        )],
        tools: vec![],
        max_tokens: None,
        thinking: None,
        reasoning_effort: None,
    }
}

#[tokio::test]
async fn small_requests_reach_the_provider_with_a_bounded_reply() {
    let inner = Arc::new(Probe::default());
    let provider = BudgetedProvider::new(inner.clone(), "z-ai/glm-5.3-flash", None);
    provider.stream(&request()).await.unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(inner.output_cap.load(Ordering::SeqCst), 16_384);
}

#[tokio::test]
async fn explicit_smaller_reply_caps_are_preserved() {
    let inner = Arc::new(Probe::default());
    let provider = BudgetedProvider::new(inner.clone(), "z-ai/glm-5.3", None);
    let mut bounded = request();
    bounded.max_tokens = Some(64);
    provider.stream(&bounded).await.unwrap();
    assert_eq!(inner.output_cap.load(Ordering::SeqCst), 64);
}

#[tokio::test]
async fn system_history_tools_images_and_output_are_checked_before_any_upstream_call() {
    let inner = Arc::new(Probe::default());
    let provider = BudgetedProvider::new(inner.clone(), "z-ai/glm-5.3-flash", None);
    let large = "한".repeat(90_000);
    let mut cases = Vec::new();
    let mut system = request();
    system.system = large.clone();
    cases.push(system);
    let mut history = request();
    history.messages.insert(
        0,
        Message::new(Role::Assistant, vec![ContentBlock::Text { text: large.clone() }]),
    );
    cases.push(history);
    let mut tools = request();
    tools.tools.push(ToolDef {
        name: "test".into(),
        description: large.clone(),
        input_schema: serde_json::json!({}),
        deferred: false,
    });
    cases.push(tools);
    let mut output = request();
    output.max_tokens = Some(262_124);
    cases.push(output);
    let mut image = request();
    image.messages[0].content.push(ContentBlock::Image {
        image_url: aion_types::message::ImageUrl {
            url: format!("data:image/png;base64,{}", "A".repeat(270_000)),
        },
    });
    cases.push(image);
    let mut tool_result = request();
    tool_result.messages.push(Message::new(
        Role::Tool,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call".into(),
            content: large,
            is_error: false,
        }],
    ));
    cases.push(tool_result);
    for case in cases {
        let error = provider.stream(&case).await.unwrap_err();
        assert!(matches!(error, ProviderError::PromptTooLong(_)));
        assert!(!error.to_string().contains('한'));
    }
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn every_iteration_is_checked_including_tool_result_growth() {
    let inner = Arc::new(Probe::default());
    let provider = BudgetedProvider::new(inner.clone(), "z-ai/glm-5.3", None);
    provider.stream(&request()).await.unwrap();
    let mut second = request();
    second.system = "x".repeat(262_144);
    assert!(matches!(
        provider.stream(&second).await.unwrap_err(),
        ProviderError::PromptTooLong(_)
    ));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn configured_limits_only_lower_known_model_windows() {
    assert_eq!(context_limit("deepseek/deepseek-v4.1-flash", None), Some(1_000_000));
    assert_eq!(context_limit("z-ai/glm-5.3", None), Some(262_144));
    assert_eq!(context_limit("z-ai/glm-5.3-flash", Some(1_000_000)), Some(262_124));
    assert_eq!(context_limit("z-ai/glm-5.3", Some(65_536)), Some(65_536));
    assert_eq!(context_limit("other", None), None);
}
