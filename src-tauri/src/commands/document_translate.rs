//! Document translation Tauri command + shared core.

use crate::app_error::AppCommandError;
use crate::document_translate::{
    DocumentTranslationService, TranslateDocumentParams, TranslateDocumentResult,
};

/// Shared core for Tauri + Axum.
pub async fn translate_document_core(
    service: &std::sync::Arc<DocumentTranslationService>,
    params: TranslateDocumentParams,
) -> Result<TranslateDocumentResult, AppCommandError> {
    service
        .translate(params)
        .await
        .map_err(|e| e.into_app_command_error())
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn translate_document(
    params: TranslateDocumentParams,
    #[cfg(feature = "tauri-runtime")]
    service: tauri::State<'_, std::sync::Arc<DocumentTranslationService>>,
) -> Result<TranslateDocumentResult, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        translate_document_core(&service, params).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = params;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}
