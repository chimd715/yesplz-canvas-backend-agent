use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aion_config::config::{CliArgs, Config};
use aion_providers::{LlmProvider, ProviderError};
use aion_types::llm::{LlmEvent, LlmRequest};
use tokio::sync::{broadcast, mpsc};

use super::*;
use crate::capability::backend_output_sink::BackendOutputSink;

struct Provider {
    recover_tool_error: bool,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for Provider {
    async fn stream(&self, _: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
        let (tx, rx) = mpsc::channel(4);
        let stop_reason = if self.recover_tool_error && self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            tx.send(LlmEvent::ToolUse {
                id: "fixture-tool".into(),
                name: "missing_tool".into(),
                input: serde_json::json!({}),
                extra: None,
            })
            .await
            .unwrap();
            StopReason::ToolUse
        } else {
            tx.send(LlmEvent::TextDelta("Finished fixture turn".into()))
                .await
                .unwrap();
            StopReason::EndTurn
        };
        tx.send(LlmEvent::Done {
            stop_reason,
            usage: TokenUsage {
                input_tokens: 5,
                output_tokens: 3,
                ..Default::default()
            },
        })
        .await
        .unwrap();
        Ok(rx)
    }
}

fn config(directory: &Path) -> Config {
    let mut config = Config::resolve(&CliArgs {
        provider: Some("anthropic".into()),
        api_key: Some("fixture".into()),
        base_url: None,
        model: Some("fixture-model".into()),
        max_tokens: None,
        thinking: None,
        thinking_budget: None,
        max_turns: None,
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        system_prompt: None,
        profile: None,
        auto_approve: true,
        project_dir: Some(directory.to_path_buf()),
    })
    .unwrap();
    config.session.enabled = true;
    config.session.directory = directory.to_string_lossy().into_owned();
    config.max_turns = Some(4);
    config
}

fn engine(directory: &Path, recover_tool_error: bool) -> (AgentEngine, Arc<AtomicBool>) {
    let (tx, _) = broadcast::channel(16); // Latch must work even without a subscriber.
    let sink = BackendOutputSink::new(tx);
    let failed = sink.session_save_failure_flag();
    let provider = Arc::new(Provider {
        recover_tool_error,
        calls: AtomicUsize::new(0),
    });
    let mut engine = AgentEngine::new_with_provider(
        provider,
        config(directory),
        Default::default(),
        Arc::new(sink),
        directory.to_path_buf(),
    );
    engine
        .init_session("fixture", directory.to_str().unwrap(), Some("conversation"))
        .unwrap();
    (engine, failed)
}

