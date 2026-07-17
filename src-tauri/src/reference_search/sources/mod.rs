//! Source cursor contract for incremental reference search.
//!
//! Production file / conversation / commit cursors land in later tasks.
//! Task 3 only defines the trait surface and `SourcePage` so the registry can
//! run against test factories.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app_error::AppCommandError;
use crate::reference_search::matcher::SearchPattern;
use crate::reference_search::types::{
    ReferenceCandidate, ReferenceDoneReason, StartReferenceSearchRequest,
};

/// One pull page from a source cursor (not yet stamped with identity/page index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePage {
    pub items: Vec<ReferenceCandidate>,
    pub source_epoch: Option<String>,
    pub done: bool,
    pub done_reason: Option<ReferenceDoneReason>,
}

/// Pull-driven source enumerator retained between pages.
#[async_trait]
pub trait ReferenceSourceCursor: Send {
    async fn next_page(
        &mut self,
        page_size: usize,
        token: CancellationToken,
    ) -> Result<SourcePage, AppCommandError>;

    async fn close(&mut self);
}

/// Opens a source cursor for a validated start request.
#[async_trait]
pub trait ReferenceSourceFactory: Send + Sync {
    async fn open(
        &self,
        request: &StartReferenceSearchRequest,
        pattern: SearchPattern,
        limit: usize,
    ) -> Result<Box<dyn ReferenceSourceCursor>, AppCommandError>;
}
