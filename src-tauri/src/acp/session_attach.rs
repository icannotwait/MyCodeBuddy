//! Session attach modes for ACP connection bootstrap.
//!
//! `ResumeExistingOnly` is the continue-delegation path: prefer `session/resume`,
//! fall back to `session/load`, and **never** open `session/new`. After a
//! successful resume/load the returned external session id must equal the
//! recorded conversation external id; mismatch is a typed resumability failure.

use serde::{Deserialize, Serialize};

/// How an ACP connection should attach to an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttachMode {
    /// Default user / fresh-delegation path: resume → load → new fallthrough.
    #[default]
    Default,
    /// Continue path: resume → load only. Never `session/new`.
    ResumeExistingOnly,
}

impl SessionAttachMode {
    pub fn allows_session_new(self) -> bool {
        matches!(self, Self::Default)
    }

    pub fn is_resume_existing_only(self) -> bool {
        matches!(self, Self::ResumeExistingOnly)
    }
}

/// Result of verifying the post-resume/load external session id against the
/// durable conversation identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalIdVerifyResult {
    /// Returned id matches the recorded conversation external id.
    Match,
    /// Mismatch — caller must not emit SessionStarted that rewrites identity,
    /// must not enqueue a prompt, must disconnect only the new incarnation, and
    /// must settle the run `failed`/`unresumable`.
    Mismatch {
        expected: String,
        actual: String,
    },
    /// Resume/load returned an empty id (treat as unresumable).
    MissingActual { expected: String },
}

/// Compare the recorded conversation external id with the id returned by
/// resume/load. `expected` is the durable conversation `external_id`.
pub fn verify_external_session_id(
    expected: &str,
    actual: Option<&str>,
) -> ExternalIdVerifyResult {
    let expected = expected.trim();
    match actual.map(str::trim).filter(|s| !s.is_empty()) {
        Some(actual) if actual == expected => ExternalIdVerifyResult::Match,
        Some(actual) => ExternalIdVerifyResult::Mismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        },
        None => ExternalIdVerifyResult::MissingActual {
            expected: expected.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_existing_only_never_allows_session_new() {
        assert!(!SessionAttachMode::ResumeExistingOnly.allows_session_new());
        assert!(SessionAttachMode::Default.allows_session_new());
    }

    #[test]
    fn external_id_match() {
        assert_eq!(
            verify_external_session_id("sess-x", Some("sess-x")),
            ExternalIdVerifyResult::Match
        );
    }

    #[test]
    fn external_id_mismatch() {
        assert_eq!(
            verify_external_session_id("sess-x", Some("sess-y")),
            ExternalIdVerifyResult::Mismatch {
                expected: "sess-x".into(),
                actual: "sess-y".into(),
            }
        );
    }

    #[test]
    fn external_id_missing_actual() {
        assert_eq!(
            verify_external_session_id("sess-x", None),
            ExternalIdVerifyResult::MissingActual {
                expected: "sess-x".into(),
            }
        );
        assert_eq!(
            verify_external_session_id("sess-x", Some("  ")),
            ExternalIdVerifyResult::MissingActual {
                expected: "sess-x".into(),
            }
        );
    }
}
