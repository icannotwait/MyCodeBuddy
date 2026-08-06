//! Authenticated HTTP parity for typed completion mutations.

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::acp::delegation::types::{
    CompletionMutationResult, ResolveCompletionDecisionRequest, ResolveDesignSelfReviewRequest,
    RestartLegacyWorkflowRequest, RetryCompletionArtifactRequest,
};
use crate::acp::delegation::workflow::LegacyWorkflowRestartProjection;
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::workflow_completion::{
    completion_attention_parent_conversation_id, get_completion_protocol_settings_core,
    resolve_completion_decision_core, resolve_design_self_review_core,
    restart_legacy_workflow_authenticated_core, retry_completion_artifact_core,
    CompletionProtocolSettingsSnapshot,
};
use crate::web::auth::AuthenticatedApplication;

fn unauthorized_context_error() -> AppCommandError {
    AppCommandError::permission_denied("completion attention is owned by another root conversation")
        .with_detail("unauthorized")
}

pub async fn get_completion_protocol_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<CompletionProtocolSettingsSnapshot>, AppCommandError> {
    Ok(Json(get_completion_protocol_settings_core(
        state.delegation_metrics.as_ref(),
        state.completion_protocol_rollout.as_ref(),
    )))
}

pub async fn resolve_completion_decision(
    Extension(state): Extension<Arc<AppState>>,
    Extension(authenticated): Extension<AuthenticatedApplication>,
    Json(request): Json<ResolveCompletionDecisionRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    let parent_conversation_id =
        completion_attention_parent_conversation_id(&state.db, &request.cas.attention_id).await?;
    let context = authenticated
        .authorize_completion_root(parent_conversation_id)
        .map_err(|()| unauthorized_context_error())?;
    resolve_completion_decision_core(
        &state.db,
        state.delegation_metrics.as_ref(),
        state.completion_outbox_dispatcher.as_ref(),
        &context,
        request,
    )
    .await
    .map(Json)
}

pub async fn retry_completion_artifact(
    Extension(state): Extension<Arc<AppState>>,
    Extension(authenticated): Extension<AuthenticatedApplication>,
    Json(request): Json<RetryCompletionArtifactRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    let parent_conversation_id =
        completion_attention_parent_conversation_id(&state.db, &request.cas.attention_id).await?;
    let context = authenticated
        .authorize_completion_root(parent_conversation_id)
        .map_err(|()| unauthorized_context_error())?;
    retry_completion_artifact_core(
        &state.db,
        state.delegation_metrics.as_ref(),
        state.completion_outbox_dispatcher.as_ref(),
        &context,
        request,
    )
    .await
    .map(Json)
}

pub async fn resolve_design_self_review(
    Extension(state): Extension<Arc<AppState>>,
    Extension(authenticated): Extension<AuthenticatedApplication>,
    Json(request): Json<ResolveDesignSelfReviewRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    let parent_conversation_id =
        completion_attention_parent_conversation_id(&state.db, &request.cas.attention_id).await?;
    let context = authenticated
        .authorize_completion_root(parent_conversation_id)
        .map_err(|()| unauthorized_context_error())?;
    resolve_design_self_review_core(
        &state.db,
        state.delegation_metrics.as_ref(),
        state.completion_outbox_dispatcher.as_ref(),
        &context,
        request,
    )
    .await
    .map(Json)
}

pub async fn restart_legacy_workflow(
    Extension(state): Extension<Arc<AppState>>,
    Extension(authenticated): Extension<AuthenticatedApplication>,
    Json(request): Json<RestartLegacyWorkflowRequest>,
) -> Result<Json<LegacyWorkflowRestartProjection>, AppCommandError> {
    let source_conversation_id = i32::try_from(request.source_conversation_id)
        .map_err(|_| AppCommandError::invalid_input("source conversation id is invalid"))?;
    let context = authenticated
        .authorize_completion_root(source_conversation_id)
        .map_err(|()| unauthorized_context_error())?;
    restart_legacy_workflow_authenticated_core(
        &state.db,
        state.delegation_metrics.as_ref(),
        state.completion_protocol_rollout.as_ref(),
        &context,
        request,
    )
    .await
    .map(Json)
}
