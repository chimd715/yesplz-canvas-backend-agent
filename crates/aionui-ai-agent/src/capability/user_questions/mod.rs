//! Structured customer questions use a dedicated channel, independent of tool permissions.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use aion_protocol::events::ToolCategory;
use aion_tools::Tool;
use aion_types::tool::{JsonSchema, ToolResult};
use aionui_api_types::AskQuestionAnswer;
use aionui_common::Confirmation;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, oneshot};

use crate::error::AgentError;
use crate::protocol::events::AgentStreamEvent;

pub(crate) const TOOL_NAME: &str = "AskUserQuestion";
const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 8;
const MAX_TEXT_BYTES: usize = 2048;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Question {
    question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header: Option<String>,
    options: Vec<QuestionOption>,
    #[serde(default, rename = "multiSelect", alias = "multi_select")]
    multi_select: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuestionOption {
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionInput {
    questions: Vec<Question>,
}

struct Pending {
    questions: Vec<Question>,
    sender: oneshot::Sender<Value>,
}

#[derive(Default)]
struct Registry {
    pending: HashMap<String, Pending>,
    // Only fingerprints, never answers. Bounded receipts let a lost HTTP response retry safely.
    completed: VecDeque<(String, [u8; 32])>,
}

#[derive(Clone)]
pub(crate) struct UserQuestions {
    conversation_id: String,
    registry: Arc<Mutex<Registry>>,
    confirmations: Arc<RwLock<Vec<Confirmation>>>,
    event_tx: broadcast::Sender<AgentStreamEvent>,
}

impl UserQuestions {
    pub(crate) fn new(
        conversation_id: String,
        confirmations: Arc<RwLock<Vec<Confirmation>>>,
        event_tx: broadcast::Sender<AgentStreamEvent>,
    ) -> Self {
        Self {
            conversation_id,
            registry: Arc::new(Mutex::new(Registry::default())),
            confirmations,
            event_tx,
        }
    }

    pub(crate) fn is_pending(&self, request_id: &str) -> bool {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .contains_key(request_id)
    }

    pub(crate) fn cancel_all(&self) {
        let ids: HashSet<_> = self
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .drain()
            .map(|(id, _)| id)
            .collect();
        self.confirmations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| !ids.contains(&c.call_id));
    }

    fn remove(&self, request_id: &str) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(request_id);
        self.confirmations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.call_id != request_id);
    }

    pub(crate) fn answer(&self, request_id: &str, answers: Option<Vec<AskQuestionAnswer>>) -> Result<(), AgentError> {
        let response = answer_value(answers)?;
        let fingerprint: [u8; 32] = Sha256::digest(response.to_string().as_bytes()).into();
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, accepted)) = registry.completed.iter().find(|(id, _)| id == request_id) {
            return if *accepted == fingerprint {
                Ok(())
            } else {
                Err(AgentError::conflict("Question already answered"))
            };
        }
        let pending = registry
            .pending
            .get(request_id)
            .ok_or_else(|| AgentError::not_found("Pending question not found"))?;
        if response["declined"] != true {
            validate_answers(&pending.questions, &response["answers"])?;
        }
        let pending = registry
            .pending
            .remove(request_id)
            .expect("pending question checked under lock");
        pending
            .sender
            .send(response)
            .map_err(|_| AgentError::conflict("Question no longer pending"))?;
        registry.completed.push_back((request_id.to_owned(), fingerprint));
        if registry.completed.len() > 64 {
            registry.completed.pop_front();
        }
        self.confirmations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.call_id != request_id);
        tracing::info!(conversation_id = %self.conversation_id, request_id, "Structured customer question answered");
        Ok(())
    }

    async fn request(&self, input: Value) -> ToolResult {
        let questions = match parse_questions(input) {
            Ok(questions) => questions,
            Err(message) => return tool_error(message),
        };
        let request_id = format!("ask_{}", uuid::Uuid::new_v4());
        let (sender, receiver) = oneshot::channel();
        let payload = json!({"session_id": self.conversation_id, "request_id": request_id, "questions": questions});
        {
            let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            if !registry.pending.is_empty() {
                return tool_error("Another question is already pending");
            }
            registry
                .pending
                .insert(request_id.clone(), Pending { questions, sender });
            self.confirmations
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push(Confirmation {
                    id: request_id.clone(),
                    call_id: request_id.clone(),
                    title: Some("Customer questions".into()),
                    action: Some(TOOL_NAME.into()),
                    description: "Waiting for customer answers".into(),
                    command_type: None,
                    options: vec![],
                    questions: Some(payload["questions"].clone()),
                });
        }
        let _cleanup = PendingGuard {
            questions: self.clone(),
            request_id: request_id.clone(),
        };
        let _ = self.event_tx.send(AgentStreamEvent::Ask(payload));
        tracing::info!(conversation_id = %self.conversation_id, request_id, "Structured customer question requested");
        match receiver.await {
            Ok(answer) => ToolResult {
                content: answer.to_string(),
                is_error: false,
            },
            Err(_) => tool_error("Question canceled"),
        }
    }
}

