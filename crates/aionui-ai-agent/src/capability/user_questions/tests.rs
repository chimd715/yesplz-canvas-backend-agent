use super::*;
use tokio::time::{Duration, timeout};

fn fixture() -> (UserQuestions, broadcast::Receiver<AgentStreamEvent>) {
    let (tx, rx) = broadcast::channel(8);
    (
        UserQuestions::new("conversation-a".into(), Arc::new(RwLock::new(vec![])), tx),
        rx,
    )
}
fn input() -> Value {
    json!({"questions":[
        {"header":"Style","question":"Which style?","options":[{"label":"Simple"},{"label":"Detailed"}]},
        {"question":"Which sections?","options":[{"label":"Summary"},{"label":"Details"}],"multiSelect":true}
    ]})
}
fn answers() -> Vec<AskQuestionAnswer> {
    vec![
        AskQuestionAnswer {
            question: "Which style?".into(),
            labels: vec!["Simple".into()],
        },
        AskQuestionAnswer {
            question: "Which sections?".into(),
            labels: vec!["Summary".into(), "Custom section".into()],
        },
    ]
}
async fn pending(
    questions: &UserQuestions,
    rx: &mut broadcast::Receiver<AgentStreamEvent>,
) -> (String, tokio::task::JoinHandle<ToolResult>) {
    let tool = AskUserQuestionTool(questions.clone());
    let task = tokio::spawn(async move { tool.execute(input()).await });
    let AgentStreamEvent::Ask(payload) = timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap() else {
        panic!("Expected ask frame");
    };
    assert_eq!(payload["session_id"], "conversation-a");
    (payload["request_id"].as_str().unwrap().to_owned(), task)
}

#[tokio::test]
async fn native_question_waits_and_returns_complete_batch_with_recovery() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    assert!(!task.is_finished());
    let recovery = questions.confirmations.read().unwrap().clone();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].id, id);
    assert_eq!(recovery[0].questions.as_ref().unwrap().as_array().unwrap().len(), 2);
    assert!(
        recovery[0].options.is_empty(),
        "Questions cannot be generic allow/deny permissions"
    );
    questions.answer(&id, Some(answers())).unwrap();
    let result = task.await.unwrap();
    assert!(!result.is_error);
    let result: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(result["answers"].as_array().unwrap().len(), 2);
    assert!(questions.confirmations.read().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_answer_batches_do_not_consume_pending_request() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    let cases = [
        (vec![], "Complete question answers required"),
        (
            vec![AskQuestionAnswer {
                question: "Which style?".into(),
                labels: vec!["Simple".into()],
            }],
            "Complete question answers required",
        ),
        (
            vec![
                AskQuestionAnswer {
                    question: "Unknown".into(),
                    labels: vec!["Simple".into()],
                },
                AskQuestionAnswer {
                    question: "Which sections?".into(),
                    labels: vec!["Summary".into()],
                },
            ],
            "Unknown question in answers",
        ),
        (
            vec![
                AskQuestionAnswer {
                    question: "Which style?".into(),
                    labels: vec!["Simple".into()],
                },
                AskQuestionAnswer {
                    question: "Which style?".into(),
                    labels: vec!["Simple".into()],
                },
            ],
            "Duplicate question answer",
        ),
        (
            vec![
                AskQuestionAnswer {
                    question: "Which style?".into(),
                    labels: vec!["Simple".into(), "Detailed".into()],
                },
                AskQuestionAnswer {
                    question: "Which sections?".into(),
                    labels: vec!["Summary".into()],
                },
            ],
            "Invalid number of selected answers",
        ),
        (
            vec![
                AskQuestionAnswer {
                    question: "Which style?".into(),
                    labels: vec!["  ".into()],
                },
                AskQuestionAnswer {
                    question: "Which sections?".into(),
                    labels: vec!["Summary".into()],
                },
            ],
            "Answers must be nonempty, bounded and unique",
        ),
    ];
    for (answers, expected) in cases {
        assert_eq!(
            questions.answer(&id, Some(answers)).unwrap_err().to_string(),
            format!("Bad request: {expected}")
        );
        assert!(questions.is_pending(&id));
        assert!(!task.is_finished());
    }
    questions.answer(&id, None).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&task.await.unwrap().content).unwrap(),
        json!({"declined":true})
    );
}

#[tokio::test]
async fn retries_are_idempotent_and_cannot_change_accepted_answers() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    questions.answer(&id, Some(answers())).unwrap();
    questions.answer(&id, Some(answers())).unwrap();
    assert_eq!(
        questions.answer(&id, None).unwrap_err().to_string(),
        "Conflict: Question already answered"
    );
    assert!(!task.await.unwrap().is_error);
}

#[tokio::test]
async fn cancel_unblocks_tool_and_removes_recovery_and_unknown_scope_is_rejected() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    let (other, _) = fixture();
    assert_eq!(
        other.answer(&id, None).unwrap_err().to_string(),
        "Not found: Pending question not found"
    );
    questions.cancel_all();
    let result = task.await.unwrap();
    assert!(result.is_error);
    assert_eq!(result.content, "Question canceled");
    assert!(questions.confirmations.read().unwrap().is_empty());
    assert_eq!(
        questions.answer(&id, None).unwrap_err().to_string(),
        "Not found: Pending question not found"
    );
}

#[tokio::test]
async fn aborted_execution_cleans_pending_state_and_allows_next_question() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    task.abort();
    let _ = task.await;
    assert!(!questions.is_pending(&id));
    assert!(questions.confirmations.read().unwrap().is_empty());
    let (next, task) = pending(&questions, &mut rx).await;
    questions.answer(&next, None).unwrap();
    assert!(!task.await.unwrap().is_error);
}

#[tokio::test]
async fn concurrent_request_is_rejected_without_overwriting_first() {
    let (questions, mut rx) = fixture();
    let (id, task) = pending(&questions, &mut rx).await;
    let second = AskUserQuestionTool(questions.clone()).execute(input()).await;
    assert!(second.is_error);
    assert_eq!(second.content, "Another question is already pending");
    assert!(questions.is_pending(&id));
    questions.answer(&id, None).unwrap();
    task.await.unwrap();
}

#[test]
fn input_validation_bounds_utf8_duplicates_and_schema_without_echoing_input() {
    let invalid = [
        json!({"questions":[]}),
        json!({"questions":[{"question":" ","options":[]}]}),
        json!({"questions":[{"question":"x","options":[{"label":"x"},{"label":"x"}]}]}),
        json!({"questions":[{"question":"한".repeat(1000),"options":[]}]}),
        json!({"questions":[],"secret":"hidden"}),
    ];
    for value in invalid {
        let message = parse_questions(value).err().unwrap();
        assert!(!message.contains("hidden"));
        assert!(!message.contains('한'));
    }
    let (questions, _) = fixture();
    let tool = AskUserQuestionTool(questions);
    assert_eq!(tool.name(), "AskUserQuestion");
    assert!(!tool.is_deferred());
    assert!(!tool.is_concurrency_safe(&json!({})));
    assert_eq!(tool.input_schema()["properties"]["questions"]["maxItems"], 4);
    assert!(tool.description().contains("request_secret"));
    assert_eq!(tool.describe(&json!({"secret":"hidden"})), "Request customer answers");
}
