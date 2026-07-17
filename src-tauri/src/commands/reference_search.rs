//! Reference search command surface.
//!
//! Registry / source orchestration arrives in later tasks. This module exposes
//! the descriptor regex helper that shares the authoritative matcher.

use crate::app_error::AppCommandError;
use crate::reference_search::matcher::match_reference_regex;
use crate::reference_search::types::{MatchReferenceRegexRequest, ReferenceRegexMatch};

/// Rank caller-provided descriptors with the authoritative Rust regex matcher.
///
/// Returns stable IDs plus `ReferenceRegexRank` for every match in the accepted
/// batch; never truncates within the batch. Oversize / duplicate input is
/// `InvalidRequest`.
pub fn match_reference_regex_core(
    request: MatchReferenceRegexRequest,
) -> Result<Vec<ReferenceRegexMatch>, AppCommandError> {
    match_reference_regex(request).map_err(Into::into)
}
