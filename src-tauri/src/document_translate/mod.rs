//! On-demand document translation (Markdown / plain text).
//!
//! Fail-closed Markdown protection (Task 4) plus process-wide service and
//! hidden runner (Task 5). Exclusive save-as for translation results (Task 7).

pub mod protect;
pub mod runner;
pub mod save;
pub mod service;
pub mod types;

pub use protect::{
    protect_markdown, protect_markdown_with_nonce, restore_markdown, ProtectError, ProtectedDocument,
};
pub use runner::{DocumentTranslateAgent, DocumentTranslateRunner, InertDocumentTranslateAgent};
pub use save::{resolve_save_target, save_translation_as_to_root};
pub use service::{build_production_document_translation_service, DocumentTranslationService};
pub use types::{
    DocumentTranslateError, DocumentTranslateFormat, SaveTranslationAsParams,
    SaveTranslationAsResult, TranslateDocumentParams, TranslateDocumentResult, DEADLINE_SECS,
    MAX_INPUT_SCALARS, MAX_OUTPUT_BYTES, TRANSLATE_CAPACITY,
};