struct PendingGuard {
    questions: UserQuestions,
    request_id: String,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.questions.remove(&self.request_id);
    }
}

fn tool_error(message: &str) -> ToolResult {
    ToolResult {
        content: message.into(),
        is_error: true,
    }
}

fn parse_questions(input: Value) -> Result<Vec<Question>, &'static str> {
    if input.to_string().len() > 32_768 {
        return Err("Question request exceeds size limit");
    }
    let input: QuestionInput = serde_json::from_value(input).map_err(|_| "Invalid question request")?;
    if input.questions.is_empty() || input.questions.len() > MAX_QUESTIONS {
        return Err("Expected one to four questions");
    }
    let mut names = HashSet::new();
    for question in &input.questions {
        if question.question.trim().is_empty()
            || question.question.len() > MAX_TEXT_BYTES
            || !names.insert(&question.question)
        {
            return Err("Questions must be nonempty, bounded and unique");
        }
        if question.header.as_ref().is_some_and(|header| header.len() > 128) || question.options.len() > MAX_OPTIONS {
            return Err("Question header or options exceed limits");
        }
        let mut labels = HashSet::new();
        for option in &question.options {
            if option.label.trim().is_empty()
                || option.label.len() > 256
                || option.label == "__aionui_other__"
                || !labels.insert(&option.label)
                || option.description.as_ref().is_some_and(|value| value.len() > 1024)
            {
                return Err("Question options must be nonempty, bounded and unique");
            }
        }
    }
    Ok(input.questions)
}

fn answer_value(answers: Option<Vec<AskQuestionAnswer>>) -> Result<Value, AgentError> {
    let Some(answers) = answers else {
        return Ok(json!({"declined": true}));
    };
    if answers.is_empty() || answers.len() > MAX_QUESTIONS {
        return Err(AgentError::bad_request("Complete question answers required"));
    }
    let mut answers: Vec<_> = answers
        .into_iter()
        .map(|answer| json!({"question": answer.question, "labels": answer.labels}))
        .collect();
    answers.sort_by(|a, b| a["question"].as_str().cmp(&b["question"].as_str()));
    let response = json!({"answers": answers});
    if response.to_string().len() > 16_384 {
        return Err(AgentError::bad_request("Question answers exceed size limit"));
    }
    Ok(response)
}

fn validate_answers(questions: &[Question], answers: &Value) -> Result<(), AgentError> {
    let answers = answers.as_array().expect("answer_value produces an array");
    if answers.len() != questions.len() {
        return Err(AgentError::bad_request("Complete question answers required"));
    }
    let mut seen = HashSet::new();
    for answer in answers {
        let name = answer["question"].as_str().expect("answer_value produces strings");
        let question = questions
            .iter()
            .find(|q| q.question == name)
            .ok_or_else(|| AgentError::bad_request("Unknown question in answers"))?;
        if !seen.insert(name) {
            return Err(AgentError::bad_request("Duplicate question answer"));
        }
        let labels = answer["labels"].as_array().expect("answer_value produces arrays");
        if labels.is_empty() || labels.len() > MAX_OPTIONS + 1 || (!question.multi_select && labels.len() != 1) {
            return Err(AgentError::bad_request("Invalid number of selected answers"));
        }
        let mut selected = HashSet::new();
        for label in labels {
            let label = label.as_str().expect("answer_value produces strings");
            if label.trim().is_empty() || label.len() > MAX_TEXT_BYTES || !selected.insert(label) {
                return Err(AgentError::bad_request("Answers must be nonempty, bounded and unique"));
            }
        }
    }
    Ok(())
}

pub(crate) struct AskUserQuestionTool(pub(crate) UserQuestions);
#[async_trait::async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }
    fn description(&self) -> &str {
        "Ask the customer one to four questions. Waits for a complete answer batch or an explicit decline. Never request passwords, tokens or credentials here; use the secure request_secret tool for those."
    }
    fn input_schema(&self) -> JsonSchema {
        json!({"type":"object","additionalProperties":false,"required":["questions"],"properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["question","options"],"properties":{"header":{"type":"string","maxLength":128},"question":{"type":"string","maxLength":2048},"multiSelect":{"type":"boolean"},"options":{"type":"array","maxItems":8,"items":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"label":{"type":"string","maxLength":256},"description":{"type":"string","maxLength":1024}}}}}}}}})
    }
    fn is_concurrency_safe(&self, _: &Value) -> bool {
        false
    }
    async fn execute(&self, input: Value) -> ToolResult {
        self.0.request(input).await
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }
    fn describe(&self, _: &Value) -> String {
        "Request customer answers".into()
    }
}

#[cfg(test)]
mod tests;
