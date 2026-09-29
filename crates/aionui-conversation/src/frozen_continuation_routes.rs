//! Internal control routes: a bootstrap credential and a current user session
//! are both required. Runtime helper tokens must never grant archive authority.
#![allow(clippy::disallowed_types)]
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Extension, Request, State, rejection::JsonRejection};
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};

use aionui_ai_agent::manager::aionrs::FrozenNativeSession;
use aionui_api_types::{
    ApiResponse, FrozenArtifactMappingResponse, FrozenContinuationImportResponse, FrozenContinuationRequest,
    FrozenContinuationResponse, FrozenMessageMappingResponse, PreparedNativeContinuationResponse,
};
use aionui_auth::{
    AuthIdentityMode, CurrentUser, RUNTIME_TOKEN_HEADER, RateLimiter, authenticated_action_rate_limit_middleware,
    require_bootstrap_secret,
};
use aionui_common::ApiError;

use crate::{
    ConversationError, FrozenContinuationPreparation, FrozenContinuationRouterState, PreparedFrozenContinuation,
};

const BODY_LIMIT: usize = 128 * 1024 * 1024;

/// Composition must add JWT-only auth and CSRF protection. This route group
/// additionally rejects Local mode and runtime-token headers before body reads.
pub fn frozen_continuation_routes(state: FrozenContinuationRouterState) -> Router {
    Router::new()
        .route(
            "/api/conversations/internal/frozen-continuations/prepare",
            post(prepare),
        )
        .route(
            "/api/conversations/internal/frozen-continuations/recover",
            post(recover),
        )
        .route_layer(from_fn_with_state(
            Arc::new(RateLimiter::authenticated_action()),
            authenticated_action_rate_limit_middleware,
        ))
        .route_layer(from_fn_with_state(state.clone(), control_guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

async fn control_guard(
    State(state): State<FrozenContinuationRouterState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    require_bootstrap_secret(request.headers(), state.bootstrap_secret.as_deref())?;
    if state.identity_mode == AuthIdentityMode::Local || request.headers().contains_key(RUNTIME_TOKEN_HEADER) {
        return Err(ApiError::Forbidden(
            "Frozen continuation control requires a user session".into(),
        ));
    }
    if state.runtime_build_sha256.is_none() {
        return Err(ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "NATIVE_BUILD_UNAVAILABLE",
            "Running native build identity is unavailable",
            None,
        ));
    }
    if let Some(length) = request.headers().get(header::CONTENT_LENGTH) {
        let length = length
            .to_str()
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| ApiError::BadRequest("Invalid content length".into()))?;
        if length > BODY_LIMIT as u64 {
            return Err(ApiError::coded(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "Frozen continuation request exceeds size limit",
                None,
            ));
        }
    }
    Ok(next.run(request).await)
}

async fn prepare(
    State(state): State<FrozenContinuationRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<FrozenContinuationRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<FrozenContinuationResponse>>, ApiError> {
    execute(state, user, body, false).await
}

async fn recover(
    State(state): State<FrozenContinuationRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<FrozenContinuationRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<FrozenContinuationResponse>>, ApiError> {
    execute(state, user, body, true).await
}

async fn execute(
    state: FrozenContinuationRouterState,
    user: CurrentUser,
    body: Result<Json<FrozenContinuationRequest>, JsonRejection>,
    read_only: bool,
) -> Result<Json<ApiResponse<FrozenContinuationResponse>>, ApiError> {
    let Json(body) = body.map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::coded(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "Frozen continuation request exceeds size limit",
                None,
            )
        } else {
            ApiError::BadRequest("Invalid frozen continuation request".into())
        }
    })?;
    let input = FrozenContinuationPreparation {
        rows: serde_json::from_value(body.rows)
            .map_err(|_| ApiError::BadRequest("Invalid frozen continuation rows".into()))?,
        native: FrozenNativeSession {
            owner_id: body.native.owner_id,
            runtime_build_sha256: body.native.runtime_build_sha256,
            manifest_json: body.native.manifest_json,
            session_json: body.native.session_json,
        },
    };
    let build = state.runtime_build_sha256.as_deref().ok_or_else(|| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "NATIVE_BUILD_UNAVAILABLE",
            "Running native build identity is unavailable",
            None,
        )
    })?;
    let prepared = if read_only {
        state
            .service
            .recover_frozen_continuation(&user.id, build, &state.native_session_directory, input)
            .await
    } else {
        state
            .service
            .prepare_frozen_continuation(&user.id, build, &state.native_session_directory, input)
            .await
    }
    .map_err(|error| match error {
        // A database error can contain SQL, paths or private record contents.
        ConversationError::Internal { .. } => ApiError::Internal("Frozen continuation operation failed".into()),
        error => error.into(),
    })?;
    Ok(Json(ApiResponse::ok(response(prepared, user.id, build.to_owned()))))
}

fn response(
    value: PreparedFrozenContinuation,
    core_user_id: String,
    runtime_build_sha256: String,
) -> FrozenContinuationResponse {
    FrozenContinuationResponse {
        imported: FrozenContinuationImportResponse {
            conversation_id: value.imported.conversation_id,
            copied_messages: value
                .imported
                .copied_messages
                .into_iter()
                .map(|item| FrozenMessageMappingResponse {
                    source_message_id: item.source_message_id,
                    message_id: item.message_id,
                    original_turn_id: item.original_turn_id,
                })
                .collect(),
            copied_artifacts: value
                .imported
                .copied_artifacts
                .into_iter()
                .map(|item| FrozenArtifactMappingResponse {
                    source_artifact_id: item.source_artifact_id,
                    artifact_id: item.artifact_id,
                })
                .collect(),
        },
        native: PreparedNativeContinuationResponse {
            conversation_id: value.native.conversation_id,
            source_conversation_id: value.native.source_conversation_id,
            source_turn_id: value.native.source_turn_id,
            native_state_sha256: value.native.native_state_sha256,
            session_bytes: value.native.session_bytes,
        },
        core_user_id,
        runtime_build_sha256,
    }
}
