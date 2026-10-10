//! All room commands run through the same authenticated service as desktop.

use std::sync::Arc;

use axum::{body::Bytes, extract::Extension, http::StatusCode, Json};
use roundtable_protocol::{ClientKind, ErrorCode};
use serde_json::{json, Value};

use crate::app_state::AppState;
use crate::web::auth::AuthenticatedApplication;

async fn dispatch(
    state: Arc<AppState>,
    auth: AuthenticatedApplication,
    command: &str,
    params: Bytes,
) -> (StatusCode, Json<Value>) {
    if !auth.is_global_operator() {
        return (StatusCode::FORBIDDEN, Json(json!({"code":"forbidden"})));
    }
    let result = async {
        let service = state.roundtable.current().ok_or_else(|| {
            crate::roundtable::rt_error(ErrorCode::RuntimeUnavailable, "coordinator_unavailable")
        })?;
        let params = roundtable_protocol::parse_strict_json(
            &params,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        let request = crate::commands::roundtable::request_body(params)?;
        service
            .execute_command(
                &crate::commands::roundtable::operator_actor(ClientKind::Web),
                command,
                request,
            )
            .await
    }
    .await;
    match result {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(error) => {
            let status = if error.details.reason.as_deref() == Some("not_found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::from_u16(error.code.http_status()).unwrap_or(StatusCode::BAD_REQUEST)
            };
            (
                status,
                Json(
                    serde_json::to_value(error).unwrap_or_else(|_| json!({"code":"invalid_state"})),
                ),
            )
        }
    }
}

macro_rules! handlers {
    ($($name:ident),* $(,)?)=>{$(
        pub async fn $name(
            Extension(state):Extension<Arc<AppState>>,
            Extension(auth):Extension<AuthenticatedApplication>,
            params:Bytes,
        )->(StatusCode,Json<Value>){ dispatch(state,auth,stringify!($name),params).await }
    )*};
}
handlers!(
    roundtable_preflight,
    roundtable_create,
    roundtable_update_draft,
    roundtable_start,
    roundtable_get,
    roundtable_list,
    roundtable_pause,
    roundtable_resume,
    roundtable_stop,
    roundtable_interject,
    roundtable_retry_synthesis,
    roundtable_events,
    roundtable_messages,
    roundtable_evidence,
    roundtable_operation,
    roundtable_clone,
    roundtable_attach,
    roundtable_detach,
    roundtable_conclusion_export,
    roundtable_conclusion_save,
    roundtable_agents
);
