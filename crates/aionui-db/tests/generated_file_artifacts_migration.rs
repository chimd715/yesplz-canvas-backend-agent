use std::borrow::Cow;
use std::path::Path;

use aionui_db::{
    ConversationArtifactRow, DbError, IConversationRepository, SqliteConversationRepository, init_database_memory,
};
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

async fn run_migrations_through(pool: &sqlx::SqlitePool, max_version: i64) {
    // Match the production migration connection's table-rebuild settings.
    sqlx::query("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON")
        .execute(pool)
        .await
        .unwrap();
    let full = Migrator::new(Path::new("migrations")).await.unwrap();
    let migrations = full
        .migrations
        .iter()
        .filter(|migration| migration.version <= max_version)
        .cloned()
        .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
    .run(pool)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON; PRAGMA legacy_alter_table = OFF")
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_conversations(pool: &sqlx::SqlitePool) {
    for user in ["system_default_user", "artifact-owner", "artifact-other"] {
        sqlx::query(
            "INSERT OR IGNORE INTO users (id, username, password_hash, created_at, updated_at) \
             VALUES (?, ?, 'hash', 1, 1)",
        )
        .bind(user)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    }
    for (id, deleted_at) in [("live", None), ("retained", Some(99_i64))] {
        sqlx::query(
            "INSERT INTO conversations (id, user_id, name, type, extra, status, created_at, updated_at, deleted_at) \
             VALUES (?, 'artifact-owner', 'Document', 'aionrs', '{}', 'finished', 1, 2, ?)",
        )
        .bind(id)
        .bind(deleted_at)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn artifact_indexes(pool: &sqlx::SqlitePool) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'conversation_artifacts' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn migration_045_preserves_old_artifacts_indexes_and_constraints() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations_through(&pool, 44).await;
    seed_conversations(&pool).await;
    for kind in ["cron_trigger", "skill_suggest"] {
        for status in ["active", "pending", "dismissed", "saved"] {
            for conversation in ["live", "retained"] {
                sqlx::query(
                    "INSERT INTO conversation_artifacts \
                     (id, conversation_id, cron_job_id, kind, status, payload, created_at, updated_at) \
                     VALUES (?, ?, 'cron-1', ?, ?, '{\"preserve\":true}', 10, 20)",
                )
                .bind(format!("{kind}-{status}-{conversation}"))
                .bind(conversation)
                .bind(kind)
                .bind(status)
                .execute(&pool)
                .await
                .unwrap();
            }
        }
    }
    let before: Vec<ConversationArtifactRow> = sqlx::query_as("SELECT * FROM conversation_artifacts ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let indexes = artifact_indexes(&pool).await;
    assert_eq!(indexes.len(), 6, "five lookup indexes and the primary key must remain");
    let error = sqlx::query(
        "INSERT INTO conversation_artifacts (id, conversation_id, kind, created_at, updated_at) \
         VALUES ('generated-before-upgrade', 'live', 'generated_file', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint failed"));

    run_migrations_through(&pool, 45).await;
    let after: Vec<ConversationArtifactRow> = sqlx::query_as("SELECT * FROM conversation_artifacts ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert_eq!(artifact_indexes(&pool).await, indexes);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT deleted_at FROM conversations WHERE id = 'retained'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        99
    );

    for (kind, status) in [("unknown", "active"), ("generated_file", "unknown")] {
        let error = sqlx::query(
            "INSERT INTO conversation_artifacts (id, conversation_id, kind, status, created_at, updated_at) \
             VALUES ('invalid', 'live', ?, ?, 1, 1)",
        )
        .bind(kind)
        .bind(status)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "wrong error: {error}"
        );
    }
    let error = sqlx::query(
        "INSERT INTO conversation_artifacts (id, conversation_id, kind, created_at, updated_at) \
         VALUES ('orphan', 'missing', 'generated_file', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("FOREIGN KEY constraint failed"));
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("DELETE FROM conversations WHERE id = 'live'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_artifacts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        8,
        "parent cascade removes only the physically deleted conversation's artifacts"
    );
}

#[tokio::test]
async fn fresh_generated_file_artifacts_keep_repository_ownership_and_retention() {
    let db = init_database_memory().await.unwrap();
    seed_conversations(db.pool()).await;
    let repo = SqliteConversationRepository::new(db.pool().clone());
    let artifact = ConversationArtifactRow {
        id: "generated-document".into(),
        conversation_id: "live".into(),
        cron_job_id: None,
        kind: "generated_file".into(),
        status: "active".into(),
        payload: "{\"file_name\":\"report.html\"}".into(),
        created_at: 10,
        updated_at: 20,
    };
    let stored = repo.upsert_artifact("artifact-owner", &artifact).await.unwrap();
    assert_eq!(stored.kind, "generated_file");
    assert_eq!(stored.payload, artifact.payload);
    assert!(
        repo.get_artifact("artifact-other", "live", &artifact.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        repo.upsert_artifact("artifact-other", &artifact).await,
        Err(DbError::NotFound(_))
    ));
    repo.delete("artifact-owner", "live").await.unwrap();
    assert!(
        repo.get_artifact("artifact-owner", "live", &artifact.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT payload FROM conversation_artifacts WHERE id = ?")
            .bind(&artifact.id)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        artifact.payload,
        "customer deletion retains the generated artifact"
    );
}
