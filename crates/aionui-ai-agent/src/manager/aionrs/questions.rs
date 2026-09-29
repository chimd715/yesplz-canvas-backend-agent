use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use aion_agent::session::Session;
use aion_config::hooks::HooksConfig;
use aion_protocol::events::ToolCategory;
use aion_tools::registry::ToolRegistry;
use aion_tools::{Tool, ToolExecutionOutput};
use aion_types::message::{ContentBlock, Role};
use aion_types::skill_types::ContextModifier;
use aion_types::tool::{JsonSchema, ToolResult};
use aionui_api_types::AskQuestionAnswer;
use aionui_common::{Confirmation, generate_id};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tracing::info;

use crate::error::AgentError;
use crate::protocol::events::AgentStreamEvent;

const PLATFORM: &str = "Which platform is your store on?";
const CONFIRM: &str = "ConfirmWidgetData";
const ASK: &str = "AskUserQuestion";

struct PendingQuestion {
    id: String,
    questions: Value,
    answer: oneshot::Sender<Option<Value>>,
}

/// Conversation-owned question state, independent of automatic tool approvals.
pub(super) struct NativeQuestions {
    conversation_id: String,
    events: broadcast::Sender<AgentStreamEvent>,
    confirmations: Arc<RwLock<Vec<Confirmation>>>,
    pending: Mutex<Option<PendingQuestion>>,
    widget: AtomicBool,
    confirmed: AtomicBool,
    source: Mutex<Option<Value>>,
}

fn widget_instructions(text: &str) -> bool {
    // ponytail: bundled header identifies widget intent; use resolved metadata if skill transport changes.
    text.starts_with("---\nname: shop-widget\n")
}

fn result(content: impl Into<String>, is_error: bool) -> ToolResult {
    ToolResult {
        content: content.into(),
        is_error,
    }
}

