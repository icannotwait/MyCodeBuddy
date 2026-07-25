//! Session attach modes for ACP connection bootstrap.
//!
//! `ResumeExistingOnly` is the continue-delegation path: prefer `session/resume`,
//! fall back to `session/load`, and **never** open `session/new`. After a
//! successful resume/load, an **explicit** agent-returned external session id
//! must equal the recorded conversation external id; mismatch is a typed
//! resumability failure. When the agent omits the id or returns blank, the
//! gate emits the requested/expected id (standard ACP resume/load responses
//! often omit `sessionId`).

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

/// Post-resume/load decision for SessionStarted identity publication.
///
/// On [`SessionStartedDecision::RefuseUnresumable`] the caller must **not**
/// emit `SessionStarted` (so lifecycle does not rewrite `external_id`), must
/// **not** enqueue a prompt, must disconnect only the new connection
/// incarnation, and must settle the run `failed`/`unresumable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStartedDecision {
    /// Safe to emit SessionStarted with this durable session id.
    Emit { session_id: String },
    /// Identity unsafe — refuse SessionStarted rewrite and prompt.
    RefuseUnresumable { reason: String },
}

/// Gate SessionStarted after resume/load using [`verify_external_session_id`].
///
/// `expected_external_id` is the conversation's durable external id (the id
/// passed into resume/load). `actual_session_id` is the id returned by the
/// agent when present; callers may fall back to the requested id only when
/// the agent response omits a session id field (typed resume/load responses
/// historically do).
pub fn decide_session_started(
    expected_external_id: &str,
    actual_session_id: Option<&str>,
) -> SessionStartedDecision {
    match verify_external_session_id(expected_external_id, actual_session_id) {
        ExternalIdVerifyResult::Match => SessionStartedDecision::Emit {
            session_id: expected_external_id.trim().to_string(),
        },
        ExternalIdVerifyResult::Mismatch { expected, actual } => {
            SessionStartedDecision::RefuseUnresumable {
                reason: format!(
                    "external session id mismatch: expected `{expected}`, got `{actual}`"
                ),
            }
        }
        ExternalIdVerifyResult::MissingActual { expected } => {
            SessionStartedDecision::RefuseUnresumable {
                reason: format!(
                    "external session id missing after resume/load (expected `{expected}`)"
                ),
            }
        }
    }
}

/// `ResumeExistingOnly` requires a non-empty session id — without one the
/// path must never fall through to `session/new`.
pub fn resume_existing_has_session_id(session_id: Option<&str>) -> bool {
    session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
}

/// Gate `SessionStarted` publication for an attach mode.
///
/// - **Default**: emit with the requested/expected external id (existing UX).
/// - **ResumeExistingOnly**: own the identity matrix via
///   [`verify_external_session_id`]. Omitted or blank agent-returned ids are
///   treated as accept-and-emit the requested id. A **present** mismatched
///   agent-returned id is [`SessionStartedDecision::RefuseUnresumable`]
///   (no identity rewrite, no prompt, disconnect only the new incarnation,
///   settle `failed`/`unresumable`). Never falls through to `session/new`.
pub fn gate_session_started_for_attach(
    mode: SessionAttachMode,
    expected_external_id: &str,
    agent_returned_session_id: Option<&str>,
) -> SessionStartedDecision {
    if !mode.is_resume_existing_only() {
        return SessionStartedDecision::Emit {
            session_id: expected_external_id.trim().to_string(),
        };
    }
    let actual = agent_returned_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match actual {
        None => SessionStartedDecision::Emit {
            session_id: expected_external_id.trim().to_string(),
        },
        Some(actual) => match verify_external_session_id(expected_external_id, Some(actual)) {
            ExternalIdVerifyResult::Match => SessionStartedDecision::Emit {
                session_id: expected_external_id.trim().to_string(),
            },
            ExternalIdVerifyResult::Mismatch { expected, actual } => {
                SessionStartedDecision::RefuseUnresumable {
                    reason: format!(
                        "external session id mismatch: expected `{expected}`, got `{actual}`"
                    ),
                }
            }
            ExternalIdVerifyResult::MissingActual { .. } => SessionStartedDecision::Emit {
                session_id: expected_external_id.trim().to_string(),
            },
        },
    }
}

