//! HTTP handler for `translate_document`.

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::document_translate::translate_document_core;
use crate::document_translate::{TranslateDocumentParams, TranslateDocumentResult};

pub async fn translate_document(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<TranslateDocumentParams>,
) -> Result<Json<TranslateDocumentResult>, AppCommandError> {
    Ok(Json(
        translate_document_core(&state.document_translation, params).await?,
    ))
}
