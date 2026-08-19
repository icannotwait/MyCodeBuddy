//! Process-local delegation reliability metrics and secret-free audit records.
//!
//! Counters are fixed `AtomicU64` fields plus small labeled `BTreeMap`s under a
//! `std::sync::Mutex`. Snapshots are deterministic and serializable for the
//! authenticated debug endpoint.
//!
//! **Security:** never log or serialize task prompts, result text, API keys,
//! companion tokens, env/config values, raw MCP/tool messages, provider
//! payloads, or credentials. Labels are stable enums / agent type / error codes
//! / ids and bounded numeric durations only.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acp::cursor_enrichment::CursorEnrichmentFailure;
use crate::acp::delegation::broker::IdentitylessBackfillResult;
use crate::acp::delegation::continuation::types::{
    ContinuationFailureCode, ContinuationState, ContinuationWakeReason,
};
use crate::acp::delegation::route::{
    DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource, NativeSuppressionPlan,
    RouteDegradedReason, RouteResolutionError,
};
use crate::acp::delegation::transport::CancelDelegationReason;
use crate::acp::delegation::types::{TaskObservation, TaskStatus};
use crate::acp::delegation::workflow::{
    ArtifactFailure, CompletionIntentReason, CompletionIntentSource, CompletionRole,
    PlanReviewChangeV2, PlanReviewNextAction,
};
use crate::models::AgentType;

// ── Runtime projection diagnostics (Task 8; Task 11 adds counters) ─────────

/// Stable diagnostic kind for runtime projection failures.
/// Task 11 adds a counter without changing Task 8 call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProjectionErrorKind {
    Event,
    Persistence,
    TerminalPersistence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionMetricPhase {
    Design,
    Plan,
    Tasks,
    Final,
    Unknown,
}

impl CompletionMetricPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Plan => "plan",
            Self::Tasks => "tasks",
            Self::Final => "final",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionScopeInvalidationDimension {
    Node,
    Producer,
    Instruction,
    Policy,
    Requirements,
    FinalFindings,
    Lineage,
    TaskScope,
    Artifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionFinalMetricState {
    ContextAvailable,
    ContextMissing,
    PackagePersisted,
    PackageResolved,
    PackageIncomplete,
    DecisionRequired,
}

impl CompletionFinalMetricState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ContextAvailable => "context_available",
            Self::ContextMissing => "context_missing",
            Self::PackagePersisted => "package_persisted",
            Self::PackageResolved => "package_resolved",
            Self::PackageIncomplete => "package_incomplete",
            Self::DecisionRequired => "decision_required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionContinuationReason {
    DecisionResolved,
    ArtifactRecovered,
    PlanReview,
}

impl CompletionContinuationReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::DecisionResolved => "decision_resolved",
            Self::ArtifactRecovered => "artifact_recovered",
            Self::PlanReview => "plan_review",
        }
    }
}

impl CompletionScopeInvalidationDimension {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Producer => "producer",
            Self::Instruction => "instruction",
            Self::Policy => "policy",
            Self::Requirements => "requirements",
            Self::FinalFindings => "final_findings",
            Self::Lineage => "lineage",
            Self::TaskScope => "task_scope",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryMetricEventKind {
    #[serde(rename = "recovery.decision")]
    Decision,
    #[serde(rename = "recovery.confirmation_requested")]
    ConfirmationRequested,
    #[serde(rename = "recovery.confirmation_approved")]
    ConfirmationApproved,
    #[serde(rename = "recovery.confirmation_declined")]
    ConfirmationDeclined,
    #[serde(rename = "recovery.authorization_consumed")]
    AuthorizationConsumed,
    #[serde(rename = "recovery.authorization_rejected")]
    AuthorizationRejected,
    #[serde(rename = "recovery.resume_failed")]
    ResumeFailed,
    #[serde(rename = "recovery.replacement_admitted")]
    ReplacementAdmitted,
}

impl RecoveryMetricEventKind {
    pub const ALL: [Self; 8] = [
        Self::Decision,
        Self::ConfirmationRequested,
        Self::ConfirmationApproved,
        Self::ConfirmationDeclined,
        Self::AuthorizationConsumed,
        Self::AuthorizationRejected,
        Self::ResumeFailed,
        Self::ReplacementAdmitted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "recovery.decision",
            Self::ConfirmationRequested => "recovery.confirmation_requested",
            Self::ConfirmationApproved => "recovery.confirmation_approved",
            Self::ConfirmationDeclined => "recovery.confirmation_declined",
            Self::AuthorizationConsumed => "recovery.authorization_consumed",
            Self::AuthorizationRejected => "recovery.authorization_rejected",
            Self::ResumeFailed => "recovery.resume_failed",
            Self::ReplacementAdmitted => "recovery.replacement_admitted",
        }
    }
}

macro_rules! stable_recovery_metric_value {
    ($name:ident, [$($value:literal),+ $(,)?]) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Option<Self> {
                matches!(value, $($value)|+).then(|| Self(value.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

stable_recovery_metric_value!(
    RecoveryMetricAction,
    ["continue", "fresh_dispatch", "replace",]
);

stable_recovery_metric_value!(
    RecoveryMetricCause,
    [
        "completed",
        "revision_eligible_failure",
        "unexpected_transport_loss",
        "unexpected_process_loss",
        "unexpected_session_loss",
        "unexpected_host_restart",
        "unexpected_child_connection_loss",
        "parent_canceled",
        "parent_turn_failed",
        "join_abandoned",
        "user_cancelled",
        "tool_stalled_timeout",
        "legacy_parent_disconnect",
        "intentional_parent_disconnect",
        "malformed_termination_audit",
        "pre_admission_retry",
        "pre_admission_abort",
        "admission_failed",
        "admission_unknown",
        "missing_resume_identity",
        "unsupported_reuse",
        "persisted_unresumable",
        "continue_budget_exhausted",
        "replacement_budget_exhausted",
        "route_rejected",
        "stale_source",
        "busy_source",
        "structural_fence",
        "contradictory_evidence",
    ]
);

stable_recovery_metric_value!(
    RecoveryMetricRisk,
    [
        "normal",
        "execution_may_have_occurred",
        "explicit_user_stop",
        "legacy_unknown_origin",
    ]
);

stable_recovery_metric_value!(
    RecoveryMetricCode,
    [
        "recovery_confirmation_required",
        "recovery_authorization_database_error",
        "recovery_authorization_not_found",
        "recovery_authorization_blocked",
        "recovery_authorization_cancelled",
        "recovery_authorization_challenge_conflict",
        "recovery_authorization_question_binding_conflict",
        "recovery_authorization_parent_mismatch",
        "recovery_authorization_subject_kind_mismatch",
        "recovery_authorization_subject_id_mismatch",
        "recovery_authorization_fingerprint_mismatch",
        "recovery_authorization_action_mismatch",
        "recovery_authorization_payload_mismatch",
        "recovery_authorization_pending",
        "recovery_authorization_declined",
        "recovery_authorization_expired",
        "recovery_authorization_abandoned",
        "recovery_authorization_consumed_conflict",
        "unresumable",
    ]
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DelegationRecoveryMetricEvent {
    pub kind: RecoveryMetricEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<RecoveryMetricAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<RecoveryMetricCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<RecoveryMetricRisk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<RecoveryMetricCode>,
}

impl DelegationRecoveryMetricEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn validated(
        kind: RecoveryMetricEventKind,
        task_id: Option<&str>,
        authorization_id: Option<&str>,
        parent_id: Option<String>,
        child_id: Option<String>,
        action: Option<&str>,
        cause: Option<&str>,
        risk: Option<&str>,
        code: Option<&str>,
    ) -> Option<Self> {
        let action = match action {
            Some(value) => Some(RecoveryMetricAction::parse(value)?),
            None => None,
        };
        let cause = match cause {
            Some(value) => Some(RecoveryMetricCause::parse(value)?),
            None => None,
        };
        let risk = match risk {
            Some(value) => Some(RecoveryMetricRisk::parse(value)?),
            None => None,
        };
        let code = match code {
            Some(value) => Some(RecoveryMetricCode::parse(value)?),
            None => None,
        };
        Some(Self {
            kind,
            task_id: task_id.map(str::to_string),
            authorization_id: authorization_id.map(str::to_string),
            parent_id,
            child_id,
            action,
            cause,
            risk,
            code,
        })
    }
}

impl RuntimeProjectionErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Persistence => "persistence",
            Self::TerminalPersistence => "terminal_persistence",
        }
    }
}

// ── Wait labels ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitModeLabel {
    Snapshot,
    Supervised,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReturnReason {
    Snapshot,
    Terminal,
    Observation,
    Deadline,
    PeerClosed,
}

/// Bounded source label for public external prompt admission paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAdmissionSource {
    Foreground,
    Background,
    LinkedForeground,
    LinkedBackground,
}

impl PromptAdmissionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::LinkedForeground => "linked_foreground",
            Self::LinkedBackground => "linked_background",
        }
    }
}

/// Result of applying a native-suppression plan at process launch (no secrets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionApplication {
    /// Suppression tokens/env were applied for a Codeg-effective managed plan.
    Applied,
    /// Plan does not require native suppression (native / unmanaged / None).
    NotApplicable,
    /// Application failed (invalid configuration).
    Failed,
}

// ── Snapshot ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationMetricsSnapshot {
    pub route_selections: BTreeMap<String, u64>,
    pub safe_fallbacks: BTreeMap<String, u64>,
    pub suppression_failures: BTreeMap<String, u64>,
    pub accepted_count: u64,
    /// Per-agent accepted generations at the durable `reserving → running`
    /// boundary. Labels use stable [`agent_type_label`] values only.
    #[serde(default)]
    pub accepted_by_agent: BTreeMap<String, u64>,
    pub completed_count: u64,
    pub failed_count: u64,
    pub canceled_count: u64,
    pub terminal_duration_ms_total: u64,
    pub stalled_episode_count: u64,
    pub stalled_recovery_count: u64,
    pub snapshot_wait_count: u64,
    pub supervised_wait_count: u64,
    pub terminal_wait_count: u64,
    pub wait_duration_ms_total: u64,
    pub wait_return_reasons: BTreeMap<String, u64>,
    pub explicit_taskfail_cancel_count: u64,
    pub explicit_user_cancel_count: u64,
    pub explicit_other_cancel_count: u64,
    pub mcp_request_cancel_count: u64,
    pub mixed_route_invariant_violations: u64,
    pub prompt_rejected: BTreeMap<String, u64>,
    pub continuation_armed: u64,
    pub continuation_suspended: u64,
    pub continuation_wake_claimed: BTreeMap<String, u64>,
    pub continuation_prompt_admitted: u64,
    pub continuation_cancelled: BTreeMap<String, u64>,
    pub continuation_failed: BTreeMap<String, u64>,
    pub continuation_reconciled: BTreeMap<String, u64>,
    pub continuation_duplicate_claim_suppressed: u64,
    pub continuation_wait_duration_ms_count: BTreeMap<String, u64>,
    pub continuation_wait_duration_ms_total: BTreeMap<String, u64>,
    pub continuation_suspend_duration_ms_count: u64,
    pub continuation_suspend_duration_ms_total: u64,
    pub continuation_prompt_delivery_retry: u64,
    /// Promote-local transient retries: labels `busy`, `locked`, `busy_snapshot`.
    /// `busy_snapshot` only when extended SQLite code 517 was extracted.
    #[serde(default)]
    pub promote_retries: BTreeMap<String, u64>,
    /// Final promote failure classes: `cas`, `budget`, `busy_exhausted`, `permanent`.
    /// Pairing: `busy_exhausted` is lock-class retry budget only (not single-shot permanent).
    #[serde(default)]
    pub promote_failures: BTreeMap<String, u64>,
    /// `admission_failed` settlements keyed by stable agent label.
    #[serde(default)]
    pub admission_failed_by_agent: BTreeMap<String, u64>,
    /// New single-flight settlement retry ownership after bounded settle exhaust
    /// (or freeze ownership that will not clear on immediate success).
    /// Pairing with [`Self::settlement_retry_exhausted`]:
    /// - new owner after exhaust → both increment
    /// - existing owner after exhaust → only exhausted
    /// - fence removed after immediate settle success → neither
    #[serde(default)]
    pub settlement_retry_enqueued: u64,
    /// Bounded settlement loop handed durable truth to a retry owner (new or existing).
    #[serde(default)]
    pub settlement_retry_exhausted: u64,
    #[serde(default)]
    pub recovery_events: BTreeMap<String, u64>,
    #[serde(default)]
    pub recovery_lookup_unknown: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_resolutions: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_tool_supersessions: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_decisions: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_artifact_failures: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_scope_invalidations: BTreeMap<String, u64>,
    #[serde(default)]
    pub completion_protocol: CompletionProtocolMetricsSnapshot,
    /// Count of Cursor-store enrichment lookups scheduled (only after the
    /// spawned session gate confirms a live `Cursor` connection with a
    /// validated external session id).
    #[serde(default)]
    pub cursor_enrichment_scheduled: u64,
    /// Count of successful identity-less backfills (store match found and
    /// applied/observed before the deadline).
    #[serde(default)]
    pub cursor_enrichment_resolved: u64,
    /// Closed failure-class counters. Keys are exactly
    /// [`crate::acp::cursor_enrichment::CursorEnrichmentFailure::as_str`].
    #[serde(default)]
    pub cursor_enrichment_failed: BTreeMap<String, u64>,
    /// Closed backfill-result counters (`applied` | `same` | `conflict` | `stale`).
    #[serde(default)]
    pub cursor_enrichment_backfill: BTreeMap<String, u64>,
    #[serde(default)]
    pub cursor_enrichment_duration_ms_count: u64,
    #[serde(default)]
    pub cursor_enrichment_duration_ms_total: u64,
}

