use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::{Deserialize, Serialize};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::web::{
    do_get_web_server_status, do_probe_web_service_port, do_stop_web_server,
    load_web_service_config, update_web_service_config_core, WebServerInfo, WebServiceConfig,
    WebServicePortProbe,
};

pub async fn get_web_server_status(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Option<WebServerInfo>>, AppCommandError> {
    Ok(Json(do_get_web_server_status(&state.web_server_state)))
}

pub async fn get_web_service_config(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<WebServiceConfig>, AppCommandError> {
    load_web_service_config(&state.db.conn).await.map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWebServiceConfigParams {
    pub config: WebServiceConfig,
}

pub async fn update_web_service_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<UpdateWebServiceConfigParams>,
) -> Result<Json<WebServiceConfig>, AppCommandError> {
    update_web_service_config_core(&state.db.conn, params.config)
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartWebServerParams {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub token: Option<String>,
}

pub async fn start_web_server(
    Extension(state): Extension<Arc<AppState>>,
    Json(_params): Json<StartWebServerParams>,
) -> Result<Json<WebServerInfo>, AppCommandError> {
    // In web mode, the server is already running (this handler itself is served by it).
    // This endpoint is mainly useful in Tauri mode. Return current status as a noop.
    let ws = &state.web_server_state;
    if ws.running.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(info) = do_get_web_server_status(ws) {
            return Ok(Json(info));
        }
    }
    Err(AppCommandError::new(
        crate::app_error::AppErrorCode::InvalidInput,
        "Cannot start web server from within web mode",
    ))
}

pub async fn stop_web_server(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<()>, AppCommandError> {
    // In web mode the serve task is owned by `codeg-server`'s main loop,
    // not WebServerState. Calling do_stop_web_server here would not stop
    // the process but WOULD trigger shutdown_signal — killing every live
    // WebSocket including the caller's own session. Reject instead.
    if state.web_server_state.is_externally_managed() {
        return Err(AppCommandError::new(
            crate::app_error::AppErrorCode::InvalidInput,
            "Cannot stop web server from within web mode",
        ));
    }
    do_stop_web_server(&state.web_server_state).await;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeWebServicePortParams {
    pub port: Option<u16>,
}

pub async fn probe_web_service_port(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ProbeWebServicePortParams>,
) -> Result<Json<WebServicePortProbe>, AppCommandError> {
    do_probe_web_service_port(&state.db.conn, params.port)
        .await
        .map(Json)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    pub version: String,
    pub body: String,
    pub date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheckResult {
    pub current_version: String,
    pub update: Option<AppUpdateInfo>,
    /// Whether *this* process can apply the update in place. Always false for
    /// this fork's standalone server endpoint; desktop updates use Tauri IPC.
    pub self_update_supported: bool,
    /// Retained wire metadata for client compatibility. Ignored while
    /// `self_update_supported` is false.
    pub capability: crate::update::runtime::UpdateCapability,
    /// `"docker"` | `"standalone"` — drives the post-upgrade hint.
    pub runtime: String,
    /// Relaunch delay (ms) the frontend countdown should use after a
    /// supervised restart.
    pub restart_delay_ms: u64,
    /// Always false because the public standalone rollback endpoint is gated.
    pub rollback_available: bool,
    /// This server speaks the detached `app_update_state` protocol (background
    /// download + progress events + ready-to-restart snapshot). Always true on
    /// this build; absent on older servers, which a newer client must treat as
    /// unsupported rather than driving the new flow against the old blocking
    /// `perform_app_update`.
    pub live_progress: bool,
}

fn server_self_update_supported() -> bool {
    // This fork publishes no standalone Linux/macOS server assets, and Windows
    // server self-update is unsupported. Desktop builds use the separate Tauri
    // updater command path.
    false
}

fn server_rollback_available() -> bool {
    false
}

pub async fn check_app_update() -> Result<Json<AppUpdateCheckResult>, AppCommandError> {
    use crate::update::{runtime, version};

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let manifest = version::fetch_latest_manifest().await?;

    let update = if version::is_newer(&manifest.version, &current_version) {
        Some(AppUpdateInfo {
            version: version::trim_v_prefix(&manifest.version).to_string(),
            body: manifest.notes.unwrap_or_default(),
            date: manifest.pub_date,
        })
    } else {
        None
    };

    Ok(Json(AppUpdateCheckResult {
        current_version,
        update,
        self_update_supported: server_self_update_supported(),
        capability: runtime::capability(),
        runtime: runtime::runtime_label().to_string(),
        restart_delay_ms: runtime::restart_delay_ms(),
        rollback_available: server_rollback_available(),
        live_progress: true,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpdateStatus {
    /// The running binary's own version — read locally (no manifest), so the
    /// settings page can show the current version even when the release source
    /// is unreachable.
    pub current_version: String,
    /// Whether this process can apply an in-place update. Always false for the
    /// standalone server in this fork.
    pub self_update_supported: bool,
    /// Retained wire metadata for client compatibility.
    pub capability: crate::update::runtime::UpdateCapability,
    /// `"docker"` | `"standalone"` — drives the post-upgrade hint.
    pub runtime: String,
    /// Relaunch delay (ms) the frontend countdown should use.
    pub restart_delay_ms: u64,
    /// Always false because the public standalone rollback endpoint is gated.
    pub rollback_available: bool,
    /// This server speaks the detached `app_update_state` protocol. See
    /// [`AppUpdateCheckResult::live_progress`].
    pub live_progress: bool,
}

/// Local-only counterpart to [`check_app_update`]: reports what this process
/// can do WITHOUT contacting the release source. The standalone update and
/// rollback capabilities are intentionally false under this fork's release
/// policy.
pub async fn app_update_status() -> Json<ServerUpdateStatus> {
    use crate::update::runtime;
    Json(ServerUpdateStatus {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        self_update_supported: server_self_update_supported(),
        capability: runtime::capability(),
        runtime: runtime::runtime_label().to_string(),
        restart_delay_ms: runtime::restart_delay_ms(),
        rollback_available: server_rollback_available(),
        live_progress: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn standalone_server_status_never_advertises_in_place_update_support() {
        let Json(status) = app_update_status().await;
        assert!(!status.self_update_supported);
        assert!(!status.rollback_available);
    }
}
