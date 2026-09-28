use aionui_db::{
    ConversationOutboxError, IConversationOutboxRepository, SqliteConversationOutboxRepository, init_database_memory,
};

async fn conversation(pool: &aionui_db::SqlitePool, id: &str, user_id: &str) {
    sqlx::query(
        "INSERT INTO conversations (id, user_id, name, type, created_at, updated_at) \
         VALUES (?, ?, 'Fixture', 'acp', 1, 1)",
    )
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn delivery_is_ordered_scoped_replayable_and_requires_export_before_ack() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    let repo = SqliteConversationOutboxRepository::new(pool.clone());
    sqlx::query("INSERT INTO users (id, password_hash, created_at, updated_at) VALUES ('other', 'fixture', 1, 1)")
        .execute(pool)
        .await
        .unwrap();
    conversation(pool, "first", "system_default_user").await;
    conversation(pool, "private", "other").await;
    conversation(pool, "second", "system_default_user").await;

    assert!(matches!(
        repo.acknowledge("system_default_user", 1).await,
        Err(ConversationOutboxError::UnexportedAck)
    ));
    let first = repo.export("system_default_user", None, 1).await.unwrap();
    assert_eq!(first.after_sequence, 0);
    assert_eq!(first.acknowledged_sequence, 0);
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].conversation_id, "first");
    assert!(first.has_more);
    let retry = repo.export("system_default_user", None, 1).await.unwrap();
    assert_eq!(retry.through_sequence, first.through_sequence);
    assert!(matches!(
        repo.export("system_default_user", Some(first.through_sequence), 1).await,
        Err(ConversationOutboxError::UnacknowledgedCursor)
    ));
    assert!(matches!(
        repo.acknowledge("system_default_user", first.latest_sequence).await,
        Err(ConversationOutboxError::UnexportedAck)
    ));
    repo.acknowledge("system_default_user", first.through_sequence).await.unwrap();
    let next = repo.export("system_default_user", None, 100).await.unwrap();
    assert_eq!(next.events.len(), 1);
    assert_eq!(next.events[0].conversation_id, "second");
    assert!(next.through_sequence > first.through_sequence + 1); // Another user's gap is valid.
    assert!(!next.has_more);
    assert!(matches!(
        repo.acknowledge("system_default_user", first.through_sequence + 1).await,
        Err(ConversationOutboxError::UnexportedAck)
    ));
    repo.acknowledge("system_default_user", next.through_sequence).await.unwrap();
    assert_eq!(
        repo.acknowledge("system_default_user", first.through_sequence).await.unwrap(),
        next.through_sequence
    );
    let replay = repo.export("system_default_user", Some(0), 100).await.unwrap();
    assert_eq!(replay.events.len(), 2);
    assert!(replay.events.iter().all(|event| event.user_id == "system_default_user"));
    assert!(repo.export("system_default_user", None, 100).await.unwrap().events.is_empty());
    assert_eq!(
        repo.export("other", None, 100).await.unwrap().events[0].conversation_id,
        "private"
    );
}

#[tokio::test]
async fn invalid_cursors_and_oversized_records_never_advance_delivery() {
    let db = init_database_memory().await.unwrap();
    let repo = SqliteConversationOutboxRepository::new(db.pool().clone());
    assert!(matches!(
        repo.export("system_default_user", None, 0).await,
        Err(ConversationOutboxError::InvalidLimit)
    ));
    assert!(matches!(
        repo.export("system_default_user", None, 101).await,
        Err(ConversationOutboxError::InvalidLimit)
    ));
    assert!(matches!(
        repo.export("system_default_user", Some(-1), 100).await,
        Err(ConversationOutboxError::InvalidCursor)
    ));
    assert!(matches!(
        repo.acknowledge("system_default_user", 9_007_199_254_740_992).await,
        Err(ConversationOutboxError::InvalidCursor)
    ));
    conversation(db.pool(), "large", "system_default_user").await;
    // The normal insert is exportable; the following direct update is not.
    let first = repo.export("system_default_user", None, 100).await.unwrap();
    repo.acknowledge("system_default_user", first.through_sequence).await.unwrap();
    sqlx::query("UPDATE conversations SET extra = ? WHERE id = 'large'")
        .bind("x".repeat(16 * 1024 * 1024))
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        repo.export("system_default_user", None, 100).await,
        Err(ConversationOutboxError::RecordTooLarge)
    ));
    let watermarks: (i64, i64) = sqlx::query_as(
        "SELECT acknowledged_sequence, exported_sequence FROM conversation_outbox_delivery \
         WHERE subject_id = 'system_default_user'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(watermarks, (first.through_sequence, first.through_sequence));
}
