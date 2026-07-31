//! Secret-safe process-local counters for the tool-execution watchdog.
//!
//! Labels are limited to agent type and coarse tool category. Never record raw
//! tool input, provider tool_call_id, cancel handles, prompts, env, or tokens.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::acp::tool_watchdog::types::ToolCategory;
use crate::models::AgentType;

/// Coarse label key: `agent_type` + `tool_category` (stable snake_case enums).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WatchdogMetricLabel {
    pub agent_type: String,
    pub tool_category: String,
}

impl WatchdogMetricLabel {
    pub fn new(agent_type: Option<AgentType>, category: ToolCategory) -> Self {
        Self {
            agent_type: agent_type
                .map(agent_type_label)
                .unwrap_or("unknown")
                .to_string(),
            tool_category: category.as_str().to_string(),
        }
    }

    fn key(&self) -> String {
        format!("{}:{}", self.agent_type, self.tool_category)
    }
}

/// Stable snake_case agent label (matches delegation metrics vocabulary).
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
        AgentType::Cursor => "cursor",
        AgentType::Custom(_) => "custom",
    }
}

/// Snapshot of secret-safe counters for diagnostics / tests.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolWatchdogMetricsSnapshot {
    pub warning_episodes: BTreeMap<String, u64>,
    pub extensions: BTreeMap<String, u64>,
    pub automatic_timeouts: BTreeMap<String, u64>,
    pub user_stops: BTreeMap<String, u64>,
    pub specific_cancel_success: BTreeMap<String, u64>,
    pub turn_fallback: BTreeMap<String, u64>,
    pub disconnect_fallback: BTreeMap<String, u64>,
    pub cancellation_failure: BTreeMap<String, u64>,
    /// Totals (sum of labeled maps) for quick asserts without iterating labels.
    pub warning_episodes_total: u64,
    pub extensions_total: u64,
    pub automatic_timeouts_total: u64,
    pub user_stops_total: u64,
    pub specific_cancel_success_total: u64,
    pub turn_fallback_total: u64,
    pub disconnect_fallback_total: u64,
    pub cancellation_failure_total: u64,
}

/// Process-local tool-watchdog reliability counters.
#[derive(Debug, Default)]
pub struct ToolWatchdogMetrics {
    warning_episodes: Mutex<BTreeMap<String, u64>>,
    extensions: Mutex<BTreeMap<String, u64>>,
    automatic_timeouts: Mutex<BTreeMap<String, u64>>,
    user_stops: Mutex<BTreeMap<String, u64>>,
    specific_cancel_success: Mutex<BTreeMap<String, u64>>,
    turn_fallback: Mutex<BTreeMap<String, u64>>,
    disconnect_fallback: Mutex<BTreeMap<String, u64>>,
    cancellation_failure: Mutex<BTreeMap<String, u64>>,
    warning_episodes_total: AtomicU64,
    extensions_total: AtomicU64,
    automatic_timeouts_total: AtomicU64,
    user_stops_total: AtomicU64,
    specific_cancel_success_total: AtomicU64,
    turn_fallback_total: AtomicU64,
    disconnect_fallback_total: AtomicU64,
    cancellation_failure_total: AtomicU64,
}

impl ToolWatchdogMetrics {
    fn inc_labeled(map: &Mutex<BTreeMap<String, u64>>, label: &WatchdogMetricLabel) {
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(label.key()).or_insert(0);
        *entry = (*entry).saturating_add(1);
    }