impl NativeQuestions {
    pub fn new(
        conversation_id: String,
        events: broadcast::Sender<AgentStreamEvent>,
        confirmations: Arc<RwLock<Vec<Confirmation>>>,
        session: Option<&Session>,
    ) -> Arc<Self> {
        let mut widget = false;
        let mut source = None;
        let mut calls = HashSet::new();
        if let Some(session) = session {
            for message in &session.messages {
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } if message.role == Role::User => {
                            widget |= widget_instructions(text)
                        }
                        ContentBlock::ToolUse { id, name, .. } if name == CONFIRM => {
                            calls.insert(id.clone());
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: false,
                        } if calls.contains(tool_use_id) => {
                            if let Ok(value) = serde_json::from_str::<Value>(content)
                                && value["widget_data_confirmed"] == true
                                && value["needs_store_data"] == true
                                && !value["source"].is_null()
                            {
                                source = Some(value["source"].clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Arc::new(Self {
            conversation_id,
            events,
            confirmations,
            pending: Mutex::new(None),
            widget: AtomicBool::new(widget),
            confirmed: AtomicBool::new(false),
            source: Mutex::new(source),
        })
    }

    pub fn observe_input(&self, text: &str) {
        if widget_instructions(text) {
            self.widget.store(true, Ordering::Release);
        }
        if self.widget.load(Ordering::Acquire) {
            self.confirmed.store(false, Ordering::Release);
        }
    }

    fn blocked(&self, name: &str) -> bool {
        if !self.widget.load(Ordering::Acquire) {
            return false;
        }
        let waiting = self.pending.lock().unwrap().is_some();
        (waiting || !self.confirmed.load(Ordering::Acquire)) && !matches!(name, "Read" | "Glob" | "Grep" | "ViewImage")
    }

    pub fn cancel(&self) {
        if let Some(pending) = self.pending.lock().unwrap().take() {
            self.remove_confirmation(&pending.id);
            let _ = pending.answer.send(None);
        }
    }

    fn remove_confirmation(&self, id: &str) {
        if let Ok(mut items) = self.confirmations.write() {
            items.retain(|c| c.call_id != id);
        }
    }

    pub fn answer(&self, id: &str, answers: Option<Vec<AskQuestionAnswer>>) -> Result<(), AgentError> {
        let mut slot = self.pending.lock().unwrap();
        let pending = slot
            .as_ref()
            .filter(|p| p.id == id)
            .ok_or_else(|| AgentError::bad_request("Question is no longer pending"))?;
        let value = if let Some(answers) = answers {
            let questions = pending.questions.as_array().unwrap();
            if answers.len() != questions.len()
                || !questions.iter().all(|q| {
                    let matches: Vec<_> = answers
                        .iter()
                        .filter(|a| a.question == q["question"].as_str().unwrap())
                        .collect();
                    matches.len() == 1
                        && !matches[0].labels.is_empty()
                        && (q["multiSelect"] == true || matches[0].labels.len() == 1)
                        && matches[0]
                            .labels
                            .iter()
                            .all(|s| !s.trim().is_empty() && s.len() <= 4000)
                        && (q["question"] != PLATFORM
                            || matches[0].labels.iter().all(|s| {
                                s == "Other"
                                    || s.strip_prefix("Shopify (selected store: ")
                                        .and_then(|s| s.strip_suffix(')'))
                                        .is_some_and(|shop| {
                                            shop.ends_with(".myshopify.com") && !shop.contains([' ', '/', '\n'])
                                        })
                            }))
                })
            {
                return Err(AgentError::bad_request("Answer every question with a valid selection"));
            }
            let mut parameters = serde_json::Map::new();
            for q in questions {
                if let Some(key) = q["key"].as_str() {
                    let answer = answers
                        .iter()
                        .find(|a| a.question == q["question"].as_str().unwrap())
                        .unwrap();
                    parameters.insert(
                        key.to_owned(),
                        if q["multiSelect"] == true {
                            json!(answer.labels)
                        } else {
                            json!(answer.labels[0])
                        },
                    );
                }
            }
            Some(
                json!({"answers": answers.into_iter().map(|a| json!({"question": a.question, "labels": a.labels})).collect::<Vec<_>>(), "parameters": parameters}),
            )
        } else {
            None
        };
        let pending = slot.take().unwrap();
        self.remove_confirmation(id);
        pending
            .answer
            .send(value)
            .map_err(|_| AgentError::bad_request("Question is no longer pending"))?;
        info!(conversation_id = %self.conversation_id, request_id = id, "Native question resolved");
        Ok(())
    }

    async fn ask(&self, questions: Value) -> Option<Value> {
        let id = generate_id();
        let (answer, receiver) = oneshot::channel();
        {
            let mut slot = self.pending.lock().unwrap();
            if slot.is_some() {
                return None;
            }
            *slot = Some(PendingQuestion {
                id: id.clone(),
                questions: questions.clone(),
                answer,
            });
        }
        if let Ok(mut items) = self.confirmations.write() {
            items.push(Confirmation {
                id: generate_id(),
                call_id: id.clone(),
                title: None,
                action: Some(ASK.into()),
                description: String::new(),
                command_type: Some("ask".into()),
                options: vec![],
                questions: Some(questions.clone()),
            });
        }
        let _ = self.events.send(AgentStreamEvent::Ask(json!({
            "session_id": self.conversation_id, "request_id": id, "questions": questions,
        })));
        info!(conversation_id = %self.conversation_id, request_id = %id, "Native question waiting");
        let answer = receiver.await.ok().flatten();
        if answer.is_none() && self.widget.load(Ordering::Acquire) {
            self.confirmed.store(false, Ordering::Release);
        }
        answer
    }

    pub fn register(self: &Arc<Self>, registry: &mut ToolRegistry) {
        let original = Arc::new(std::mem::take(registry));
        for name in original.tool_names() {
            registry.register(Box::new(GatedTool {
                name,
                original: original.clone(),
                questions: self.clone(),
            }));
        }
        registry.register(Box::new(QuestionTool {
            questions: self.clone(),
            confirm: false,
        }));
        registry.register(Box::new(QuestionTool {
            questions: self.clone(),
            confirm: true,
        }));
    }
}

struct GatedTool {
    name: String,
    original: Arc<ToolRegistry>,
    questions: Arc<NativeQuestions>,
}
impl GatedTool {
    fn inner(&self) -> &dyn Tool {
        self.original.get(&self.name).unwrap()
    }
}

#[async_trait]
impl Tool for GatedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        self.inner().description()
    }
    fn input_schema(&self) -> JsonSchema {
        self.inner().input_schema()
    }
    fn category(&self) -> ToolCategory {
        self.inner().category()
    }
    fn is_concurrency_safe(&self, input: &Value) -> bool {
        !self.questions.widget.load(Ordering::Acquire) && self.inner().is_concurrency_safe(input)
    }
    fn requires_image_input(&self) -> bool {
        self.inner().requires_image_input()
    }
    fn is_deferred(&self) -> bool {
        self.inner().is_deferred()
    }
    fn max_result_size(&self) -> usize {
        self.inner().max_result_size()
    }
    fn describe(&self, input: &Value) -> String {
        self.inner().describe(input)
    }
    fn context_modifier_for(&self, input: &Value) -> Option<ContextModifier> {
        self.inner().context_modifier_for(input)
    }
    fn skill_hooks_for(&self, input: &Value) -> Option<HooksConfig> {
        self.inner().skill_hooks_for(input)
    }
    async fn execute(&self, input: Value) -> ToolResult {
        self.execute_with_follow_up(input).await.result
    }
    async fn execute_with_follow_up(&self, input: Value) -> ToolExecutionOutput {
        if self.questions.blocked(&self.name) {
            info!(conversation_id = %self.questions.conversation_id, tool = %self.name, "Widget tool blocked pending data decision");
            return result("Blocked: call ConfirmWidgetData before implementation. Product questions require an answer even in YOLO.", true).into();
        }
        self.inner().execute_with_follow_up(input).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataDecision {
    needs_store_data: bool,
    #[serde(default)]
    reselect_source: bool,
}

struct QuestionTool {
    questions: Arc<NativeQuestions>,
    confirm: bool,
}

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        if self.confirm { CONFIRM } else { ASK }
    }
    fn description(&self) -> &str {
        if self.confirm {
            "Required before implementation on every Shop Widget turn. Decide whether real store/product data is needed. Static FAQ/size guides: false. Product/recommendation widgets: true. Waits for Shopify/Other and source confirmation, or reuses an already confirmed source. Set reselect_source true when the user requests a different source. No connection means no claimed product or recommendation access."
        } else {
            "Ask missing parameters for any skill or extension through the existing question UI. Supply a stable key per parameter, question text and optional choices. Empty options means free text. Returns answers and parameters keyed by key; multiSelect returns an array, otherwise a string. The current turn waits even in YOLO."
        }
    }
    fn input_schema(&self) -> JsonSchema {
        if self.confirm {
            return json!({"type":"object", "properties":{"needs_store_data":{"type":"boolean"}, "reselect_source":{"type":"boolean"}}, "required":["needs_store_data"], "additionalProperties":false});
        }
        json!({"type":"object", "properties":{"questions":{"type":"array", "minItems":1, "maxItems":4,
            "items":{"type":"object", "properties":{
                "key":{"type":"string","description":"Stable parameter name, such as store or api_url; letters, numbers and underscores"},
                "question":{"type":"string"}, "header":{"type":"string"},
                "multiSelect":{"type":"boolean"}, "options":{"type":"array", "items":{"type":"object", "properties":{
                    "label":{"type":"string"}, "description":{"type":"string"}}, "required":["label"]}}},
                "required":["question"]}}}, "required":["questions"]})
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }
    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }
    async fn execute(&self, input: Value) -> ToolResult {
        if !self.confirm {
            let Some(qs) = input["questions"]
                .as_array()
                .filter(|qs| !qs.is_empty() && qs.len() <= 4)
            else {
                return result("Invalid questions", true);
            };
            let mut qs = qs.clone();
            for q in &mut qs {
                if let Some(object) = q.as_object_mut() {
                    object.entry("options").or_insert_with(|| json!([]));
                }
            }
            let mut keys = HashSet::new();
            let mut texts = HashSet::new();
            if !qs.iter().all(|q| {
                q["question"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 2000)
                    && texts.insert(q["question"].as_str().unwrap())
                    && q.get("key").is_none_or(|key| {
                        key.as_str().is_some_and(|s| {
                            !s.is_empty()
                                && s.len() <= 80
                                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                                && keys.insert(s)
                        })
                    })
                    && q.get("header")
                        .is_none_or(|h| h.as_str().is_some_and(|s| s.len() <= 400))
                    && q["options"].as_array().is_some_and(|opts| {
                        opts.len() <= 8
                            && opts.iter().all(|o| {
                                o["label"]
                                    .as_str()
                                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 400)
                                    && o.get("description")
                                        .is_none_or(|d| d.as_str().is_some_and(|s| s.len() <= 2000))
                            })
                    })
                    && q.get("multiSelect").is_none_or(Value::is_boolean)
            }) {
                return result("Invalid questions", true);
            }
            return match self.questions.ask(Value::Array(qs)).await {
                Some(answer) => result(answer.to_string(), false),
                None => result(
                    "User declined the question. Do not assume an answer or continue implementation.",
                    true,
                ),
            };
        }
        let Ok(decision) = serde_json::from_value::<DataDecision>(input) else {
            return result("needs_store_data must be a boolean", true);
        };
        let cached = self.questions.source.lock().unwrap().clone();
        if decision.needs_store_data
            && !decision.reselect_source
            && let Some(source) = cached
        {
            self.questions.confirmed.store(true, Ordering::Release);
            return result(
                json!({"widget_data_confirmed":true,"needs_store_data":true,"source":source,"reused":true}).to_string(),
                false,
            );
        }
        let mut source = Value::Null;
        if decision.needs_store_data {
            let Some(answer) = self
                .questions
                .ask(json!([{"question":PLATFORM,"options":[{"label":"Shopify"},{"label":"Other"}]}]))
                .await
            else {
                return result("Data source not confirmed. Implementation remains blocked.", true);
            };
            source = answer;
            if source["answers"][0]["labels"][0] == "Other" {
                let Some(answer) = self.questions.ask(json!([{"question":"Which data source should this widget use? Describe the shop or existing API, or choose sample data.","options":[{"label":"Sample data","description":"A preview with clearly labeled sample data"}]}])).await else {
                    return result("Data source not confirmed. Implementation remains blocked.", true);
                };
                source = json!({"platform":"Other","source":answer});
            }
        }
        if decision.needs_store_data {
            *self.questions.source.lock().unwrap() = Some(source.clone());
        }
        self.questions.confirmed.store(true, Ordering::Release);
        result(json!({"widget_data_confirmed":true,"needs_store_data":decision.needs_store_data,"source":source,
            "note":"A store choice is not evidence of available product/recommendation tools. Ask further missing data questions; never invent live data."}).to_string(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn widget_writes_wait_for_decisions_and_user_answers() {
        let (events, mut receiver) = broadcast::channel(8);
        let state = NativeQuestions::new("test".into(), events, Arc::new(RwLock::new(vec![])), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("widget.js");
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(aion_tools::write::WriteTool::new(None)));
        state.register(&mut registry);
        let write = json!({"file_path":path,"content":"widget"});
        state.observe_input("---\nname: shop-widget\n");
        assert!(registry.get("Write").unwrap().execute(write.clone()).await.is_error);
        assert!(!path.exists());
        assert!(state.blocked("ExecCommand") && state.blocked("Spawn") && !state.blocked("Read"));
        assert!(
            !registry
                .get(CONFIRM)
                .unwrap()
                .execute(json!({"needs_store_data":false}))
                .await
                .is_error
        );
        assert!(!registry.get("Write").unwrap().execute(write.clone()).await.is_error);
        assert!(path.exists());
        state.observe_input("Use real products now");
        assert!(state.blocked("Write"));
        let tool = QuestionTool {
            questions: state.clone(),
            confirm: true,
        };
        let task = tokio::spawn(async move { tool.execute(json!({"needs_store_data":true})).await });
        let AgentStreamEvent::Ask(q) = receiver.recv().await.unwrap() else {
            panic!("Missing question");
        };
        assert!(!task.is_finished());
        assert!(state.answer("wrong", None).is_err());
        assert!(
            state
                .answer(
                    q["request_id"].as_str().unwrap(),
                    Some(vec![AskQuestionAnswer {
                        question: PLATFORM.into(),
                        labels: vec!["Shopify".into()]
                    }])
                )
                .is_err()
        );
        state
            .answer(
                q["request_id"].as_str().unwrap(),
                Some(vec![AskQuestionAnswer {
                    question: PLATFORM.into(),
                    labels: vec!["Other".into()],
                }]),
            )
            .unwrap();
        let AgentStreamEvent::Ask(q) = receiver.recv().await.unwrap() else {
            panic!("Missing source question");
        };
        assert!(state.blocked("Write"));
        state
            .answer(
                q["request_id"].as_str().unwrap(),
                Some(vec![AskQuestionAnswer {
                    question: q["questions"][0]["question"].as_str().unwrap().into(),
                    labels: vec!["Sample data".into()],
                }]),
            )
            .unwrap();
        assert!(!task.await.unwrap().is_error && !state.blocked("Write"));
        let tool = QuestionTool {
            questions: state.clone(),
            confirm: false,
        };
        let task = tokio::spawn(async move {
            tool.execute(json!({"questions":[{"key":"api_url","question":"Which API URL?"}]}))
                .await
        });
        let AgentStreamEvent::Ask(q) = receiver.recv().await.unwrap() else {
            panic!("Missing parameter question");
        };
        assert!(registry.get("Write").unwrap().execute(write).await.is_error);
        state
            .answer(
                q["request_id"].as_str().unwrap(),
                Some(vec![AskQuestionAnswer {
                    question: "Which API URL?".into(),
                    labels: vec!["https://example.com/api".into()],
                }]),
            )
            .unwrap();
        let output: Value = serde_json::from_str(&task.await.unwrap().content).unwrap();
        assert_eq!(output["parameters"]["api_url"], "https://example.com/api");
        let tool = QuestionTool {
            questions: state.clone(),
            confirm: false,
        };
        let task = tokio::spawn(async move { tool.execute(json!({"questions":[{"question":"Continue?"}]})).await });
        receiver.recv().await.unwrap();
        state.cancel();
        assert!(task.await.unwrap().is_error && state.blocked("Write"));
        assert!(state.confirmations.read().unwrap().is_empty());
    }
}
