use std::sync::Arc;

use aion_agent::output::OutputSink;
use aionui_ai_agent::{
    AgentStreamEvent, BackendOutputSink,
    protocol::events::{FinishEventData, TipType, TipsEventData},
};
use aionui_common::now_ms;
use aionui_conversation::stream_relay::{RelayTerminal, StreamRelay, SupersedingTipTotals};
use aionui_db::{
    IConversationRepository, MessagePageDirection, MessagePageParams, SqliteConversationRepository,
    init_database_memory, models::ConversationRow,
};
use aionui_realtime::BroadcastEventBus;
use serde_json::json;
use tokio::sync::broadcast;

async fn setup_repo() -> (Arc<SqliteConversationRepository>, aionui_db::Database) {
    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let now = now_ms();
    repo.create(&ConversationRow {
        id: "conv-1".into(),
        user_id: "system_default_user".into(),
        name: "Stream relay tips test".into(),
        r#type: "acp".into(),
        extra: "{}".into(),
        model: None,
        status: Some("running".into()),
        source: Some("aionui".into()),
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: now,
        updated_at: now,
        project_id: None,
        folder_id: None,
        name_source: None,
    })
    .await
    .unwrap();

    (repo, db)
}

#[tokio::test]
async fn autocompact_warning_keeps_relay_open_for_native_output() {
    let (repo, _db) = setup_repo().await;
    let bus = Arc::new(BroadcastEventBus::new(64));
    let (tx, rx) = broadcast::channel(64);
    let sink = BackendOutputSink::new(tx);
    let relay = StreamRelay::new(
        "conv-1".into(),
        "asst-1".into(),
        "turn-1".into(),
        "system_default_user".into(),
        repo.clone(),
        bus,
    );

    // Verified: pinned aionrs v0.2.11 engine.rs continues after this diagnostic.
    let warning = "Autocompact failed: Stream error: connection interrupted";
    sink.emit_error(warning);
    sink.emit_text_delta("Continuing the requested work.", "native-msg");
    sink.emit_tool_call("read-1", "Read", r#"{"path":"index.html"}"#);
    sink.emit_tool_result("read-1", "Read", false, "file contents");
    sink.emit_stream_end("native-msg", 1, 100, 20, 0, 0);

    let outcome = relay.consume(rx).await;
    assert_eq!(outcome.terminal, RelayTerminal::Finish);
    assert!(outcome.attempt.saw_visible_output);
    assert!(outcome.attempt.saw_tool_or_side_effect);
    assert!(outcome.attempt.persisted_assistant_output);
    assert!(outcome.attempt.terminal_error.is_none());

    let messages = repo
        .list_messages_page(
            "system_default_user",
            "conv-1",
            &MessagePageParams {
                limit: 100,
                direction: MessagePageDirection::InitialLatest,
            },
        )
        .await
        .unwrap();
    let tip = messages.items.iter().find(|row| row.r#type == "tips").unwrap();
    let content: serde_json::Value = serde_json::from_str(&tip.content).unwrap();
    assert_eq!(content["content"], warning);
    assert_eq!(content["type"], "warning");
    assert!(messages.items.iter().any(|row| {
        row.r#type == "text"
            && row.status.as_deref() == Some("finish")
            && row.content.contains("Continuing the requested work.")
    }));
    assert!(
        messages
            .items
            .iter()
            .any(|row| { row.status.as_deref() == Some("finish") && row.content.contains("aionrs-read-1") })
    );
    assert!(!messages.items.iter().any(|row| row.status.as_deref() == Some("error")));
}

#[tokio::test]
async fn persist_info_tip_preserves_code_and_params() {
    let (repo, _db) = setup_repo().await;
    let bus = Arc::new(BroadcastEventBus::new(64));
    let (tx, _) = broadcast::channel(64);

    let relay = StreamRelay::new(
        "conv-1".into(),
        "asst-1".into(),
        "turn-1".into(),
        "system_default_user".into(),
        repo.clone(),
        bus,
    );

    let rx = tx.subscribe();
    tx.send(AgentStreamEvent::Tips(TipsEventData {
        content: String::new(),
        tip_type: TipType::Info,
        code: Some("ACP_EMPTY_TURN".into()),
        params: Some(json!({ "scope": "session", "cleared": 12 })),
        supersedes_key: None,
    }))
    .unwrap();
    tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

    relay.consume(rx).await;

    let messages = repo
        .list_messages_page(
            "system_default_user",
            "conv-1",
            &MessagePageParams {
                limit: 100,
                direction: MessagePageDirection::InitialLatest,
            },
        )
        .await
        .unwrap();
    let tip = messages
        .items
        .iter()
        .find(|row| row.r#type == "tips")
        .expect("info tip should be persisted");

    assert_eq!(tip.status.as_deref(), Some("finish"));

    let content: serde_json::Value = serde_json::from_str(&tip.content).unwrap();
    assert_eq!(content["content"], "");
    assert_eq!(content["type"], "info");
    assert_eq!(content["code"], "ACP_EMPTY_TURN");
    assert_eq!(content["params"], json!({ "scope": "session", "cleared": 12 }));
}

/// A retry tip is the same card counting up, so its merge key has to survive
/// persistence: reloading the conversation folds history with the same rule the
/// live stream uses, and without the key a stalled turn comes back as a stack of
/// near-identical "Reconnecting... N/5" cards.
#[tokio::test]
async fn persist_warning_tip_preserves_supersedes_key() {
    let (repo, _db) = setup_repo().await;
    let bus = Arc::new(BroadcastEventBus::new(64));
    let (tx, _) = broadcast::channel(64);

    let relay = StreamRelay::new(
        "conv-1".into(),
        "asst-1".into(),
        "turn-1".into(),
        "system_default_user".into(),
        repo.clone(),
        bus,
    );

    let rx = tx.subscribe();
    for attempt in 1..=3 {
        tx.send(AgentStreamEvent::Tips(TipsEventData {
            content: format!("Reconnecting... {attempt}/5"),
            tip_type: TipType::Warning,
            code: None,
            params: None,
            supersedes_key: Some("codex-retry:turn-1".into()),
        }))
        .unwrap();
    }
    tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

    relay.consume(rx).await;

    let messages = repo
        .list_messages_page(
            "system_default_user",
            "conv-1",
            &MessagePageParams {
                limit: 100,
                direction: MessagePageDirection::InitialLatest,
            },
        )
        .await
        .unwrap();
    let tips: Vec<_> = messages.items.iter().filter(|row| row.r#type == "tips").collect();
    assert_eq!(tips.len(), 3, "every attempt is persisted; the merge happens on read");

    for row in tips {
        let content: serde_json::Value = serde_json::from_str(&row.content).unwrap();
        assert_eq!(content["supersedes_key"], "codex-retry:turn-1");
    }
}
/// A stalled prompt gets replayed against a freshly spawned CLI, and the new
/// process starts its own retry counter at 1. The card the user is watching
/// must not restart with it: the relay outlives the attempts, so it owns the
/// running totals.
#[tokio::test]
async fn retry_totals_keep_counting_across_a_replay() {
    let (repo, _db) = setup_repo().await;
    let bus = Arc::new(BroadcastEventBus::new(64));
    let (tx, _) = broadcast::channel(64);
    let totals = SupersedingTipTotals::default();

    // Two rounds of "1/5 … 2/5", each consumed by its OWN relay — that is what
    // an auto-replay does: the orchestrator builds a fresh relay per attempt
    // against a freshly spawned CLI. Only the shared totals span the boundary.
    for _ in 0..2 {
        let relay = StreamRelay::new(
            "conv-1".into(),
            "asst-1".into(),
            "turn-1".into(),
            "system_default_user".into(),
            repo.clone(),
            bus.clone(),
        )
        .with_superseding_tip_totals(totals.clone());

        let rx = tx.subscribe();
        for attempt in 1..=2 {
            tx.send(AgentStreamEvent::Tips(TipsEventData {
                content: format!("Reconnecting... {attempt}/5"),
                tip_type: TipType::Warning,
                code: Some("CODEX_RETRYING".into()),
                params: Some(json!({ "detail": format!("Reconnecting... {attempt}/5") })),
                supersedes_key: Some("codex-retry:turn-1".into()),
            }))
            .unwrap();
        }
        tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();
        relay.consume(rx).await;
    }

    let messages = repo
        .list_messages_page(
            "system_default_user",
            "conv-1",
            &MessagePageParams {
                limit: 100,
                direction: MessagePageDirection::InitialLatest,
            },
        )
        .await
        .unwrap();
    let attempts: Vec<u64> = messages
        .items
        .iter()
        .filter(|row| row.r#type == "tips")
        .map(|row| {
            let content: serde_json::Value = serde_json::from_str(&row.content).unwrap();
            content["params"]["attempts"]
                .as_u64()
                .expect("the card reports its totals")
        })
        .collect();

    // Sorted, because four rows written inside the same millisecond come back in
    // an unspecified order — what matters is that the count kept going instead
    // of restarting at 1 with the replay.
    let mut counted = attempts.clone();
    counted.sort_unstable();
    assert_eq!(
        counted,
        vec![1, 2, 3, 4],
        "the count spans both attempts, got {attempts:?}"
    );

    let highest: serde_json::Value = messages
        .items
        .iter()
        .filter(|row| row.r#type == "tips")
        .map(|row| serde_json::from_str::<serde_json::Value>(&row.content).unwrap())
        .max_by_key(|content| content["params"]["attempts"].as_u64().unwrap_or(0))
        .unwrap();
    assert!(
        highest["params"]["elapsed"].is_string(),
        "the card also reports how long the prompt has been waiting"
    );
    assert_eq!(
        highest["params"]["detail"], "Reconnecting... 2/5",
        "the CLI's own line survives as the body parameter"
    );
}

/// A tip with no merge key is left exactly as the producer sent it.
#[tokio::test]
async fn a_plain_tip_gets_no_totals() {
    let (repo, _db) = setup_repo().await;
    let bus = Arc::new(BroadcastEventBus::new(64));
    let (tx, _) = broadcast::channel(64);

    let relay = StreamRelay::new(
        "conv-1".into(),
        "asst-1".into(),
        "turn-1".into(),
        "system_default_user".into(),
        repo.clone(),
        bus,
    );

    let rx = tx.subscribe();
    tx.send(AgentStreamEvent::Tips(TipsEventData {
        content: "The installed codex is newer than the version AionUi verified.".into(),
        tip_type: TipType::Info,
        code: Some("CLI_VERSION_NEWER".into()),
        params: Some(json!({ "cli": "codex" })),
        supersedes_key: None,
    }))
    .unwrap();
    tx.send(AgentStreamEvent::Finish(FinishEventData::default())).unwrap();

    relay.consume(rx).await;

    let messages = repo
        .list_messages_page(
            "system_default_user",
            "conv-1",
            &MessagePageParams {
                limit: 100,
                direction: MessagePageDirection::InitialLatest,
            },
        )
        .await
        .unwrap();
    let tip = messages.items.iter().find(|row| row.r#type == "tips").unwrap();
    let content: serde_json::Value = serde_json::from_str(&tip.content).unwrap();
    assert_eq!(content["params"], json!({ "cli": "codex" }));
}
