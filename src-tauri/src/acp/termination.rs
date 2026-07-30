use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::acp::delegation::types::ParentTurnEndReason;
use crate::db::entities::delegation_task_run::{AdmissionClass, DelegationRunStatus};

pub const TERMINATION_AUDIT_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpDisconnectOrigin {
    ExplicitUser,
    ProviderUnmount,
    DisconnectAll,
    ApplicationShutdown,
    ConnectionSuperseded,
    IdleTimeout,
    ConfigReapply,
    DraftRetarget,
    AbandonedConnect,
    InternalJobComplete,
    LegacyUnspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationSource {
    Transport,
    Process,
    Session,
    Frontend,
    HostRestart,
    ParentTurn,
    Watchdog,
    ChildConnection,
    Admission,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationReason {
    TransportDisconnected,
    ProcessExited,
    SessionLost,
    FrontendDisconnected,
    HostRestarted,
    ParentCanceled,
    ParentTurnFailed,
    JoinAbandoned,
    UserCancelled,
    ToolStalledTimeout,
    SuspensionDrainTimeout,
    ChildTerminal,
    AdmissionFailed,
    AdmissionUnknown,
    LegacyUnspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationClassification {
    Unexpected,
    Intentional,
    Explicit,
    AutomatedAmbiguous,
    LegacyUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpTerminationSummaryV1 {
    pub version: u8,
    pub source: AcpTerminationSource,
    pub reason: AcpTerminationReason,
    pub classification: AcpTerminationClassification,
    pub frontend_origin: Option<AcpDisconnectOrigin>,
    pub prompt_may_have_executed: bool,
    pub requested_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

impl AcpTerminationSummaryV1 {
    pub fn new(
        source: AcpTerminationSource,
        reason: AcpTerminationReason,
        classification: AcpTerminationClassification,
        prompt_may_have_executed: bool,
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            version: TERMINATION_AUDIT_VERSION,
            source,
            reason,
            classification,
            frontend_origin: None,
            prompt_may_have_executed,
            requested_at: None,
            observed_at,
        }
    }

    pub fn legacy_unspecified(prompt_may_have_executed: bool, observed_at: DateTime<Utc>) -> Self {
        Self::new(
            AcpTerminationSource::Legacy,
            AcpTerminationReason::LegacyUnspecified,
            AcpTerminationClassification::LegacyUnknown,
            prompt_may_have_executed,
            observed_at,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationTerminationAuditV1 {
    pub termination: AcpTerminationSummaryV1,
    pub prior_status: DelegationRunStatus,
    pub admission_class: AdmissionClass,
    pub parent_tool_use_id: Option<String>,
    pub child_connection_id: Option<String>,
}

impl DelegationTerminationAuditV1 {
    pub fn new(
        termination: AcpTerminationSummaryV1,
        prior_status: DelegationRunStatus,
        admission_class: AdmissionClass,
        parent_tool_use_id: Option<String>,
        child_connection_id: Option<String>,
    ) -> Self {
        Self {
            termination,
            prior_status,
            admission_class,
            parent_tool_use_id,
            child_connection_id,
        }
    }

    pub fn for_terminal_code(
        error_code: &str,
        prior_status: DelegationRunStatus,
        prompt_may_have_executed: bool,
        observed_at: DateTime<Utc>,
    ) -> Self {
        let (source, reason, classification, requested) = match error_code {
            "parent_canceled" => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentCanceled,
                AcpTerminationClassification::Explicit,
                true,
            ),
            "parent_turn_failed" => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentTurnFailed,
                AcpTerminationClassification::Unexpected,
                false,
            ),
            "join_abandoned" => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::JoinAbandoned,
                AcpTerminationClassification::Intentional,
                true,
            ),
            "user_cancelled" | "canceled" => (
                AcpTerminationSource::Frontend,
                AcpTerminationReason::UserCancelled,
                AcpTerminationClassification::Explicit,
                true,
            ),
            "tool_stalled_timeout" => (
                AcpTerminationSource::Watchdog,
                AcpTerminationReason::ToolStalledTimeout,
                AcpTerminationClassification::AutomatedAmbiguous,
                true,
            ),
            "host_restarted" => (
                AcpTerminationSource::HostRestart,
                AcpTerminationReason::HostRestarted,
                AcpTerminationClassification::Unexpected,
                false,
            ),
            "admission_failed"
            | "spawn_failed"
            | "route_policy_rejected"
            | "budget_exhausted"
            | "not_supported" => (
                AcpTerminationSource::Admission,
                AcpTerminationReason::AdmissionFailed,
                AcpTerminationClassification::Intentional,
                false,
            ),
            "admission_unknown" => (
                AcpTerminationSource::Admission,
                AcpTerminationReason::AdmissionUnknown,
                AcpTerminationClassification::AutomatedAmbiguous,
                false,
            ),
            "process_exited" => (
                AcpTerminationSource::Process,
                AcpTerminationReason::ProcessExited,
                AcpTerminationClassification::Unexpected,
                false,
            ),
            "send_failed" | "transport_disconnected" => (
                AcpTerminationSource::Transport,
                AcpTerminationReason::TransportDisconnected,
                AcpTerminationClassification::Unexpected,
                false,
            ),
            "session_lost" | "unresumable" => (
                AcpTerminationSource::Session,
                AcpTerminationReason::SessionLost,
                AcpTerminationClassification::Unexpected,
                false,
            ),
            "parent_disconnected" => (
                AcpTerminationSource::Legacy,
                AcpTerminationReason::LegacyUnspecified,
                AcpTerminationClassification::LegacyUnknown,
                false,
            ),
            _ => (
                AcpTerminationSource::ChildConnection,
                AcpTerminationReason::ChildTerminal,
                AcpTerminationClassification::Unexpected,
                false,
            ),
        };
        let mut termination = AcpTerminationSummaryV1::new(
            source,
            reason,
            classification,
            prompt_may_have_executed,
            observed_at,
        );
        if requested {
            termination.requested_at = Some(observed_at);
        }
        if source == AcpTerminationSource::Frontend {
            termination.frontend_origin = Some(AcpDisconnectOrigin::LegacyUnspecified);
        }
        Self::new(
            termination,
            prior_status,
            AdmissionClass::NormalRevision,
            None,
            None,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedDelegationTermination {
    Typed(DelegationTerminationAuditV1),
    LegacyParentDisconnect,
    LegacyUnspecified,
    Malformed { raw_sha256: String },
}

impl ParsedDelegationTermination {
    pub fn is_automatic_unexpected_termination(&self) -> bool {
        matches!(
            self,
            Self::Typed(DelegationTerminationAuditV1 {
                termination: AcpTerminationSummaryV1 {
                    classification: AcpTerminationClassification::Unexpected,
                    prompt_may_have_executed: true,
                    ..
                },
                prior_status: DelegationRunStatus::Running,
                ..
            })
        )
    }
}

pub fn parse_delegation_termination(
    status: DelegationRunStatus,
    error_code: Option<&str>,
    reached_running: bool,
    raw_audit: Option<&str>,
) -> ParsedDelegationTermination {
    let Some(raw) = raw_audit else {
        if status == DelegationRunStatus::Canceled
            && error_code == Some("parent_disconnected")
            && reached_running
        {
            return ParsedDelegationTermination::LegacyParentDisconnect;
        }
        return ParsedDelegationTermination::LegacyUnspecified;
    };

    match serde_json::from_str::<DelegationTerminationAuditV1>(raw) {
        Ok(audit) if audit.termination.version == TERMINATION_AUDIT_VERSION => {
            ParsedDelegationTermination::Typed(audit)
        }
        Ok(_) | Err(_) => ParsedDelegationTermination::Malformed {
            raw_sha256: hex_sha256(raw.as_bytes()),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentEndContext {
    pub reason: ParentTurnEndReason,
    pub termination: AcpTerminationSummaryV1,
}

impl ParentEndContext {
    pub fn legacy(reason: ParentTurnEndReason, observed_at: DateTime<Utc>) -> Self {
        let (source, termination_reason, classification, requested_at) = match reason {
            ParentTurnEndReason::ParentCanceled => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentCanceled,
                AcpTerminationClassification::Explicit,
                Some(observed_at),
            ),
            ParentTurnEndReason::ParentTurnFailed => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentTurnFailed,
                AcpTerminationClassification::Unexpected,
                None,
            ),
            ParentTurnEndReason::JoinAbandoned => (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::JoinAbandoned,
                AcpTerminationClassification::Intentional,
                Some(observed_at),
            ),
            ParentTurnEndReason::ParentDisconnected => (
                AcpTerminationSource::Legacy,
                AcpTerminationReason::LegacyUnspecified,
                AcpTerminationClassification::LegacyUnknown,
                None,
            ),
        };
        let mut termination = AcpTerminationSummaryV1::new(
            source,
            termination_reason,
            classification,
            true,
            observed_at,
        );
        termination.requested_at = requested_at;
        Self {
            reason,
            termination,
        }
    }

    pub fn audit(
        &self,
        prior_status: DelegationRunStatus,
        admission_class: AdmissionClass,
        parent_tool_use_id: Option<String>,
        child_connection_id: Option<String>,
    ) -> DelegationTerminationAuditV1 {
        DelegationTerminationAuditV1::new(
            self.termination.clone(),
            prior_status,
            admission_class,
            parent_tool_use_id,
            child_connection_id,
        )
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
