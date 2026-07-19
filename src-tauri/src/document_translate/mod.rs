//! On-demand document translation (Markdown / plain text).
//!
//! Fail-closed Markdown protection (Task 4) plus process-wide service and
//! hidden runner (Task 5).

pub mod protect;
pub mod runner;
pub mod service;
pub mod types;

pub use protect::{
    protect_markdown, protect_markdown_with_nonce, restore_markdown, ProtectError, ProtectedDocument,
};
pub use runner::{DocumentTranslateAgent, DocumentTranslateRunner, InertDocumentTranslateAgent};
pub use service::{build_production_document_translation_service, DocumentTranslationService};
pub use types::{
    DocumentTranslateError, DocumentTranslateFormat, TranslateDocumentParams,
    TranslateDocumentResult, DEADLINE_SECS, MAX_INPUT_SCALARS, MAX_OUTPUT_BYTES, TRANSLATE_CAPACITY,
};
