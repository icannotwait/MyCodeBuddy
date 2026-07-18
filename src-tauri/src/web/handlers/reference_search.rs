//! HTTP handlers for incremental reference search.
//!
//! Mirrors the Tauri commands so desktop and server share the same cores.
//! Request bodies are flat camelCase protocol objects (no nested `request`).

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::reference_search::{
    cancel_reference_search_core, match_reference_regex_core, next_reference_search_page_core,
    start_reference_search_core, validate_reference_candidate_core_cmd,
};
use crate::reference_search::types::{
    CancelReferenceSearchRequest, MatchReferenceRegexRequest, NextReferenceSearchPageRequest,
    ReferenceCandidateValidation, ReferenceRegexMatch, ReferenceSearchPage,
    StartReferenceSearchRequest, ValidateReferenceCandidateRequest,
};

/// Upper bound for the match_reference_regex HTTP body only.
/// Covers worst-case JSON escaping under Task 2 descriptor bounds without
/// weakening core searchable-byte / slot limits.
pub const MAX_REFERENCE_REGEX_HTTP_BODY_BYTES: usize = 64 * 1024 * 1024;

pub async fn start_reference_search(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<StartReferenceSearchRequest>,
) -> Result<Json<ReferenceSearchPage>, AppCommandError> {
    Ok(Json(
        start_reference_search_core(&state.reference_search_registry, request).await?,
    ))
}

pub async fn next_reference_search_page(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<NextReferenceSearchPageRequest>,
) -> Result<Json<ReferenceSearchPage>, AppCommandError> {
    Ok(Json(
        next_reference_search_page_core(&state.reference_search_registry, request).await?,
    ))
}

pub async fn cancel_reference_search(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<CancelReferenceSearchRequest>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(
        cancel_reference_search_core(&state.reference_search_registry, request).await?,
    ))
}

pub async fn validate_reference_candidate(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<ValidateReferenceCandidateRequest>,
) -> Result<Json<ReferenceCandidateValidation>, AppCommandError> {
    Ok(Json(
        validate_reference_candidate_core_cmd(&state.db, request).await?,
    ))
}

pub async fn match_reference_regex(
    Json(request): Json<MatchReferenceRegexRequest>,
) -> Result<Json<Vec<ReferenceRegexMatch>>, AppCommandError> {
    Ok(Json(match_reference_regex_core(request)?))
}
