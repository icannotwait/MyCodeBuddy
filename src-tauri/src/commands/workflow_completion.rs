//! Authenticated typed completion mutations shared by Tauri and Axum.

#[cfg(feature = "tauri-runtime")]
use std::sync::Arc;

use crate::acp::delegation::event_emitter::CompletionOutboxDispatcher;
use crate::acp::delegation::types::{
    CompletionMutationContext, CompletionMutationResult, ResolveCompletionDecisionRequest,
    ResolveDesignSelfReviewRequest, RestartLegacyWorkflowRequest, RetryCompletionArtifactRequest,
};
use crate::acp::delegation::workflow::{
    resolve_completion_decision_txn, resolve_design_self_review_txn,
    restart_legacy_workflow_if_enforced, retry_completion_artifact_for_user_txn,
    CompletionMutationError, CompletionProtocolRolloutConfig, LegacyWorkflowRestartProjection,
    WorkflowStoreError,
};
use crate::app_error::AppCommandError;
use crate::db::entities::delegation_attention_request;
use crate::db::AppDatabase;
use chrono::Utc;
use sea_orm::EntityTrait;

pub async fn completion_attention_parent_conversation_id(
    db: &AppDatabase,
    attention_id: &str,
) -> Result<i32, AppCommandError> {
    delegation_attention_request::Entity::find_by_id(attention_id)
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?
        .map(|row| row.parent_conversation_id)
        .ok_or_else(|| {
            AppCommandError::already_exists("completion decision was superseded")
                .with_detail("completion_decision_superseded")
        })
}

#[cfg(feature = "tauri-runtime")]
fn unauthorized_context_error() -> AppCommandError {
    AppCommandError::permission_denied("completion attention is owned by another root conversation")
        .with_detail("unauthorized")
}

pub async fn restart_legacy_workflow_authenticated_core(
    db: &AppDatabase,
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    rollout: &CompletionProtocolRolloutConfig,
    context: &CompletionMutationContext,
    request: RestartLegacyWorkflowRequest,
) -> Result<LegacyWorkflowRestartProjection, AppCommandError> {
    if i64::from(context.parent_conversation_id()) != request.source_conversation_id {
        metrics.record_completion_restart(
            crate::acp::delegation::metrics::CompletionRestartOutcome::Rejected,
        );
        return Err(unauthorized_context_error());
    }
    match restart_legacy_workflow_if_enforced(db, context.parent_conversation_id(), None, rollout)
        .await
    {
        Ok(Some(projection)) => {
            metrics.record_completion_restart(if projection.idempotent_replay {
                crate::acp::delegation::metrics::CompletionRestartOutcome::Reused
            } else {
                crate::acp::delegation::metrics::CompletionRestartOutcome::Created
            });
            Ok(projection)
        }
        Ok(None) => {
            metrics.record_completion_restart(
                crate::acp::delegation::metrics::CompletionRestartOutcome::Rejected,
            );
            Err(
                AppCommandError::invalid_input("legacy restart requires current v2_enforce mode")
                    .with_detail("legacy_completion_protocol_restart_not_required"),
            )
        }
        Err(error) => {
            metrics.record_completion_restart(
                crate::acp::delegation::metrics::CompletionRestartOutcome::Failed,
            );
            Err(map_legacy_restart_error(error))
        }
    }
}

fn map_legacy_restart_error(error: WorkflowStoreError) -> AppCommandError {
    let code = error.code();
    let message = error.to_string();
    match error {
        WorkflowStoreError::CrossParent { .. } => {
            AppCommandError::permission_denied(message).with_detail(code)
        }
        WorkflowStoreError::LegacyCompletionProtocolRestartRequired(_)
        | WorkflowStoreError::Persistence(_)
        | WorkflowStoreError::Busy(_) => AppCommandError::database_error(message).with_detail(code),
        _ => AppCommandError::invalid_input(message).with_detail(code),
    }
}

pub async fn resolve_completion_decision_core(
    db: &AppDatabase,
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    dispatcher: &CompletionOutboxDispatcher,
    context: &CompletionMutationContext,
    request: ResolveCompletionDecisionRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let opened_at = delegation_attention_request::Entity::find_by_id(&request.cas.attention_id)
        .one(&db.conn)
        .await
        .ok()
        .flatten()
        .map(|row| row.created_at);
    let result = resolve_completion_decision_txn(
        db,
        context.parent_conversation_id(),
        request.cas,
        request.outcome,
        context.actor_identity(),
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if matches!(error, CompletionMutationError::Superseded) {
                metrics.record_completion_decision_superseded();
            }
            return Err(map_completion_mutation_error(error));
        }
    };
    let latency = opened_at
        .map(|opened_at| Utc::now().signed_duration_since(opened_at))
        .and_then(|latency| latency.to_std().ok())
        .unwrap_or_default();
    metrics.record_completion_decision_resolved(latency, result.idempotent_replay);
    dispatch_after_commit(dispatcher).await;
    Ok(result)
}

