//! Debug endpoint exposing process-local `DelegationMetrics` for the
//! operator-facing `/api/debug/delegation_metrics` route.
//!
//! Behind the same auth layer as every other handler — operators tail it
//! with `curl -H "Authorization: Bearer $TOKEN" http://host:port/api/debug/delegation_metrics`.
//! No product UI consumes this endpoint.

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::acp::delegation::metrics::DelegationMetricsSnapshot;
use crate::app_error::AppCommandError;
use crate::app_state::AppState;

/// Snapshot the current process-local delegation reliability metrics.
pub async fn get_delegation_metrics(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationMetricsSnapshot>, AppCommandError> {
    Ok(Json(
        crate::commands::delegation::get_delegation_metrics_core(state.delegation_metrics.as_ref()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AgentType;
    use crate::web::auth::require_token;
    use axum::http::StatusCode;
    use std::net::SocketAddr;

    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(db, dir.path().to_path_buf()));
        (state, dir)
    }

    /// Serve a mini router with the same `require_token` middleware pattern as
    /// `build_router`, hit it over loopback HTTP, return status.
    async fn call_protected(
        state: Arc<AppState>,
        token: &str,
        auth_header: Option<&str>,
    ) -> StatusCode {
        let token = token.to_string();
        let app = axum::Router::new()
            .route(
                "/api/debug/delegation_metrics",
                axum::routing::get(get_delegation_metrics),
            )
            .layer(axum::Extension(state))
            .layer(axum::middleware::from_fn(move |req, next| {
                let token = token.clone();
                async move { require_token(req, next, token).await }
            }));
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().unwrap();
        let server = axum::serve(listener, app);
        let handle = tokio::spawn(async move {
            let _ = server.await;
        });
        // Give the accept loop a moment to start.
        tokio::task::yield_now().await;
        let client = reqwest::Client::new();
        let mut req = client.get(format!("http://{addr}/api/debug/delegation_metrics"));
        if let Some(h) = auth_header {
            req = req.header("Authorization", h);
        }
        let resp = req.send().await.expect("http call");
        let status = StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        handle.abort();
        status
    }

    #[tokio::test]
    async fn delegation_metrics_requires_auth() {
        let (state, _dir) = test_state().await;
        let status = call_protected(state, "secret-token", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delegation_metrics_returns_increment_without_sensitive_fields() {
        let (state, _dir) = test_state().await;
        state.delegation_metrics.record_accepted(AgentType::Codex);
        state.delegation_metrics.record_explicit_cancel(
            crate::acp::delegation::transport::CancelDelegationReason::UserCancel,
        );

        // Handler path (post-auth) exposes the increment and safe snapshot.
        let Json(snap) = get_delegation_metrics(Extension(state.clone()))
            .await
            .expect("handler ok");
        assert_eq!(snap.accepted_count, 1);
        assert_eq!(snap.explicit_user_cancel_count, 1);

        let text = serde_json::to_string(&snap).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let object = value.as_object().expect("snapshot object");
        for forbidden in [
            "prompt",
            "task",
            "result_text",
            "token",
            "api_key",
            "environment",
            "raw_payload",
            "companion_token",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "snapshot must not have sensitive field {forbidden}: {text}"
            );
        }
        // Values must not embed free-form secrets either.
        assert!(!text.contains("api_key="));
        assert!(!text.contains("Bearer "));

        // Authenticated request through the same middleware pattern as production.
        let status = call_protected(state, "secret-token", Some("Bearer secret-token")).await;
        assert_eq!(status, StatusCode::OK);
    }
}
