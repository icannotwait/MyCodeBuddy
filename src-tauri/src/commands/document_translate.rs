//! Document translation Tauri command + shared core.
//!
//! Tauri IPC accepts **flat** camelCase args matching `api.ts`
//! (`content`, `format`, `locale`, `displayName`) — not a nested `params`
//! object. HTTP uses the same flat JSON body via `TranslateDocumentParams`.
//!
//! `save_translation_as` likewise uses flat args: `folderId`, `relativePath`,
//! `content` → `{ absolutePath }`.

use crate::app_error::AppCommandError;
use crate::db::service::folder_service;
use crate::db::AppDatabase;
use crate::document_translate::{
    save_translation_as_to_root, DocumentTranslationService, SaveTranslationAsParams,
    SaveTranslationAsResult, TranslateDocumentParams, TranslateDocumentResult,
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

/// Flat camelCase args so `invoke("translate_document", { content, format, ... })`
/// matches the FE payload (same convention as `reference_search`).
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn translate_document(
    content: String,
    format: String,
    locale: Option<String>,
    display_name: Option<String>,
    #[cfg(feature = "tauri-runtime")] service: tauri::State<
        '_,
        std::sync::Arc<DocumentTranslationService>,
    >,
) -> Result<TranslateDocumentResult, AppCommandError> {
    let params = TranslateDocumentParams {
        content,
        format,
        locale,
        display_name,
    };
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

/// Shared core: resolve `folder_id` → root, exclusive create under root.
pub async fn save_translation_as_core(
    db: &AppDatabase,
    params: SaveTranslationAsParams,
) -> Result<SaveTranslationAsResult, AppCommandError> {
    let folder = folder_service::get_folder_by_id(&db.conn, params.folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Folder {} not found", params.folder_id))
        })?;

    let root = std::path::PathBuf::from(&folder.path);
    // File I/O off the async runtime (same pattern as folder file commands).
    let relative_path = params.relative_path;
    let content = params.content;
    tokio::task::spawn_blocking(move || {
        save_translation_as_to_root(&root, &relative_path, &content)
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Save translation task failed")
            .with_detail(e.to_string())
    })?
}

/// Flat camelCase args: `folderId`, `relativePath`, `content`.
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn save_translation_as(
    folder_id: i32,
    relative_path: String,
    content: String,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
) -> Result<SaveTranslationAsResult, AppCommandError> {
    let params = SaveTranslationAsParams {
        folder_id,
        relative_path,
        content,
    };
    #[cfg(feature = "tauri-runtime")]
    {
        save_translation_as_core(&db, params).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = params;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_translate::DocumentTranslateError;

    #[test]
    fn flat_tauri_args_build_params_matching_api_ts() {
        // Mirrors the FE object: { content, format, locale?, displayName? }.
        let params = TranslateDocumentParams {
            content: "Hello".into(),
            format: "markdown".into(),
            locale: Some("ja".into()),
            display_name: Some("README.md".into()),
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["content"], "Hello");
        assert_eq!(v["format"], "markdown");
        assert_eq!(v["locale"], "ja");
        assert_eq!(v["displayName"], "README.md");
        // Must not be nested under `params` (Tauri would require that envelope).
        assert!(v.get("params").is_none());
    }

    #[test]
    fn unsupported_format_from_core_params_maps_i18n() {
        // Service validates wire format; this only checks the domain→API mapping
        // stays stable when core builds params from flat command fields.
        let err = DocumentTranslateError::UnsupportedFormat.into_app_command_error();
        assert_eq!(
            err.i18n_key.as_deref(),
            Some(crate::document_translate::types::I18N_UNSUPPORTED_FORMAT)
        );
    }

    #[test]
    fn save_translation_as_flat_args_build_params_matching_api_ts() {
        let params = SaveTranslationAsParams {
            folder_id: 42,
            relative_path: "README.zh_cn.md".into(),
            content: "你好".into(),
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["folderId"], 42);
        assert_eq!(v["relativePath"], "README.zh_cn.md");
        assert_eq!(v["content"], "你好");
        assert!(v.get("params").is_none());
    }
}
