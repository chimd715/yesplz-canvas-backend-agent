#![allow(clippy::disallowed_types)]

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};

use aionui_api_types::{
    AcknowledgeConversationOutboxRequest, AcknowledgeConversationOutboxResponse, ApiResponse,
    ConversationOutboxEvent, ConversationOutboxQuery, ConversationOutboxResponse,
};
use aionui_common::ApiError;
use aionui_db::ConversationOutboxError;

use crate::routes::{AuthRouterState, require_bootstrap_secret};
use crate::service::OutboxServiceError;

fn outbox_error(error: OutboxServiceError) -> ApiError {
    match error {
        OutboxServiceError::UserUnavailable => ApiError::NotFound("Active Core user not found".into()),
        OutboxServiceError::Outbox(
            error @ (ConversationOutboxError::InvalidCursor | ConversationOutboxError::InvalidLimit),
        ) => ApiError::BadRequest(error.to_string()),
        OutboxServiceError::Outbox(
            error @ (ConversationOutboxError::UnacknowledgedCursor | ConversationOutboxError::UnexportedAck),
        ) => ApiError::Conflict(error.to_string()),
        OutboxServiceError::Outbox(ConversationOutboxError::RecordTooLarge) => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "OUTBOX_RECORD_TOO_LARGE",
            "Conversation change exceeds the export size limit.",
            None,
        ),
        error => {
            tracing::error!(error = %error, "conversation outbox operation failed");
            ApiError::Internal("Conversation synchronization unavailable".into())
        }
    }
}

pub(crate) async fn export(
    State(state): State<AuthRouterState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    query: Result<Query<ConversationOutboxQuery>, QueryRejection>,
) -> Result<Json<ApiResponse<ConversationOutboxResponse>>, ApiError> {
    require_bootstrap_secret(&headers, state.bootstrap_secret.as_deref().map(AsRef::as_ref))?;
    let Query(query) = query.map_err(|_| ApiError::BadRequest("Invalid conversation outbox query".into()))?;
    let batch = state
        .conversation_outbox
        .export(&user_id, query.after_sequence, query.limit.unwrap_or(100))
        .await
        .map_err(outbox_error)?;
    tracing::debug!(
        user_id,
        through_sequence = batch.through_sequence,
        count = batch.events.len(),
        "conversation outbox exported"
    );
    Ok(Json(ApiResponse::ok(ConversationOutboxResponse {
        schema_version: 1,
        after_sequence: batch.after_sequence,
        through_sequence: batch.through_sequence,
        acknowledged_sequence: batch.acknowledged_sequence,
        latest_sequence: batch.latest_sequence,
        has_more: batch.has_more,
        events: batch
            .events
            .into_iter()
            .map(|event| ConversationOutboxEvent {
                sequence: event.sequence,
                user_id: event.user_id,
                conversation_id: event.conversation_id,
                entity: event.entity,
                entity_id: event.entity_id,
                operation: event.operation,
                payload: event.payload,
            })
            .collect(),
    })))
}

pub(crate) async fn acknowledge(
    State(state): State<AuthRouterState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    body: Result<Json<AcknowledgeConversationOutboxRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AcknowledgeConversationOutboxResponse>>, ApiError> {
    require_bootstrap_secret(&headers, state.bootstrap_secret.as_deref().map(AsRef::as_ref))?;
    let Json(request) = body.map_err(ApiError::from)?;
    let acknowledged_sequence = state
        .conversation_outbox
        .acknowledge(&user_id, request.through_sequence)
        .await
        .map_err(outbox_error)?;
    tracing::info!(user_id, acknowledged_sequence, "conversation outbox acknowledged");
    Ok(Json(ApiResponse::ok(AcknowledgeConversationOutboxResponse {
        acknowledged_sequence,
    })))
}