#[tokio::test]
async fn normal_turn_checkpoint_is_exact_private_immutable_and_resumable() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let (mut engine, failed) = engine(directory.path(), true);
    let store = NativeCheckpointStore::new(directory.path()).unwrap();
    engine.set_next_turn_id(Some("turn_one".into()));
    let result = engine.run("Recover from the missing tool", "message").await.unwrap();
    assert!(!failed.load(Ordering::Acquire)); // A recovered tool error is not a save error.
    let manifest = store
        .capture(&engine, &result, "conversation", "turn_one")
        .unwrap()
        .unwrap();
    let checkpoint = directory.path().join("checkpoints/conversation/turn_one");
    let bytes = fs::read(checkpoint.join("state.json")).unwrap();
    assert_eq!(
        bytes,
        fs::read(directory.path().join("sessions/conversation/state.json")).unwrap()
    );
    assert_eq!(manifest.session_sha256, hex::encode(Sha256::digest(&bytes)));
    assert_eq!(manifest.session_bytes, bytes.len());
    assert!(manifest.local_only);
    assert_eq!(
        fs::metadata(checkpoint.join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&checkpoint).unwrap().permissions().mode() & 0o777, 0o700);
    let saved: Session = serde_json::from_slice(&bytes).unwrap();
    assert!(
        saved
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|block| matches!(
                block,
                aion_types::message::ContentBlock::ToolResult { is_error: true, .. }
            ))
    );
    let resume_directory = tempfile::tempdir().unwrap();
    let resume = aion_agent::session::SessionManager::new(resume_directory.path().into(), 100);
    resume.save(&saved).unwrap();
    assert_eq!(
        serde_json::to_value(resume.load("conversation").unwrap()).unwrap(),
        serde_json::to_value(&saved).unwrap()
    );
    let (tx, _) = broadcast::channel(16);
    let mut resumed = AgentEngine::resume_with_provider(
        Arc::new(Provider {
            recover_tool_error: false,
            calls: AtomicUsize::new(0),
        }),
        config(resume_directory.path()),
        Default::default(),
        Arc::new(BackendOutputSink::new(tx)),
        saved,
        resume_directory.path().into(),
    );
    resumed.set_next_turn_id(Some("resumed_turn".into()));
    let resumed_result = resumed.run("Continue the preserved session", "message3").await.unwrap();
    assert_eq!(resumed_result.usage.input_tokens, result.usage.input_tokens + 5);
    let continued = resume.load("conversation").unwrap();
    assert!(
        continued
            .messages
            .iter()
            .any(|message| message.turn_id.as_deref() == Some("turn_one"))
    );
    assert_eq!(
        continued.messages.last().unwrap().turn_id.as_deref(),
        Some("resumed_turn")
    );
    assert_eq!(
        store
            .capture(&engine, &result, "conversation", "turn_one")
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    engine.set_next_turn_id(Some("turn_two".into()));
    let next = engine.run("Another turn", "message2").await.unwrap();
    store.capture(&engine, &next, "conversation", "turn_two").unwrap();
    assert_eq!(bytes, fs::read(checkpoint.join("state.json")).unwrap());
}

#[tokio::test]
async fn rejects_wrong_identity_stale_accounting_and_symlink_without_publishing() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let (mut engine, _) = engine(directory.path(), false);
    let store = NativeCheckpointStore::new(directory.path()).unwrap();
    engine.set_next_turn_id(Some("turn_one".into()));
    let result = engine.run("Fixture", "message").await.unwrap();
    assert_eq!(
        store.capture(&engine, &result, "other", "turn_one").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        store
            .capture(&engine, &result, "conversation", "wrong_turn")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    let mut usage = result.usage.clone();
    usage.output_tokens += 1;
    assert!(
        store
            .capture_verified("conversation", "turn_one", &usage, &engine.context_status())
            .is_err()
    );
    let state = directory.path().join("sessions/conversation/state.json");
    let outside = directory.path().join("outside.json");
    fs::rename(&state, &outside).unwrap();
    symlink(&outside, &state).unwrap();
    assert!(store.capture(&engine, &result, "conversation", "turn_one").is_err());
    assert!(!directory.path().join("checkpoints/conversation/turn_one").exists());
}

#[tokio::test]
async fn real_native_save_failure_is_latched_without_broadcast_receiver() {
    let directory = tempfile::tempdir().unwrap();
    let (mut engine, failed) = engine(directory.path(), false);
    let state = directory.path().join("sessions/conversation/state.json");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap(); // Native atomic rename cannot replace a directory.
    engine.set_next_turn_id(Some("turn_one".into()));
    assert!(engine.run("Fixture", "message").await.is_ok()); // Upstream swallows the I/O error.
    assert!(failed.load(Ordering::Acquire)); // Our synchronous latch still sees it.
}

#[tokio::test]
async fn slash_commands_do_not_publish_new_checkpoint_proof() {
    let directory = tempfile::tempdir().unwrap();
    let (mut engine, _) = engine(directory.path(), false);
    let store = NativeCheckpointStore::new(directory.path()).unwrap();
    engine.set_next_turn_id(Some("slash_turn".into()));
    let result = engine.run("/clear", "message").await.unwrap();
    assert!(
        store
            .capture(&engine, &result, "conversation", "slash_turn")
            .unwrap()
            .is_none()
    );
    assert!(!directory.path().join("checkpoints/conversation/slash_turn").exists());
}

#[test]
fn rejects_untrusted_checkpoint_identifiers() {
    for id in ["", ".", "..", "../other", "other/path", "a\\b"] {
        assert!(!valid_id(id));
    }
}
