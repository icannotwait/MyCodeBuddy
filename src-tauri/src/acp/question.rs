//! Interactive multiple-choice question ("ask the user") domain types.
//!
//! Mid-turn an agent can ask the user one or more multiple-choice questions and
//! BLOCK until the user answers — the `ask_user_question` MCP tool exposed by
//! `codeg-mcp`. Unlike live-feedback ([`crate::acp::feedback`]), which is a
//! non-blocking pull the user pushes into, a question PAUSES the agent's tool
//! call: the questions render as an interactive card above the conversation
//! input box (driven by [`crate::acp::session_state::SessionState`], in-memory
//! and turn-scoped — it is real-time steering, not durable history), and the
//! tool call returns only once the user submits their choices.
//!
//! This module holds the pieces shared across layers so the manager, the
//! delegation listener, the MCP companion plumbing, and the settings command
//! don't each grow their own copy:
//!   * [`QuestionSpec`] / [`QuestionOption`] — one question + its choices.
//!   * [`PendingQuestionState`] — the awaiting-answer set stored on the session.
//!   * [`QuestionAnswer`] / [`QuestionAnswerItem`] — the user's submission
//!     (frontend → backend).
//!   * [`QuestionOutcome`] / [`QuestionAnsweredItem`] — the self-describing
//!     result handed back to the blocked tool (so the companion can render it
//!     without re-holding the questions).
//!   * [`SessionQuestionAccess`] — the listener-facing trait the production
//!     `ConnectionManager` implements (kept here so the listener can be unit
//!     tested with an in-memory stub, mirroring `SessionFeedbackAccess`).
//!   * [`QuestionRuntimeConfig`] — the hot-swappable "is the feature on?" flag,
//!     read at MCP injection time (mirrors [`crate::acp::feedback`]).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, RwLock};

/// Max questions per `ask_user_question` call. Matches Claude Code's
/// `AskUserQuestion` contract; the JSON schema advertises the same `maxItems`.
pub const MAX_QUESTIONS: usize = 4;
/// Min / max selectable options per question. Fewer than two options is not a
/// meaningful choice; more than four overwhelms the card. Matches Claude Code.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
/// Max characters for a question's short `header` chip.
pub const MAX_HEADER_CHARS: usize = 12;
/// Per-field sanity bound (characters) for every agent/user-supplied free-text
/// field: the question text, each option label + description, and the free-text
/// "Other" answer. The full text rides in the broadcast event, the snapshot, and
/// the agent-facing tool result, so this caps the blast radius of a pathological
/// field — whether from a malformed agent (`parse_questions`) or a hand-rolled
/// client hitting `acp_answer_question` directly (`build_outcome`). The UI can't
/// produce anything this long.
pub const MAX_QUESTION_TEXT_CHARS: usize = 4096;

/// One selectable choice in a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Concise display text. A recommended option puts itself first and ends
    /// its label with "(Recommended)" (a string convention, like Claude Code).
    pub label: String,
    /// What this choice means / its trade-off. May be empty.
    pub description: String,
}

/// A single multiple-choice question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionSpec {
    /// Backend-minted stable id. Used as the answer correlation key instead of
    /// the question text (which Claude Code keys on) so duplicate question
    /// strings or reordering can't collide.
    pub id: String,
    /// The full question shown to the user.
    pub question: String,
    /// Short category label (≤ [`MAX_HEADER_CHARS`]) rendered as a chip.
    pub header: String,
    /// When true the user may select multiple options.
    pub multi_select: bool,
    /// The choices ([`MIN_OPTIONS`]..=[`MAX_OPTIONS`]).
    pub options: Vec<QuestionOption>,
}

/// The pending (awaiting-answer) question set stored on
/// `SessionState.pending_question` and carried on `to_snapshot()` so a client
/// attaching mid-turn (cold attach, reconnect, another window) re-renders the
/// card even though the one-shot `QuestionRequest` event won't replay for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingQuestionState {
    pub question_id: String,
    pub questions: Vec<QuestionSpec>,
    pub created_at: DateTime<Utc>,
}

/// One question's answer (frontend → backend). `labels` carries the selected
/// option labels (and any free-text "Other" the user typed, which the host UI
/// always offers); single-select submits exactly one label. camelCase on the
/// wire — this is constructed by the frontend, not read from an event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswerItem {
    pub question_id: String,
    pub labels: Vec<String>,
}

