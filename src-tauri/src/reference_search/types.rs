//! Wire protocol types for incremental reference search.
//!
//! Request/page/candidate shapes mirror the approved TypeScript contract.
//! `ReferenceSearchError` reuses [`AppErrorCode`] — there is no second error
//! code enum.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::models::agent::AgentType;

/// Upper bound on `source_sequence` / safe JSON integers (Number.MAX_SAFE_INTEGER).
pub const MAX_SAFE_SOURCE_SEQUENCE: u64 = 9_007_199_254_740_991;

/// Resource source participating in reference search.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceSearchSource {
    File,
    Conversation,
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartReferenceSearchRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
    pub query: String,
    pub workspace_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NextReferenceSearchPageRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
    pub page_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelReferenceSearchRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReferenceCandidateRequest {
    pub validation_request_id: String,
    pub source: ReferenceSearchSource,
    pub uri: String,
    pub query: String,
    pub workspace_path: Option<String>,
    pub source_epoch: Option<String>,
}

/// Outcome of validating a selected reference candidate against live state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReferenceCandidateValidation {
    Match {
        validation_request_id: String,
        candidate: ReferenceCandidate,
        regex_rank: Option<ReferenceRegexRank>,
    },
    NotMatch {
        validation_request_id: String,
        candidate: ReferenceCandidate,
        regex_rank: Option<ReferenceRegexRank>,
    },
    NotFound {
        validation_request_id: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceDoneReason {
    Exhausted,
    Limit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceSearchPage {
    pub source_sequence: u64,
    pub request_id: String,
    pub page_index: u32,
    pub items: Vec<ReferenceCandidate>,
    pub source_epoch: Option<String>,
    pub done: bool,
    pub done_reason: Option<ReferenceDoneReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceRegexRank {
    pub field_tier: u32,
    pub start: u32,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceFieldMatch {
    pub field_tier: u32,
    pub regex_rank: Option<ReferenceRegexRank>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReferenceCandidateMetadata {
    File {
        canonical_workspace_root: String,
        relative_path: String,
        entry_kind: ReferenceFileKind,
    },
    Conversation {
        conversation_id: i32,
        agent_type: AgentType,
        status: String,
        branch: Option<String>,
        project_name: String,
        project_path: String,
    },
    Commit {
        canonical_repo: String,
        full_hash: String,
        short_hash: String,
        subject: String,
        message: String,
        author: String,
        authored_at: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFileKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceCandidate {
    pub source: ReferenceSearchSource,
    pub uri: String,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    pub keywords: String,
    pub metadata: ReferenceCandidateMetadata,
    pub source_ordinal: u64,
    pub regex_rank: Option<ReferenceRegexRank>,
}

/// Caller-provided descriptor for the bulk regex rank helper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceDescriptor {
    pub id: String,
    pub source_ordinal: u64,
    pub primary: Vec<String>,
    pub secondary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchReferenceRegexRequest {
    pub query: String,
    pub descriptors: Vec<ReferenceDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceRegexMatch {
    pub id: String,
    pub source_ordinal: u64,
    pub rank: ReferenceRegexRank,
}

/// Immutable start arguments used for equal-sequence join/replay comparison.
///
/// Computed without compiling the query so ordering preflight can advance the
/// high-water mark before pattern validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFingerprint {
    pub query: String,
    pub workspace_path: Option<String>,
}

impl RequestFingerprint {
    pub fn from_start(request: &StartReferenceSearchRequest) -> Self {
        Self {
            query: request.query.clone(),
            workspace_path: request.workspace_path.clone(),
        }
    }
}

/// Validated search / page / cancel identity triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchIdentity {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
}

impl SearchIdentity {
    /// Parse session/request UUIDs and reject non-v4, non-canonical, or unsafe
    /// sequence values. Reuses [`parse_canonical_uuid_v4`].
    pub fn parse(
        search_session_id: &str,
        source_sequence: u64,
        request_id: &str,
    ) -> Result<Self, ReferenceSearchError> {
        let session = parse_canonical_uuid_v4(search_session_id)?;
        let request = parse_canonical_uuid_v4(request_id)?;
        if source_sequence == 0 || source_sequence > MAX_SAFE_SOURCE_SEQUENCE {
            return Err(ReferenceSearchError::invalid_request(format!(
                "source_sequence must be in 1..={MAX_SAFE_SOURCE_SEQUENCE}, got {source_sequence}"
            )));
        }
        Ok(Self {
            search_session_id: session.hyphenated().to_string(),
            source_sequence,
            request_id: request.hyphenated().to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ReferenceSearchError {
    pub code: AppErrorCode,
    pub message: String,
}

impl ReferenceSearchError {
    pub fn new(code: AppErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AppErrorCode::InvalidRequest, message)
    }

    pub fn invalid_pattern(message: impl Into<String>) -> Self {
        Self::new(AppErrorCode::InvalidPattern, message)
    }
}

impl From<ReferenceSearchError> for AppCommandError {
    fn from(error: ReferenceSearchError) -> Self {
        AppCommandError::new(error.code, error.message)
    }
}

/// Parse a UUID that must be version 4 and exactly the lowercase hyphenated
/// form (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`). Uppercase, braced, simple,
/// and URN spellings are rejected even when the UUID crate could parse them.
pub fn parse_canonical_uuid_v4(value: &str) -> Result<Uuid, ReferenceSearchError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| ReferenceSearchError::invalid_request(format!("invalid UUID: {value}")))?;
    if parsed.get_version_num() != 4 {
        return Err(ReferenceSearchError::invalid_request(format!(
            "UUID must be version 4: {value}"
        )));
    }
    if parsed.hyphenated().to_string() != value {
        return Err(ReferenceSearchError::invalid_request(format!(
            "UUID must be lowercase hyphenated canonical form: {value}"
        )));
    }
    Ok(parsed)
}

/// Workspace path is required (non-empty) only for file/commit sources;
/// conversation accepts only `None`.
pub fn validate_source_scope(
    source: ReferenceSearchSource,
    workspace_path: Option<&str>,
) -> Result<(), ReferenceSearchError> {
    match (source, workspace_path) {
        (ReferenceSearchSource::File | ReferenceSearchSource::Commit, Some(path))
            if !path.is_empty() =>
        {
            Ok(())
        }
        (ReferenceSearchSource::Conversation, None) => Ok(()),
        _ => Err(ReferenceSearchError::invalid_request(format!(
            "workspace_path scope is invalid for source {source:?}"
        ))),
    }
}

/// Commit validation requires a non-empty source epoch; file/conversation
/// require `None`.
pub fn validate_source_epoch_scope(
    source: ReferenceSearchSource,
    source_epoch: Option<&str>,
) -> Result<(), ReferenceSearchError> {
    match (source, source_epoch) {
        (ReferenceSearchSource::Commit, Some(epoch)) if !epoch.is_empty() => Ok(()),
        (ReferenceSearchSource::File | ReferenceSearchSource::Conversation, None) => Ok(()),
        _ => Err(ReferenceSearchError::invalid_request(format!(
            "source_epoch scope is invalid for source {source:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference_search::matcher::{
        build_commit_uri, build_file_uri, build_session_uri, SearchPattern,
    };
    use std::path::Path;

    const UUID_V4: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn query_identity_and_regex_bounds_are_exact() {
        assert!(matches!(
            SearchPattern::parse("").unwrap_err().code,
            AppErrorCode::InvalidRequest
        ));
        assert!(
            matches!(SearchPattern::parse(" File ").unwrap(), SearchPattern::Literal { raw, .. } if raw == " File ")
        );
        assert!(
            matches!(SearchPattern::parse("re:(?i)^src/").unwrap(), SearchPattern::Regex { raw, .. } if raw == "re:(?i)^src/")
        );
        assert!(matches!(
            SearchPattern::parse("re:").unwrap_err().code,
            AppErrorCode::InvalidPattern
        ));
        assert!(matches!(
            SearchPattern::parse(&format!("re:{}", "x".repeat(257)))
                .unwrap_err()
                .code,
            AppErrorCode::InvalidPattern
        ));
        assert!(matches!(
            SearchPattern::parse(&"x".repeat(513)).unwrap_err().code,
            AppErrorCode::InvalidRequest
        ));
    }

    #[test]
    fn uuid_and_sequence_validation_rejects_non_v4_or_unsafe_values() {
        assert!(SearchIdentity::parse(UUID_V4, 1, UUID_V4).is_ok());
        assert!(SearchIdentity::parse("not-a-uuid", 1, UUID_V4).is_err());
        assert!(SearchIdentity::parse(UUID_V4, 0, UUID_V4).is_err());
        assert!(SearchIdentity::parse(UUID_V4, 9_007_199_254_740_992, UUID_V4).is_err());
    }

    #[test]
    fn source_scope_requires_workspace_only_for_file_and_commit() {
        assert!(validate_source_scope(ReferenceSearchSource::File, Some("workspace-root")).is_ok());
        assert!(
            validate_source_scope(ReferenceSearchSource::Commit, Some("workspace-root")).is_ok()
        );
        assert!(validate_source_scope(ReferenceSearchSource::Conversation, None).is_ok());
        assert!(validate_source_scope(ReferenceSearchSource::File, None).is_err());
        assert!(validate_source_scope(ReferenceSearchSource::Commit, Some("")).is_err());
        assert!(
            validate_source_scope(ReferenceSearchSource::Conversation, Some("workspace-root"))
                .is_err()
        );
        assert!(
            validate_source_epoch_scope(ReferenceSearchSource::Commit, Some("v1:epoch")).is_ok()
        );
        assert!(validate_source_epoch_scope(ReferenceSearchSource::Commit, None).is_err());
        assert!(
            validate_source_epoch_scope(ReferenceSearchSource::File, Some("v1:epoch")).is_err()
        );
    }

    #[test]
    fn canonical_resource_uris_match_the_existing_frontend_codec() {
        assert_eq!(
            build_file_uri(Path::new("/repo/a b#c.ts")),
            "file:///repo/a%20b%23c.ts"
        );
        assert_eq!(
            build_file_uri(Path::new(r"C:\repo\app.ts")),
            "file:///C%3A/repo/app.ts"
        );
        assert_eq!(
            build_file_uri(Path::new(r"\\server\share\文档.md")),
            "file://server/share/%E6%96%87%E6%A1%A3.md"
        );
        assert_eq!(
            build_file_uri(Path::new(r"\\?\C:\repo\app.ts")),
            "file:///C%3A/repo/app.ts"
        );
        assert_eq!(
            build_file_uri(Path::new(r"\\?\UNC\server\share\文档.md")),
            "file://server/share/%E6%96%87%E6%A1%A3.md"
        );
        assert_eq!(build_session_uri(42), "codeg://session/42");
        assert_eq!(
            build_commit_uri("/repo with space", "abc123"),
            "codeg://commit/%2Frepo%20with%20space@abc123",
        );
    }

    #[test]
    fn parse_canonical_uuid_v4_rejects_non_canonical_spellings() {
        assert!(parse_canonical_uuid_v4(UUID_V4).is_ok());
        // Must include a-f so uppercase actually differs from the canonical form.
        let with_letters = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        assert!(parse_canonical_uuid_v4(with_letters).is_ok());
        let upper = with_letters.to_uppercase();
        assert!(parse_canonical_uuid_v4(&upper).is_err());
        assert!(parse_canonical_uuid_v4("11111111111141118111111111111111").is_err());
        assert!(parse_canonical_uuid_v4("{11111111-1111-4111-8111-111111111111}").is_err());
        // version 1
        assert!(parse_canonical_uuid_v4("11111111-1111-1111-8111-111111111111").is_err());
    }
}
