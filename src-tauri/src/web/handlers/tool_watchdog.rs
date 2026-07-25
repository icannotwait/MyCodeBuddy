//! Axum mirrors of tool-watchdog settings and lease control APIs.
//!
//! Routes (nested under `/api`):
//! - `POST /acp_get_tool_watchdog_settings`
//! - `POST /acp_set_tool_watchdog_settings`
//! - `POST /acp_tool_watchdog_extend`
//! - `POST /acp_tool_watchdog_cancel`
//!
//! Extend/cancel bodies contain only `leaseId` and `version` (camelCase).

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::acp::tool_watchdog::{ToolWatchdogProjection, ToolWatchdogSettings};
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::tool_watchdog::{
    acp_get_tool_watchdog_settings_core, acp_set_tool_watchdog_settings_core,
    acp_tool_watchdog_cancel_core, acp_tool_watchdog_extend_core, ToolWatchdogLeaseAction,
};

pub async fn acp_get_tool_watchdog_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<ToolWatchdogSettings>, AppCommandError> {
    Ok(Json(
        acp_get_tool_watchdog_settings_core(&state.db.conn).await,
    ))
}

/// Flat set body. camelCase wire keys match Tauri command arg renaming
/// (`warningAfterSeconds`, `graceSeconds`) so desktop and server agree.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetToolWatchdogSettingsParams {
    pub enabled: bool,
    pub warning_after_seconds: u32,
    pub grace_seconds: u32,
}

pub async fn acp_set_tool_watchdog_settings(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetToolWatchdogSettingsParams>,
) -> Result<Json<ToolWatchdogSettings>, AppCommandError> {
    let settings = acp_set_tool_watchdog_settings_core(
        &state.db.conn,
        &state.connection_manager,
        ToolWatchdogSettings {
            enabled: params.enabled,
            warning_after_seconds: params.warning_after_seconds,
            grace_seconds: params.grace_seconds,
        },
    )
    .await?;
    Ok(Json(settings))
}

pub async fn acp_tool_watchdog_extend(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ToolWatchdogLeaseAction>,
) -> Result<Json<ToolWatchdogProjection>, AppCommandError> {
    let projection =
        acp_tool_watchdog_extend_core(&state.connection_manager, params.lease_id, params.version)
            .await?;
    Ok(Json(projection))
}

pub async fn acp_tool_watchdog_cancel(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ToolWatchdogLeaseAction>,
) -> Result<Json<ToolWatchdogProjection>, AppCommandError> {
    let projection =
        acp_tool_watchdog_cancel_core(&state.connection_manager, params.lease_id, params.version)
            .await?;
    Ok(Json(projection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::auth::require_token;
    use axum::http::StatusCode;
    use serde_json::json;
    use std::net::SocketAddr;

    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(db, dir.path().to_path_buf()));
        (state, dir)
    }

    async fn call_json(
        state: Arc<AppState>,
        token: &str,
        path: &str,
        body: serde_json::Value,
        auth_header: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let token = token.to_string();
        let app = axum::Router::new()
            .route(
                "/api/acp_get_tool_watchdog_settings",
                axum::routing::post(acp_get_tool_watchdog_settings),
            )
            .route(
                "/api/acp_set_tool_watchdog_settings",
                axum::routing::post(acp_set_tool_watchdog_settings),
            )
            .route(
                "/api/acp_tool_watchdog_extend",
                axum::routing::post(acp_tool_watchdog_extend),
            )
            .route(
                "/api/acp_tool_watchdog_cancel",
                axum::routing::post(acp_tool_watchdog_cancel),
            )
            .layer(axum::Extension(state))
            .layer(axum::middleware::from_fn(move |req, next| {
                let token = token.clone();
                async move { require_token(req, next, token).await }
            }));
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = axum::serve(listener, app);
        let handle = tokio::spawn(async move {
            let _ = server.await;
        });
        tokio::task::yield_now().await;
        let client = reqwest::Client::new();
        let mut req = client.post(format!("http://{addr}{path}")).json(&body);
        if let Some(h) = auth_header {
            req = req.header("Authorization", h);
        }
        let resp = req.send().await.expect("http");
        let status = StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let value = resp.json::<serde_json::Value>().await.unwrap_or(json!({}));
        handle.abort();
        (status, value)
    }

    #[tokio::test]
    async fn settings_routes_require_auth_and_share_cores() {
        let (state, _dir) = test_state().await;

        let (status, _) = call_json(
            state.clone(),
            "secret",
            "/api/acp_get_tool_watchdog_settings",
            json!({}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = call_json(
            state.clone(),
            "secret",
            "/api/acp_get_tool_watchdog_settings",
            json!({}),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["warning_after_seconds"], 600);
        assert_eq!(body["grace_seconds"], 600);

        let (status, body) = call_json(
            state.clone(),
            "secret",
            "/api/acp_set_tool_watchdog_settings",
            json!({
                "enabled": false,
                "warningAfterSeconds": 59,
                "graceSeconds": 3601,
            }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
        assert_eq!(body["warning_after_seconds"], 60);
        assert_eq!(body["grace_seconds"], 3600);

        // Live registry applied after persist.
        let live = state
            .connection_manager
            .tool_lease_registry()
            .settings()
            .await;
        assert!(!live.enabled);
        assert_eq!(live.warning_after_seconds, 60);
        assert_eq!(live.grace_seconds, 3600);
    }

    #[tokio::test]
    async fn extend_cancel_body_shape_and_stale_code() {
        let (state, _dir) = test_state().await;

        // Stale lease → stable code without mutation (empty registry).
        let (status, body) = call_json(
            state.clone(),
            "secret",
            "/api/acp_tool_watchdog_extend",
            json!({ "leaseId": "missing", "version": 1 }),
            Some("Bearer secret"),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
        let msg = body["message"].as_str().unwrap_or("");
        assert_eq!(msg, "stale_tool_watchdog_lease");

        let (status, body) = call_json(
            state,
            "secret",
            "/api/acp_tool_watchdog_cancel",
            json!({ "leaseId": "missing", "version": 1 }),
            Some("Bearer secret"),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
        assert_eq!(
            body["message"].as_str().unwrap_or(""),
            "stale_tool_watchdog_lease"
        );
    }

    #[test]
    fn set_params_and_lease_action_wire_shapes() {
        // Axum set body: flat camelCase fields only (no nested settings wrapper).
        let set: SetToolWatchdogSettingsParams = serde_json::from_value(json!({
            "enabled": true,
            "warningAfterSeconds": 120,
            "graceSeconds": 90,
        }))
        .unwrap();
        assert!(set.enabled);
        assert_eq!(set.warning_after_seconds, 120);

        let action: ToolWatchdogLeaseAction = serde_json::from_value(json!({
            "leaseId": "l1",
            "version": 2u64,
        }))
        .unwrap();
        assert_eq!(action.lease_id, "l1");
        assert_eq!(action.version, 2);
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 2);
        assert_eq!(v["leaseId"], "l1");
    }
}
