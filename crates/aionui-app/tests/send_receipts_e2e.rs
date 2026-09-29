//! Native send acceptance protocol through the real auth/router/SQLite stack;
//! agent execution uses the existing mock factory and never calls an LLM.
mod common;

use axum::http::StatusCode;
use common::{body_json, build_app_with_mock_agents, get_request, get_with_token, json_with_token, setup_and_login};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn send_receipts_http_auth_csrf_replay_and_owner_isolation() {
    let (mut app, services) = build_app_with_mock_agents().await;
    let anonymous = app
        .clone()
        .oneshot(get_request("/api/conversations/unknown/requests/request-1"))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let workspace = tempfile::tempdir().unwrap();
    let create = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/conversations",
            json!({ "type": "aionrs", "extra": { "workspace": workspace.path() } }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let created = body_json(create).await;
    let id = created["data"]["id"].as_str().unwrap();
    let send_url = format!("/api/conversations/{id}/messages");
    let receipt_url = format!("/api/conversations/{id}/requests/request-1");
    let absent = app.clone().oneshot(get_with_token(&receipt_url, &token)).await.unwrap();
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    let body = json!({ "content": "hello", "client_request_id": "request-1" });
    let no_csrf = axum::http::Request::builder()
        .method("POST")
        .uri(&send_url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(no_csrf).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let first = app
        .clone()
        .oneshot(json_with_token("POST", &send_url, body.clone(), &token, &csrf))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first = body_json(first).await;
    assert_eq!(first["data"]["client_request_id"], "request-1");
    let repeat = app
        .clone()
        .oneshot(json_with_token("POST", &send_url, body, &token, &csrf))
        .await
        .unwrap();
    assert_eq!(repeat.status(), StatusCode::ACCEPTED);
    let repeat = body_json(repeat).await;
    let read = app.clone().oneshot(get_with_token(&receipt_url, &token)).await.unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let read = body_json(read).await;
    for response in [repeat, read] {
        for key in ["client_request_id", "msg_id", "turn_id"] {
            assert_eq!(response["data"][key], first["data"][key]);
        }
        assert!(response["data"]["runtime"].is_object());
        for private in ["payload_sha256", "content", "password", "token"] {
            assert!(response["data"].get(private).is_none());
        }
    }
    let conflict = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            &send_url,
            json!({ "content": "", "client_request_id": "request-1" }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(conflict).await["code"], "CONFLICT");
    let (other_token, _) = setup_and_login(&mut app, &services, "other", "StrongP@ss2").await;
    let foreign = app.oneshot(get_with_token(&receipt_url, &other_token)).await.unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}