/// The user's full submission for a pending question set (frontend → backend →
/// the blocked tool). `declined` is set when the user dismissed the card
/// without choosing — the agent then proceeds with its own judgment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    #[serde(default)]
    pub answers: Vec<QuestionAnswerItem>,
    #[serde(default)]
    pub declined: bool,
}

/// One answered question, joined with its prompt text so the result is
/// self-describing (the companion renders it without holding the questions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnsweredItem {
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    /// The labels the user chose (or typed via "Other").
    pub selected: Vec<String>,
}

/// The resolved outcome delivered over the broker socket to the blocked tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOutcome {
    #[serde(default)]
    pub answers: Vec<QuestionAnsweredItem>,
    #[serde(default)]
    pub declined: bool,
}

/// What [`SessionQuestionAccess::register_question`] hands back to the listener:
/// the new question id plus the receiver to await the user's answer on.
pub struct RegisteredQuestion {
    pub question_id: String,
    pub answer_rx: oneshot::Receiver<QuestionOutcome>,
}

/// Listener-facing access to register / cancel a pending question on a parent
/// connection. The production impl (`ConnectionManagerQuestionLookup`) wraps the
/// `ConnectionManager`; tests use an in-memory stub. Mirrors
/// [`crate::acp::feedback::SessionFeedbackAccess`] and
/// `crate::acp::delegation::listener::ParentSessionLookup`.
#[async_trait]
pub trait SessionQuestionAccess: Send + Sync {
    /// Register a question set on the parent connection (resolved from the
    /// per-launch token), broadcast it to every attached client, and return a
    /// receiver that resolves when the user answers (or the question is
    /// canceled). `None` when the connection is gone — nothing to ask.
    async fn register_question(
        &self,
        parent_connection_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion>;

    /// Cancel a pending question — the companion's tool call was canceled
    /// (peer-close) or the connection is tearing down. Removes it and clears
    /// the card on every client. No-op if it was already answered / gone.
    async fn cancel_question(&self, parent_connection_id: &str, question_id: &str);

    /// Cancel every pending question parked on a connection that is tearing
    /// down. Called from the `run_connection` cleanup guard (alongside the
    /// delegation `cancel_by_parent` cascade) so a question entry — and the
    /// listener task parked on it — is reclaimed synchronously on disconnect,
    /// rather than lingering until the companion's ask socket happens to close.
    /// No-op when the connection has no pending ask.
    async fn cancel_questions_by_parent(&self, parent_connection_id: &str);
}

/// Validate + parse the MCP `ask_user_question` arguments into typed
/// [`QuestionSpec`]s, minting a stable id per question. Enforces the contract
/// (1..=[`MAX_QUESTIONS`] questions, each with a non-empty question + header
/// ≤ [`MAX_HEADER_CHARS`] and [`MIN_OPTIONS`]..=[`MAX_OPTIONS`] labeled options)
/// so a malformed call is rejected synchronously with a helpful message the LLM
/// can fix, rather than round-tripping bad data. `multiSelect` defaults to
/// false; an option `description` defaults to empty (lenient).
pub fn parse_questions(arguments: &Value) -> Result<Vec<QuestionSpec>, String> {
    let arr = arguments
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "ask_user_question requires a `questions` array".to_string())?;
    if arr.is_empty() {
        return Err("ask_user_question requires at least one question".to_string());
    }
    if arr.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask_user_question supports at most {MAX_QUESTIONS} questions per call"
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (qi, q) in arr.iter().enumerate() {
        let question = q
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("questions[{qi}] is missing a non-empty `question`"))?;
        if question.chars().count() > MAX_QUESTION_TEXT_CHARS {
            return Err(format!(
                "questions[{qi}] `question` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
            ));
        }
        let header = q
            .get("header")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("questions[{qi}] is missing a non-empty `header`"))?;
        if header.chars().count() > MAX_HEADER_CHARS {
            return Err(format!(
                "questions[{qi}] `header` exceeds {MAX_HEADER_CHARS} characters"
            ));
        }
        let multi_select = q
            .get("multiSelect")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let opts = q
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("questions[{qi}] is missing an `options` array"))?;
        if opts.len() < MIN_OPTIONS || opts.len() > MAX_OPTIONS {
            return Err(format!(
                "questions[{qi}] must have between {MIN_OPTIONS} and {MAX_OPTIONS} options"
            ));
        }
        let mut options = Vec::with_capacity(opts.len());
        for (oi, o) in opts.iter().enumerate() {
            let label = o
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("questions[{qi}].options[{oi}] is missing a non-empty `label`")
                })?;
            if label.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `label` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            let description = o
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if description.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `description` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            options.push(QuestionOption {
                label: label.to_string(),
                description,
            });
        }
        // Reject duplicate option labels within a question: the UI uses the
        // label as both the React key and the selection identity, and the
        // answer is submitted by label — duplicates would be ambiguous (select
        // one, select both) and collide on the key.
        let mut seen_labels = std::collections::HashSet::new();
        for o in &options {
            if !seen_labels.insert(o.label.as_str()) {
                return Err(format!(
                    "questions[{qi}] has duplicate option label {:?}",
                    o.label
                ));
            }
        }
        out.push(QuestionSpec {
            id: uuid::Uuid::new_v4().to_string(),
            question: question.to_string(),
            header: header.to_string(),
            multi_select,
            options,
        });
    }
    Ok(out)
}

