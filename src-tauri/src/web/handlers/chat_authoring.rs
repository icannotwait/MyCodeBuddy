//! HTTP handlers for chat-authoring settings — the web-mode mirror of the
//! Tauri commands in `commands::chat_authoring`.
//!
//! Both endpoints share the same core helpers (`load_chat_authoring_settings`,
//! `set_chat_authoring_settings_core`) so the persist + runtime-config re-apply
//! behavior stays identical across transports.

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::chat_authoring::{
    load_chat_authoring_settings, set_chat_authoring_settings_core, ChatAuthoringSettings,
};

pub async fn get_chat_authoring_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<ChatAuthoringSettings>, AppCommandError> {
    Ok(Json(load_chat_authoring_settings(&state.db.conn).await))
}

#[derive(Deserialize)]
pub struct SetChatAuthoringSettingsParams {
    pub settings: ChatAuthoringSettings,
}

pub async fn set_chat_authoring_settings(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetChatAuthoringSettingsParams>,
) -> Result<Json<ChatAuthoringSettings>, AppCommandError> {
    let saved = set_chat_authoring_settings_core(
        &state.db.conn,
        &state.chat_authoring_config,
        &state.emitter,
        params.settings,
    )
    .await?;
    Ok(Json(saved))
}
