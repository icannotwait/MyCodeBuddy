//! Reference search command surface (desktop + shared cores).
//!
//! Handlers delegate to the process-wide [`ReferenceSearchRegistry`] or to the
//! shared validation / matcher cores. Start requests carry no result limit —
//! the backend-owned registry cap is authoritative.

use std::sync::Arc;

use crate::app_error::AppCommandError;
use crate::db::AppDatabase;
use crate::reference_search::matcher::match_reference_regex as rank_reference_descriptors;
use crate::reference_search::types::{
    CancelReferenceSearchRequest, MatchReferenceRegexRequest, NextReferenceSearchPageRequest,
    ReferenceCandidateValidation, ReferenceRegexMatch, ReferenceSearchPage,
    ReferenceSearchSource, StartReferenceSearchRequest, ValidateReferenceCandidateRequest,
};
use crate::reference_search::validation::validate_reference_candidate_core;
use crate::reference_search::ReferenceSearchRegistry;

/// Start (or join) a guarded incremental search and return page 0.
pub async fn start_reference_search_core(
    registry: &Arc<ReferenceSearchRegistry>,
    request: StartReferenceSearchRequest,
) -> Result<ReferenceSearchPage, AppCommandError> {
    registry.start(request).await
}

/// Pull the next sequential page for a live job (or replay the latest page).
pub async fn next_reference_search_page_core(
    registry: &Arc<ReferenceSearchRegistry>,
    request: NextReferenceSearchPageRequest,
) -> Result<ReferenceSearchPage, AppCommandError> {
    registry.next_page(request).await
}

/// Cancel a live job or install a pre-cancel tombstone for a higher sequence.
pub async fn cancel_reference_search_core(
    registry: &Arc<ReferenceSearchRegistry>,
    request: CancelReferenceSearchRequest,
) -> Result<bool, AppCommandError> {
    registry.cancel(request).await
}

/// Validate a selected candidate against live workspace / session / git state.
pub async fn validate_reference_candidate_core_cmd(
    db: &AppDatabase,
    request: ValidateReferenceCandidateRequest,
) -> Result<ReferenceCandidateValidation, AppCommandError> {
    validate_reference_candidate_core(db, request).await
}

/// Rank caller-provided descriptors with the authoritative Rust regex matcher.
///
/// Returns stable IDs plus `ReferenceRegexRank` for every match in the accepted
/// batch; never truncates within the batch. Oversize / duplicate input is
/// `InvalidRequest`.
pub fn match_reference_regex_core(
    request: MatchReferenceRegexRequest,
) -> Result<Vec<ReferenceRegexMatch>, AppCommandError> {
    rank_reference_descriptors(request).map_err(Into::into)
}

// -------- Tauri commands -----------------------------------------------------
// Individual protocol fields so the JS argument object matches the flat Axum
// JSON body (no `{ request: ... }` envelope).

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn start_reference_search(
    search_session_id: String,
    source_sequence: u64,
    request_id: String,
    source: ReferenceSearchSource,
    query: String,
    workspace_path: Option<String>,
    #[cfg(feature = "tauri-runtime")]
    registry: tauri::State<'_, Arc<ReferenceSearchRegistry>>,
) -> Result<ReferenceSearchPage, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        start_reference_search_core(
            &registry,
            StartReferenceSearchRequest {
                search_session_id,
                source_sequence,
                request_id,
                source,
                query,
                workspace_path,
            },
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (
            search_session_id,
            source_sequence,
            request_id,
            source,
            query,
            workspace_path,
        );
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn next_reference_search_page(
    search_session_id: String,
    source_sequence: u64,
    request_id: String,
    source: ReferenceSearchSource,
    page_index: u32,
    #[cfg(feature = "tauri-runtime")]
    registry: tauri::State<'_, Arc<ReferenceSearchRegistry>>,
) -> Result<ReferenceSearchPage, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        next_reference_search_page_core(
            &registry,
            NextReferenceSearchPageRequest {
                search_session_id,
                source_sequence,
                request_id,
                source,
                page_index,
            },
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (
            search_session_id,
            source_sequence,
            request_id,
            source,
            page_index,
        );
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn cancel_reference_search(
    search_session_id: String,
    source_sequence: u64,
    request_id: String,
    source: ReferenceSearchSource,
    #[cfg(feature = "tauri-runtime")]
    registry: tauri::State<'_, Arc<ReferenceSearchRegistry>>,
) -> Result<bool, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        cancel_reference_search_core(
            &registry,
            CancelReferenceSearchRequest {
                search_session_id,
                source_sequence,
                request_id,
                source,
            },
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (search_session_id, source_sequence, request_id, source);
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn validate_reference_candidate(
    validation_request_id: String,
    source: ReferenceSearchSource,
    uri: String,
    query: String,
    workspace_path: Option<String>,
    source_epoch: Option<String>,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
) -> Result<ReferenceCandidateValidation, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        validate_reference_candidate_core_cmd(
            &db,
            ValidateReferenceCandidateRequest {
                validation_request_id,
                source,
                uri,
                query,
                workspace_path,
                source_epoch,
            },
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (
            validation_request_id,
            source,
            uri,
            query,
            workspace_path,
            source_epoch,
        );
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub fn match_reference_regex(
    query: String,
    descriptors: Vec<crate::reference_search::types::ReferenceDescriptor>,
) -> Result<Vec<ReferenceRegexMatch>, AppCommandError> {
    match_reference_regex_core(MatchReferenceRegexRequest {
        query,
        descriptors,
    })
}