/// Re-assert the [`parse_questions`] count + size bounds on already-typed specs.
/// The companion validates before sending, but the broker socket is only
/// token-gated, so a hand-rolled client could bypass that path and ride
/// oversized or malformed specs straight into the broadcast `QuestionRequest`
/// event and the `pending_question` snapshot. The listener registers through
/// this, declining the ask on `Err` rather than trusting unbounded input — the
/// authoritative answer-side bounds already live in [`build_outcome`], so this
/// closes the matching gap on the request side. Bounds mirror `parse_questions`.
pub fn validate_specs(specs: &[QuestionSpec]) -> Result<(), String> {
    if specs.is_empty() || specs.len() > MAX_QUESTIONS {
        return Err(format!(
            "expected 1..={MAX_QUESTIONS} questions, got {}",
            specs.len()
        ));
    }
    let mut seen_ids = std::collections::HashSet::new();
    for (qi, q) in specs.iter().enumerate() {
        // `parse_questions` mints a fresh uuid per question; a hand-rolled client
        // could send empty / colliding ids, and the answer routing + UI state map
        // key on `id`, so duplicates would misroute or collide.
        if q.id.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `id`"));
        }
        if !seen_ids.insert(q.id.as_str()) {
            return Err(format!("questions[{qi}] has a duplicate `id` {:?}", q.id));
        }
        if q.question.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `question`"));
        }
        if q.question.chars().count() > MAX_QUESTION_TEXT_CHARS {
            return Err(format!(
                "questions[{qi}] `question` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
            ));
        }
        if q.header.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `header`"));
        }
        if q.header.chars().count() > MAX_HEADER_CHARS {
            return Err(format!(
                "questions[{qi}] `header` exceeds {MAX_HEADER_CHARS} characters"
            ));
        }
        if q.options.len() < MIN_OPTIONS || q.options.len() > MAX_OPTIONS {
            return Err(format!(
                "questions[{qi}] must have between {MIN_OPTIONS} and {MAX_OPTIONS} options"
            ));
        }
        let mut seen_labels = std::collections::HashSet::new();
        for (oi, o) in q.options.iter().enumerate() {
            if o.label.trim().is_empty() {
                return Err(format!(
                    "questions[{qi}].options[{oi}] has an empty `label`"
                ));
            }
            if o.label.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `label` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            // Mirror parse_questions: labels are the React key + selection identity
            // and answers are submitted by label, so duplicates (trimmed) are
            // ambiguous.
            if !seen_labels.insert(o.label.trim()) {
                return Err(format!(
                    "questions[{qi}] has a duplicate option label {:?}",
                    o.label
                ));
            }
            if o.description.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `description` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
        }
    }
    Ok(())
}

