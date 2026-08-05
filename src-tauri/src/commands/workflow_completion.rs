//! Authenticated typed completion mutations shared by Tauri and Axum.

#[cfg(feature = "tauri-runtime")]
use std::sync::Arc;

use crate::acp::delegation::event_emitter::CompletionOutboxDispatcher;
use crate::acp::delegation::types::{
    CompletionMutationResult, ResolveCompletionDecisionRequest, ResolveDesignSelfReviewRequest,
    RetryCompletionArtifactRequest,
};
use crate::acp::delegation::workflow::{
    resolve_completion_decision_txn, resolve_design_self_review_txn,
    retry_completion_artifact_for_user_txn, CompletionMutationError,
};
use crate::app_error::AppCommandError;
use crate::db::AppDatabase;

const AUTHENTICATED_APPLICATION_ACTOR: &str = "application_user";

pub async fn resolve_completion_decision_core(
    db: &AppDatabase,
    dispatcher: &CompletionOutboxDispatcher,
    request: ResolveCompletionDecisionRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let result = resolve_completion_decision_txn(
        db,
        request.parent_conversation_id,
        request.cas,
        request.outcome,
        AUTHENTICATED_APPLICATION_ACTOR,
    )
    .await
    .map_err(map_completion_mutation_error)?;
    dispatch_after_commit(dispatcher).await;
    Ok(result)
}

pub async fn retry_completion_artifact_core(
    db: &AppDatabase,
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    dispatcher: &CompletionOutboxDispatcher,
    request: RetryCompletionArtifactRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let result = retry_completion_artifact_for_user_txn(
        db,
        request.parent_conversation_id,
        request.cas,
        metrics,
    )
    .await
    .map_err(map_completion_mutation_error)?;
    dispatch_after_commit(dispatcher).await;
    Ok(result)
}

pub async fn resolve_design_self_review_core(
    db: &AppDatabase,
    dispatcher: &CompletionOutboxDispatcher,
    request: ResolveDesignSelfReviewRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let result = resolve_design_self_review_txn(
        db,
        request.parent_conversation_id,
        request.cas,
        request.outcome,
        AUTHENTICATED_APPLICATION_ACTOR,
    )
    .await
    .map_err(map_completion_mutation_error)?;
    dispatch_after_commit(dispatcher).await;
    Ok(result)
}

async fn dispatch_after_commit(dispatcher: &CompletionOutboxDispatcher) {
    if let Err(error) = dispatcher.dispatch_pending().await {
        tracing::warn!(error = %error, "completion outbox dispatch deferred to retry loop");
    }
}

fn map_completion_mutation_error(error: CompletionMutationError) -> AppCommandError {
    let stable_code = error.code();
    let message = error.to_string();
    match error {
        CompletionMutationError::Unauthorized => {
            AppCommandError::permission_denied(message).with_detail(stable_code)
        }
        CompletionMutationError::Superseded | CompletionMutationError::Conflict => {
            AppCommandError::already_exists(message).with_detail(stable_code)
        }
        CompletionMutationError::Evidence(
            crate::acp::delegation::workflow::CompletionEvidenceError::Persistence(_),
        ) => AppCommandError::database_error(message).with_detail(stable_code),
        _ => AppCommandError::invalid_input(message).with_detail(stable_code),
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn resolve_completion_decision(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] dispatcher: tauri::State<'_, Arc<CompletionOutboxDispatcher>>,
    request: ResolveCompletionDecisionRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        resolve_completion_decision_core(&db, dispatcher.inner(), request).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = request;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn retry_completion_artifact(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] metrics: tauri::State<
        '_,
        Arc<crate::acp::delegation::metrics::DelegationMetrics>,
    >,
    #[cfg(feature = "tauri-runtime")] dispatcher: tauri::State<'_, Arc<CompletionOutboxDispatcher>>,
    request: RetryCompletionArtifactRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        retry_completion_artifact_core(&db, metrics.inner(), dispatcher.inner(), request).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = request;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn resolve_design_self_review(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] dispatcher: tauri::State<'_, Arc<CompletionOutboxDispatcher>>,
    request: ResolveDesignSelfReviewRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        resolve_design_self_review_core(&db, dispatcher.inner(), request).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = request;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}
