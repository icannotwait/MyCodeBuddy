//! HTTP handlers for document translation and exclusive save-as.

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::document_translate::{save_translation_as_core, translate_document_core};
use crate::document_translate::{
    SaveTranslationAsParams, SaveTranslationAsResult, TranslateDocumentParams,
    TranslateDocumentResult,
};

pub async fn translate_document(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<TranslateDocumentParams>,
) -> Result<Json<TranslateDocumentResult>, AppCommandError> {
    Ok(Json(
        translate_document_core(&state.document_translation, params).await?,
    ))
}

pub async fn save_translation_as(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SaveTranslationAsParams>,
) -> Result<Json<SaveTranslationAsResult>, AppCommandError> {
    Ok(Json(save_translation_as_core(&state.db, params).await?))
}