/// Bounded completion-protocol observability. Every label is produced from a
/// closed enum or stored protocol mode; no prompts, paths, report bytes, or
/// profile configuration enter this snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionProtocolMetricsSnapshot {
    pub resolutions: BTreeMap<String, u64>,
    pub tool_accepted: BTreeMap<String, u64>,
    pub tool_superseded: BTreeMap<String, u64>,
    pub intent_diagnostics: BTreeMap<String, u64>,
    pub decision_lifecycle: BTreeMap<String, u64>,
    pub artifact_failures: BTreeMap<String, u64>,
    pub scope_invalidations: BTreeMap<String, u64>,
    pub plan_classifications: BTreeMap<String, u64>,
    pub final_context_states: BTreeMap<String, u64>,
    pub outbox_states: BTreeMap<String, u64>,
    pub plan_reducer_states: BTreeMap<String, u64>,
    pub continuation_reasons: BTreeMap<String, u64>,
    pub natural_language_fallback_count: u64,
    pub resolution_count: u64,
    pub adjudication_latency_ms_count: u64,
    pub adjudication_latency_ms_total: u64,
    pub oldest_open_decision_age_ms: u64,
    pub outbox_latency_ms_count: u64,
    pub outbox_latency_ms_total: u64,
    pub format_only_child_runs: u64,
    pub card_reemit_prompts: u64,
    pub sibling_reruns: u64,
}

// ── Metrics ────────────────────────────────────────────────────────────────

/// Process-local counters for delegation reliability observability.
#[derive(Debug, Default)]
pub struct DelegationMetrics {
    route_selections: Mutex<BTreeMap<String, u64>>,
    safe_fallbacks: Mutex<BTreeMap<String, u64>>,
    suppression_failures: Mutex<BTreeMap<String, u64>>,
    accepted_count: AtomicU64,
    accepted_by_agent: Mutex<BTreeMap<String, u64>>,
    /// Process-local set of task ids that already emitted accepted metrics for
    /// the current process. Prevents double-count on idempotent AlreadyRunning
    /// and commit-ambiguity Promoted reread after a prior emission.
    accepted_task_ids: Mutex<HashSet<String>>,
    completed_count: AtomicU64,
    failed_count: AtomicU64,
    canceled_count: AtomicU64,
    terminal_duration_ms_total: AtomicU64,
    stalled_episode_count: AtomicU64,
    stalled_recovery_count: AtomicU64,
    snapshot_wait_count: AtomicU64,
    supervised_wait_count: AtomicU64,
    terminal_wait_count: AtomicU64,
    wait_duration_ms_total: AtomicU64,
    wait_return_reasons: Mutex<BTreeMap<String, u64>>,
    explicit_taskfail_cancel_count: AtomicU64,
    explicit_user_cancel_count: AtomicU64,
    explicit_other_cancel_count: AtomicU64,
    mcp_request_cancel_count: AtomicU64,
    mixed_route_invariant_violations: AtomicU64,
    prompt_rejected: Mutex<BTreeMap<String, u64>>,
    continuation_armed: AtomicU64,
    continuation_suspended: AtomicU64,
    continuation_wake_claimed: Mutex<BTreeMap<String, u64>>,
    continuation_prompt_admitted: AtomicU64,
    continuation_cancelled: Mutex<BTreeMap<String, u64>>,
    continuation_failed: Mutex<BTreeMap<String, u64>>,
    continuation_reconciled: Mutex<BTreeMap<String, u64>>,
    continuation_duplicate_claim_suppressed: AtomicU64,
    continuation_wait_duration_ms_count: Mutex<BTreeMap<String, u64>>,
    continuation_wait_duration_ms_total: Mutex<BTreeMap<String, u64>>,
    continuation_suspend_duration_ms_count: AtomicU64,
    continuation_suspend_duration_ms_total: AtomicU64,
    continuation_prompt_delivery_retry: AtomicU64,
    promote_retries: Mutex<BTreeMap<String, u64>>,
    promote_failures: Mutex<BTreeMap<String, u64>>,
    admission_failed_by_agent: Mutex<BTreeMap<String, u64>>,
    settlement_retry_enqueued: AtomicU64,
    settlement_retry_exhausted: AtomicU64,
    recovery_event_counts: Mutex<BTreeMap<String, u64>>,
    recovery_lookup_unknown: Mutex<BTreeMap<String, u64>>,
    recovery_event_log: Mutex<Vec<DelegationRecoveryMetricEvent>>,
    completion_resolutions: Mutex<BTreeMap<String, u64>>,
    completion_tool_supersessions: Mutex<BTreeMap<String, u64>>,
    completion_decisions: Mutex<BTreeMap<String, u64>>,
    completion_artifact_failures: Mutex<BTreeMap<String, u64>>,
    completion_scope_invalidations: Mutex<BTreeMap<String, u64>>,
    completion_tool_accepted: Mutex<BTreeMap<String, u64>>,
    completion_decision_lifecycle: Mutex<BTreeMap<String, u64>>,
    completion_plan_classifications: Mutex<BTreeMap<String, u64>>,
    completion_final_context_states: Mutex<BTreeMap<String, u64>>,
    completion_outbox_states: Mutex<BTreeMap<String, u64>>,
    completion_plan_reducer_states: Mutex<BTreeMap<String, u64>>,
    completion_continuation_reasons: Mutex<BTreeMap<String, u64>>,
    completion_natural_language_fallback_count: AtomicU64,
    completion_resolution_count: AtomicU64,
    completion_adjudication_latency_ms_count: AtomicU64,
    completion_adjudication_latency_ms_total: AtomicU64,
    completion_oldest_open_decision_age_ms: AtomicU64,
    completion_outbox_latency_ms_count: AtomicU64,
    completion_outbox_latency_ms_total: AtomicU64,
    completion_format_only_child_runs: AtomicU64,
    completion_card_reemit_prompts: AtomicU64,
    completion_sibling_reruns: AtomicU64,
    cursor_enrichment_scheduled: AtomicU64,
    cursor_enrichment_resolved: AtomicU64,
    cursor_enrichment_failed: Mutex<BTreeMap<String, u64>>,
    cursor_enrichment_backfill: Mutex<BTreeMap<String, u64>>,
    cursor_enrichment_duration_ms_count: AtomicU64,
    cursor_enrichment_duration_ms_total: AtomicU64,
}

impl DelegationMetrics {
    fn inc_labeled(map: &Mutex<BTreeMap<String, u64>>, key: String) {
        Self::add_labeled(map, key, 1);
    }

