//! Authenticated HTTP parity for typed completion mutations.

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::acp::delegation::types::{
    CompletionMutationResult, ResolveCompletionDecisionRequest, ResolveDesignSelfReviewRequest,
    RetryCompletionArtifactRequest,
};
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::workflow_completion::{
    resolve_completion_decision_core, resolve_design_self_review_core,
    retry_completion_artifact_core,
};

pub async fn resolve_completion_decision(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<ResolveCompletionDecisionRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    resolve_completion_decision_core(
        &state.db,
        state.completion_outbox_dispatcher.as_ref(),
        request,
    )
    .await
    .map(Json)
}

pub async fn retry_completion_artifact(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<RetryCompletionArtifactRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    retry_completion_artifact_core(
        &state.db,
        state.delegation_metrics.as_ref(),
        state.completion_outbox_dispatcher.as_ref(),
        request,
    )
    .await
    .map(Json)
}

pub async fn resolve_design_self_review(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<ResolveDesignSelfReviewRequest>,
) -> Result<Json<CompletionMutationResult>, AppCommandError> {
    resolve_design_self_review_core(
        &state.db,
        state.completion_outbox_dispatcher.as_ref(),
        request,
    )
    .await
    .map(Json)
}