/// Join the user's submission with the original questions into a self-describing
/// [`QuestionOutcome`], normalizing + validating against the stored specs. The
/// UI enforces these rules, but `acp_answer_question` is a plain API a stale or
/// hand-rolled client can hit directly, so the authoritative checks live here.
///
/// Iterates the TRUSTED `questions` (≤ [`MAX_QUESTIONS`]), not the client's
/// `answers`, so a flood of unknown / duplicate answer items can neither grow an
/// intermediate set nor bloat the output — extra items are simply never looked
/// up. For each spec question it takes the first matching answer (dedup) and:
///   * trims each label, drops empties, bounds each to [`MAX_QUESTION_TEXT_CHARS`];
///   * caps the count — single-select keeps 1, multi-select keeps at most every
///     real option plus one free-text "Other" (`options.len() + 1`);
///   * drops a question left with no usable label.
///
/// Output is therefore bounded by the question set's own size, in asked order.
/// A declined submission yields an empty, `declined: true` outcome.
pub fn build_outcome(questions: &[QuestionSpec], answer: &QuestionAnswer) -> QuestionOutcome {
    if answer.declined {
        return QuestionOutcome {
            answers: Vec::new(),
            declined: true,
        };
    }
    let answers = questions
        .iter()
        .filter_map(|spec| {
            let a = answer.answers.iter().find(|a| a.question_id == spec.id)?;
            // Cap selections to the question's own size: single-select → 1;
            // multi-select → every real option plus one "Other". Enforce the cap
            // DURING iteration (early break, allocate only kept labels) so a
            // pathological `labels` array can't do unbounded intermediate work.
            let cap = if spec.multi_select {
                spec.options.len() + 1
            } else {
                1
            };
            let mut labels: Vec<String> = Vec::with_capacity(cap);
            for l in &a.labels {
                if labels.len() == cap {
                    break;
                }
                let trimmed = l.trim();
                if trimmed.is_empty() {
                    continue;
                }
                labels.push(trimmed.chars().take(MAX_QUESTION_TEXT_CHARS).collect());
            }
            if labels.is_empty() {
                return None;
            }
            Some(QuestionAnsweredItem {
                question: spec.question.clone(),
                header: spec.header.clone(),
                multi_select: spec.multi_select,
                selected: labels,
            })
        })
        .collect();
    QuestionOutcome {
        answers,
        declined: false,
    }
}

/// The hot-swappable feature config read at MCP injection time. Kept tiny and
/// separate from `FeedbackConfig` / `DelegationConfig` so the three features
/// toggle independently — `codeg-mcp` is injected when ANY is enabled, and each
/// tool is listed only when its own feature is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionConfig {
    pub enabled: bool,
}

/// Shared, hot-swappable handle to [`QuestionConfig`]. Cloned into
/// `DelegationInjection` (read at injection) and `AppState` (updated on save).
/// Mirrors [`crate::acp::feedback::FeedbackRuntimeConfig`].
#[derive(Clone, Default)]
pub struct QuestionRuntimeConfig {
    inner: Arc<RwLock<QuestionConfig>>,
}

