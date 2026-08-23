use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::grok_session_image::{
    resolve_grok_session_image_core, ResolveGrokSessionImageRequest,
    ResolveGrokSessionImageResponse,
};
use crate::parsers::grok::resolve_grok_home_dir;

pub async fn resolve_grok_session_image(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<ResolveGrokSessionImageRequest>,
) -> Result<Json<ResolveGrokSessionImageResponse>, AppCommandError> {
    let response = resolve_grok_session_image_core(
        &state.db,
        resolve_grok_home_dir().join("sessions"),
        request,
    )
    .await?;
    Ok(Json(response))
}