    pub fn record_warning_episode(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.warning_episodes, &label);
        self.warning_episodes_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_extension(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.extensions, &label);
        self.extensions_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_automatic_timeout(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.automatic_timeouts, &label);
        self.automatic_timeouts_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_user_stop(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.user_stops, &label);
        self.user_stops_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_specific_cancel_success(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.specific_cancel_success, &label);
        self.specific_cancel_success_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_turn_fallback(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.turn_fallback, &label);
        self.turn_fallback_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_disconnect_fallback(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.disconnect_fallback, &label);
        self.disconnect_fallback_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cancellation_failure(&self, label: WatchdogMetricLabel) {
        Self::inc_labeled(&self.cancellation_failure, &label);
        self.cancellation_failure_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record escalation stage counters and any specific/turn/disconnect failures.
    pub fn record_escalation(
        &self,
        label: WatchdogMetricLabel,
        report: &crate::acp::tool_watchdog::EscalationReport,
    ) {
        use crate::acp::tool_watchdog::EscalationStage;
        match report.stage {
            EscalationStage::Specific | EscalationStage::AlreadyTerminal => {
                self.record_specific_cancel_success(label.clone());
            }
            EscalationStage::Turn => self.record_turn_fallback(label.clone()),
            EscalationStage::Disconnect => self.record_disconnect_fallback(label.clone()),
        }
        if report.had_operation_failure() {
            self.record_cancellation_failure(label);
        }
    }

    pub fn snapshot(&self) -> ToolWatchdogMetricsSnapshot {
        fn clone_map(m: &Mutex<BTreeMap<String, u64>>) -> BTreeMap<String, u64> {
            m.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
        ToolWatchdogMetricsSnapshot {
            warning_episodes: clone_map(&self.warning_episodes),
            extensions: clone_map(&self.extensions),
            automatic_timeouts: clone_map(&self.automatic_timeouts),
            user_stops: clone_map(&self.user_stops),
            specific_cancel_success: clone_map(&self.specific_cancel_success),
            turn_fallback: clone_map(&self.turn_fallback),
            disconnect_fallback: clone_map(&self.disconnect_fallback),
            cancellation_failure: clone_map(&self.cancellation_failure),
            warning_episodes_total: self.warning_episodes_total.load(Ordering::Relaxed),
            extensions_total: self.extensions_total.load(Ordering::Relaxed),
            automatic_timeouts_total: self.automatic_timeouts_total.load(Ordering::Relaxed),
            user_stops_total: self.user_stops_total.load(Ordering::Relaxed),
            specific_cancel_success_total: self
                .specific_cancel_success_total
                .load(Ordering::Relaxed),
            turn_fallback_total: self.turn_fallback_total.load(Ordering::Relaxed),
            disconnect_fallback_total: self.disconnect_fallback_total.load(Ordering::Relaxed),
            cancellation_failure_total: self.cancellation_failure_total.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::tool_watchdog::EscalationStage;

    fn sample_report(
        stage: EscalationStage,
        specific_failed: bool,
        turn_failed: bool,
        disconnect_failed: bool,
    ) -> crate::acp::tool_watchdog::EscalationReport {
        use crate::acp::tool_watchdog::{CancellationScope, EscalationReport};
        EscalationReport {
            stage,
            error_code: "tool_stalled_timeout".into(),
            cancellation_scope: CancellationScope::Turn,
            specific_converged: stage == EscalationStage::Specific,
            turn_converged: matches!(
                stage,
                EscalationStage::Specific
                    | EscalationStage::Turn
                    | EscalationStage::AlreadyTerminal
            ),
            disconnected: stage == EscalationStage::Disconnect,
            specific_failed,
            turn_failed,
            disconnect_failed,
            settled_projection: None,
        }
    }

    #[test]
    fn counters_increment_by_label_without_secrets() {
        let m = ToolWatchdogMetrics::default();
        let label = WatchdogMetricLabel::new(Some(AgentType::Codex), ToolCategory::Terminal);
        m.record_warning_episode(label.clone());
        m.record_extension(label.clone());
        m.record_automatic_timeout(label.clone());
        m.record_user_stop(label.clone());
        m.record_escalation(
            label.clone(),
            &sample_report(EscalationStage::Specific, false, false, false),
        );
        m.record_escalation(
            label.clone(),
            &sample_report(EscalationStage::Turn, true, false, false),
        );
        m.record_escalation(
            label.clone(),
            &sample_report(EscalationStage::Disconnect, false, true, true),
        );

        let snap = m.snapshot();
        assert_eq!(snap.warning_episodes_total, 1);
        assert_eq!(snap.extensions_total, 1);
        assert_eq!(snap.automatic_timeouts_total, 1);
        assert_eq!(snap.user_stops_total, 1);
        assert_eq!(snap.specific_cancel_success_total, 1);
        assert_eq!(snap.turn_fallback_total, 1);
        assert_eq!(snap.disconnect_fallback_total, 1);
        // Failures from turn (specific_failed) + disconnect (turn+disconnect failed).
        assert_eq!(snap.cancellation_failure_total, 2);

        let text = serde_json::to_string(&snap).expect("serialize");
        for forbidden in [
            "tool_call_id",
            "raw_input",
            "cancel_token",
            "session_id",
            "terminal_id",
            "api_key",
            "prompt",
            "ENV_SECRET",
            "Bearer ",
        ] {
            assert!(
                !text.contains(forbidden),
                "metrics snapshot must not contain {forbidden}: {text}"
            );
        }
        // Labels are only agent + category.
        assert!(text.contains("codex:terminal") || text.contains("\"codex\""));
    }

    #[test]
    fn unknown_agent_label_is_stable() {
        let label = WatchdogMetricLabel::new(None, ToolCategory::Mcp);
        assert_eq!(label.agent_type, "unknown");
        assert_eq!(label.tool_category, "mcp");
        assert_eq!(label.key(), "unknown:mcp");
    }

    #[test]
    fn custom_agent_label_does_not_expose_registry_id() {
        let label =
            WatchdogMetricLabel::new(Some(AgentType::Custom("private-id")), ToolCategory::Mcp);
        assert_eq!(label.agent_type, "custom");
        assert_eq!(label.key(), "custom:mcp");
    }
}
