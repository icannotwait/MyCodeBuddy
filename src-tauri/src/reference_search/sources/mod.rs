//! Source cursor contract for incremental reference search.
//!
//! Production file / conversation / commit cursors live here. Task 3 defined
//! the trait surface and `SourcePage` so the registry can run against test
//! factories; Task 5 adds the production factory.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app_error::AppCommandError;
use crate::reference_search::matcher::SearchPattern;
use crate::reference_search::types::{
    ReferenceCandidate, ReferenceDoneReason, StartReferenceSearchRequest,
};

pub mod commit;
pub mod conversation;
pub mod file;

pub use commit::CommitCursor;
pub use conversation::ConversationCursor;
pub use file::FileCursor;

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

#[cfg(test)]
pub(crate) fn literal(query: &str) -> SearchPattern {
    SearchPattern::parse(query).expect("literal pattern")
}

#[cfg(test)]
pub(crate) async fn drain_cursor(
    cursor: &mut dyn ReferenceSourceCursor,
) -> Vec<ReferenceCandidate> {
    let mut items = Vec::new();
    loop {
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .expect("source page");
        items.extend(page.items);
        assert!(items.len() <= 500, "test cursor exceeded the protocol cap");
        if page.done {
            return items;
        }
    }
}
