//! Authenticated typed completion mutations shared by Tauri and Axum.

#[cfg(feature = "tauri-runtime")]
use std::sync::Arc;

use crate::acp::delegation::event_emitter::CompletionOutboxDispatcher;
use crate::acp::delegation::types::{
    CompletionMutationContext, CompletionMutationResult, ResolveCompletionDecisionRequest,
    ResolveDesignSelfReviewRequest, RetryCompletionArtifactRequest,
};
use crate::acp::delegation::workflow::{
    require_writable_conversation_workflow, resolve_completion_decision_txn,
    resolve_design_self_review_txn, retry_completion_artifact_for_user_txn,
    CompletionMutationError, WorkflowStoreError,
};
use crate::acp::error::AcpError;
use crate::app_error::{AppCommandError, AppErrorCode};
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

pub async fn resolve_completion_decision_core(
    db: &AppDatabase,
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
    dispatcher: &CompletionOutboxDispatcher,
    context: &CompletionMutationContext,
    request: ResolveCompletionDecisionRequest,
) -> Result<CompletionMutationResult, AppCommandError> {
    require_writable_completion_context(db, context).await?;
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
    require_writable_completion_context(db, context).await?;
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
    require_writable_completion_context(db, context).await?;
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

async fn require_writable_completion_context(
    db: &AppDatabase,
    context: &CompletionMutationContext,
) -> Result<(), AppCommandError> {
    require_writable_conversation_workflow(&db.conn, context.parent_conversation_id())
        .await
        .map_err(map_workflow_store_error)
}

fn map_workflow_store_error(error: WorkflowStoreError) -> AppCommandError {
    let error = AcpError::from(error);
    error.app_command_error().unwrap_or_else(|| {
        AppCommandError::invalid_input(error.to_string())
            .with_detail(error.code().unwrap_or("workflow_invalid"))
    })
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
        CompletionMutationError::Protocol { code, .. }
        | CompletionMutationError::Evidence(
            crate::acp::delegation::workflow::CompletionEvidenceError::Protocol { code, .. },
        ) => match code {
            "legacy_completion_protocol_read_only" => {
                AppCommandError::new(AppErrorCode::LegacyCompletionProtocolReadOnly, message)
                    .with_detail(code)
            }
            "unsupported_completion_protocol" => {
                AppCommandError::new(AppErrorCode::UnsupportedCompletionProtocol, message)
                    .with_detail(code)
            }
            "workflow_v2_retired" => {
                AppCommandError::new(AppErrorCode::WorkflowV2Retired, message).with_detail(code)
            }
            "workflow_identity_corrupt" => {
                AppCommandError::new(AppErrorCode::WorkflowIdentityCorrupt, message)
                    .with_detail(code)
            }
            _ => AppCommandError::invalid_input(message).with_detail(code),
        },
        _ => AppCommandError::invalid_input(message).with_detail(stable_code),
    }
}

#[cfg(test)]
mod protocol_error_tests {
    use super::*;
    use crate::acp::delegation::workflow::CompletionEvidenceError;

    #[test]
    fn completion_protocol_mutations_preserve_stable_app_error_codes() {
        let read_only = map_completion_mutation_error(CompletionMutationError::Protocol {
            code: "legacy_completion_protocol_read_only",
            message: "legacy workflow is read-only".into(),
        });
        assert_eq!(
            read_only.code,
            AppErrorCode::LegacyCompletionProtocolReadOnly
        );
        assert_eq!(
            read_only.detail.as_deref(),
            Some("legacy_completion_protocol_read_only")
        );

        let unsupported = map_completion_mutation_error(CompletionMutationError::Evidence(
            CompletionEvidenceError::Protocol {
                code: "unsupported_completion_protocol",
                message: "workflow protocol header is unsupported".into(),
            },
        ));
        assert_eq!(
            unsupported.code,
            AppErrorCode::UnsupportedCompletionProtocol
        );
        assert_eq!(
            unsupported.detail.as_deref(),
            Some("unsupported_completion_protocol")
        );

        for (code, expected) in [
            ("workflow_v2_retired", AppErrorCode::WorkflowV2Retired),
            (
                "workflow_identity_corrupt",
                AppErrorCode::WorkflowIdentityCorrupt,
            ),
        ] {
            let mapped = map_completion_mutation_error(CompletionMutationError::Protocol {
                code,
                message: "structured workflow retirement failure".into(),
            });
            assert_eq!(mapped.code, expected);
            assert_eq!(mapped.detail.as_deref(), Some(code));
        }
    }

    #[test]
    fn completion_entry_guard_preserves_retirement_navigation() {
        let retired =
            map_workflow_store_error(WorkflowStoreError::workflow_v2_retired_with_navigation(41));
        assert_eq!(retired.code, AppErrorCode::WorkflowV2Retired);
        assert_eq!(
            retired.message,
            "This workflow is archived and read-only. Create a new conversation and use a new Design."
        );
        let navigation = retired.i18n_params.expect("retirement navigation");
        assert_eq!(
            navigation.get("source_conversation_id").map(String::as_str),
            Some("41")
        );
        assert!(navigation.get("successor_conversation_id").is_none());
        assert_eq!(
            navigation
                .get("can_create_simple_successor")
                .map(String::as_str),
            Some("false")
        );

        let corrupt = map_workflow_store_error(WorkflowStoreError::WorkflowIdentityCorrupt {
            source_conversation_id: 41,
        });
        assert_eq!(corrupt.code, AppErrorCode::WorkflowIdentityCorrupt);
        assert_eq!(
            corrupt
                .i18n_params
                .as_ref()
                .and_then(|params| params.get("source_conversation_id"))
                .map(String::as_str),
            Some("41")
        );
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
    cas: crate::acp::delegation::workflow::CompletionAttentionCas,
    outcome: crate::acp::delegation::workflow::CompletionOutcome,
) -> Result<CompletionMutationResult, AppCommandError> {
    let request = ResolveCompletionDecisionRequest { cas, outcome };
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
    cas: crate::acp::delegation::workflow::CompletionAttentionCas,
) -> Result<CompletionMutationResult, AppCommandError> {
    let request = RetryCompletionArtifactRequest { cas };
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
    cas: crate::acp::delegation::workflow::CompletionAttentionCas,
    outcome: crate::acp::delegation::workflow::CompletionOutcome,
) -> Result<CompletionMutationResult, AppCommandError> {
    let request = ResolveDesignSelfReviewRequest { cas, outcome };
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

#[cfg(all(test, feature = "tauri-runtime"))]
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