impl QuestionRuntimeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> QuestionConfig {
        self.inner.read().await.clone()
    }

    pub async fn set(&self, cfg: QuestionConfig) {
        *self.inner.write().await = cfg;
    }

    /// Convenience read used at MCP injection time.
    pub async fn is_enabled(&self) -> bool {
        self.inner.read().await.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_args() -> Value {
        json!({
            "questions": [{
                "question": "Which approach?",
                "header": "Approach",
                "multiSelect": false,
                "options": [
                    { "label": "Incremental", "description": "smaller diffs" },
                    { "label": "Rewrite", "description": "clean slate" }
                ]
            }]
        })
    }

    #[test]
    fn parse_questions_happy_path_mints_ids() {
        let qs = parse_questions(&valid_args()).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].question, "Which approach?");
        assert_eq!(qs[0].header, "Approach");
        assert!(!qs[0].multi_select);
        assert_eq!(qs[0].options.len(), 2);
        assert!(!qs[0].id.is_empty());
    }

    #[test]
    fn validate_specs_accepts_well_formed_and_rejects_malformed() {
        // What parse_questions mints passes the request-side re-check.
        let good = parse_questions(&valid_args()).unwrap();
        assert!(validate_specs(&good).is_ok());

        // Build a spec with a tunable question, option count, and first-label
        // length so each bound can be tripped independently.
        let spec = |question: &str, options: usize, first_label_len: usize| QuestionSpec {
            id: "q".into(),
            question: question.into(),
            header: "H".into(),
            multi_select: false,
            options: (0..options)
                .map(|i| QuestionOption {
                    label: if first_label_len > 0 && i == 0 {
                        "x".repeat(first_label_len)
                    } else {
                        format!("opt{i}")
                    },
                    description: String::new(),
                })
                .collect(),
        };

        assert!(validate_specs(&[]).is_err(), "empty set");
        assert!(
            validate_specs(&[spec(&"q".repeat(MAX_QUESTION_TEXT_CHARS + 1), 2, 0)]).is_err(),
            "oversized question text"
        );
        assert!(
            validate_specs(&[spec("ok", MIN_OPTIONS - 1, 0)]).is_err(),
            "too few options"
        );
        assert!(
            validate_specs(&[spec("ok", MAX_OPTIONS + 1, 0)]).is_err(),
            "too many options"
        );
        assert!(
            validate_specs(&[spec("ok", 2, MAX_QUESTION_TEXT_CHARS + 1)]).is_err(),
            "oversized option label"
        );
        assert!(
            validate_specs(&[spec("   ", 2, 0)]).is_err(),
            "blank question"
        );

        // Duplicate question id across the set (spec() hardcodes id "q") — answer
        // routing + UI state key on id, so duplicates must be rejected.
        assert!(
            validate_specs(&[spec("a", 2, 0), spec("b", 2, 0)]).is_err(),
            "duplicate question id"
        );
        let blank_id = QuestionSpec {
            id: "  ".into(),
            question: "ok".into(),
            header: "H".into(),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "a".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "b".into(),
                    description: String::new(),
                },
            ],
        };
        assert!(validate_specs(&[blank_id]).is_err(), "blank id");
        // Duplicate option label within one question (parse_questions rejects it).
        let dup_label = QuestionSpec {
            id: "q".into(),
            question: "ok".into(),
            header: "H".into(),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "same".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: " same ".into(),
                    description: String::new(),
                },
            ],
        };
        assert!(
            validate_specs(&[dup_label]).is_err(),
            "duplicate option label (trimmed)"
        );
    }

    #[test]
    fn parse_questions_rejects_empty_and_overlong_sets() {
        assert!(parse_questions(&json!({ "questions": [] })).is_err());
        assert!(parse_questions(&json!({})).is_err());
        let mut many = Vec::new();
        for _ in 0..(MAX_QUESTIONS + 1) {
            many.push(json!({
                "question": "q", "header": "h", "multiSelect": false,
                "options": [{ "label": "a", "description": "" }, { "label": "b", "description": "" }]
            }));
        }
        assert!(parse_questions(&json!({ "questions": many })).is_err());
    }

    #[test]
    fn parse_questions_enforces_option_count_and_header_len() {
        // One option is not a choice.
        let one_opt = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [{ "label": "only", "description": "" }]
        }] });
        assert!(parse_questions(&one_opt).is_err());
        // Header too long.
        let long_header = json!({ "questions": [{
            "question": "q", "header": "this-header-is-way-too-long", "multiSelect": false,
            "options": [{ "label": "a", "description": "" }, { "label": "b", "description": "" }]
        }] });
        assert!(parse_questions(&long_header).is_err());
    }

    #[test]
    fn build_outcome_maps_labels_by_id_and_drops_unknown() {
        let qs = parse_questions(&valid_args()).unwrap();
        let qid = qs[0].id.clone();
        let answer = QuestionAnswer {
            answers: vec![
                QuestionAnswerItem {
                    question_id: qid,
                    labels: vec!["Incremental".into()],
                },
                QuestionAnswerItem {
                    question_id: "does-not-exist".into(),
                    labels: vec!["ghost".into()],
                },
            ],
            declined: false,
        };
        let outcome = build_outcome(&qs, &answer);
        assert!(!outcome.declined);
        assert_eq!(outcome.answers.len(), 1);
        assert_eq!(outcome.answers[0].question, "Which approach?");
        assert_eq!(outcome.answers[0].selected, vec!["Incremental".to_string()]);
    }

    #[test]
    fn parse_questions_rejects_overlong_option_label() {
        let huge = "x".repeat(MAX_QUESTION_TEXT_CHARS + 1);
        let bad = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [
                { "label": huge, "description": "" },
                { "label": "B", "description": "" }
            ]
        }] });
        let err = parse_questions(&bad).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn build_outcome_caps_multiselect_labels_and_ignores_unknown() {
        // A multi-select question with 2 options: a flood of submitted labels
        // plus a flood of unknown answer items must NOT bloat the outcome.
        let args = json!({ "questions": [{
            "question": "Which modules?", "header": "Scope", "multiSelect": true,
            "options": [
                { "label": "auth", "description": "" },
                { "label": "billing", "description": "" }
            ]
        }] });
        let qs = parse_questions(&args).unwrap();
        let qid = qs[0].id.clone();
        let mut items = vec![QuestionAnswerItem {
            question_id: qid,
            labels: (0..1000).map(|i| format!("l{i}")).collect(),
        }];
        // 10k unknown answer items — must be ignored without growth.
        for i in 0..10_000 {
            items.push(QuestionAnswerItem {
                question_id: format!("ghost-{i}"),
                labels: vec!["x".into()],
            });
        }
        let outcome = build_outcome(
            &qs,
            &QuestionAnswer {
                answers: items,
                declined: false,
            },
        );
        assert_eq!(outcome.answers.len(), 1);
        // Cap = options.len() + 1 = 3 (every real option plus one "Other"); the
        // FIRST three are kept (early break — labels past the cap and the 10k
        // unknown items are never processed/retained).
        assert_eq!(
            outcome.answers[0].selected,
            vec!["l0".to_string(), "l1".to_string(), "l2".to_string()]
        );
    }

    #[test]
    fn parse_questions_rejects_duplicate_option_labels() {
        let dup = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [
                { "label": "Same", "description": "a" },
                { "label": "Same", "description": "b" }
            ]
        }] });
        let err = parse_questions(&dup).unwrap_err();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn build_outcome_normalizes_malformed_answer() {
        // Single-select with two labels + an empty + an oversize one, plus a
        // duplicate answer item and an unknown question id. The endpoint must
        // not trust this; build_outcome sanitizes it.
        let qs = parse_questions(&valid_args()).unwrap();
        let qid = qs[0].id.clone();
        let huge = "x".repeat(MAX_QUESTION_TEXT_CHARS + 50);
        let answer = QuestionAnswer {
            answers: vec![
                QuestionAnswerItem {
                    question_id: qid.clone(),
                    labels: vec!["  ".into(), "Incremental".into(), huge.clone()],
                },
                // Duplicate item for the same question — must be deduped (first wins).
                QuestionAnswerItem {
                    question_id: qid,
                    labels: vec!["Rewrite".into()],
                },
                // Unknown question — dropped.
                QuestionAnswerItem {
                    question_id: "ghost".into(),
                    labels: vec!["x".into()],
                },
            ],
            declined: false,
        };
        let outcome = build_outcome(&qs, &answer);
        assert_eq!(outcome.answers.len(), 1, "deduped + unknown dropped");
        // Single-select: empty trimmed away, then truncated to one → "Incremental".
        assert_eq!(outcome.answers[0].selected, vec!["Incremental".to_string()]);
    }

    #[test]
    fn build_outcome_drops_question_with_only_empty_labels() {
        let qs = parse_questions(&valid_args()).unwrap();
        let outcome = build_outcome(
            &qs,
            &QuestionAnswer {
                answers: vec![QuestionAnswerItem {
                    question_id: qs[0].id.clone(),
                    labels: vec!["   ".into(), "".into()],
                }],
                declined: false,
            },
        );
        assert!(outcome.answers.is_empty());
    }

    #[test]
    fn build_outcome_declined_is_empty() {
        let qs = parse_questions(&valid_args()).unwrap();
        let outcome = build_outcome(
            &qs,
            &QuestionAnswer {
                answers: vec![],
                declined: true,
            },
        );
        assert!(outcome.declined);
        assert!(outcome.answers.is_empty());
    }

    #[tokio::test]
    async fn runtime_config_hot_swaps() {
        let cfg = QuestionRuntimeConfig::new();
        assert!(!cfg.is_enabled().await);
        cfg.set(QuestionConfig { enabled: true }).await;
        assert!(cfg.is_enabled().await);
    }
}