/// Pull a session id out of a raw resume/load JSON body when agents include
/// `sessionId` / `session_id` beyond the typed schema.
pub fn extract_session_id_from_raw_response(raw: &serde_json::Value) -> Option<String> {
    raw.get("sessionId")
        .or_else(|| raw.get("session_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
    fn resume_existing_requires_non_empty_session_id() {
        assert!(!resume_existing_has_session_id(None));
        assert!(!resume_existing_has_session_id(Some("")));
        assert!(!resume_existing_has_session_id(Some("   ")));
        assert!(resume_existing_has_session_id(Some("sess-x")));
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

    #[test]
    fn decide_session_started_emits_on_match() {
        assert_eq!(
            decide_session_started("sess-x", Some("sess-x")),
            SessionStartedDecision::Emit {
                session_id: "sess-x".into(),
            }
        );
    }

    #[test]
    fn decide_session_started_refuses_on_mismatch_without_rewrite() {
        match decide_session_started("sess-old", Some("sess-new")) {
            SessionStartedDecision::RefuseUnresumable { reason } => {
                assert!(reason.contains("mismatch"));
                assert!(reason.contains("sess-old"));
                assert!(reason.contains("sess-new"));
            }
            other => panic!("expected refuse, got {other:?}"),
        }
    }

    #[test]
    fn decide_session_started_refuses_on_missing_actual() {
        assert!(matches!(
            decide_session_started("sess-x", None),
            SessionStartedDecision::RefuseUnresumable { .. }
        ));
    }

    #[test]
    fn extract_session_id_from_raw_prefers_camel_case() {
        let raw = serde_json::json!({"sessionId": "sid-1", "modes": null});
        assert_eq!(
            extract_session_id_from_raw_response(&raw).as_deref(),
            Some("sid-1")
        );
        let raw = serde_json::json!({"session_id": "sid-2"});
        assert_eq!(
            extract_session_id_from_raw_response(&raw).as_deref(),
            Some("sid-2")
        );
        assert_eq!(
            extract_session_id_from_raw_response(&serde_json::json!({})),
            None
        );
    }

    #[test]
    fn gate_default_always_emits_expected() {
        assert_eq!(
            gate_session_started_for_attach(
                SessionAttachMode::Default,
                "sess-x",
                Some("sess-other"),
            ),
            SessionStartedDecision::Emit {
                session_id: "sess-x".into(),
            }
        );
    }

    #[test]
    fn gate_resume_existing_emits_when_agent_omits_or_blanks_id() {
        assert_eq!(
            gate_session_started_for_attach(
                SessionAttachMode::ResumeExistingOnly,
                "sess-x",
                None,
            ),
            SessionStartedDecision::Emit {
                session_id: "sess-x".into(),
            }
        );
        assert_eq!(
            gate_session_started_for_attach(
                SessionAttachMode::ResumeExistingOnly,
                "sess-x",
                Some("   "),
            ),
            SessionStartedDecision::Emit {
                session_id: "sess-x".into(),
            }
        );
    }

    #[test]
    fn gate_resume_existing_emits_on_match() {
        assert_eq!(
            gate_session_started_for_attach(
                SessionAttachMode::ResumeExistingOnly,
                "sess-x",
                Some("sess-x"),
            ),
            SessionStartedDecision::Emit {
                session_id: "sess-x".into(),
            }
        );
    }

    #[test]
    fn gate_resume_existing_refuses_on_mismatch() {
        match gate_session_started_for_attach(
            SessionAttachMode::ResumeExistingOnly,
            "sess-old",
            Some("sess-new"),
        ) {
            SessionStartedDecision::RefuseUnresumable { reason } => {
                assert!(reason.contains("mismatch"));
                assert!(reason.contains("sess-old"));
                assert!(reason.contains("sess-new"));
            }
            other => panic!("expected refuse, got {other:?}"),
        }
    }
}
