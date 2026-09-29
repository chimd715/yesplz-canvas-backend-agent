use aionui_db::{
    DbError, IConversationRepository, SendRequestInsert, SendRequestReceipt, SqliteConversationRepository,
    init_database_memory, models::MessageRow,
};

fn message(id: &str) -> MessageRow {
    MessageRow {
        id: id.into(),
        conversation_id: "conversation".into(),
        msg_id: Some(id.into()),
        r#type: "text".into(),
        content: r#"{"content":"Hello"}"#.into(),
        position: Some("right".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at: 10,
        backend_turn_id: None,
    }
}

fn receipt(id: &str) -> SendRequestReceipt {
    SendRequestReceipt {
        client_request_id: "request-1".into(),
        payload_sha256: "a".repeat(64),
        msg_id: id.into(),
        turn_id: format!("turn_{id}"),
    }
}

async fn seed(pool: &aionui_db::SqlitePool) {
    sqlx::query("INSERT INTO conversations(id, user_id, name, type, created_at, updated_at) VALUES ('conversation', 'system_default_user', 'Fixture', 'aionrs', 1, 1)")
        .execute(pool).await.unwrap();
}

#[tokio::test]
async fn send_receipt_concurrency_replay_reset_and_ownership() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    let first = message("first");
    let second = message("second");
    let first_receipt = receipt("first");
    let second_receipt = receipt("second");
    let (a, b) = tokio::join!(
        repo.insert_message_with_receipt("system_default_user", &first, &first_receipt),
        repo.insert_message_with_receipt("system_default_user", &second, &second_receipt),
    );
    let saved = match (a.unwrap(), b.unwrap()) {
        (SendRequestInsert::Inserted, SendRequestInsert::Existing(saved)) => {
            assert_eq!(saved, first_receipt);
            saved
        }
        (SendRequestInsert::Existing(saved), SendRequestInsert::Inserted) => {
            assert_eq!(saved, second_receipt);
            saved
        }
        other => panic!("must accept exactly once: {other:?}"),
    };
    let counts: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM messages), (SELECT count(*) FROM conversation_send_receipts), (SELECT count(*) FROM conversation_outbox WHERE entity = 'message')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(counts, (1, 1, 1));
    let updated: i64 = sqlx::query_scalar("SELECT updated_at FROM conversations WHERE id = 'conversation'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(updated, 10);
    assert!(
        repo.get_send_receipt("other", "conversation", "request-1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        repo.insert_message_with_receipt("other", &first, &first_receipt).await,
        Err(DbError::NotFound(_))
    ));
    assert!(
        repo.get_send_receipt("system_default_user", "another", "request-1")
            .await
            .unwrap()
            .is_none()
    );
    let mut changed = first_receipt.clone();
    changed.payload_sha256 = "b".repeat(64);
    assert!(
        matches!(repo.insert_message_with_receipt("system_default_user", &first, &changed).await,
        Err(DbError::Conflict(reason)) if reason == "client_request_id already accepted with another payload")
    );
    repo.delete_messages_by_conversation("system_default_user", "conversation")
        .await
        .unwrap();
    let restarted = SqliteConversationRepository::new(db.pool().clone());
    assert_eq!(
        restarted
            .insert_message_with_receipt("system_default_user", &first, &first_receipt)
            .await
            .unwrap(),
        SendRequestInsert::Existing(saved)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn send_receipt_failure_rolls_back_message_recency_and_outbox() {
    let db = init_database_memory().await.unwrap();
    seed(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    sqlx::query("CREATE TRIGGER reject_receipt BEFORE INSERT ON conversation_send_receipts BEGIN SELECT RAISE(ABORT, 'fixture rejects receipt'); END;")
        .execute(db.pool()).await.unwrap();
    assert!(matches!(
        repo.insert_message_with_receipt("system_default_user", &message("first"), &receipt("first"))
            .await,
        Err(DbError::Query(_))
    ));
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM messages), (SELECT count(*) FROM conversation_send_receipts), (SELECT count(*) FROM conversation_outbox), (SELECT updated_at FROM conversations WHERE id = 'conversation')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(counts, (0, 0, 1, 1));
    sqlx::query("DROP TRIGGER reject_receipt")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo.insert_message_with_receipt("system_default_user", &message("first"), &receipt("first"))
            .await
            .unwrap(),
        SendRequestInsert::Inserted
    );
}
