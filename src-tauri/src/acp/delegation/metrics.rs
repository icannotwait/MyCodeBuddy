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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acp::delegation::continuation::types::{
    ContinuationFailureCode, ContinuationState, ContinuationWakeReason,
};
use crate::acp::delegation::route::{
    DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource, NativeSuppressionPlan,
    RouteDegradedReason, RouteResolutionError,
};
use crate::acp::delegation::transport::CancelDelegationReason;
use crate::acp::delegation::types::{TaskObservation, TaskStatus};
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
}

// ── Metrics ────────────────────────────────────────────────────────────────

/// Process-local counters for delegation reliability observability.
#[derive(Debug, Default)]
pub struct DelegationMetrics {
    route_selections: Mutex<BTreeMap<String, u64>>,
    safe_fallbacks: Mutex<BTreeMap<String, u64>>,
    suppression_failures: Mutex<BTreeMap<String, u64>>,
    accepted_count: AtomicU64,
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
}

impl DelegationMetrics {
    fn inc_labeled(map: &Mutex<BTreeMap<String, u64>>, key: String) {
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key).or_insert(0);
        *entry = (*entry).saturating_add(1);
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

    /// Accepted only after the durable accepted boundary (Task 8).
    pub fn record_accepted(&self, _agent_type: AgentType) {
        self.accepted_count.fetch_add(1, Ordering::Relaxed);
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
        DelegationMetricsSnapshot {
            route_selections,
            safe_fallbacks,
            suppression_failures,
            accepted_count: self.accepted_count.load(Ordering::Relaxed),
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
        }
    }
}

// ── Label helpers (stable enums only) ──────────────────────────────────────

fn agent_type_label(agent: AgentType) -> &'static str {
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::route::ROUTE_ADAPTER_CONTRACT_VERSION;

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
