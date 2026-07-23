//! HTTP handlers for conversation-experience settings (automatic titles +
//! reference-search limit + document translate agent).
//!
//! Mirrors the Tauri commands so desktop and server share the `_core` setters
//! and [`get_conversation_experience_settings_core`].

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::auto_title::title_settings::SetAutoTitleApiConfigRequest;
use crate::commands::conversation_experience::{
    get_conversation_experience_settings_core, set_auto_title_api_config_core,
    set_document_translate_agent_core, set_reference_search_limit_core,
    ConversationExperienceSettings,
};
use crate::models::agent::AgentType;

pub async fn get_conversation_experience_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<ConversationExperienceSettings>, AppCommandError> {
    Ok(Json(
        get_conversation_experience_settings_core(&state.db.conn).await?,
    ))
}

pub async fn set_auto_title_api_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetAutoTitleApiConfigRequest>,
) -> Result<Json<ConversationExperienceSettings>, AppCommandError> {
    let saved = set_auto_title_api_config_core(
        &state.db,
        &state.emitter,
        &state.auto_title_coordinator,
        &state.conversation_experience_gate,
        params.api_url,
        params.api_key_update,
        params.model,
    )
    .await?;
    Ok(Json(saved))
}

#[derive(Deserialize)]
pub struct SetDocumentTranslateAgentParams {
    pub agent: Option<AgentType>,
}

pub async fn set_document_translate_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDocumentTranslateAgentParams>,
) -> Result<Json<ConversationExperienceSettings>, AppCommandError> {
    let saved = set_document_translate_agent_core(
        &state.db,
        &state.emitter,
        &state.conversation_experience_gate,
        params.agent,
    )
    .await?;
    Ok(Json(saved))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetReferenceSearchLimitParams {
    pub limit: u16,
}

pub async fn set_reference_search_limit(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetReferenceSearchLimitParams>,
) -> Result<Json<ConversationExperienceSettings>, AppCommandError> {
    let saved = set_reference_search_limit_core(
        &state.db.conn,
        &state.emitter,
        &state.reference_search_registry,
        &state.conversation_experience_gate,
        params.limit,
    )
    .await?;
    Ok(Json(saved))
}