    fn add_labeled(map: &Mutex<BTreeMap<String, u64>>, key: String, n: u64) {
        if n == 0 {
            return;
        }
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key).or_insert(0);
        *entry = (*entry).saturating_add(n);
    }

    pub fn record_completion_resolution(
        &self,
        source: CompletionIntentSource,
        role: CompletionRole,
    ) {
        self.completion_resolution_count
            .fetch_add(1, Ordering::Relaxed);
        if matches!(
            source,
            CompletionIntentSource::AssistantConclusion | CompletionIntentSource::Report
        ) {
            self.completion_natural_language_fallback_count
                .fetch_add(1, Ordering::Relaxed);
        }
        Self::inc_labeled(
            &self.completion_resolutions,
            format!(
                "{}:{}",
                completion_source_label(source),
                completion_role_label(role)
            ),
        );
    }

    pub fn record_completion_tool_accepted(&self, role: CompletionRole) {
        Self::inc_labeled(
            &self.completion_tool_accepted,
            completion_role_label(role).into(),
        );
    }

    pub fn record_completion_decision_opened(&self) {
        Self::inc_labeled(&self.completion_decision_lifecycle, "opened".into());
    }

    pub fn record_completion_decision_resolved(&self, latency: Duration, idempotent_replay: bool) {
        if idempotent_replay {
            return;
        }
        Self::inc_labeled(&self.completion_decision_lifecycle, "resolved".into());
        self.completion_adjudication_latency_ms_count
            .fetch_add(1, Ordering::Relaxed);
        self.completion_adjudication_latency_ms_total
            .fetch_add(Self::duration_ms_saturating(latency), Ordering::Relaxed);
    }

    pub fn record_completion_decision_superseded(&self) {
        Self::inc_labeled(&self.completion_decision_lifecycle, "superseded".into());
    }

    pub fn record_completion_open_decision_age(&self, age: Duration) {
        self.completion_oldest_open_decision_age_ms
            .store(Self::duration_ms_saturating(age), Ordering::Relaxed);
    }

    pub fn record_completion_outbox_pending(&self, count: u64) {
        Self::add_labeled(&self.completion_outbox_states, "pending".into(), count);
    }

    pub fn record_completion_outbox_retry(&self) {
        Self::inc_labeled(&self.completion_outbox_states, "retry".into());
    }

    pub fn record_completion_outbox_delivered(&self, latency: Duration) {
        Self::inc_labeled(&self.completion_outbox_states, "delivered".into());
        self.completion_outbox_latency_ms_count
            .fetch_add(1, Ordering::Relaxed);
        self.completion_outbox_latency_ms_total
            .fetch_add(Self::duration_ms_saturating(latency), Ordering::Relaxed);
    }

    pub fn record_completion_plan_classification(
        &self,
        change: PlanReviewChangeV2,
        localized_intersection: bool,
        lineage_reset: bool,
    ) {
        let change = match change {
            PlanReviewChangeV2::InitialOrNewLineage => "initial_or_new_lineage",
            PlanReviewChangeV2::Corrective => "corrective",
            PlanReviewChangeV2::HolisticRewrite => "holistic_rewrite",
            PlanReviewChangeV2::RosterOnly => "roster_only",
        };
        let intersection = if localized_intersection {
            "intersects"
        } else {
            "full_cohort"
        };
        Self::inc_labeled(
            &self.completion_plan_classifications,
            format!("{change}:{intersection}"),
        );
        if lineage_reset {
            Self::inc_labeled(
                &self.completion_plan_classifications,
                "lineage_reset".into(),
            );
        }
    }

    pub fn record_completion_plan_reducer(
        &self,
        action: PlanReviewNextAction,
        stagnation_count: u32,
        rewrite_used: bool,
    ) {
        let action = match action {
            PlanReviewNextAction::ContinueReview => "continue_review",
            PlanReviewNextAction::HolisticRewriteRequired => "holistic_rewrite_required",
            PlanReviewNextAction::UserDecisionRequired => "user_decision_required",
            PlanReviewNextAction::Approved => "approved",
        };
        let stagnation = match stagnation_count {
            0 => "stagnation_0",
            1 => "stagnation_1",
            _ => "stagnation_2_plus",
        };
        let rewrite = if rewrite_used {
            "rewrite"
        } else {
            "no_rewrite"
        };
        Self::inc_labeled(
            &self.completion_plan_reducer_states,
            format!("{action}:{stagnation}:{rewrite}"),
        );
    }

    pub fn record_completion_final_state(&self, state: CompletionFinalMetricState) {
        Self::inc_labeled(&self.completion_final_context_states, state.as_str().into());
    }

    pub fn record_completion_continuation(&self, reason: CompletionContinuationReason) {
        Self::inc_labeled(
            &self.completion_continuation_reasons,
            reason.as_str().into(),
        );
    }

    pub fn record_completion_sibling_reruns(&self, count: u64) {
        self.completion_sibling_reruns
            .fetch_add(count, Ordering::Relaxed);
    }

    /// A bounded Cursor-store enrichment lookup was scheduled off the broker
    /// worker. Only called after the spawned session gate confirms a live
    /// `Cursor` connection with a validated external session id — a missing
    /// or non-`Cursor` connection is skipped without incrementing this.
    pub fn record_cursor_enrichment_scheduled(&self) {
        self.cursor_enrichment_scheduled
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A scheduled lookup found and applied/observed its identity-less
    /// backfill before the deadline. `elapsed` is the wall time since the
    /// lookup was scheduled (`maybe_schedule`'s `started` capture).
    pub fn record_cursor_enrichment_resolved(&self, elapsed: Duration) {
        self.cursor_enrichment_resolved
            .fetch_add(1, Ordering::Relaxed);
        self.cursor_enrichment_duration_ms_count
            .fetch_add(1, Ordering::Relaxed);
        self.cursor_enrichment_duration_ms_total
            .fetch_add(Self::duration_ms_saturating(elapsed), Ordering::Relaxed);
    }

    /// Closed failure-class counter for a scheduled Cursor-store enrichment
    /// lookup that did not resolve. Label is the failure's stable
    /// [`CursorEnrichmentFailure::as_str`].
    pub fn record_cursor_enrichment_failed(&self, failure: CursorEnrichmentFailure) {
        Self::inc_labeled(&self.cursor_enrichment_failed, failure.as_str().to_string());
    }

    /// Closed backfill-result counter (`applied` | `same` | `conflict` | `stale`).
    pub fn record_cursor_enrichment_backfill(&self, result: IdentitylessBackfillResult) {
        let label = match result {
            IdentitylessBackfillResult::Applied => "applied",
            IdentitylessBackfillResult::Same => "same",
            IdentitylessBackfillResult::Conflict => "conflict",
            IdentitylessBackfillResult::Stale => "stale",
        };
        Self::inc_labeled(&self.cursor_enrichment_backfill, label.to_string());
    }

    /// Guarded invariant boundary. Protocol v2 may never create a format-only
    /// child run; callers receive `false` when the counter was rejected.
    pub fn record_format_repair_child_run(
        &self,
        mode: crate::db::entities::delegation_workflow::CompletionProtocolMode,
    ) -> bool {
        if mode == crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce {
            self.completion_format_only_child_runs
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub fn record_card_reemit_prompt(
        &self,
        mode: crate::db::entities::delegation_workflow::CompletionProtocolMode,
    ) -> bool {
        if mode == crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce {
            self.completion_card_reemit_prompts
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub fn record_completion_tool_supersession(&self, role: CompletionRole) {
        Self::inc_labeled(
            &self.completion_tool_supersessions,
            completion_role_label(role).into(),
        );
    }

    pub fn record_completion_decision(&self, reason: CompletionIntentReason) {
        Self::inc_labeled(
            &self.completion_decisions,
            completion_reason_label(reason).into(),
        );
    }

    pub fn record_completion_artifact_failure(
        &self,
        phase: CompletionMetricPhase,
        reason: ArtifactFailure,
    ) {
        Self::inc_labeled(
            &self.completion_artifact_failures,
            format!("{}:{}", phase.as_str(), artifact_failure_label(reason)),
        );
    }

    pub fn record_completion_scope_invalidation(
        &self,
        phase: CompletionMetricPhase,
        dimension: CompletionScopeInvalidationDimension,
    ) {
        Self::inc_labeled(
            &self.completion_scope_invalidations,
            format!("{}:{}", phase.as_str(), dimension.as_str()),
        );
    }

    pub fn record_recovery_event(&self, event: DelegationRecoveryMetricEvent) {
        Self::inc_labeled(&self.recovery_event_counts, event.kind.as_str().to_string());
        tracing::info!(
            target: "codeg::delegation::recovery",
            event = event.kind.as_str(),
            task_id = event.task_id.as_deref(),
            authorization_id = event.authorization_id.as_deref(),
            parent_id = event.parent_id.as_deref(),
            child_id = event.child_id.as_deref(),
            action = event.action.as_ref().map(RecoveryMetricAction::as_str),
            cause = event.cause.as_ref().map(RecoveryMetricCause::as_str),
            risk = event.risk.as_ref().map(RecoveryMetricRisk::as_str),
            code = event.code.as_ref().map(RecoveryMetricCode::as_str),
            "delegation recovery event"
        );
        self.recovery_event_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
    }

    pub fn recovery_events(&self) -> Vec<DelegationRecoveryMetricEvent> {
        self.recovery_event_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn record_recovery_lookup_unknown(&self, code: &'static str) {
        if !matches!(
            code,
            "db_not_found"
                | "ownership_mismatch"
                | "token_parent_mismatch"
                | "store_error"
                | "prefix_ambiguous"
        ) {
            return;
        }
        Self::inc_labeled(&self.recovery_lookup_unknown, code.to_string());
    }

    pub fn recovery_lookup_unknown_codes(&self) -> BTreeMap<String, u64> {
        self.recovery_lookup_unknown
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(crate) fn duration_ms_saturating(d: Duration) -> u64 {
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
    }

    /// Validate exclusivity, count mixed violations, then record a successful plan.
    pub fn validate_and_record_route(
        &self,
        agent_type: AgentType,
        plan: &DelegationRoutePlan,
    ) -> Result<(), RouteResolutionError> {
        if plan.assert_exclusive().is_err() {
            self.mixed_route_invariant_violations
                .fetch_add(1, Ordering::Relaxed);
            return Err(RouteResolutionError::MixedCreationSurfaces);
        }
        self.record_route(agent_type, plan);
        Ok(())
    }

    /// Record a validated route selection (and safe-fallback / suppression labels).
    pub fn record_route(&self, agent_type: AgentType, plan: &DelegationRoutePlan) {
        let label = route_selection_label(agent_type, plan.effective);
        Self::inc_labeled(&self.route_selections, label);

        if plan.source == DelegationRouteSource::SafeFallback {
            let fb = format!(
                "{}:{}",
                agent_type_label(agent_type),
                plan.degraded_reason
                    .map(degraded_reason_label)
                    .unwrap_or("unknown")
            );
            Self::inc_labeled(&self.safe_fallbacks, fb);
        }

        if let Some(reason) = plan.degraded_reason {
            if matches!(
                reason,
                RouteDegradedReason::NativeSuppressionUnsupported
                    | RouteDegradedReason::NativeSuppressionInvalid
            ) {
                let key = format!(
                    "{}:{}",
                    agent_type_label(agent_type),
                    degraded_reason_label(reason)
                );
                Self::inc_labeled(&self.suppression_failures, key);
            }
        }
    }

    /// Count a safe fallback at the actual decision boundary (once, not per poll).
    pub fn record_safe_fallback(&self, agent_type: AgentType, reason: RouteDegradedReason) {
        let key = format!(
            "{}:{}",
            agent_type_label(agent_type),
            degraded_reason_label(reason)
        );
        Self::inc_labeled(&self.safe_fallbacks, key);
    }

    /// Count a suppression failure at the actual outcome (once).
    pub fn record_suppression_failure(&self, agent_type: AgentType, reason: RouteDegradedReason) {
        let key = format!(
            "{}:{}",
            agent_type_label(agent_type),
            degraded_reason_label(reason)
        );
        Self::inc_labeled(&self.suppression_failures, key);
    }

    /// Accepted only after the durable accepted boundary (`reserving → running`
    /// winner). Prefer [`Self::record_accepted_for_task`] so commit-reread /
    /// idempotent re-entry cannot double-count the same generation.
    pub fn record_accepted(&self, agent_type: AgentType) {
        self.accepted_count.fetch_add(1, Ordering::Relaxed);
        Self::inc_labeled(
            &self.accepted_by_agent,
            agent_type_label(agent_type).to_string(),
        );
    }

    /// Exactly-once accepted metric for a durable run generation (`task_id`).
    ///
    /// Returns `true` when this call emitted the metric, `false` when the task
    /// was already counted in this process (idempotent / commit-reread).
    pub fn record_accepted_for_task(&self, task_id: &str, agent_type: AgentType) -> bool {
        {
            let mut seen = self
                .accepted_task_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !seen.insert(task_id.to_string()) {
                return false;
            }
        }
        self.record_accepted(agent_type);
        true
    }

    /// Record promote-local transient retries from attempt meta.
    ///
    /// Labels: `busy`, `locked`, `busy_snapshot`. Counts use the meta totals
    /// (each observed transient class across attempts). `busy_snapshot` is only
    /// non-zero when extended code 517 was classified.
    pub fn record_promote_retries(&self, busy: u32, locked: u32, busy_snapshot: u32) {
        Self::add_labeled(&self.promote_retries, "busy".into(), u64::from(busy));
        Self::add_labeled(&self.promote_retries, "locked".into(), u64::from(locked));
        Self::add_labeled(
            &self.promote_retries,
            "busy_snapshot".into(),
            u64::from(busy_snapshot),
        );
    }

    /// Final promote failure class. Stable labels only:
    /// `cas`, `budget`, `busy_exhausted`, `permanent`.
    pub fn record_promote_failure(&self, label: &'static str) {
        debug_assert!(
            matches!(
                label,
                PROMOTE_FAILURE_CAS
                    | PROMOTE_FAILURE_BUDGET
                    | PROMOTE_FAILURE_BUSY_EXHAUSTED
                    | PROMOTE_FAILURE_PERMANENT
            ),
            "unexpected promote_failures label: {label}"
        );
        Self::inc_labeled(&self.promote_failures, label.to_string());
    }

    /// `admission_failed` terminal settlement (not budget_exhausted / spawn_failed).
    pub fn record_admission_failed(&self, agent_type: AgentType) {
        Self::inc_labeled(
            &self.admission_failed_by_agent,
            agent_type_label(agent_type).to_string(),
        );
    }

    /// New settlement-retry owner after bounded settle exhaust / freeze install.
    pub fn record_settlement_retry_enqueued(&self) {
        self.settlement_retry_enqueued
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Bounded settlement loop failed; durable truth handed to a retry owner.
    pub fn record_settlement_retry_exhausted(&self) {
        self.settlement_retry_exhausted
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Terminal only for the CAS winner (loser/replay must not call this).
    pub fn record_terminal(&self, status: TaskStatus, duration: Duration) {
        let ms = Self::duration_ms_saturating(duration);
        let _ = self.terminal_duration_ms_total.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |v| Some(v.saturating_add(ms)),
        );
        match status {
            TaskStatus::Completed => {
                self.completed_count.fetch_add(1, Ordering::Relaxed);
            }
            TaskStatus::Failed => {
                self.failed_count.fetch_add(1, Ordering::Relaxed);
            }
            TaskStatus::Canceled => {
                self.canceled_count.fetch_add(1, Ordering::Relaxed);
            }
            TaskStatus::Running | TaskStatus::Unknown => {}
        }
    }

    /// Observation transition actually emitted by the soft supervisor.
    pub fn record_observation_transition(&self, from: TaskObservation, to: TaskObservation) {
        use TaskObservation::*;
        match (from, to) {
            (Active | WaitingInput, Stalled) => {
                self.stalled_episode_count.fetch_add(1, Ordering::Relaxed);
            }
            (Stalled, Active | WaitingInput) => {
                self.stalled_recovery_count.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// One wait outcome after a status request returns (or peer closes).
    pub fn record_wait(&self, mode: WaitModeLabel, wall: Duration, reason: WaitReturnReason) {
        match mode {
            WaitModeLabel::Snapshot => {
                self.snapshot_wait_count.fetch_add(1, Ordering::Relaxed);
            }
            WaitModeLabel::Supervised => {
                self.supervised_wait_count.fetch_add(1, Ordering::Relaxed);
            }
            WaitModeLabel::Terminal => {
                self.terminal_wait_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        let ms = Self::duration_ms_saturating(wall);
        let _ =
            self.wait_duration_ms_total
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_add(ms))
                });
        Self::inc_labeled(
            &self.wait_return_reasons,
            wait_return_reason_label(reason).to_string(),
        );
    }

    /// Explicit `cancel_delegation` reasons (not MCP status-request cancel).
    pub fn record_explicit_cancel(&self, reason: CancelDelegationReason) {
        match reason {
            CancelDelegationReason::TaskFail => {
                self.explicit_taskfail_cancel_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            CancelDelegationReason::UserCancel => {
                self.explicit_user_cancel_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            CancelDelegationReason::Others => {
                self.explicit_other_cancel_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            CancelDelegationReason::Timeout => {}
        }
    }

    /// MCP `notifications/cancelled` / request cancel (distinct from task cancel).
    pub fn record_mcp_request_cancel(&self) {
        self.mcp_request_cancel_count
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record an external prompt rejected by the continuation admission gate.
    /// Both labels are fixed enum values, never caller-controlled content.
    pub fn record_prompt_rejected_waiting(&self, source: PromptAdmissionSource) {
        Self::inc_labeled(
            &self.prompt_rejected,
            format!("waiting_for_subagents:{}", source.as_str()),
        );
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_armed(&self) {
        self.continuation_armed.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_suspended(&self, duration: Duration) {
        self.continuation_suspended.fetch_add(1, Ordering::Relaxed);
        self.continuation_suspend_duration_ms_count
            .fetch_add(1, Ordering::Relaxed);
        let ms = Self::duration_ms_saturating(duration);
        let _ = self.continuation_suspend_duration_ms_total.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |value| Some(value.saturating_add(ms)),
        );
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_wake_claimed(
        &self,
        reason: ContinuationWakeReason,
        duration: Duration,
    ) {
        let label = reason.as_str().to_string();
        Self::inc_labeled(&self.continuation_wake_claimed, label.clone());
        Self::inc_labeled(&self.continuation_wait_duration_ms_count, label.clone());
        let ms = Self::duration_ms_saturating(duration);
        let mut totals = self
            .continuation_wait_duration_ms_total
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let total = totals.entry(label).or_insert(0);
        *total = total.saturating_add(ms);
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_prompt_admitted(&self) {
        self.continuation_prompt_admitted
            .fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code, reason = "Task 8 records ordered cancellation winners")]
    pub(crate) fn record_continuation_cancelled(&self, phase: ContinuationState) {
        Self::inc_labeled(&self.continuation_cancelled, phase.as_str().to_string());
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_failed(
        &self,
        phase: ContinuationState,
        code: ContinuationFailureCode,
    ) {
        Self::inc_labeled(
            &self.continuation_failed,
            format!("{}:{}", phase.as_str(), code.as_str()),
        );
    }

    #[allow(dead_code, reason = "Task 8 records reconciliation winners")]
    pub(crate) fn record_continuation_reconciled(&self, state: ContinuationState) {
        Self::inc_labeled(&self.continuation_reconciled, state.as_str().to_string());
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_duplicate_claim_suppressed(&self) {
        self.continuation_duplicate_claim_suppressed
            .fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code, reason = "Task 7 activates coordinator metrics")]
    pub(crate) fn record_continuation_prompt_delivery_retry(&self) {
        self.continuation_prompt_delivery_retry
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Deterministic, serializable snapshot of all counters.
    pub fn snapshot(&self) -> DelegationMetricsSnapshot {
        let route_selections = self
            .route_selections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let safe_fallbacks = self
            .safe_fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let suppression_failures = self
            .suppression_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let wait_return_reasons = self
            .wait_return_reasons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let prompt_rejected = self
            .prompt_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_wake_claimed = self
            .continuation_wake_claimed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_cancelled = self
            .continuation_cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_failed = self
            .continuation_failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_reconciled = self
            .continuation_reconciled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_wait_duration_ms_count = self
            .continuation_wait_duration_ms_count
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let continuation_wait_duration_ms_total = self
            .continuation_wait_duration_ms_total
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let accepted_by_agent = self
            .accepted_by_agent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let promote_retries = self
            .promote_retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let promote_failures = self
            .promote_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let admission_failed_by_agent = self
            .admission_failed_by_agent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let recovery_events = self
            .recovery_event_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let recovery_lookup_unknown = self
            .recovery_lookup_unknown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_resolutions = self
            .completion_resolutions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_tool_supersessions = self
            .completion_tool_supersessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_decisions = self
            .completion_decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_artifact_failures = self
            .completion_artifact_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_scope_invalidations = self
            .completion_scope_invalidations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cursor_enrichment_failed = self
            .cursor_enrichment_failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cursor_enrichment_backfill = self
            .cursor_enrichment_backfill
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let completion_protocol = CompletionProtocolMetricsSnapshot {
            resolutions: completion_resolutions.clone(),
            tool_accepted: self
                .completion_tool_accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            tool_superseded: completion_tool_supersessions.clone(),
            intent_diagnostics: completion_decisions.clone(),
            decision_lifecycle: self
                .completion_decision_lifecycle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            artifact_failures: completion_artifact_failures.clone(),
            scope_invalidations: completion_scope_invalidations.clone(),
            plan_classifications: self
                .completion_plan_classifications
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            final_context_states: self
                .completion_final_context_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            outbox_states: self
                .completion_outbox_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            plan_reducer_states: self
                .completion_plan_reducer_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            continuation_reasons: self
                .completion_continuation_reasons
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            natural_language_fallback_count: self
                .completion_natural_language_fallback_count
                .load(Ordering::Relaxed),
            resolution_count: self.completion_resolution_count.load(Ordering::Relaxed),
            adjudication_latency_ms_count: self
                .completion_adjudication_latency_ms_count
                .load(Ordering::Relaxed),
            adjudication_latency_ms_total: self
                .completion_adjudication_latency_ms_total
                .load(Ordering::Relaxed),
            oldest_open_decision_age_ms: self
                .completion_oldest_open_decision_age_ms
                .load(Ordering::Relaxed),
            outbox_latency_ms_count: self
                .completion_outbox_latency_ms_count
                .load(Ordering::Relaxed),
            outbox_latency_ms_total: self
                .completion_outbox_latency_ms_total
                .load(Ordering::Relaxed),
            format_only_child_runs: self
                .completion_format_only_child_runs
                .load(Ordering::Relaxed),
            card_reemit_prompts: self.completion_card_reemit_prompts.load(Ordering::Relaxed),
            sibling_reruns: self.completion_sibling_reruns.load(Ordering::Relaxed),
        };
        DelegationMetricsSnapshot {
            route_selections,
            safe_fallbacks,
            suppression_failures,
            accepted_count: self.accepted_count.load(Ordering::Relaxed),
            accepted_by_agent,
            completed_count: self.completed_count.load(Ordering::Relaxed),
            failed_count: self.failed_count.load(Ordering::Relaxed),
            canceled_count: self.canceled_count.load(Ordering::Relaxed),
            terminal_duration_ms_total: self.terminal_duration_ms_total.load(Ordering::Relaxed),
            stalled_episode_count: self.stalled_episode_count.load(Ordering::Relaxed),
            stalled_recovery_count: self.stalled_recovery_count.load(Ordering::Relaxed),
            snapshot_wait_count: self.snapshot_wait_count.load(Ordering::Relaxed),
            supervised_wait_count: self.supervised_wait_count.load(Ordering::Relaxed),
            terminal_wait_count: self.terminal_wait_count.load(Ordering::Relaxed),
            wait_duration_ms_total: self.wait_duration_ms_total.load(Ordering::Relaxed),
            wait_return_reasons,
            explicit_taskfail_cancel_count: self
                .explicit_taskfail_cancel_count
                .load(Ordering::Relaxed),
            explicit_user_cancel_count: self.explicit_user_cancel_count.load(Ordering::Relaxed),
            explicit_other_cancel_count: self.explicit_other_cancel_count.load(Ordering::Relaxed),
            mcp_request_cancel_count: self.mcp_request_cancel_count.load(Ordering::Relaxed),
            mixed_route_invariant_violations: self
                .mixed_route_invariant_violations
                .load(Ordering::Relaxed),
            prompt_rejected,
            continuation_armed: self.continuation_armed.load(Ordering::Relaxed),
            continuation_suspended: self.continuation_suspended.load(Ordering::Relaxed),
            continuation_wake_claimed,
            continuation_prompt_admitted: self.continuation_prompt_admitted.load(Ordering::Relaxed),
            continuation_cancelled,
            continuation_failed,
            continuation_reconciled,
            continuation_duplicate_claim_suppressed: self
                .continuation_duplicate_claim_suppressed
                .load(Ordering::Relaxed),
            continuation_wait_duration_ms_count,
            continuation_wait_duration_ms_total,
            continuation_suspend_duration_ms_count: self
                .continuation_suspend_duration_ms_count
                .load(Ordering::Relaxed),
            continuation_suspend_duration_ms_total: self
                .continuation_suspend_duration_ms_total
                .load(Ordering::Relaxed),
            continuation_prompt_delivery_retry: self
                .continuation_prompt_delivery_retry
                .load(Ordering::Relaxed),
            promote_retries,
            promote_failures,
            admission_failed_by_agent,
            settlement_retry_enqueued: self.settlement_retry_enqueued.load(Ordering::Relaxed),
            settlement_retry_exhausted: self.settlement_retry_exhausted.load(Ordering::Relaxed),
            recovery_events,
            recovery_lookup_unknown,
            completion_resolutions,
            completion_tool_supersessions,
            completion_decisions,
            completion_artifact_failures,
            completion_scope_invalidations,
            completion_protocol,
            cursor_enrichment_scheduled: self.cursor_enrichment_scheduled.load(Ordering::Relaxed),
            cursor_enrichment_resolved: self.cursor_enrichment_resolved.load(Ordering::Relaxed),
            cursor_enrichment_failed,
            cursor_enrichment_backfill,
            cursor_enrichment_duration_ms_count: self
                .cursor_enrichment_duration_ms_count
                .load(Ordering::Relaxed),
            cursor_enrichment_duration_ms_total: self
                .cursor_enrichment_duration_ms_total
                .load(Ordering::Relaxed),
        }
    }
}

// ── Label helpers (stable enums only) ──────────────────────────────────────

fn completion_source_label(source: CompletionIntentSource) -> &'static str {
    match source {
        CompletionIntentSource::CompleteWork => "complete_work",
        CompletionIntentSource::AssistantConclusion => "assistant_conclusion",
        CompletionIntentSource::Report => "report",
        CompletionIntentSource::UserAdjudication => "user_adjudication",
    }
}

fn completion_role_label(role: CompletionRole) -> &'static str {
    match role {
        CompletionRole::Reviewer => "reviewer",
        CompletionRole::Author => "author",
        CompletionRole::Implementer => "implementer",
        CompletionRole::Fixer => "fixer",
    }
}

fn completion_reason_label(reason: CompletionIntentReason) -> &'static str {
    match reason {
        CompletionIntentReason::Missing => "completion_intent_missing",
        CompletionIntentReason::Conflict => "completion_intent_conflict",
        CompletionIntentReason::RoleMismatch => "completion_outcome_role_mismatch",
        CompletionIntentReason::RemediationContextRequired => {
            "completion_remediation_context_required"
        }
    }
}

fn artifact_failure_label(reason: ArtifactFailure) -> &'static str {
    match reason {
        ArtifactFailure::InvalidPath => "invalid_path",
        ArtifactFailure::WorkspaceUnavailable => "workspace_unavailable",
        ArtifactFailure::WorkspaceEscape => "workspace_escape",
        ArtifactFailure::NotFile => "not_file",
        ArtifactFailure::SizeLimitExceeded => "size_limit_exceeded",
        ArtifactFailure::ReadFailed => "read_failed",
        ArtifactFailure::GitCommandFailed => "git_command_failed",
        ArtifactFailure::MissingHead => "missing_head",
        ArtifactFailure::MalformedHead => "malformed_head",
        ArtifactFailure::DirtyWorktree => "dirty_worktree",
        ArtifactFailure::CommitRequired => "commit_required",
        ArtifactFailure::ExpectedArtifactInvalid => "expected_artifact_invalid",
    }
}

/// Stable metrics / audit label for [`AgentType`] (snake_case, low cardinality).
pub fn agent_type_label(agent: AgentType) -> &'static str {
    match agent {
        AgentType::ClaudeCode => "claude_code",
        AgentType::Codex => "codex",
        AgentType::OpenCode => "open_code",
        AgentType::Gemini => "gemini",
        AgentType::Cline => "cline",
        AgentType::Hermes => "hermes",
        AgentType::CodeBuddy => "code_buddy",
        AgentType::KimiCode => "kimi_code",
        AgentType::Pi => "pi",
        AgentType::Grok => "grok",
        AgentType::Cursor => "cursor",
        AgentType::DeepSeek => "deepseek",
        AgentType::Custom(_) => "custom",
    }
}

fn route_policy_label(policy: DelegationRoutePolicy) -> &'static str {
    match policy {
        DelegationRoutePolicy::Codeg => "codeg",
        DelegationRoutePolicy::Native => "native",
    }
}

fn route_selection_label(agent: AgentType, effective: DelegationRoutePolicy) -> String {
    format!(
        "{}:{}",
        agent_type_label(agent),
        route_policy_label(effective)
    )
}

fn degraded_reason_label(reason: RouteDegradedReason) -> &'static str {
    match reason {
        RouteDegradedReason::NativeSuppressionUnsupported => "native_suppression_unsupported",
        RouteDegradedReason::NativeSuppressionInvalid => "native_suppression_invalid",
        RouteDegradedReason::CompanionBinaryUnavailable => "companion_binary_unavailable",
        RouteDegradedReason::AgentMcpUnsupported => "agent_mcp_unsupported",
        RouteDegradedReason::CompanionInitializationFailed => "companion_initialization_failed",
    }
}

fn wait_return_reason_label(reason: WaitReturnReason) -> &'static str {
    match reason {
        WaitReturnReason::Snapshot => "snapshot",
        WaitReturnReason::Terminal => "terminal",
        WaitReturnReason::Observation => "observation",
        WaitReturnReason::Deadline => "deadline",
        WaitReturnReason::PeerClosed => "peer_closed",
    }
}

fn suppression_adapter_label(plan: &NativeSuppressionPlan) -> &'static str {
    match plan {
        NativeSuppressionPlan::None => "none",
        NativeSuppressionPlan::CodexMultiAgentFalse => "codex_multi_agent_false",
        NativeSuppressionPlan::GrokNoSubagents => "grok_no_subagents",
        NativeSuppressionPlan::CodeBuddyDisallowedTools { .. } => "code_buddy_disallowed_tools",
        NativeSuppressionPlan::ClaudeDisallowedTools { .. } => "claude_disallowed_tools",
    }
}

/// Stable English label for route source (audit / debug).
pub fn route_source_label(source: DelegationRouteSource) -> &'static str {
    match source {
        DelegationRouteSource::ForcedChild => "forced_child",
        DelegationRouteSource::SessionOverride => "session_override",
        DelegationRouteSource::GlobalDefault => "global_default",
        DelegationRouteSource::FeatureDisabled => "feature_disabled",
        DelegationRouteSource::SafeFallback => "safe_fallback",
    }
}

// ── Audit records (private fields, named constructors only) ────────────────

/// Immutable, secret-free audit record for structured tracing.
///
/// No generic metadata map or free-form string payload — callers cannot
/// opportunistically attach prompts, tokens, or env values.
#[derive(Debug, Clone, Serialize)]
pub struct DelegationAuditRecord {
    kind: AuditKind,
    connection_id: Option<String>,
    conversation_id: Option<i32>,
    agent_type: Option<AgentType>,
    requested_route: Option<DelegationRoutePolicy>,
    effective_route: Option<DelegationRoutePolicy>,
    route_source: Option<DelegationRouteSource>,
    managed: Option<bool>,
    degraded_reason: Option<RouteDegradedReason>,
    expose_codeg_delegation: Option<bool>,
    native_creation_exposed: Option<bool>,
    suppression_adapter: Option<&'static str>,
    suppression_application: Option<SuppressionApplication>,
    task_id: Option<String>,
    child_conversation_id: Option<i32>,
    task_status: Option<TaskStatus>,
    error_code: Option<&'static str>,
    observation_from: Option<TaskObservation>,
    observation_to: Option<TaskObservation>,
    wait_mode: Option<WaitModeLabel>,
    requested_wait_ms: Option<u64>,
    wait_wall_ms: Option<u64>,
    wait_return_reason: Option<WaitReturnReason>,
    cancel_reason: Option<CancelDelegationReason>,
    terminal_winner: Option<bool>,
    duration_ms: Option<u64>,
    /// Stable English code for route-degraded / companion-unavailable state
    /// (never free-form; only interned constants).
    stable_code: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuditKind {
    Route,
    TaskTransition,
    Observation,
    Wait,
    Cancel,
    /// Post-ready companion availability transition (false only).
    Availability,
}

impl DelegationAuditRecord {
    /// Route resolution / application audit (no secrets).
    pub fn route(
        connection_id: impl Into<String>,
        conversation_id: Option<i32>,
        agent_type: AgentType,
        plan: &DelegationRoutePlan,
        suppression: SuppressionApplication,
    ) -> Self {
        Self {
            kind: AuditKind::Route,
            connection_id: Some(connection_id.into()),
            conversation_id,
            agent_type: Some(agent_type),
            requested_route: Some(plan.requested),
            effective_route: Some(plan.effective),
            route_source: Some(plan.source),
            managed: Some(plan.managed),
            degraded_reason: plan.degraded_reason,
            expose_codeg_delegation: Some(plan.expose_codeg_delegation),
            native_creation_exposed: Some(plan.native_creation_exposed()),
            suppression_adapter: Some(suppression_adapter_label(&plan.native_suppression)),
            suppression_application: Some(suppression),
            task_id: None,
            child_conversation_id: None,
            task_status: None,
            error_code: None,
            observation_from: None,
            observation_to: None,
            wait_mode: None,
            requested_wait_ms: None,
            wait_wall_ms: None,
            wait_return_reason: None,
            cancel_reason: None,
            terminal_winner: None,
            duration_ms: None,
            // Only when the plan is actually degraded — not for healthy routes.
            stable_code: plan.degraded_reason.map(|_| ROUTE_DEGRADED_CODE),
        }
    }

    /// Task lifecycle transition (accepted / terminal). No result text.
    #[allow(clippy::too_many_arguments)]
    pub fn task_transition(
        connection_id: impl Into<String>,
        conversation_id: Option<i32>,
        agent_type: AgentType,
        task_id: impl Into<String>,
        child_conversation_id: Option<i32>,
        status: TaskStatus,
        error_code: Option<&'static str>,
        duration_ms: Option<u64>,
        terminal_winner: Option<bool>,
    ) -> Self {
        Self {
            kind: AuditKind::TaskTransition,
            connection_id: Some(connection_id.into()),
            conversation_id,
            agent_type: Some(agent_type),
            requested_route: None,
            effective_route: None,
            route_source: None,
            managed: None,
            degraded_reason: None,
            expose_codeg_delegation: None,
            native_creation_exposed: None,
            suppression_adapter: None,
            suppression_application: None,
            task_id: Some(task_id.into()),
            child_conversation_id,
            task_status: Some(status),
            error_code,
            observation_from: None,
            observation_to: None,
            wait_mode: None,
            requested_wait_ms: None,
            wait_wall_ms: None,
            wait_return_reason: None,
            cancel_reason: None,
            terminal_winner,
            duration_ms,
            stable_code: None,
        }
    }

    /// Soft-supervisor observation transition.
    pub fn observation(
        task_id: impl Into<String>,
        from: TaskObservation,
        to: TaskObservation,
    ) -> Self {
        Self {
            kind: AuditKind::Observation,
            connection_id: None,
            conversation_id: None,
            agent_type: None,
            requested_route: None,
            effective_route: None,
            route_source: None,
            managed: None,
            degraded_reason: None,
            expose_codeg_delegation: None,
            native_creation_exposed: None,
            suppression_adapter: None,
            suppression_application: None,
            task_id: Some(task_id.into()),
            child_conversation_id: None,
            task_status: None,
            error_code: None,
            observation_from: Some(from),
            observation_to: Some(to),
            wait_mode: None,
            requested_wait_ms: None,
            wait_wall_ms: None,
            wait_return_reason: None,
            cancel_reason: None,
            terminal_winner: None,
            duration_ms: None,
            stable_code: None,
        }
    }

    /// Status wait outcome (mode / requested / wall / reason).
    pub fn wait(
        mode: WaitModeLabel,
        requested_wait_ms: Option<u64>,
        wall: Duration,
        reason: WaitReturnReason,
    ) -> Self {
        Self {
            kind: AuditKind::Wait,
            connection_id: None,
            conversation_id: None,
            agent_type: None,
            requested_route: None,
            effective_route: None,
            route_source: None,
            managed: None,
            degraded_reason: None,
            expose_codeg_delegation: None,
            native_creation_exposed: None,
            suppression_adapter: None,
            suppression_application: None,
            task_id: None,
            child_conversation_id: None,
            task_status: None,
            error_code: None,
            observation_from: None,
            observation_to: None,
            wait_mode: Some(mode),
            requested_wait_ms,
            wait_wall_ms: Some(DelegationMetrics::duration_ms_saturating(wall)),
            wait_return_reason: Some(reason),
            cancel_reason: None,
            terminal_winner: None,
            duration_ms: None,
            stable_code: None,
        }
    }

    /// Explicit task cancel (not MCP request cancel).
    pub fn cancel(
        connection_id: impl Into<String>,
        task_id: impl Into<String>,
        reason: CancelDelegationReason,
    ) -> Self {
        Self {
            kind: AuditKind::Cancel,
            connection_id: Some(connection_id.into()),
            conversation_id: None,
            agent_type: None,
            requested_route: None,
            effective_route: None,
            route_source: None,
            managed: None,
            degraded_reason: None,
            expose_codeg_delegation: None,
            native_creation_exposed: None,
            suppression_adapter: None,
            suppression_application: None,
            task_id: Some(task_id.into()),
            child_conversation_id: None,
            task_status: None,
            error_code: None,
            observation_from: None,
            observation_to: None,
            wait_mode: None,
            requested_wait_ms: None,
            wait_wall_ms: None,
            wait_return_reason: None,
            cancel_reason: Some(reason),
            terminal_winner: None,
            duration_ms: None,
            stable_code: None,
        }
    }

    /// Post-ready companion availability became false (state flip only).
    ///
    /// Carries stable code [`DELEGATION_UNAVAILABLE_CODE`]. Never mutates route
    /// fields; no free-form / secret-bearing payload.
    pub fn availability(
        connection_id: impl Into<String>,
        conversation_id: Option<i32>,
        agent_type: AgentType,
    ) -> Self {
        Self {
            kind: AuditKind::Availability,
            connection_id: Some(connection_id.into()),
            conversation_id,
            agent_type: Some(agent_type),
            requested_route: None,
            effective_route: None,
            route_source: None,
            managed: None,
            degraded_reason: None,
            expose_codeg_delegation: None,
            native_creation_exposed: None,
            suppression_adapter: None,
            suppression_application: None,
            task_id: None,
            child_conversation_id: None,
            task_status: None,
            error_code: None,
            observation_from: None,
            observation_to: None,
            wait_mode: None,
            requested_wait_ms: None,
            wait_wall_ms: None,
            wait_return_reason: None,
            cancel_reason: None,
            terminal_winner: None,
            duration_ms: None,
            stable_code: Some(DELEGATION_UNAVAILABLE_CODE),
        }
    }

    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    pub fn conversation_id(&self) -> Option<i32> {
        self.conversation_id
    }

    pub fn agent_type(&self) -> Option<AgentType> {
        self.agent_type
    }

    pub fn requested_route(&self) -> Option<DelegationRoutePolicy> {
        self.requested_route
    }

    pub fn effective_route(&self) -> Option<DelegationRoutePolicy> {
        self.effective_route
    }

    pub fn route_source(&self) -> Option<DelegationRouteSource> {
        self.route_source
    }

    pub fn degraded_reason(&self) -> Option<RouteDegradedReason> {
        self.degraded_reason
    }

    pub fn managed(&self) -> Option<bool> {
        self.managed
    }

    pub fn expose_codeg_delegation(&self) -> Option<bool> {
        self.expose_codeg_delegation
    }

    pub fn suppression_adapter(&self) -> Option<&'static str> {
        self.suppression_adapter
    }

    pub fn suppression_application(&self) -> Option<SuppressionApplication> {
        self.suppression_application
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn task_status(&self) -> Option<TaskStatus> {
        self.task_status
    }

    /// Interned terminal / admission error code when present.
    pub fn error_code(&self) -> Option<&'static str> {
        self.error_code
    }

    pub fn terminal_winner(&self) -> Option<bool> {
        self.terminal_winner
    }

    pub fn wait_mode(&self) -> Option<WaitModeLabel> {
        self.wait_mode
    }

    pub fn wait_return_reason(&self) -> Option<WaitReturnReason> {
        self.wait_return_reason
    }

    pub fn cancel_reason(&self) -> Option<CancelDelegationReason> {
        self.cancel_reason
    }

    pub fn stable_code(&self) -> Option<&'static str> {
        self.stable_code
    }

    /// Emit a structured info log for a route audit record.
    pub fn emit_route_resolved(&self) {
        tracing::info!(
            target: "codeg::delegation",
            connection_id = self.connection_id().unwrap_or(""),
            conversation_id = ?self.conversation_id(),
            agent_type = ?self.agent_type(),
            requested_route = ?self.requested_route(),
            effective_route = ?self.effective_route(),
            route_source = ?self.route_source(),
            route_source_code = self.route_source().map(route_source_label).unwrap_or(""),
            managed = ?self.managed(),
            degraded_reason = ?self.degraded_reason(),
            stable_code = self.stable_code().unwrap_or(""),
            expose_codeg_delegation = ?self.expose_codeg_delegation,
            native_creation_exposed = ?self.native_creation_exposed,
            suppression_adapter = ?self.suppression_adapter(),
            suppression_application = ?self.suppression_application(),
            "delegation route resolved"
        );
    }

    /// Emit a structured info log for a task lifecycle transition.
    pub fn emit_task_transition(&self) {
        tracing::info!(
            target: "codeg::delegation",
            connection_id = self.connection_id().unwrap_or(""),
            conversation_id = ?self.conversation_id(),
            agent_type = ?self.agent_type(),
            task_id = self.task_id().unwrap_or(""),
            child_conversation_id = ?self.child_conversation_id,
            task_status = ?self.task_status(),
            error_code = ?self.error_code,
            duration_ms = ?self.duration_ms,
            terminal_winner = ?self.terminal_winner(),
            "delegation task transition"
        );
    }

    /// Emit a structured info log for an observation transition.
    pub fn emit_observation(&self) {
        tracing::info!(
            target: "codeg::delegation",
            task_id = self.task_id().unwrap_or(""),
            observation_from = ?self.observation_from,
            observation_to = ?self.observation_to,
            "delegation observation transition"
        );
    }

    /// Emit a structured info log for a wait outcome.
    pub fn emit_wait(&self) {
        tracing::info!(
            target: "codeg::delegation",
            wait_mode = ?self.wait_mode(),
            requested_wait_ms = ?self.requested_wait_ms,
            wait_wall_ms = ?self.wait_wall_ms,
            wait_return_reason = ?self.wait_return_reason(),
            "delegation status wait returned"
        );
    }

    /// Emit a structured info log for an explicit cancel.
    pub fn emit_cancel(&self) {
        tracing::info!(
            target: "codeg::delegation",
            connection_id = self.connection_id().unwrap_or(""),
            task_id = self.task_id().unwrap_or(""),
            cancel_reason = ?self.cancel_reason(),
            "delegation explicit cancel"
        );
    }

    /// Emit a structured info log for post-ready companion unavailability.
    pub fn emit_availability(&self) {
        tracing::info!(
            target: "codeg::delegation",
            connection_id = self.connection_id().unwrap_or(""),
            conversation_id = ?self.conversation_id(),
            agent_type = ?self.agent_type(),
            stable_code = self.stable_code().unwrap_or(""),
            "delegation companion unavailable"
        );
    }
}

/// Stable English code for route-degraded state (audit / metrics label only).
pub const ROUTE_DEGRADED_CODE: &str = "route_degraded";
/// Stable English code for post-launch delegation unavailability.
pub const DELEGATION_UNAVAILABLE_CODE: &str = "delegation_unavailable";

// ── Promote / admission interned audit codes (Task 7) ──────────────────────

/// Post-accept admission failure (prompt accepted, promote did not stick).
pub const ADMISSION_FAILED_CODE: &str = "admission_failed";
/// Recovery budget refused the promote charge.
pub const BUDGET_EXHAUSTED_CODE: &str = "budget_exhausted";
/// Host restart while bound reserving — prompt may have been accepted.
pub const ADMISSION_UNKNOWN_CODE: &str = "admission_unknown";
/// Pre-accept spawn / bind / send failure (not post-accept admission).
pub const SPAWN_FAILED_CODE: &str = "spawn_failed";

/// `promote_failures` label: CAS / state-conflict promote loss.
pub const PROMOTE_FAILURE_CAS: &str = "cas";
/// `promote_failures` label: budget charge refused.
pub const PROMOTE_FAILURE_BUDGET: &str = "budget";
/// `promote_failures` label: lock-class retry budget exhausted.
pub const PROMOTE_FAILURE_BUSY_EXHAUSTED: &str = "busy_exhausted";
/// `promote_failures` label: permanent / non-retryable promote failure.
pub const PROMOTE_FAILURE_PERMANENT: &str = "permanent";

/// Required structured field names for promote retry / final-failure logs.
/// Logs must include these and must never attach prompt bodies or secrets.
pub const PROMOTE_LOG_REQUIRED_FIELDS: &[&str] = &[
    "task_id",
    "generation",
    "agent_type",
    "admission_class",
    "attempt",
    "sqlite_primary",
    "sqlite_extended",
    "failure_class",
];

/// Secret / free-form keys that must never appear on promote structured logs.
pub const PROMOTE_LOG_FORBIDDEN_FIELDS: &[&str] = &[
    "prompt",
    "task",
    "result_text",
    "token",
    "api_key",
    "environment",
    "raw_payload",
    "companion_token",
    "config_values",
    "secret",
];

/// Map a durable/wire error code to an interned `&'static str` for
/// [`DelegationAuditRecord::error_code`]. Unknown free-form codes become `None`
/// so opportunistic strings cannot enter the audit surface.
pub fn intern_terminal_error_code(code: &str) -> Option<&'static str> {
    match code {
        SPAWN_FAILED_CODE => Some(SPAWN_FAILED_CODE),
        ADMISSION_FAILED_CODE => Some(ADMISSION_FAILED_CODE),
        BUDGET_EXHAUSTED_CODE => Some(BUDGET_EXHAUSTED_CODE),
        ADMISSION_UNKNOWN_CODE => Some(ADMISSION_UNKNOWN_CODE),
        "persistence_error" => Some("persistence_error"),
        "host_restarted" => Some("host_restarted"),
        "depth_limit_exceeded" => Some("depth_limit_exceeded"),
        "canceled" => Some("canceled"),
        "user_cancelled" => Some("user_cancelled"),
        "tool_stalled_timeout" => Some("tool_stalled_timeout"),
        "unresumable" => Some("unresumable"),
        "parent_canceled" => Some("parent_canceled"),
        _ => None,
    }
}

/// Emit a secret-free structured promote log with the required field set.
///
/// **Logging policy (Task 7):** aggregate broker-side logs after the promote
/// retry loop (and settlement exhaust) carry full required context. Per-attempt
/// logs stay in `run_store` outside this file map and are **not** amended here —
/// the broker aggregate outcome/failure lines satisfy the brief field contract.
/// Never attach prompt bodies, tokens, raw `DbErr` / free-form messages, or
/// full configuration values.
#[allow(clippy::too_many_arguments)]
pub fn emit_promote_structured_log(
    level: tracing::Level,
    message: &'static str,
    task_id: &str,
    generation: i64,
    agent_type: AgentType,
    admission_class: &dyn std::fmt::Debug,
    attempt: u32,
    sqlite_primary: Option<i32>,
    sqlite_extended: Option<i32>,
    failure_class: &str,
    intended_code: Option<&'static str>,
    settlement_retry_owner: bool,
) {
    // Stable snake label only — never Debug dump of secrets.
    let agent = agent_type_label(agent_type);
    let admission = format!("{admission_class:?}");
    let intended = intended_code.unwrap_or("");
    // `msg` is a stable interned label (static str), not free-form caller content.
    match level {
        tracing::Level::ERROR => {
            tracing::error!(
                target: "codeg::delegation",
                task_id = %task_id,
                generation,
                agent_type = agent,
                admission_class = %admission,
                attempt,
                sqlite_primary,
                sqlite_extended,
                failure_class,
                intended_code = intended,
                settlement_retry_owner,
                msg = message,
                "delegation promote structured"
            );
        }
        tracing::Level::WARN => {
            tracing::warn!(
                target: "codeg::delegation",
                task_id = %task_id,
                generation,
                agent_type = agent,
                admission_class = %admission,
                attempt,
                sqlite_primary,
                sqlite_extended,
                failure_class,
                intended_code = intended,
                settlement_retry_owner,
                msg = message,
                "delegation promote structured"
            );
        }
        _ => {
            tracing::info!(
                target: "codeg::delegation",
                task_id = %task_id,
                generation,
                agent_type = agent,
                admission_class = %admission,
                attempt,
                sqlite_primary,
                sqlite_extended,
                failure_class,
                intended_code = intended,
                settlement_retry_owner,
                msg = message,
                "delegation promote structured"
            );
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::route::ROUTE_ADAPTER_CONTRACT_VERSION;

    #[test]
    fn completion_metrics_v2_only() {
        let metrics = DelegationMetrics::default();
        metrics.record_completion_resolution(
            crate::acp::delegation::workflow::CompletionIntentSource::AssistantConclusion,
            crate::acp::delegation::workflow::CompletionRole::Reviewer,
        );
        metrics.record_completion_tool_supersession(
            crate::acp::delegation::workflow::CompletionRole::Reviewer,
        );
        metrics.record_completion_decision(
            crate::acp::delegation::workflow::CompletionIntentReason::Conflict,
        );
        metrics.record_completion_artifact_failure(
            CompletionMetricPhase::Plan,
            crate::acp::delegation::workflow::ArtifactFailure::ReadFailed,
        );
        metrics.record_completion_scope_invalidation(
            CompletionMetricPhase::Tasks,
            CompletionScopeInvalidationDimension::Instruction,
        );
        metrics.record_completion_decision_opened();
        metrics.record_completion_outbox_pending(1);
        assert!(!metrics.record_format_repair_child_run(
            crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce,
        ));
        assert!(!metrics.record_card_reemit_prompt(
            crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce,
        ));

        let snapshot = metrics.snapshot();
        let completion_json = serde_json::to_value(&snapshot.completion_protocol).unwrap();
        let completion = completion_json.as_object().unwrap();
        let removed = [
            "default_mode".to_string(),
            "profile_overrides".to_string(),
            "creation_modes".to_string(),
            ["shadow", "differences"].join("_"),
            ["rollout", "windows"].join("_"),
            ["rollout", "decisions"].join("_"),
            ["restart", "outcomes"].join("_"),
        ];
        for removed in removed {
            assert!(
                !completion.contains_key(&removed),
                "obsolete completion metric {removed} remains serialized"
            );
        }
        for retained in [
            "resolutions",
            "tool_accepted",
            "intent_diagnostics",
            "decision_lifecycle",
            "artifact_failures",
            "scope_invalidations",
            "outbox_states",
        ] {
            assert!(
                completion.contains_key(retained),
                "retained v2 completion metric {retained} is missing"
            );
        }
        assert_eq!(
            snapshot.completion_resolutions["assistant_conclusion:reviewer"],
            1
        );
        assert_eq!(snapshot.completion_tool_supersessions["reviewer"], 1);
        assert_eq!(
            snapshot.completion_decisions["completion_intent_conflict"],
            1
        );
        assert_eq!(snapshot.completion_artifact_failures["plan:read_failed"], 1);
        assert_eq!(
            snapshot.completion_scope_invalidations["tasks:instruction"],
            1
        );
        assert_eq!(snapshot.completion_protocol.decision_lifecycle["opened"], 1);
        assert_eq!(snapshot.completion_protocol.outbox_states["pending"], 1);
        assert_eq!(snapshot.completion_protocol.format_only_child_runs, 1);
        assert_eq!(snapshot.completion_protocol.card_reemit_prompts, 1);
    }

    #[test]
    fn cursor_uses_stable_metrics_label() {
        assert_eq!(agent_type_label(AgentType::Cursor), "cursor");
    }

    #[test]
    fn custom_agents_share_one_low_cardinality_metrics_label() {
        assert_eq!(agent_type_label(AgentType::Custom("private-id")), "custom");
        assert_eq!(agent_type_label(AgentType::Custom("another-id")), "custom");
    }

    fn codeg_plan(agent_type: AgentType) -> DelegationRoutePlan {
        let native_suppression = match agent_type {
            AgentType::Codex => NativeSuppressionPlan::CodexMultiAgentFalse,
            AgentType::Grok => NativeSuppressionPlan::GrokNoSubagents,
            AgentType::CodeBuddy => NativeSuppressionPlan::CodeBuddyDisallowedTools {
                tools: vec!["Agent".into(), "Task".into()],
            },
            AgentType::ClaudeCode => NativeSuppressionPlan::ClaudeDisallowedTools {
                tools: vec!["Agent".into(), "Task".into()],
            },
            _ => NativeSuppressionPlan::None,
        };
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: format!("test-codeg-{agent_type:?}"),
        }
    }

    fn invalid_mixed_plan_for_test(agent_type: AgentType) -> DelegationRoutePlan {
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression: NativeSuppressionPlan::None,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: format!("test-mixed-{agent_type:?}"),
        }
    }

    #[test]
    fn metrics_record_route_lifecycle_observation_wait_and_cancel() {
        let metrics = DelegationMetrics::default();
        metrics.record_route(AgentType::Codex, &codeg_plan(AgentType::Codex));
        metrics.record_accepted(AgentType::Codex);
        metrics.record_observation_transition(TaskObservation::Active, TaskObservation::Stalled);
        metrics.record_observation_transition(TaskObservation::Stalled, TaskObservation::Active);
        metrics.record_wait(
            WaitModeLabel::Supervised,
            Duration::from_millis(1250),
            WaitReturnReason::Observation,
        );
        metrics.record_terminal(TaskStatus::Completed, Duration::from_secs(12));
        metrics.record_explicit_cancel(CancelDelegationReason::UserCancel);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.route_selections["codex:codeg"], 1);
        assert_eq!(snapshot.accepted_count, 1);
        assert_eq!(snapshot.completed_count, 1);
        assert_eq!(snapshot.stalled_episode_count, 1);
        assert_eq!(snapshot.stalled_recovery_count, 1);
        assert_eq!(snapshot.supervised_wait_count, 1);
        assert_eq!(snapshot.wait_duration_ms_total, 1250);
        assert_eq!(snapshot.explicit_user_cancel_count, 1);
        assert_eq!(snapshot.mixed_route_invariant_violations, 0);
    }

    #[test]
    fn audit_record_cannot_serialize_prompt_token_or_credentials() {
        let record = DelegationAuditRecord::route(
            "conn-1",
            Some(42),
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            SuppressionApplication::Applied,
        );
        let value = serde_json::to_value(record).unwrap();
        let object = value.as_object().unwrap();
        for forbidden in [
            "prompt",
            "task",
            "result_text",
            "token",
            "api_key",
            "environment",
            "raw_payload",
        ] {
            assert!(!object.contains_key(forbidden));
        }
    }

    #[test]
    fn mixed_route_attempt_is_counted_and_rejected() {
        let metrics = DelegationMetrics::default();
        let mixed = invalid_mixed_plan_for_test(AgentType::Grok);
        assert_eq!(
            metrics
                .validate_and_record_route(AgentType::Grok, &mixed)
                .unwrap_err()
                .stable_code(),
            "native_suppression_invalid"
        );
        assert_eq!(metrics.snapshot().mixed_route_invariant_violations, 1);
    }

    #[test]
    fn terminal_duration_saturates_on_overflow() {
        let metrics = DelegationMetrics::default();
        metrics
            .terminal_duration_ms_total
            .store(u64::MAX - 5, Ordering::Relaxed);
        metrics.record_terminal(TaskStatus::Completed, Duration::from_millis(100));
        assert_eq!(
            metrics.snapshot().terminal_duration_ms_total,
            u64::MAX,
            "duration addition must saturate"
        );
    }

    #[test]
    fn accepted_count_and_by_agent_increment_together() {
        let metrics = DelegationMetrics::default();
        assert!(metrics.record_accepted_for_task("t1", AgentType::ClaudeCode));
        assert!(metrics.record_accepted_for_task("t2", AgentType::ClaudeCode));
        assert!(metrics.record_accepted_for_task("t3", AgentType::Codex));
        // Same task_id must not double-count.
        assert!(!metrics.record_accepted_for_task("t1", AgentType::ClaudeCode));
        let snap = metrics.snapshot();
        assert_eq!(snap.accepted_count, 3);
        assert_eq!(snap.accepted_by_agent["claude_code"], 2);
        assert_eq!(snap.accepted_by_agent["codex"], 1);
    }

    #[test]
    fn promote_failures_labels_cas_budget_busy_exhausted_permanent() {
        let metrics = DelegationMetrics::default();
        metrics.record_promote_failure(PROMOTE_FAILURE_CAS);
        metrics.record_promote_failure(PROMOTE_FAILURE_BUDGET);
        metrics.record_promote_failure(PROMOTE_FAILURE_BUSY_EXHAUSTED);
        metrics.record_promote_failure(PROMOTE_FAILURE_PERMANENT);
        let snap = metrics.snapshot();
        assert_eq!(snap.promote_failures["cas"], 1);
        assert_eq!(snap.promote_failures["budget"], 1);
        assert_eq!(snap.promote_failures["busy_exhausted"], 1);
        assert_eq!(snap.promote_failures["permanent"], 1);
        assert_eq!(snap.promote_failures.len(), 4);
    }

    #[test]
    fn admission_failed_by_agent_increments_on_admission_failed() {
        let metrics = DelegationMetrics::default();
        metrics.record_admission_failed(AgentType::Grok);
        metrics.record_admission_failed(AgentType::Grok);
        metrics.record_admission_failed(AgentType::Codex);
        let snap = metrics.snapshot();
        assert_eq!(snap.admission_failed_by_agent["grok"], 2);
        assert_eq!(snap.admission_failed_by_agent["codex"], 1);
        // budget_exhausted must not use this counter path.
        assert!(!snap
            .admission_failed_by_agent
            .contains_key("budget_exhausted"));
    }

    #[test]
    fn settlement_retry_counter_pairing_new_vs_existing_owner() {
        let metrics = DelegationMetrics::default();
        // New owner after exhaust → both.
        metrics.record_settlement_retry_enqueued();
        metrics.record_settlement_retry_exhausted();
        // Existing owner after exhaust → only exhausted.
        metrics.record_settlement_retry_exhausted();
        // Immediate settle success with fence removed → neither (no further increments).
        let snap = metrics.snapshot();
        assert_eq!(snap.settlement_retry_enqueued, 1);
        assert_eq!(snap.settlement_retry_exhausted, 2);
    }

    #[test]
    fn busy_snapshot_metric_only_on_extended_517() {
        let metrics = DelegationMetrics::default();
        // Ordinary busy/locked without 517.
        metrics.record_promote_retries(2, 1, 0);
        let snap = metrics.snapshot();
        assert_eq!(snap.promote_retries.get("busy").copied().unwrap_or(0), 2);
        assert_eq!(snap.promote_retries.get("locked").copied().unwrap_or(0), 1);
        assert_eq!(
            snap.promote_retries
                .get("busy_snapshot")
                .copied()
                .unwrap_or(0),
            0,
            "busy_snapshot must stay zero when extended 517 was not classified"
        );
        // Only when meta reports busy_snapshot (from extended 517 extraction).
        metrics.record_promote_retries(0, 0, 1);
        let snap = metrics.snapshot();
        assert_eq!(snap.promote_retries["busy_snapshot"], 1);
    }

    #[test]
    fn metrics_snapshot_default_empty_maps_serde() {
        // Legacy JSON without new Task 7 maps deserializes to empty defaults.
        let legacy = r#"{
            "route_selections": {},
            "safe_fallbacks": {},
            "suppression_failures": {},
            "accepted_count": 0,
            "completed_count": 0,
            "failed_count": 0,
            "canceled_count": 0,
            "terminal_duration_ms_total": 0,
            "stalled_episode_count": 0,
            "stalled_recovery_count": 0,
            "snapshot_wait_count": 0,
            "supervised_wait_count": 0,
            "terminal_wait_count": 0,
            "wait_duration_ms_total": 0,
            "wait_return_reasons": {},
            "explicit_taskfail_cancel_count": 0,
            "explicit_user_cancel_count": 0,
            "explicit_other_cancel_count": 0,
            "mcp_request_cancel_count": 0,
            "mixed_route_invariant_violations": 0,
            "prompt_rejected": {},
            "continuation_armed": 0,
            "continuation_suspended": 0,
            "continuation_wake_claimed": {},
            "continuation_prompt_admitted": 0,
            "continuation_cancelled": {},
            "continuation_failed": {},
            "continuation_reconciled": {},
            "continuation_duplicate_claim_suppressed": 0,
            "continuation_wait_duration_ms_count": {},
            "continuation_wait_duration_ms_total": {},
            "continuation_suspend_duration_ms_count": 0,
            "continuation_suspend_duration_ms_total": 0,
            "continuation_prompt_delivery_retry": 0
        }"#;
        let snap: DelegationMetricsSnapshot =
            serde_json::from_str(legacy).expect("legacy snapshot deserializes");
        assert!(snap.accepted_by_agent.is_empty());
        assert!(snap.promote_retries.is_empty());
        assert!(snap.promote_failures.is_empty());
        assert!(snap.admission_failed_by_agent.is_empty());
        assert_eq!(snap.settlement_retry_enqueued, 0);
        assert_eq!(snap.settlement_retry_exhausted, 0);
        // cursor_enrichment_* fields are absent from this legacy blob too,
        // and must deserialize as zero/empty via #[serde(default)].
        assert_eq!(snap.cursor_enrichment_scheduled, 0);
        assert_eq!(snap.cursor_enrichment_resolved, 0);
        assert!(snap.cursor_enrichment_failed.is_empty());
        assert!(snap.cursor_enrichment_backfill.is_empty());
        assert_eq!(snap.cursor_enrichment_duration_ms_count, 0);
        assert_eq!(snap.cursor_enrichment_duration_ms_total, 0);
        // Fresh default snapshot also has empty maps and retains old fields.
        let fresh = DelegationMetrics::default().snapshot();
        assert!(fresh.accepted_by_agent.is_empty());
        assert!(fresh.promote_retries.is_empty());
        assert_eq!(fresh.accepted_count, 0);
        assert_eq!(fresh.completed_count, 0);
        assert_eq!(fresh.cursor_enrichment_scheduled, 0);
        assert_eq!(fresh.cursor_enrichment_resolved, 0);
        assert!(fresh.cursor_enrichment_failed.is_empty());
        assert!(fresh.cursor_enrichment_backfill.is_empty());
    }

    #[test]
    fn structured_promote_logs_include_required_fields_exclude_secrets() {
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing::Subscriber;
        use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
        use tracing_subscriber::Registry;

        #[derive(Default)]
        struct Capture {
            fields: Mutex<BTreeMap<String, String>>,
            message: Mutex<String>,
        }

        struct CaptureLayer {
            inner: Arc<Capture>,
        }

        struct FieldVisitor<'a> {
            fields: &'a mut BTreeMap<String, String>,
            message: &'a mut String,
        }

        impl Visit for FieldVisitor<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                let s = format!("{value:?}");
                if field.name() == "message" {
                    *self.message = s.trim_matches('"').to_string();
                } else {
                    self.fields.insert(field.name().to_string(), s);
                }
            }
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" {
                    *self.message = value.to_string();
                } else {
                    self.fields
                        .insert(field.name().to_string(), value.to_string());
                }
            }
            fn record_i64(&mut self, field: &Field, value: i64) {
                self.fields
                    .insert(field.name().to_string(), value.to_string());
            }
            fn record_u64(&mut self, field: &Field, value: u64) {
                self.fields
                    .insert(field.name().to_string(), value.to_string());
            }
            fn record_bool(&mut self, field: &Field, value: bool) {
                self.fields
                    .insert(field.name().to_string(), value.to_string());
            }
        }

        impl<S> Layer<S> for CaptureLayer
        where
            S: Subscriber,
        {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut fields = BTreeMap::new();
                let mut message = String::new();
                let mut visitor = FieldVisitor {
                    fields: &mut fields,
                    message: &mut message,
                };
                event.record(&mut visitor);
                *self.inner.fields.lock().unwrap() = fields;
                *self.inner.message.lock().unwrap() = message;
            }
        }

        let capture = Arc::new(Capture::default());
        let subscriber = Registry::default().with(CaptureLayer {
            inner: capture.clone(),
        });
        tracing::subscriber::with_default(subscriber, || {
            emit_promote_structured_log(
                tracing::Level::ERROR,
                "[delegation] post-accept promote failure; settling intended terminal",
                "task-abc",
                2,
                AgentType::Codex,
                &"NormalRevision",
                3,
                Some(5),
                Some(517),
                "retry_exhausted",
                Some(ADMISSION_FAILED_CODE),
                false,
            );
        });

        let fields = capture.fields.lock().unwrap().clone();
        let message = capture.message.lock().unwrap().clone();
        assert!(
            fields
                .get("msg")
                .is_some_and(|v| v.contains("post-accept promote failure"))
                || message.contains("delegation promote structured"),
            "production emitter msg missing: fields={fields:?} message={message}"
        );
        for required in PROMOTE_LOG_REQUIRED_FIELDS {
            assert!(
                fields.contains_key(*required),
                "missing required promote log field {required} in {fields:?}"
            );
        }
        assert!(
            fields
                .get("task_id")
                .is_some_and(|v| v.contains("task-abc")),
            "task_id={:?}",
            fields.get("task_id")
        );
        assert!(
            fields
                .get("agent_type")
                .is_some_and(|v| v.contains("codex")),
            "agent_type={:?}",
            fields.get("agent_type")
        );
        assert!(
            fields
                .get("failure_class")
                .is_some_and(|v| v.contains("retry_exhausted")),
            "failure_class={:?}",
            fields.get("failure_class")
        );
        // No raw secret-bearing field names on the production emit.
        for forbidden in PROMOTE_LOG_FORBIDDEN_FIELDS {
            assert!(
                !fields.contains_key(*forbidden),
                "promote log must not include secret field {forbidden}: {fields:?}"
            );
        }
        // Free-form error/message dump must not appear as a field key.
        assert!(
            !fields.contains_key("error"),
            "must not log raw error field: {fields:?}"
        );
    }

    #[test]
    fn intern_terminal_error_code_covers_admission_budget_spawn() {
        assert_eq!(
            intern_terminal_error_code(ADMISSION_FAILED_CODE),
            Some(ADMISSION_FAILED_CODE)
        );
        assert_eq!(
            intern_terminal_error_code(BUDGET_EXHAUSTED_CODE),
            Some(BUDGET_EXHAUSTED_CODE)
        );
        assert_eq!(
            intern_terminal_error_code(ADMISSION_UNKNOWN_CODE),
            Some(ADMISSION_UNKNOWN_CODE)
        );
        assert_eq!(
            intern_terminal_error_code(SPAWN_FAILED_CODE),
            Some(SPAWN_FAILED_CODE)
        );
        assert_eq!(intern_terminal_error_code("free_form_prompt_secret"), None);

        // Production audit construction uses the interned constants.
        for code in [
            ADMISSION_FAILED_CODE,
            BUDGET_EXHAUSTED_CODE,
            ADMISSION_UNKNOWN_CODE,
            SPAWN_FAILED_CODE,
        ] {
            let rec = DelegationAuditRecord::task_transition(
                "conn",
                Some(1),
                AgentType::Codex,
                "task-1",
                Some(2),
                TaskStatus::Failed,
                intern_terminal_error_code(code),
                Some(10),
                Some(true),
            );
            assert_eq!(
                rec.error_code(),
                Some(code),
                "audit must surface interned {code}"
            );
            let value = serde_json::to_value(&rec).unwrap();
            let obj = value.as_object().unwrap();
            for forbidden in PROMOTE_LOG_FORBIDDEN_FIELDS {
                assert!(!obj.contains_key(*forbidden));
            }
        }
    }

    #[test]
    fn valid_four_platform_codeg_plans_leave_mixed_counter_zero() {
        let metrics = DelegationMetrics::default();
        for agent in [
            AgentType::Codex,
            AgentType::Grok,
            AgentType::CodeBuddy,
            AgentType::ClaudeCode,
        ] {
            metrics
                .validate_and_record_route(agent, &codeg_plan(agent))
                .expect("valid codeg plan");
        }
        assert_eq!(metrics.snapshot().mixed_route_invariant_violations, 0);
        assert_eq!(metrics.snapshot().route_selections.len(), 4);
    }

    #[test]
    fn record_route_counts_safe_fallback_once() {
        let metrics = DelegationMetrics::default();
        let mut plan = codeg_plan(AgentType::Codex);
        plan.effective = DelegationRoutePolicy::Native;
        plan.source = DelegationRouteSource::SafeFallback;
        plan.native_suppression = NativeSuppressionPlan::None;
        plan.expose_codeg_delegation = false;
        plan.degraded_reason = Some(RouteDegradedReason::CompanionBinaryUnavailable);
        metrics.record_route(AgentType::Codex, &plan);
        let snap = metrics.snapshot();
        assert_eq!(
            snap.safe_fallbacks
                .get("codex:companion_binary_unavailable")
                .copied()
                .unwrap_or(0),
            1
        );
    }

    #[test]
    fn wait_and_cancel_labels_are_stable() {
        let metrics = DelegationMetrics::default();
        metrics.record_wait(
            WaitModeLabel::Snapshot,
            Duration::ZERO,
            WaitReturnReason::Snapshot,
        );
        metrics.record_wait(
            WaitModeLabel::Terminal,
            Duration::from_millis(10),
            WaitReturnReason::Terminal,
        );
        metrics.record_mcp_request_cancel();
        metrics.record_explicit_cancel(CancelDelegationReason::TaskFail);
        metrics.record_explicit_cancel(CancelDelegationReason::Others);
        metrics.record_explicit_cancel(CancelDelegationReason::Timeout);
        metrics.record_prompt_rejected_waiting(PromptAdmissionSource::Foreground);
        metrics.record_prompt_rejected_waiting(PromptAdmissionSource::LinkedBackground);
        let snap = metrics.snapshot();
        assert_eq!(snap.snapshot_wait_count, 1);
        assert_eq!(snap.terminal_wait_count, 1);
        assert_eq!(snap.mcp_request_cancel_count, 1);
        assert_eq!(snap.explicit_taskfail_cancel_count, 1);
        assert_eq!(snap.explicit_other_cancel_count, 1);
        assert_eq!(snap.explicit_user_cancel_count, 0);
        assert_eq!(
            snap.prompt_rejected
                .get("waiting_for_subagents:foreground")
                .copied(),
            Some(1)
        );
        assert_eq!(
            snap.prompt_rejected
                .get("waiting_for_subagents:linked_background")
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn continuation_coordinator_metrics_labels_are_fixed_and_bounded() {
        let metrics = DelegationMetrics::default();
        metrics.record_continuation_armed();
        metrics.record_continuation_suspended(Duration::from_millis(7));
        metrics.record_continuation_prompt_admitted();
        metrics.record_continuation_duplicate_claim_suppressed();
        metrics.record_continuation_prompt_delivery_retry();
        for reason in [
            ContinuationWakeReason::AllTerminal,
            ContinuationWakeReason::AttentionRequired,
            ContinuationWakeReason::Unavailable,
            ContinuationWakeReason::Checkpoint,
        ] {
            metrics.record_continuation_wake_claimed(reason, Duration::from_millis(11));
        }
        let phases = [
            ContinuationState::Arming,
            ContinuationState::Waiting,
            ContinuationState::WakePending,
            ContinuationState::Resuming,
        ];
        for phase in phases {
            metrics.record_continuation_cancelled(phase);
            metrics.record_continuation_reconciled(phase);
            for code in [
                ContinuationFailureCode::ArmFailed,
                ContinuationFailureCode::SuspendDispatchFailed,
                ContinuationFailureCode::SuspendDrainTimeout,
                ContinuationFailureCode::ParentConnectionLost,
                ContinuationFailureCode::PromptDeliveryFailed,
                ContinuationFailureCode::StateConflict,
            ] {
                metrics.record_continuation_failed(phase, code);
            }
        }

        let snapshot = metrics.snapshot();
        let wake_keys = [
            "all_terminal",
            "attention_required",
            "checkpoint",
            "unavailable",
        ];
        let phase_keys = ["arming", "resuming", "waiting", "wake_pending"];
        let failure_codes = [
            "arm_failed",
            "parent_connection_lost",
            "prompt_delivery_failed",
            "state_conflict",
            "suspend_dispatch_failed",
            "suspend_drain_timeout",
        ];
        let mut failure_keys = phase_keys
            .iter()
            .flat_map(|phase| {
                failure_codes
                    .iter()
                    .map(move |code| format!("{phase}:{code}"))
            })
            .collect::<Vec<_>>();
        failure_keys.sort();
        assert_eq!(
            snapshot
                .continuation_wake_claimed
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            wake_keys
        );
        assert_eq!(
            snapshot
                .continuation_cancelled
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            phase_keys
        );
        assert_eq!(
            snapshot
                .continuation_failed
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            failure_keys
        );
        assert_eq!(
            snapshot
                .continuation_reconciled
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            phase_keys
        );
        assert_eq!(
            snapshot
                .continuation_wait_duration_ms_count
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            wake_keys
        );
        assert_eq!(
            snapshot
                .continuation_wait_duration_ms_total
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            wake_keys
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        for forbidden in [
            "550e8400-e29b-41d4-a716-446655440000",
            "connection-123",
            "session-123",
            "task-123",
            "prompt-123",
        ] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn audit_constructors_cover_all_kinds_without_forbidden_fields() {
        let plan = codeg_plan(AgentType::Grok);
        let records = vec![
            serde_json::to_value(DelegationAuditRecord::route(
                "c",
                None,
                AgentType::Grok,
                &plan,
                SuppressionApplication::NotApplicable,
            ))
            .unwrap(),
            serde_json::to_value(DelegationAuditRecord::task_transition(
                "c",
                Some(1),
                AgentType::Grok,
                "t1",
                Some(2),
                TaskStatus::Completed,
                None,
                Some(5),
                Some(true),
            ))
            .unwrap(),
            serde_json::to_value(DelegationAuditRecord::observation(
                "t1",
                TaskObservation::Active,
                TaskObservation::Stalled,
            ))
            .unwrap(),
            serde_json::to_value(DelegationAuditRecord::wait(
                WaitModeLabel::Supervised,
                Some(1000),
                Duration::from_millis(50),
                WaitReturnReason::Deadline,
            ))
            .unwrap(),
            serde_json::to_value(DelegationAuditRecord::cancel(
                "c",
                "t1",
                CancelDelegationReason::UserCancel,
            ))
            .unwrap(),
        ];
        for value in records {
            let s = value.to_string();
            for forbidden in [
                "prompt",
                "result_text",
                "api_key",
                "environment",
                "raw_payload",
                "companion_token",
            ] {
                assert!(
                    !s.contains(forbidden),
                    "serialized audit must not contain {forbidden}: {s}"
                );
            }
        }
    }

    #[test]
    fn route_source_and_adapter_labels_cover_variants() {
        assert_eq!(
            route_source_label(DelegationRouteSource::ForcedChild),
            "forced_child"
        );
        assert_eq!(
            suppression_adapter_label(&NativeSuppressionPlan::GrokNoSubagents),
            "grok_no_subagents"
        );
        assert_eq!(ROUTE_DEGRADED_CODE, "route_degraded");
        assert_eq!(DELEGATION_UNAVAILABLE_CODE, "delegation_unavailable");
    }

    #[test]
    fn route_audit_carries_route_degraded_only_when_degraded() {
        let healthy = codeg_plan(AgentType::Codex);
        let healthy_rec = DelegationAuditRecord::route(
            "conn-healthy",
            Some(1),
            AgentType::Codex,
            &healthy,
            SuppressionApplication::Applied,
        );
        assert_eq!(
            healthy_rec.stable_code(),
            None,
            "healthy route must not emit route_degraded"
        );
        healthy_rec.emit_route_resolved();

        let mut degraded = codeg_plan(AgentType::Codex);
        degraded.effective = DelegationRoutePolicy::Native;
        degraded.source = DelegationRouteSource::SafeFallback;
        degraded.native_suppression = NativeSuppressionPlan::None;
        degraded.expose_codeg_delegation = false;
        degraded.degraded_reason = Some(RouteDegradedReason::CompanionBinaryUnavailable);
        let degraded_rec = DelegationAuditRecord::route(
            "conn-degraded",
            Some(2),
            AgentType::Codex,
            &degraded,
            SuppressionApplication::NotApplicable,
        );
        assert_eq!(degraded_rec.stable_code(), Some(ROUTE_DEGRADED_CODE));
        let value = serde_json::to_value(&degraded_rec).unwrap();
        assert_eq!(value["stable_code"], ROUTE_DEGRADED_CODE);
        // Field *names* only — substring on full JSON false-positives structural keys.
        let object = value.as_object().unwrap();
        for forbidden in [
            "prompt",
            "task",
            "result_text",
            "token",
            "api_key",
            "environment",
            "raw_payload",
            "companion_token",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "degraded route audit must not have field {forbidden}"
            );
        }
        degraded_rec.emit_route_resolved();
    }

    #[test]
    fn availability_audit_carries_delegation_unavailable_code() {
        let rec = DelegationAuditRecord::availability("conn-1", Some(42), AgentType::Grok);
        assert_eq!(rec.stable_code(), Some(DELEGATION_UNAVAILABLE_CODE));
        let value = serde_json::to_value(&rec).unwrap();
        assert_eq!(value["kind"], "availability");
        assert_eq!(value["stable_code"], DELEGATION_UNAVAILABLE_CODE);
        assert_eq!(value["connection_id"], "conn-1");
        // Deny list is field *names* (substring on full JSON would false-positive
        // on `task_id` / similar structural keys).
        let object = value.as_object().unwrap();
        for forbidden in [
            "prompt",
            "task",
            "result_text",
            "token",
            "api_key",
            "environment",
            "raw_payload",
            "companion_token",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "availability audit must not have field {forbidden}"
            );
        }
        rec.emit_availability();
    }
}