pub async fn retry_completion_artifact_core(
    db: &AppDatabase,
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    dispatcher: &CompletionOutboxDispatcher,
    context: &CompletionMutationContext,
    request: RetryCompletionArtifactRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let result = retry_completion_artifact_for_user_txn(
        db,
        context.parent_conversation_id(),
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
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    dispatcher: &CompletionOutboxDispatcher,
    context: &CompletionMutationContext,
    request: ResolveDesignSelfReviewRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    let opened_at = delegation_attention_request::Entity::find_by_id(&request.cas.attention_id)
        .one(&db.conn)
        .await
        .ok()
        .flatten()
        .map(|row| row.created_at);
    let result = resolve_design_self_review_txn(
        db,
        context.parent_conversation_id(),
        request.cas,
        request.outcome,
        context.actor_identity(),
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if matches!(error, CompletionMutationError::Superseded) {
                metrics.record_completion_decision_superseded();
            }
            return Err(map_completion_mutation_error(error));
        }
    };
    let latency = opened_at
        .map(|opened_at| Utc::now().signed_duration_since(opened_at))
        .and_then(|latency| latency.to_std().ok())
        .unwrap_or_default();
    metrics.record_completion_decision_resolved(latency, result.idempotent_replay);
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
    #[cfg(feature = "tauri-runtime")] metrics: tauri::State<
        '_,
        Arc<crate::acp::delegation::metrics::DelegationMetrics>,
    >,
    #[cfg(feature = "tauri-runtime")] dispatcher: tauri::State<'_, Arc<CompletionOutboxDispatcher>>,
    #[cfg(feature = "tauri-runtime")] window: tauri::WebviewWindow,
    request: ResolveCompletionDecisionRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let context = desktop_completion_context(&db, &window, &request.cas.attention_id).await?;
        resolve_completion_decision_core(
            &db,
            metrics.inner(),
            dispatcher.inner(),
            &context,
            request,
        )
        .await
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
    #[cfg(feature = "tauri-runtime")] window: tauri::WebviewWindow,
    request: RetryCompletionArtifactRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let context = desktop_completion_context(&db, &window, &request.cas.attention_id).await?;
        retry_completion_artifact_core(&db, metrics.inner(), dispatcher.inner(), &context, request)
            .await
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
    #[cfg(feature = "tauri-runtime")] metrics: tauri::State<
        '_,
        Arc<crate::acp::delegation::metrics::DelegationMetrics>,
    >,
    #[cfg(feature = "tauri-runtime")] dispatcher: tauri::State<'_, Arc<CompletionOutboxDispatcher>>,
    #[cfg(feature = "tauri-runtime")] window: tauri::WebviewWindow,
    request: ResolveDesignSelfReviewRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let context = desktop_completion_context(&db, &window, &request.cas.attention_id).await?;
        resolve_design_self_review_core(&db, metrics.inner(), dispatcher.inner(), &context, request)
            .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = request;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn restart_legacy_workflow(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] metrics: tauri::State<
        '_,
        Arc<crate::acp::delegation::metrics::DelegationMetrics>,
    >,
    #[cfg(feature = "tauri-runtime")] rollout: tauri::State<
        '_,
        Arc<CompletionProtocolRolloutConfig>,
    >,
    #[cfg(feature = "tauri-runtime")] window: tauri::WebviewWindow,
    request: RestartLegacyWorkflowRequest,
) -> Result<LegacyWorkflowRestartProjection, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let source_conversation_id = i32::try_from(request.source_conversation_id)
            .map_err(|_| AppCommandError::invalid_input("source conversation id is invalid"))?;
        let context = desktop_completion_context_for_label(source_conversation_id, window.label())?;
        restart_legacy_workflow_authenticated_core(
            &db,
            metrics.inner(),
            rollout.inner(),
            &context,
            request,
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = request;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(feature = "tauri-runtime")]
async fn desktop_completion_context(
    db: &AppDatabase,
    window: &tauri::WebviewWindow,
    attention_id: &str,
) -> Result<CompletionMutationContext, AppCommandError> {
    let parent_conversation_id =
        completion_attention_parent_conversation_id(db, attention_id).await?;
    desktop_completion_context_for_label(parent_conversation_id, window.label())
}

#[cfg(feature = "tauri-runtime")]
fn desktop_completion_context_for_label(
    parent_conversation_id: i32,
    window_label: &str,
) -> Result<CompletionMutationContext, AppCommandError> {
    if window_label == "main" {
        return Ok(CompletionMutationContext::authenticated(
            parent_conversation_id,
            "desktop_main_window",
        ));
    }
    match crate::commands::conversation_popout::parse_conversation_id_from_label(window_label) {
        Some(window_root) if window_root == parent_conversation_id => {
            Ok(CompletionMutationContext::authenticated(
                parent_conversation_id,
                format!("desktop_conversation_window:{window_root}"),
            ))
        }
        Some(_) | None => Err(unauthorized_context_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_context_for_desktop_window_fails_closed_by_label() {
        let main = desktop_completion_context_for_label(42, "main").unwrap();
        assert_eq!(main.parent_conversation_id(), 42);
        assert_eq!(main.actor_identity(), "desktop_main_window");

        let popout = desktop_completion_context_for_label(42, "conversation-42").unwrap();
        assert_eq!(popout.parent_conversation_id(), 42);
        assert_eq!(popout.actor_identity(), "desktop_conversation_window:42");

        for label in [
            "conversation-41",
            "conversation-invalid",
            "remote-workspace-42",
            "settings",
            "pet",
            "unknown",
        ] {
            let error = desktop_completion_context_for_label(42, label).unwrap_err();
            assert_eq!(error.detail.as_deref(), Some("unauthorized"), "{label}");
        }
    }
}
