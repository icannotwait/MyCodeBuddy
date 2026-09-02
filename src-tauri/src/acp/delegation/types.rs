//! Broker-facing request / outcome types.
//!
//! These cross two boundaries:
//! 1. The MCP companion serializes `DelegationRequest` → JSON-RPC params and
//!    deserializes `DelegationOutcome` → MCP `tool_result`.
//! 2. The broker emits a structured outcome the listener can persist and
//!    forward to the parent's tool_use_id.
//!
//! DB ids are `i32` to match the actual `conversation.id` / `conversation.parent_id`
//! column types — keeping them strongly typed here saves us a parse-or-die step
//! at every DB boundary.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::acp::delegation::attention::AttentionResolutionCode;
use crate::acp::delegation::recovery_policy::{
    RecoveryDecision, RecoveryDisposition, ReplacementReason,
};
use crate::acp::delegation::workflow::{CompletionAttentionCas, CompletionOutcome};
use crate::models::AgentType;

/// MCP tool name for initial delegation — field 0 of `request_fingerprint`.
pub const DELEGATE_TO_AGENT_TOOL: &str = "delegate_to_agent";
/// MCP tool name for session reuse — field 0 of `request_fingerprint`.
pub const CONTINUE_DELEGATION_TOOL: &str = "continue_delegation";

pub(crate) fn is_canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.to_string() == value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationBindingV1 {
    pub schema_version: u32,
    pub namespace: String,
    pub generation: u32,
    pub route_fingerprint: String,
}

impl OrchestrationBindingV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("schema_version must be 1");
        }
        let namespace = self.namespace.as_bytes();
        if namespace.is_empty()
            || namespace.len() > 64
            || !namespace[0].is_ascii_lowercase()
            || !namespace[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err("namespace is invalid");
        }
        if self.generation == 0 {
            return Err("generation must be positive");
        }
        let fingerprint = self.route_fingerprint.as_bytes();
        if fingerprint.len() != 71
            || !fingerprint.starts_with(b"sha256:")
            || !fingerprint[7..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err("route_fingerprint is invalid");
        }
        Ok(())
    }
}

/// Strict cross-runtime input for the ticket-v1 request fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketV1PendingCall {
    pub schema_version: u32,
    pub tool_name: String,
    pub task: String,
    pub working_dir: Option<String>,
    pub work_unit_key: Option<String>,
    pub replaces_task_id: Option<String>,
    pub replacement_reason: Option<String>,
    pub target_task_id: Option<String>,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub orchestration_binding: Option<OrchestrationBindingV1>,
    pub dispatch_intent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmissionTicketV1Request {
    pub dispatch_intent_id: String,
    pub admission_ticket: String,
}

pub(crate) fn parse_admission_ticket_v1_request(
    input: &serde_json::Value,
) -> Result<Option<AdmissionTicketV1Request>, &'static str> {
    match (
        input.get("dispatch_intent_id"),
        input.get("admission_ticket"),
    ) {
        (None, None) => Ok(None),
        (Some(serde_json::Value::String(intent)), Some(serde_json::Value::String(ticket)))
            if is_canonical_uuid(intent) && is_canonical_uuid(ticket) =>
        {
            Ok(Some(AdmissionTicketV1Request {
                dispatch_intent_id: intent.clone(),
                admission_ticket: ticket.clone(),
            }))
        }
        _ => Err(
            "dispatch_intent_id and admission_ticket must be canonical lowercase UUIDs supplied together",
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketV1AdmissionCandidate {
    pub dispatch_intent_id: String,
    pub admission_ticket: String,
    pub request_fingerprint: String,
}

pub const ORCHESTRATION_BINDING_DEFAULT_LIMIT: u16 = 100;
pub const ORCHESTRATION_BINDING_MAX_LIMIT: u16 = 200;

fn default_orchestration_binding_limit() -> u16 {
    ORCHESTRATION_BINDING_DEFAULT_LIMIT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationBindingDelivery {
    Page,
    Artifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionIntentKind {
    First,
    Continue,
    Replacement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdmissionIntentV1 {
    pub schema_version: u32,
    pub dispatch_intent_id: String,
    pub request_fingerprint: String,
    pub kind: AdmissionIntentKind,
    pub work_unit_key: String,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub target_task_id: Option<String>,
    pub replacement_reason: Option<String>,
    pub orchestration_binding: Option<OrchestrationBindingV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionIntentV1Wire {
    schema_version: u32,
    dispatch_intent_id: String,
    request_fingerprint: String,
    kind: AdmissionIntentKind,
    work_unit_key: String,
    agent_type: String,
    profile_id: serde_json::Value,
    target_task_id: serde_json::Value,
    replacement_reason: serde_json::Value,
    orchestration_binding: serde_json::Value,
}

impl<'de> Deserialize<'de> for AdmissionIntentV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AdmissionIntentV1Wire::deserialize(deserializer)?;
        let profile_id: Option<String> =
            serde_json::from_value(wire.profile_id).map_err(serde::de::Error::custom)?;
        let target_task_id: Option<String> =
            serde_json::from_value(wire.target_task_id).map_err(serde::de::Error::custom)?;
        let replacement_reason: Option<String> =
            serde_json::from_value(wire.replacement_reason).map_err(serde::de::Error::custom)?;
        let orchestration_binding: Option<OrchestrationBindingV1> =
            serde_json::from_value(wire.orchestration_binding).map_err(serde::de::Error::custom)?;
        let intent = Self {
            schema_version: wire.schema_version,
            dispatch_intent_id: wire.dispatch_intent_id,
            request_fingerprint: wire.request_fingerprint,
            kind: wire.kind,
            work_unit_key: wire.work_unit_key,
            agent_type: wire.agent_type,
            profile_id,
            target_task_id,
            replacement_reason,
            orchestration_binding,
        };
        intent.validate().map_err(serde::de::Error::custom)?;
        Ok(intent)
    }
}

impl AdmissionIntentV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("schema_version must be 1");
        }
        if !is_canonical_uuid(&self.dispatch_intent_id) {
            return Err("dispatch_intent_id must be a canonical lowercase UUID");
        }
        let fingerprint = self.request_fingerprint.as_bytes();
        if fingerprint.len() != 64
            || !fingerprint
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err("request_fingerprint must be lowercase SHA-256 hex");
        }
        if self.work_unit_key.is_empty()
            || self.work_unit_key.trim() != self.work_unit_key
            || self.work_unit_key.chars().count() > 200
        {
            return Err("work_unit_key is invalid");
        }
        let agent_type =
            AgentType::from_untrusted_wire(&self.agent_type).ok_or("agent_type is invalid")?;
        if agent_type.as_wire() != self.agent_type {
            return Err("agent_type is invalid");
        }
        if self
            .profile_id
            .as_ref()
            .is_some_and(|profile_id| profile_id.is_empty() || profile_id.trim() != profile_id)
        {
            return Err("profile_id is invalid");
        }
        if let Some(binding) = self.orchestration_binding.as_ref() {
            binding.validate()?;
        }
        match self.kind {
            AdmissionIntentKind::First
                if self.target_task_id.is_none() && self.replacement_reason.is_none() => {}
            AdmissionIntentKind::Continue
                if self
                    .target_task_id
                    .as_deref()
                    .is_some_and(is_canonical_uuid)
                    && self.replacement_reason.is_none() => {}
            AdmissionIntentKind::Replacement
                if self
                    .target_task_id
                    .as_deref()
                    .is_some_and(is_canonical_uuid)
                    && self
                        .replacement_reason
                        .as_deref()
                        .and_then(ReplacementReason::parse)
                        .is_some() => {}
            _ => return Err("admission operation fields are invalid"),
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> String {
        let bytes =
            serde_json::to_vec(self).expect("admission intent contains serializable fields");
        let digest = Sha256::digest(bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AdmissionPreparation {
    Prepared {
        protocol: String,
        dispatch_intent_id: String,
        ticket: String,
        expires_at: DateTime<Utc>,
    },
    AlreadyAdmitted {
        protocol: String,
        dispatch_intent_id: String,
        task_id: String,
    },
}

impl AdmissionPreparation {
    pub(crate) fn prepared(
        dispatch_intent_id: String,
        ticket: String,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self::Prepared {
            protocol: "ticket_v1".into(),
            dispatch_intent_id,
            ticket,
            expires_at,
        }
    }

    pub(crate) fn already_admitted(dispatch_intent_id: String, task_id: String) -> Self {
        Self::AlreadyAdmitted {
            protocol: "ticket_v1".into(),
            dispatch_intent_id,
            task_id,
        }
    }
}

/// Strict companion-facing input; the broker continues to receive page DTOs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationBindingToolRequest {
    pub namespace: String,
    pub delivery: Option<OrchestrationBindingDelivery>,
    pub limit: Option<u16>,
    pub snapshot_id: Option<String>,
    pub cursor: Option<String>,
    pub admission_intent: Option<AdmissionIntentV1>,
}

/// Strict MCP input for a first page or continuation of one binding snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationBindingQueryRequest {
    pub namespace: String,
    #[serde(default = "default_orchestration_binding_limit")]
    pub limit: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl OrchestrationBindingQueryRequest {
    pub fn validate(&self) -> Result<(), OrchestrationBindingQueryError> {
        let namespace = self.namespace.as_bytes();
        if namespace.is_empty()
            || namespace.len() > 64
            || !namespace[0].is_ascii_lowercase()
            || !namespace[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err(OrchestrationBindingQueryError::Invalid);
        }
        if !(1..=ORCHESTRATION_BINDING_MAX_LIMIT).contains(&self.limit) {
            return Err(OrchestrationBindingQueryError::Invalid);
        }
        match (&self.snapshot_id, &self.cursor) {
            (None, None) => {}
            (Some(snapshot_id), Some(cursor)) => {
                let parsed = uuid::Uuid::parse_str(snapshot_id)
                    .map_err(|_| OrchestrationBindingQueryError::Invalid)?;
                if parsed.to_string() != *snapshot_id
                    || cursor.is_empty()
                    || cursor.len() > 128
                    || !cursor
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                {
                    return Err(OrchestrationBindingQueryError::Invalid);
                }
            }
            _ => return Err(OrchestrationBindingQueryError::Invalid),
        }
        Ok(())
    }
}

/// Approved durable identity for one selected delegation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationOrchestrationBindingRun {
    pub task_id: String,
    pub root_task_id: String,
    pub previous_task_id: Option<String>,
    pub lineage_root_task_id: String,
    pub replaced_task_id: Option<String>,
    pub replacement_reason: Option<String>,
    pub generic_generation: u64,
    pub work_unit_key: Option<String>,
    pub child_conversation_id: i32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub status: String,
    pub orchestration_binding: Option<OrchestrationBindingV1>,
}

/// One raw, replayable page from a process-local parent-scoped snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationOrchestrationBindingPage {
    pub schema_version: u32,
    pub namespace: String,
    pub snapshot_id: String,
    pub snapshot_revision: String,
    pub snapshot_created_at: DateTime<Utc>,
    pub snapshot_expires_at: DateTime<Utc>,
    pub total_rows: u64,
    pub page_start: u64,
    pub request_cursor: Option<String>,
    pub runs: Vec<DelegationOrchestrationBindingRun>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationBindingFirstPageEnvelope {
    pub page: DelegationOrchestrationBindingPage,
    pub admission: AdmissionPreparation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEvidenceV1 {
    pub schema_version: u32,
    pub pages: Vec<DelegationOrchestrationBindingPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationBindingArtifactDescriptor {
    pub schema_version: u32,
    pub delivery: String,
    pub namespace: String,
    pub snapshot_id: String,
    pub snapshot_revision: String,
    pub snapshot_created_at: DateTime<Utc>,
    pub snapshot_expires_at: DateTime<Utc>,
    pub total_rows: u64,
    pub artifact_path: String,
    pub artifact_format: String,
    pub artifact_bytes: u64,
    pub artifact_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OrchestrationBindingQueryError {
    #[error("invalid orchestration binding query")]
    Invalid,
    #[error("orchestration binding query exceeds the row limit")]
    TooLarge,
    #[error("orchestration binding query failed")]
    Failed,
    #[error("orchestration binding snapshot is stale")]
    SnapshotStale,
    #[error("orchestration binding artifact I/O failed")]
    ArtifactIoFailed,
    #[error("orchestration binding artifact exceeds the byte limit")]
    ArtifactTooLarge,
    #[error("orchestration binding artifact result exceeds the transport limit")]
    ArtifactResultTooLarge,
    #[error("invalid orchestration admission intent")]
    AdmissionIntentInvalid,
    #[error("delegation dispatch intent conflicts with an existing run")]
    DispatchIntentConflict,
}

impl OrchestrationBindingQueryError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "orchestration_binding_query_invalid",
            Self::TooLarge => "orchestration_binding_query_too_large",
            Self::Failed => "orchestration_binding_query_failed",
            Self::SnapshotStale => "orchestration_binding_snapshot_stale",
            Self::ArtifactIoFailed => "orchestration_binding_artifact_io_failed",
            Self::ArtifactTooLarge => "orchestration_binding_artifact_too_large",
            Self::ArtifactResultTooLarge => "orchestration_binding_artifact_result_too_large",
            Self::AdmissionIntentInvalid => "orchestration_admission_intent_invalid",
            Self::DispatchIntentConflict => "delegation_dispatch_intent_conflict",
        }
    }
}

#[cfg(test)]
mod orchestration_binding_tests {
    use std::collections::BTreeSet;

    use serde_json::{json, Value};

    use super::{
        ContinueDelegationRequest, DelegationRequest, OrchestrationBindingToolRequest,
        OrchestrationBindingV1, TicketV1PendingCall,
    };
    use crate::acp::delegation::run_store::ticket_v1_request_fingerprint;

    fn admission_intent(kind: &str, target: Value, replacement_reason: Value) -> Value {
        json!({
            "schema_version": 1,
            "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
            "request_fingerprint": "2a44be9d1662a314cbbd2c8111bcf83159be7bdc93abadff977d01447f986648",
            "kind": kind,
            "work_unit_key": "task|7|implementer|codex|none",
            "agent_type": "codex",
            "profile_id": null,
            "target_task_id": target,
            "replacement_reason": replacement_reason,
            "orchestration_binding": null
        })
    }

    fn artifact_request(intent: Value) -> Value {
        json!({
            "namespace": "brainstorm-to-delivery",
            "delivery": "artifact",
            "admission_intent": intent
        })
    }

    #[test]
    fn orchestration_admission_intent_strict_json_and_operation_matrix() {
        for intent in [
            admission_intent("first", Value::Null, Value::Null),
            admission_intent(
                "continue",
                json!("6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45"),
                Value::Null,
            ),
            admission_intent(
                "replacement",
                json!("6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45"),
                json!("unresumable"),
            ),
        ] {
            serde_json::from_value::<OrchestrationBindingToolRequest>(artifact_request(intent))
                .expect("strict admission intent should be accepted");
        }

        let invalid = [
            admission_intent(
                "first",
                json!("6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45"),
                Value::Null,
            ),
            admission_intent("continue", Value::Null, Value::Null),
            admission_intent(
                "continue",
                json!("6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45"),
                json!("unresumable"),
            ),
            admission_intent(
                "replacement",
                json!("6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45"),
                Value::Null,
            ),
        ];
        for intent in invalid {
            assert!(
                serde_json::from_value::<OrchestrationBindingToolRequest>(artifact_request(intent))
                    .is_err(),
                "invalid operation matrix must be rejected"
            );
        }
    }

    #[test]
    fn orchestration_admission_intent_rejects_noncanonical_identity_and_unknown_keys() {
        let mut cases = Vec::new();
        let mut value = admission_intent("first", Value::Null, Value::Null);
        value["dispatch_intent_id"] = json!("8F95DD45-9ECA-42A8-9909-0AC00BE8AD52");
        cases.push(value);
        let mut value = admission_intent("first", Value::Null, Value::Null);
        value["request_fingerprint"] = json!("A".repeat(64));
        cases.push(value);
        let mut value = admission_intent("continue", json!("not-a-uuid"), Value::Null);
        value["orchestration_binding"] = json!({
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "generation": 1,
            "route_fingerprint": format!("sha256:{}", "a".repeat(64))
        });
        cases.push(value);
        let mut value = admission_intent("first", Value::Null, Value::Null);
        value["unknown"] = json!(true);
        cases.push(value);
        let mut value = admission_intent("first", Value::Null, Value::Null);
        value["orchestration_binding"] = json!({
            "schema_version": 1,
            "namespace": "INVALID",
            "generation": 1,
            "route_fingerprint": format!("sha256:{}", "a".repeat(64))
        });
        cases.push(value);
        for field in [
            "profile_id",
            "target_task_id",
            "replacement_reason",
            "orchestration_binding",
        ] {
            let mut value = admission_intent("first", Value::Null, Value::Null);
            value.as_object_mut().unwrap().remove(field);
            cases.push(value);
        }

        for intent in cases {
            assert!(
                serde_json::from_value::<OrchestrationBindingToolRequest>(artifact_request(intent))
                    .is_err(),
                "noncanonical or unknown intent input must be rejected"
            );
        }
    }

    #[test]
    fn orchestration_admission_intent_digest_hashes_exact_compact_strict_object() {
        let request: OrchestrationBindingToolRequest = serde_json::from_value(artifact_request(
            admission_intent("first", Value::Null, Value::Null),
        ))
        .unwrap();
        let intent = request.admission_intent.expect("admission intent");
        assert_eq!(
            serde_json::to_value(&intent).unwrap(),
            admission_intent("first", Value::Null, Value::Null)
        );
        assert_eq!(
            intent.canonical_digest(),
            "133c7b500e6569787f652c7a6f095a69446ef08a818599d6a941bfda325f046c"
        );
    }

    #[test]
    fn ticket_v1_request_contract_request_round_trip_preserves_pair_and_raw_cwd() {
        let raw_cwd = r#"  D:\repo/mixed\path  "#;
        let delegate: DelegationRequest = serde_json::from_value(json!({
            "parent_connection_id": "parent-conn",
            "parent_conversation_id": 7,
            "parent_tool_use_id": "tool-call",
            "agent_type": "codex",
            "task": "implement",
            "working_dir": raw_cwd,
            "requested_working_dir": raw_cwd,
            "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
            "admission_ticket": "4a67bba4-e1f5-46d1-a9b1-aa796598ffce"
        }))
        .unwrap();
        let delegate = serde_json::to_value(delegate).unwrap();
        assert_eq!(delegate["requested_working_dir"], raw_cwd);
        assert_eq!(
            delegate["dispatch_intent_id"],
            "8f95dd45-9eca-42a8-9909-0ac00be8ad52"
        );
        assert_eq!(
            delegate["admission_ticket"],
            "4a67bba4-e1f5-46d1-a9b1-aa796598ffce"
        );

        let continuation: ContinueDelegationRequest = serde_json::from_value(json!({
            "parent_connection_id": "parent-conn",
            "parent_conversation_id": 7,
            "parent_tool_use_id": "tool-call",
            "target_task_id": "source-task",
            "task": "continue",
            "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
            "admission_ticket": "4a67bba4-e1f5-46d1-a9b1-aa796598ffce"
        }))
        .unwrap();
        let continuation = serde_json::to_value(continuation).unwrap();
        assert!(continuation.get("working_dir").is_none());
        assert_eq!(
            continuation["dispatch_intent_id"],
            "8f95dd45-9eca-42a8-9909-0ac00be8ad52"
        );
        assert_eq!(
            continuation["admission_ticket"],
            "4a67bba4-e1f5-46d1-a9b1-aa796598ffce"
        );
    }

    #[test]
    fn ticket_v1_request_contract_candidate_digest_uses_exact_physical_inputs() {
        let pending = TicketV1PendingCall {
            schema_version: 1,
            tool_name: "delegate_to_agent".into(),
            task: "Implement Task 9".into(),
            working_dir: Some(r#"  D:\repo/mixed\path  "#.into()),
            work_unit_key: Some("task|9|implementer|codex|none".into()),
            replaces_task_id: None,
            replacement_reason: None,
            target_task_id: None,
            agent_type: "codex".into(),
            profile_id: None,
            orchestration_binding: None,
            dispatch_intent_id: "8f95dd45-9eca-42a8-9909-0ac00be8ad52".into(),
        };
        let base = ticket_v1_request_fingerprint(&pending);
        let mut changed_task = pending.clone();
        changed_task.task.push_str(" changed");
        let mut changed_cwd = pending.clone();
        changed_cwd.working_dir = Some(r#"  D:/repo/mixed/path  "#.into());
        let mut changed_intent = pending.clone();
        changed_intent.dispatch_intent_id = "11111111-1111-4111-8111-111111111111".into();
        assert_ne!(base, ticket_v1_request_fingerprint(&changed_task));
        assert_ne!(base, ticket_v1_request_fingerprint(&changed_cwd));
        assert_ne!(base, ticket_v1_request_fingerprint(&changed_intent));

        let mut omitted_cwd = pending.clone();
        omitted_cwd.working_dir = None;
        let mut empty_cwd = pending;
        empty_cwd.working_dir = Some(String::new());
        assert_eq!(
            ticket_v1_request_fingerprint(&omitted_cwd),
            ticket_v1_request_fingerprint(&empty_cwd)
        );
    }

    #[test]
    fn delegation_orchestration_bindings_shared_corpus_is_exact_and_strict() {
        let corpus: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/orchestration_binding_v1.json"
        )))
        .expect("binding grammar corpus is JSON");
        let top = corpus.as_object().expect("corpus top level is an object");
        assert_eq!(
            top.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from(["cases", "schema_version"])
        );
        assert_eq!(top["schema_version"], 1);

        let cases = top["cases"].as_array().expect("cases is an array");
        assert_eq!(cases.len(), 24);
        let expected_names = BTreeSet::from([
            "minimum",
            "maximum",
            "brainstorm_to_delivery",
            "null",
            "non_object",
            "missing_schema_version",
            "missing_namespace",
            "missing_generation",
            "missing_route_fingerprint",
            "extra_field",
            "wrong_schema_version",
            "schema_version_string",
            "namespace_number",
            "generation_string",
            "fingerprint_number",
            "generation_zero",
            "generation_overflow",
            "namespace_empty",
            "namespace_65_bytes",
            "namespace_uppercase",
            "namespace_underscore",
            "fingerprint_uppercase_hex",
            "fingerprint_wrong_length",
            "fingerprint_wrong_prefix",
        ]);
        let mut names = BTreeSet::new();

        for case in cases {
            let case = case.as_object().expect("case is an object");
            assert_eq!(
                case.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                BTreeSet::from(["name", "valid", "value"])
            );
            let name = case["name"].as_str().expect("case name is a string");
            assert!(names.insert(name), "duplicate corpus case name {name}");
            let expected_valid = case["valid"].as_bool().expect("valid is a boolean");
            let accepted = serde_json::from_value::<OrchestrationBindingV1>(case["value"].clone())
                .ok()
                .is_some_and(|binding| binding.validate().is_ok());
            assert_eq!(
                accepted, expected_valid,
                "binding grammar case {name} had unexpected result"
            );
        }
        assert_eq!(names, expected_names);
    }
}

/// Soft-watchdog health for a **running** Broker task only. Terminal tasks
/// have no observation. Observe-only — never a lifecycle / terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskObservation {
    Active,
    Stalled,
    WaitingInput,
}

/// Snapshot published by the soft supervisor when observation or timestamps
/// change. `stalled_since` is `last_agent_activity_at + threshold` (not scan time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationSnapshot {
    pub observation: TaskObservation,
    pub last_agent_activity_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_since: Option<DateTime<Utc>>,
}

/// Per-agent defaults applied when codeg-mcp spawns a subagent on behalf of a
/// `delegate_to_agent` call. Mirrors the two knobs `ConnectionManager::spawn_agent`
/// already accepts:
///   * `mode_id` → forwarded as `preferred_mode_id`
///   * `config_values` → forwarded as `preferred_config_values`
///
/// All fields are optional / may be empty; an absent entry means "no override —
/// use whatever the agent advertises as the default."
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDelegationDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, String>,
}

impl AgentDelegationDefaults {
    pub fn is_empty(&self) -> bool {
        self.mode_id.is_none() && self.config_values.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfile {
    pub id: String,
    pub agent_type: AgentType,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfileDocument {
    #[serde(default)]
    pub profiles: Vec<DelegationProfile>,
}

/// Revisioned snapshot of profiles + effective enabled flag for mention /
/// reference-search bootstrap. Profile setter inputs remain
/// [`DelegationProfileDocument`]; this is the read/event wire type only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfileCatalog {
    #[serde(default)]
    pub profiles: Vec<DelegationProfile>,
    pub delegation_enabled: bool,
    pub revision: u64,
}

/// Result of a catalog-affecting mutation: the field-specific value plus the
/// post-commit catalog snapshot (including the advanced revision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationMutation<T> {
    pub value: T,
    pub catalog: DelegationProfileCatalog,
}

/// Everything the broker needs to dispatch a single delegation call.
///
/// `parent_connection_id` is the codeg-internal ACP connection UUID for the
/// parent session (NOT the agent-assigned ACP session id). The broker uses it
/// to inherit the parent's EventEmitter/working_dir and to scope
/// `cancel_by_parent`.
///
/// `external_handle` is a companion-minted opaque token (per MCP `tools/call`)
/// that the broker stores alongside the pending entry so an MCP-side
/// `notifications/cancelled` can target this specific delegation without the
/// companion having to know the broker-internal `call_id`. `None` for non-MCP
/// callers and tests that don't exercise the cancel path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: String,
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub task: String,
    pub working_dir: Option<String>,
    /// The `working_dir` exactly as the LLM passed it in the
    /// `delegate_to_agent` arguments, BEFORE the listener defaults a missing
    /// value to the parent's launch directory. Used only as part of the
    /// `(agent_type, task, requested_working_dir)` correlation key so two
    /// parallel calls sharing an agent and task but targeting different
    /// explicit directories don't bind to each other's `tool_call_id`.
    /// `None` when the LLM omitted it — symmetric with the ACP `raw_input`,
    /// which also omits it then. Distinct from `working_dir` above, which is
    /// the defaulted value the child is actually spawned in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_handle: Option<String>,
    /// Opaque Skill-supplied orchestration key (≤ 200 chars). Joins platform
    /// budget rows and gen-1 concurrent first-dispatch fences under
    /// `(parent_conversation_id, work_unit_key)`. `None` for ad-hoc one-shots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_key: Option<String>,
    /// Optional replacement linkage (must pair with `replacement_reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_task_id: Option<String>,
    /// Why this gen-1 replaces a prior thread (`unresumable` /
    /// `budget_exhausted_continue` / `not_supported`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_reason: Option<String>,
    /// Caller-generated opaque call correlation token (transport-only). Used
    /// when the host omits `_meta.tool_use_id` so ACP and MCP can bind the same
    /// invocation. Not a task/run/conversation id; not persisted on the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_intent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_authorization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_binding: Option<OrchestrationBindingV1>,
}

/// Everything the broker needs to dispatch a `continue_delegation` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueDelegationRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: String,
    pub target_task_id: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_handle: Option<String>,
    /// Caller-generated opaque call correlation token (transport-only). See
    /// [`DelegationRequest::correlation_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_intent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_authorization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_binding: Option<OrchestrationBindingV1>,
}

/// Max accepted length for a call `correlation_id` (inclusive).
pub const CORRELATION_ID_MAX_LEN: usize = 128;

/// What a still-running delegation child is blocked on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedKind {
    Permission,
    Question,
    PlanApproval,
}

/// The blocking prompt itself: kind plus a short label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedOn {
    pub kind: BlockedKind,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Which delegation tool produced a correlation failure — drives entry-point
/// specific retry text in parent-facing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrelationEntryPoint {
    DelegateToAgent,
    ContinueDelegation,
}

/// Correlation failure kind (maps 1:1 to wire codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrelationFailureKind {
    Missing,
    Timeout,
    Ambiguous,
    Conflict,
}

impl CorrelationFailureKind {
    pub fn wire_code(self) -> &'static str {
        match self {
            Self::Missing => "delegation_correlation_missing",
            Self::Timeout => "delegation_correlation_timeout",
            Self::Ambiguous => "delegation_correlation_ambiguous",
            Self::Conflict => "delegation_correlation_conflict",
        }
    }
}

/// Validate a call `correlation_id` against the tool contract:
/// 1–128 ASCII chars matching `[A-Za-z0-9][A-Za-z0-9._:-]{0,127}`.
///
/// Rejects empty, whitespace, leading non-alnum (including `.`), and over-length.
pub fn validate_correlation_id(raw: &str) -> Result<(), String> {
    let len = raw.len();
    if len == 0 {
        return Err("correlation_id must be non-empty".into());
    }
    if len > CORRELATION_ID_MAX_LEN {
        return Err(format!(
            "correlation_id must be at most {CORRELATION_ID_MAX_LEN} characters"
        ));
    }
    if !raw.is_ascii() {
        return Err("correlation_id must be ASCII".into());
    }
    let bytes = raw.as_bytes();
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return Err("correlation_id must start with an ASCII letter or digit".into());
    }
    for &b in &bytes[1..] {
        if !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')) {
            return Err(
                "correlation_id may only contain [A-Za-z0-9._:-] after the first character".into(),
            );
        }
    }
    Ok(())
}

/// Parent-facing correlation error message with shared fail-closed clauses and
/// entry-point-specific retry guidance.
pub fn correlation_error_message(
    kind: CorrelationFailureKind,
    entry: CorrelationEntryPoint,
) -> String {
    let condition = match kind {
        CorrelationFailureKind::Missing => {
            "Parent tool correlation failed: neither an explicit host tool id nor a valid correlation_id was available to bind this call."
        }
        CorrelationFailureKind::Timeout => {
            "Parent tool correlation failed: no matching pending ACP tool call was found for this correlation_id within the wait budget."
        }
        CorrelationFailureKind::Ambiguous => {
            "Parent tool correlation failed: more than one pending ACP tool call matched this correlation key."
        }
        CorrelationFailureKind::Conflict => {
            "Parent tool correlation failed: the matching pending ACP tool call's correlation key was conflict-tombstoned and cannot be claimed."
        }
    };
    let shared = "The target child was not evaluated or resumed for this error. \
This is not evidence of unresumable. Do not create a replacement for this error. \
Mint a fresh correlation_id on every retry; never reuse a correlation_id across concurrent calls.";
    let retry = match entry {
        CorrelationEntryPoint::DelegateToAgent => {
            "Retry delegate_to_agent with the same substantive arguments and a new correlation_id."
        }
        CorrelationEntryPoint::ContinueDelegation => {
            "Retry continue_delegation with the current latest terminal target task id \
(re-read via get_delegation_status if a concurrent run may have advanced the lineage) \
and a new correlation_id."
        }
    };
    format!("{condition} {shared} {retry}")
}

/// Everything the broker needs to resume one interrupted delegation task.
///
/// Deliberately carries NO task text: `resume_delegation` continues the
/// ORIGINAL task in the child's own (resumed) session and must not become a
/// side-channel for new instructions — the only free-form field is `reason`,
/// which is bounded and framed as interruption context in the continuation
/// prompt (see `broker::build_resume_prompt`).
///
/// `parent_connection_id` / `parent_conversation_id` identify the CALLER —
/// after a parent-session restart this is a different connection id than the
/// one that originally delegated, but the same conversation row, which is what
/// the ownership check scopes on (`ChildResumeContext::parent_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeDelegationRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    /// The broker `call_id` of the task to resume — the same id
    /// `delegate_to_agent` returned and the child row persists as
    /// `delegation_call_id`. The resumed task keeps this id, so
    /// `get_delegation_status` / `cancel_delegation` keep working unchanged.
    pub task_id: String,
    /// Optional context on WHY the task is being resumed (e.g. "the app was
    /// killed mid-run"). Interruption context only — never new work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Companion-minted cancel handle, same contract as
    /// [`DelegationRequest::external_handle`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_handle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationSuccess {
    pub text: String,
    pub child_conversation_id: i32,
    pub child_agent_type: AgentType,
    pub turn_count: u32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
}

/// Stable English guidance returned for `cancel_delegation` with `reason=timeout`.
/// Intentionally not localized: the companion speaks to the LLM, not the UI.
pub const TIMEOUT_CANCEL_GUIDANCE: &str =
    "Do not cancel a still-running sub-agent; keep polling get_delegation_status.";

/// Drop fenced code blocks (``` or ~~~) so pasted docs/examples cannot
/// install mandatory routes from illustrative links or directives.
///
/// CommonMark rules (simplified):
/// - open/close by the same run character (`` ` `` or `~`)
/// - a closing fence must be at least as long as the opener
///
/// so `~~~` inside a ``` block, or ``` inside ````, does not end it early.
fn text_outside_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // None = outside; Some((char, min_len)) = open fence.
    let mut open_fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = {
            let bytes = trimmed.as_bytes();
            if bytes.len() >= 3 && (bytes[0] == b'`' || bytes[0] == b'~') {
                let ch = bytes[0] as char;
                let mut n = 0usize;
                while n < bytes.len() && bytes[n] == bytes[0] {
                    n += 1;
                }
                // Info string after backticks must not contain more backticks
                // for a valid open fence; we only need a conservative filter
                // for route extraction, so any run of ≥3 counts.
                if n >= 3 {
                    Some((ch, n))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((ch, n)) = fence {
            match open_fence {
                None => open_fence = Some((ch, n)),
                Some((open_ch, open_n)) if open_ch == ch && n >= open_n => {
                    open_fence = None;
                }
                Some(_) => {
                    // Wrong char or too-short closer — still inside the block.
                }
            }
            continue;
        }
        if open_fence.is_none() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Extract immutable profile UUIDs from a user prompt using only structured
/// forms the composer emits — not free-form prose or pasted examples.
///
/// Accepted:
/// - A whole line that *starts* with `Codeg mandatory delegation route:` and
///   contains `profile_id="<uuid>"` (composer directive; column 0 only)
/// - Closed markdown links: `[label](codeg://delegation-profile/<uuid>)`
///   or `[label](codeg://delegation-profile/<agent_type>/<uuid>)`
///
/// Rejected: bare URIs, prose `profile_id=`, unterminated/malformed links,
/// indented/buried directive phrases, and content inside fenced code blocks.
pub fn extract_mandatory_profile_ids(text: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let text = text_outside_code_fences(text);
    let mut out = BTreeSet::new();
    let uuid = |s: &str| -> Option<String> {
        let s = s.trim();
        if uuid::Uuid::parse_str(s).is_ok() {
            Some(s.to_string())
        } else {
            None
        }
    };

    const DIRECTIVE_PREFIX: &str = "Codeg mandatory delegation route:";
    for line in text.lines() {
        // Composer-injected directive lines only (column 0 — no leading spaces).
        let Some(mut rest) = line.strip_prefix(DIRECTIVE_PREFIX) else {
            continue;
        };
        while let Some(idx) = rest.find("profile_id=\"") {
            rest = &rest[idx + "profile_id=\"".len()..];
            if let Some(end) = rest.find('"') {
                if let Some(id) = uuid(&rest[..end]) {
                    out.insert(id);
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
    }

    // Markdown link destinations only: [label](codeg://delegation-profile/...)
    // Require a matching `[` before the `]` and a closing `)`.
    const LINK_PREFIX: &str = "](codeg://delegation-profile/";
    let lower = text.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(LINK_PREFIX) {
        let close_bracket = search_from + rel; // index of ']'
        search_from = close_bracket + LINK_PREFIX.len();
        // Valid markdown: a '[' before this ']' with no intervening ']'.
        let prefix = &text[..close_bracket];
        let Some(open) = prefix.rfind('[') else {
            continue;
        };
        if prefix[open + 1..].contains(']') {
            continue;
        }
        let after = &text[search_from..];
        // Require a real closing ')' — unterminated destinations are not links.
        let Some(token_end) = after.find(')') else {
            continue;
        };
        let token = after[..token_end].trim();
        let path = token.split([' ', '"', '\'']).next().unwrap_or("");
        let candidate = path.rsplit_once('/').map(|(_, id)| id).unwrap_or(path);
        if let Some(id) = uuid(candidate) {
            out.insert(id);
        }
    }
    out.into_iter().collect()
}

/// Broker-internal failure modes. Serialized via the wrapping
/// [`DelegationOutcome::Err`] variant — the broker maps each into a stable
/// `code` string so the frontend / MCP consumer can pattern-match without
/// caring about the inner shape.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum DelegationError {
    #[error("depth limit exceeded ({current_depth} >= {limit})")]
    DepthLimitExceeded { current_depth: u32, limit: u32 },
    #[error("invalid agent type")]
    InvalidAgentType,
    #[error("invalid delegation profile: {0}")]
    InvalidDelegationProfile(String),
    #[error("delegation profile is disabled: {0}")]
    DelegationProfileDisabled(String),
    #[error("delegation profile agent does not match request: {0}")]
    DelegationProfileAgentMismatch(String),
    /// Parent user prompt mentioned one or more profiles for this request's
    /// agent_type (`M_T`), but this call did not supply a usable `profile_id`
    /// (and auto-fill could not uniquely resolve), or supplied one outside `M_T`.
    /// The `{0}` payload is **detail-only** (no second "mandatory profile_id
    /// required" prefix); full wire text is this Display template + detail.
    #[error("mandatory profile_id required: {0}")]
    MandatoryProfileRequired(String),
    #[error("invalid working dir: {0}")]
    InvalidWorkingDir(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("subagent runtime error: {0}")]
    SubagentRuntimeError(String),
    /// Child agent ended its turn via `refusal`. Often a backend / gateway
    /// error masquerading as a refusal per the ACP spec gap.
    #[error("subagent refused to continue")]
    ChildRefusal,
    #[error("subagent reached max token budget")]
    ChildMaxTokens,
    #[error("subagent reached max turn request budget")]
    ChildMaxTurnRequests,
    /// Child reported `end_turn` without producing any output (synthesized
    /// as `empty` by the connection loop's "silent EndTurn" guard).
    #[error("subagent produced no output")]
    ChildEmpty,
    #[error("subagent ended with unrecognized stop reason: {0}")]
    ChildUnknown(String),
    #[error("canceled: {reason}")]
    Canceled { reason: String },
    #[error("parent session is gone")]
    ParentSessionGone,
    /// Same parent tool use id already bound under this parent with a
    /// different or missing request fingerprint.
    #[error("duplicate parent tool use: {0}")]
    DuplicateParentTool(String),
    #[error("delegation dispatch intent conflict: {0}")]
    DispatchIntentConflict(String),
    /// Concurrent gen-1 / continue insert lost the non-terminal fence.
    #[error("busy thread: {0}")]
    BusyThread(String),
    /// Unknown task id (or cross-parent without revealing existence).
    #[error("not found: {0}")]
    NotFound(String),
    /// Task exists on the child but is not the latest terminal run.
    #[error("stale task id: {0}")]
    StaleTaskId(String),
    /// Latest terminal run fails continue eligibility.
    #[error("not continuable: {0}")]
    NotContinuable(String),
    /// Continue requires an explicit parent tool binding under concurrent cards.
    #[error("missing parent tool use id")]
    MissingParentToolUseId,
    /// Agent type is not capability-enabled for session reuse (continue only).
    #[error("session reuse not supported for this agent type")]
    NotSupported,
    /// Stored launch snapshot / profile cannot be re-launched for continue.
    #[error("unresumable: {0}")]
    Unresumable(String),
    /// Replacement inputs failed server eligibility.
    #[error("invalid replacement: {0}")]
    InvalidReplacement(String),
    #[error("orchestration binding invalid: {0}")]
    OrchestrationBindingInvalid(String),
    #[error("orchestration binding lineage mismatch")]
    OrchestrationBindingLineageMismatch,
    /// Platform recovery rail refused the operation.
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    #[error("recovery confirmation required")]
    RecoveryConfirmationRequired(DelegationRecoveryProjection),
    #[error("recovery authorization rejected: {code}")]
    RecoveryAuthorizationRejected { code: String },
    /// Soft-delete of a provisional orphan child (fence/idempotent loser)
    /// failed after retry. Fail-closed: do not return busy/idempotent success
    /// while the no-run child may still be visible under the parent.
    #[error("provisional cleanup failed: {0}")]
    ProvisionalCleanupFailed(String),
    /// Step 1 of provisional compensation (atomic terminalization) failed.
    #[error("provisional terminalization failed: {0}")]
    ProvisionalTerminalizationFailed(String),
    /// Neither explicit host tool id nor a valid call `correlation_id` is
    /// available (includes present-but-malformed / over-length ids).
    #[error("{0}")]
    CorrelationMissing(String),
    /// A valid correlation key had no claimable ACP candidate within the poll budget.
    #[error("{0}")]
    CorrelationTimeout(String),
    /// More than one non-conflicted pending ACP call matched the exact key.
    #[error("{0}")]
    CorrelationAmbiguous(String),
    /// Pending key for the would-be match was conflict-tombstoned.
    #[error("{0}")]
    CorrelationConflict(String),
    /// Workflow graph admission rejected (B2/B6/A8.3/A14). Wire `code` is the
    /// structured admission code (e.g. `final_early`), not `spawn_failed`.
    #[error("workflow admission rejected ({code}): {message}")]
    WorkflowAdmission { code: String, message: String },
    #[error(
        "This workflow is archived and read-only. Create a new conversation and use a new Design."
    )]
    WorkflowV2Retired {
        navigation: WorkflowRetirementNavigation,
    },
}

/// The single value the broker hands back to the listener / MCP companion.
/// `child_conversation_id` on the `Err` arm is best-effort — it's `Some` once
/// the broker successfully created the child DB row, even if the run later
/// fails or times out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationOutcome {
    Ok(DelegationSuccess),
    Err {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        child_conversation_id: Option<i32>,
    },
}

/// Lifecycle status of an asynchronous delegation task. Surfaced by the
/// three delegation tools — `delegate_to_agent` (returns a `Running` ack, or
/// a terminal status when the child finished during setup / setup failed),
/// `get_delegation_status`, and `cancel_delegation`. Wire-stable snake_case
/// strings: they ship to LLM context and to the frontend, so don't rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Child is running in the background; no terminal result yet.
    Running,
    /// Child ended its turn cleanly; `text` carries the result (possibly
    /// truncated — open the child session for the full output).
    Completed,
    /// Child ended in a non-cancel failure; `error_code` / `message` describe it.
    Failed,
    /// Task was canceled (by `cancel_delegation`, parent teardown, or a
    /// non-`end_turn` parent turn end).
    Canceled,
    /// Task id is not known to this parent — never existed, belonged to a
    /// different parent, or its result was evicted from the cache and no DB
    /// row backs it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DelegationRecoveryProjection {
    pub disposition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_reason: Option<ReplacementReason>,
    pub cause_code: String,
    pub risk_class: String,
    pub authorization_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRetirementNavigation {
    pub source_conversation_id: Option<i32>,
    pub successor_conversation_id: Option<i32>,
    pub can_create_simple_successor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DelegationTaskReportExtension {
    Recovery {
        recovery: DelegationRecoveryProjection,
    },
    WorkflowRetirement {
        workflow_retirement: WorkflowRetirementNavigation,
    },
    IdempotentReplay {
        idempotent_replay: bool,
    },
}

impl<'de> Deserialize<'de> for DelegationRecoveryProjection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireProjection {
            disposition: String,
            proposed_action: Option<String>,
            replacement_reason: Option<String>,
            cause_code: String,
            risk_class: String,
            authorization_required: bool,
        }

        let wire = WireProjection::deserialize(deserializer)?;
        let replacement_reason = match wire.replacement_reason.as_deref() {
            Some(value) => Some(
                ReplacementReason::parse(value)
                    .ok_or_else(|| serde::de::Error::custom("invalid replacement_reason"))?,
            ),
            None => None,
        };
        Ok(Self {
            disposition: wire.disposition,
            proposed_action: wire.proposed_action,
            replacement_reason,
            cause_code: wire.cause_code,
            risk_class: wire.risk_class,
            authorization_required: wire.authorization_required,
        })
    }
}

impl From<&RecoveryDecision> for DelegationRecoveryProjection {
    fn from(decision: &RecoveryDecision) -> Self {
        fn stable_name<T: Serialize>(value: &T) -> String {
            serde_json::to_value(value)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string())
        }

        let authorization_required = decision.requires_authorization();
        let (disposition, proposed_action, replacement_reason) = match &decision.disposition {
            RecoveryDisposition::Continue { .. } => (
                if authorization_required {
                    "confirmation_required"
                } else {
                    "continue"
                },
                Some("continue".to_string()),
                None,
            ),
            RecoveryDisposition::FreshDispatch => (
                if authorization_required {
                    "confirmation_required"
                } else {
                    "fresh_dispatch"
                },
                Some("fresh_dispatch".to_string()),
                None,
            ),
            RecoveryDisposition::Replace { replacement_reason } => (
                if authorization_required {
                    "confirmation_required"
                } else {
                    "replace"
                },
                Some("replace".to_string()),
                Some(replacement_reason.clone()),
            ),
            RecoveryDisposition::Stop { code } => {
                return Self {
                    disposition: stable_name(code),
                    proposed_action: None,
                    replacement_reason: None,
                    cause_code: stable_name(&decision.cause_code),
                    risk_class: stable_name(&decision.risk_class),
                    authorization_required: false,
                };
            }
            RecoveryDisposition::InconsistentDurableState => {
                ("inconsistent_durable_state", None, None)
            }
        };
        Self {
            disposition: disposition.to_string(),
            proposed_action,
            replacement_reason,
            cause_code: stable_name(&decision.cause_code),
            risk_class: stable_name(&decision.risk_class),
            authorization_required,
        }
    }
}

/// Unified response the broker hands the listener for every delegation tool
/// (`delegate_to_agent` / `get_delegation_status` / `cancel_delegation`). The
/// listener serializes it into `BrokerResponse.outcome`; the companion renders
/// it into the MCP `CallToolResult` (with `structuredContent` carrying this
/// whole shape so the frontend can read `status` and distinguish a running ack
/// from a terminal outcome).
///
/// Fields are all optional except `status` so one type can describe a running
/// ack (ids + `Running`), a completed result (`text` + `duration_ms`), a
/// failure (`error_code` + `message`), and a setup failure (`task_id: None`).
///
/// Soft-watchdog fields (`observation`, `last_agent_activity_at`,
/// `stalled_since`) appear **only** on `Running` reports when the supervisor
/// has published a snapshot; terminal and unknown reports omit them on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationTaskReport {
    /// Broker `call_id` (UUID) identifying the task. `None` only when setup
    /// failed before a task was registered (no id to track).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Prior task id when this running acknowledgement reused an existing
    /// child session through `continue_delegation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_from_task_id: Option<String>,
    /// Present and true only for a successful continuation after the external
    /// session id was verified unchanged by resume/load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_session: Option<bool>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_conversation_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
    /// Completed result text (capped; open the child session for the full
    /// output). Only set for `Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Wire-stable error code for `Failed` / `Canceled` (mirrors
    /// `DelegationOutcome::Err.code`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Human-readable note: the failure message, or a hint like
    /// "running in background" / "result not cached; open child session N".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Soft-watchdog health. Present only on `Running` when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<TaskObservation>,
    /// Last child agent activity timestamp from the observation cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_activity_at: Option<DateTime<Utc>>,
    /// Stall start (`last_agent_activity_at + threshold`); only when stalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_since: Option<DateTime<Utc>>,
    #[serde(default, flatten, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DelegationTaskReportExtension>,
}

impl DelegationTaskReport {
    pub fn recovery_projection(&self) -> Option<&DelegationRecoveryProjection> {
        match self.recovery.as_ref() {
            Some(DelegationTaskReportExtension::Recovery { recovery }) => Some(recovery),
            _ => None,
        }
    }

    pub fn workflow_retirement(&self) -> Option<&WorkflowRetirementNavigation> {
        match self.recovery.as_ref() {
            Some(DelegationTaskReportExtension::WorkflowRetirement {
                workflow_retirement,
            }) => Some(workflow_retirement),
            _ => None,
        }
    }

    pub fn is_idempotent_replay(&self) -> bool {
        matches!(
            self.recovery.as_ref(),
            Some(DelegationTaskReportExtension::IdempotentReplay {
                idempotent_replay: true
            })
        )
    }
}

/// Caller-facing warning when a prior prompt may already have executed
/// (`admission_unknown`). Shared by cold failed reports and successful
/// `replacement_reason = admission_unknown` acknowledgements.
pub const ADMISSION_UNKNOWN_DUPLICATE_EXECUTION_WARNING: &str =
    "the prior prompt may already have executed — do not auto-continue; use explicit replacement if needed";

/// Shared cold-report message selection (wire shape unchanged).
///
/// Used by store (`PersistedTask::to_report`) and broker (`db_report`) so
/// cold status reads surface real terminal codes instead of a generic
/// cache-miss string for failed/canceled tasks.
pub fn cold_task_report_message(
    status: TaskStatus,
    error_code: Option<&str>,
    child_conversation_id: i32,
) -> Option<String> {
    match status {
        TaskStatus::Running => Some("Running.".into()),
        TaskStatus::Completed => Some(format!(
            "Result no longer cached; open child session {} for the full output.",
            child_conversation_id
        )),
        TaskStatus::Failed => {
            let code = error_code.unwrap_or("unknown");
            let detail = match code {
                "unresumable" => "the existing agent session could not be resumed safely",
                "admission_unknown" => ADMISSION_UNKNOWN_DUPLICATE_EXECUTION_WARNING,
                _ => "see child session for details",
            };
            Some(format!(
                "Delegation failed ({code}): {detail}. Open child session {child_conversation_id} for details."
            ))
        }
        TaskStatus::Canceled => {
            let code = error_code.unwrap_or("canceled");
            Some(format!(
                "Delegation canceled ({code}). Open child session {child_conversation_id} for details."
            ))
        }
        TaskStatus::Unknown => Some(
            "Unknown task id — it never existed, isn't owned by this session, \
             or its result was evicted with no stored record."
                .into(),
        ),
    }
}

#[cfg(test)]
mod cold_task_report_message_tests {
    use super::{cold_task_report_message, TaskStatus};

    #[test]
    fn cold_message_failed_includes_error_code_not_cache_miss() {
        let msg = cold_task_report_message(TaskStatus::Failed, Some("unresumable"), 1693).unwrap();
        assert!(msg.contains("unresumable"));
        assert!(msg.contains("1693"));
        assert!(!msg.contains("Result no longer cached"));
    }

    #[test]
    fn cold_message_failed_non_unresumable_uses_generic_phrase() {
        let msg = cold_task_report_message(TaskStatus::Failed, Some("host_restarted"), 9).unwrap();
        assert!(msg.contains("host_restarted"));
        assert!(!msg.contains("could not be resumed safely"));
        assert!(!msg.contains("Result no longer cached"));
    }

    #[test]
    fn cold_message_failed_admission_unknown_includes_duplicate_execution_warning() {
        use super::ADMISSION_UNKNOWN_DUPLICATE_EXECUTION_WARNING;
        let msg =
            cold_task_report_message(TaskStatus::Failed, Some("admission_unknown"), 42).unwrap();
        assert!(msg.contains("admission_unknown"));
        assert!(msg.contains(ADMISSION_UNKNOWN_DUPLICATE_EXECUTION_WARNING));
        assert!(msg.contains("42"));
        assert!(!msg.contains("Result no longer cached"));
        assert!(!msg.contains("could not be resumed safely"));
    }

    #[test]
    fn cold_message_completed_keeps_cache_miss() {
        let msg = cold_task_report_message(TaskStatus::Completed, None, 7).unwrap();
        assert!(msg.contains("Result no longer cached"));
    }
}

/// Opt-in Join wait mode for `get_delegation_status`. Absent `return_when` keeps
/// the legacy snapshot / supervised / any-terminal wait semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationReturnWhen {
    AllTerminalOrAttention,
}

/// Why a Join wait returned. Present on every Join-shaped batch; omitted on
/// legacy `{ "tasks": [...] }` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationWakeReason {
    AllTerminal,
    AttentionRequired,
    Unavailable,
}

/// Additive status batch for Join (and a legacy wrapper around task reports).
/// Legacy callers use [`Self::legacy`] so both Join-only fields stay `None` and
/// serialize as the exact historical `{ "tasks": [...] }` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationStatusBatch {
    pub tasks: Vec<DelegationTaskReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_reason: Option<DelegationWakeReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_requests: Option<Vec<crate::acp::delegation::attention::AttentionRequestSummary>>,
}

impl DelegationStatusBatch {
    pub fn legacy(tasks: Vec<DelegationTaskReport>) -> Self {
        Self {
            tasks,
            wake_reason: None,
            attention_requests: None,
        }
    }

    pub fn joined(
        tasks: Vec<DelegationTaskReport>,
        wake_reason: DelegationWakeReason,
        attention_requests: Vec<crate::acp::delegation::attention::AttentionRequestSummary>,
    ) -> Self {
        Self {
            tasks,
            wake_reason: Some(wake_reason),
            attention_requests: Some(attention_requests),
        }
    }
}

/// Why a parent turn or connection ended while Codeg children may still be live.
/// Wire-stable snake_case codes; do **not** fold these into generic
/// [`DelegationError::Canceled`] (`"canceled"`), which collapses all four cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentTurnEndReason {
    ParentCanceled,
    ParentTurnFailed,
    JoinAbandoned,
    ParentDisconnected,
}

impl ParentTurnEndReason {
    pub fn error_code(self) -> &'static str {
        match self {
            Self::ParentCanceled => "parent_canceled",
            Self::ParentTurnFailed => "parent_turn_failed",
            Self::JoinAbandoned => "join_abandoned",
            Self::ParentDisconnected => "parent_disconnected",
        }
    }

    pub fn attention_code(self) -> AttentionResolutionCode {
        match self {
            Self::ParentCanceled => AttentionResolutionCode::ParentCanceled,
            Self::ParentTurnFailed => AttentionResolutionCode::ParentTurnFailed,
            Self::JoinAbandoned => AttentionResolutionCode::JoinAbandoned,
            Self::ParentDisconnected => AttentionResolutionCode::ParentDisconnected,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::ParentCanceled => "parent turn was canceled",
            Self::ParentTurnFailed => "parent turn failed",
            Self::JoinAbandoned => "parent ended before joining live children",
            Self::ParentDisconnected => "parent connection disconnected",
        }
    }
}

/// Result of a child `request_parent_decision` wait (MCP surface later in Task 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ParentDecisionResult {
    Replied {
        request_id: String,
        reply: String,
    },
    Closed {
        request_id: String,
        resolution_code: AttentionResolutionCode,
    },
    Rejected {
        code: String,
        message: String,
    },
}

/// Result of a parent `reply_to_delegation` attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DelegationReplyResult {
    Replied {
        request_id: String,
    },
    Idempotent {
        request_id: String,
    },
    AlreadyResolved {
        request_id: String,
        resolution_code: AttentionResolutionCode,
    },
    Missing,
    Unauthorized,
    Rejected {
        code: String,
        message: String,
    },
}

/// Server-owned authority for one root conversation's completion mutations.
/// This type is deliberately not serializable: transport adapters construct it
/// only after authenticating the application and resolving the durable
/// attention owner from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMutationContext {
    parent_conversation_id: i32,
    actor_identity: String,
}

impl CompletionMutationContext {
    pub(crate) fn authenticated(
        parent_conversation_id: i32,
        actor_identity: impl Into<String>,
    ) -> Self {
        Self {
            parent_conversation_id,
            actor_identity: actor_identity.into(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn authenticated_for_test(
        parent_conversation_id: i32,
        actor_identity: impl Into<String>,
    ) -> Self {
        Self::authenticated(parent_conversation_id, actor_identity)
    }

    pub(crate) fn parent_conversation_id(&self) -> i32 {
        self.parent_conversation_id
    }

    pub(crate) fn actor_identity(&self) -> &str {
        &self.actor_identity
    }
}

/// Authenticated application mutation for a terminal workflow completion.
/// Actor identity is intentionally absent: desktop/server wrappers derive it
/// from the authenticated application transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveCompletionDecisionRequest {
    pub cas: CompletionAttentionCas,
    pub outcome: CompletionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryCompletionArtifactRequest {
    pub cas: CompletionAttentionCas,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveDesignSelfReviewRequest {
    pub cas: CompletionAttentionCas,
    pub outcome: CompletionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionMutationResult {
    pub workflow_id: String,
    pub task_id: String,
    pub node_id: String,
    pub kind: crate::db::entities::delegation_attention_request::AttentionKind,
    pub outcome: CompletionOutcome,
    pub evidence_scope_digest: String,
    pub graph_revision: u64,
    pub idempotent_replay: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<crate::acp::delegation::workflow::CompletionProjectionV2>,
}

impl DelegationOutcome {
    /// Project a `DelegationError` onto the wire-stable `code` string used by
    /// the frontend and MCP companion. Keep these strings stable — they ship
    /// to LLM context.
    pub fn from_err(err: DelegationError, child_conversation_id: Option<i32>) -> Self {
        // Workflow admission carries a structured code used by Skill/MCP clients;
        // keep it as the wire code (never fold into spawn_failed).
        if let DelegationError::WorkflowAdmission { code, message } = &err {
            return DelegationOutcome::Err {
                code: code.clone(),
                message: message.clone(),
                child_conversation_id,
            };
        }
        if let DelegationError::RecoveryAuthorizationRejected { code } = &err {
            return DelegationOutcome::Err {
                code: code.clone(),
                message: "recovery authorization rejected".to_string(),
                child_conversation_id,
            };
        }
        if let DelegationError::WorkflowV2Retired { .. } = &err {
            return DelegationOutcome::Err {
                code: "workflow_v2_retired".into(),
                message: crate::acp::delegation::workflow::WORKFLOW_V2_RETIRED_MESSAGE.into(),
                child_conversation_id,
            };
        }
        let code = match &err {
            DelegationError::DepthLimitExceeded { .. } => "depth_limit",
            DelegationError::InvalidAgentType => "invalid_agent_type",
            DelegationError::InvalidDelegationProfile(_) => "invalid_delegation_profile",
            DelegationError::DelegationProfileDisabled(_) => "delegation_profile_disabled",
            DelegationError::DelegationProfileAgentMismatch(_) => {
                "delegation_profile_agent_mismatch"
            }
            DelegationError::MandatoryProfileRequired(_) => "mandatory_profile_required",
            DelegationError::InvalidWorkingDir(_) => "invalid_working_dir",
            DelegationError::SpawnFailed(_) => "spawn_failed",
            DelegationError::SubagentRuntimeError(_) => "subagent_error",
            DelegationError::ChildRefusal => "child_refusal",
            DelegationError::ChildMaxTokens => "child_max_tokens",
            DelegationError::ChildMaxTurnRequests => "child_max_turn_requests",
            DelegationError::ChildEmpty => "child_empty",
            DelegationError::ChildUnknown(_) => "child_unknown",
            // Preserve known initiating-cause codes (UserStop / watchdog timeout)
            // so durable task reports and MCP structured errors can distinguish
            // them. Other cancel reasons keep the generic "canceled" code.
            DelegationError::Canceled { reason } => match reason.as_str() {
                "user_cancelled" => "user_cancelled",
                "tool_stalled_timeout" => "tool_stalled_timeout",
                _ => "canceled",
            },
            DelegationError::ParentSessionGone => "canceled",
            DelegationError::DuplicateParentTool(_) => "duplicate_parent_tool",
            DelegationError::DispatchIntentConflict(_) => "delegation_dispatch_intent_conflict",
            DelegationError::BusyThread(_) => "busy_thread",
            DelegationError::NotFound(_) => "not_found",
            DelegationError::StaleTaskId(_) => "stale_task_id",
            DelegationError::NotContinuable(_) => "not_continuable",
            DelegationError::MissingParentToolUseId => "missing_parent_tool_use_id",
            DelegationError::NotSupported => "not_supported",
            DelegationError::Unresumable(_) => "unresumable",
            DelegationError::InvalidReplacement(_) => "invalid_replacement",
            DelegationError::OrchestrationBindingInvalid(_) => "orchestration_binding_invalid",
            DelegationError::OrchestrationBindingLineageMismatch => {
                "orchestration_binding_lineage_mismatch"
            }
            DelegationError::BudgetExhausted(_) => "budget_exhausted",
            DelegationError::RecoveryConfirmationRequired(_) => "recovery_confirmation_required",
            // Handled above so the validated Task 8 rejection code is retained.
            DelegationError::RecoveryAuthorizationRejected { .. } => {
                "recovery_authorization_rejected"
            }
            DelegationError::ProvisionalCleanupFailed(_) => "provisional_cleanup_failed",
            DelegationError::ProvisionalTerminalizationFailed(_) => {
                "provisional_terminalization_failed"
            }
            DelegationError::CorrelationMissing(_) => "delegation_correlation_missing",
            DelegationError::CorrelationTimeout(_) => "delegation_correlation_timeout",
            DelegationError::CorrelationAmbiguous(_) => "delegation_correlation_ambiguous",
            DelegationError::CorrelationConflict(_) => "delegation_correlation_conflict",
            // Handled above; unreachable for exhaustive match.
            DelegationError::WorkflowAdmission { .. } => "workflow_admission_rejected",
            DelegationError::WorkflowV2Retired { .. } => "workflow_v2_retired",
        };
        DelegationOutcome::Err {
            code: code.to_string(),
            message: err.to_string(),
            child_conversation_id,
        }
    }
}

#[cfg(test)]
mod from_err_cause_code_tests {
    use super::{DelegationError, DelegationOutcome};

    #[test]
    fn canceled_user_stop_preserves_user_cancelled_code() {
        let out = DelegationOutcome::from_err(
            DelegationError::Canceled {
                reason: "user_cancelled".into(),
            },
            Some(7),
        );
        match out {
            DelegationOutcome::Err {
                code,
                child_conversation_id,
                ..
            } => {
                assert_eq!(code, "user_cancelled");
                assert_eq!(child_conversation_id, Some(7));
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn canceled_watchdog_timeout_preserves_tool_stalled_timeout_code() {
        let out = DelegationOutcome::from_err(
            DelegationError::Canceled {
                reason: "tool_stalled_timeout".into(),
            },
            Some(8),
        );
        match out {
            DelegationOutcome::Err { code, .. } => {
                assert_eq!(code, "tool_stalled_timeout");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn canceled_generic_reason_stays_canceled() {
        let out = DelegationOutcome::from_err(
            DelegationError::Canceled {
                reason: "user requested".into(),
            },
            None,
        );
        match out {
            DelegationOutcome::Err { code, .. } => {
                assert_eq!(code, "canceled");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn workflow_admission_wires_structured_code_not_spawn_failed() {
        let out = DelegationOutcome::from_err(
            DelegationError::WorkflowAdmission {
                code: "final_early".into(),
                message: "Task 1 gate not passed".into(),
            },
            None,
        );
        match out {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "final_early");
                assert_eq!(message, "Task 1 gate not passed");
                assert_ne!(code, "spawn_failed");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn correlation_and_provisional_wire_codes() {
        let cases: &[(DelegationError, &str)] = &[
            (
                DelegationError::CorrelationMissing("m".into()),
                "delegation_correlation_missing",
            ),
            (
                DelegationError::CorrelationTimeout("m".into()),
                "delegation_correlation_timeout",
            ),
            (
                DelegationError::CorrelationAmbiguous("m".into()),
                "delegation_correlation_ambiguous",
            ),
            (
                DelegationError::CorrelationConflict("m".into()),
                "delegation_correlation_conflict",
            ),
            (
                DelegationError::ProvisionalTerminalizationFailed("m".into()),
                "provisional_terminalization_failed",
            ),
            (
                DelegationError::ProvisionalCleanupFailed("m".into()),
                "provisional_cleanup_failed",
            ),
        ];
        for (err, want) in cases {
            match DelegationOutcome::from_err(err.clone(), None) {
                DelegationOutcome::Err { code, message, .. } => {
                    assert_eq!(code, *want, "message={message}");
                    assert_eq!(message, err.to_string());
                }
                other => panic!("expected Err for {want}, got {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod correlation_id_validation_tests {
    use super::validate_correlation_id;

    #[test]
    fn accepts_valid_ids() {
        for sample in [
            "a",
            "Z9",
            "uuid-like_value.with:chars",
            "A",
            "0abc",
            &"x".repeat(128),
        ] {
            assert!(
                validate_correlation_id(sample).is_ok(),
                "should accept {sample:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_space_leading_dot_overlength() {
        assert!(validate_correlation_id("").is_err());
        assert!(validate_correlation_id(" ").is_err());
        assert!(validate_correlation_id("has space").is_err());
        assert!(validate_correlation_id(".leading").is_err());
        assert!(validate_correlation_id(&"x".repeat(129)).is_err());
        assert!(validate_correlation_id("-dash-first").is_err());
        assert!(validate_correlation_id("_under-first").is_err());
    }
}

#[cfg(test)]
mod correlation_message_builder_tests {
    use super::{correlation_error_message, CorrelationEntryPoint, CorrelationFailureKind};

    #[test]
    fn shared_clauses_present_for_all_kinds_and_entry_points() {
        for kind in [
            CorrelationFailureKind::Missing,
            CorrelationFailureKind::Timeout,
            CorrelationFailureKind::Ambiguous,
            CorrelationFailureKind::Conflict,
        ] {
            for entry in [
                CorrelationEntryPoint::DelegateToAgent,
                CorrelationEntryPoint::ContinueDelegation,
            ] {
                let msg = correlation_error_message(kind, entry).to_ascii_lowercase();
                assert!(
                    msg.contains("not evaluated") || msg.contains("was not evaluated"),
                    "child-not-evaluated clause missing: {msg}"
                );
                assert!(msg.contains("unresumable"), "unresumable clause: {msg}");
                assert!(
                    msg.contains("not") && msg.contains("replacement"),
                    "no-replacement clause: {msg}"
                );
                assert!(
                    msg.contains("fresh") && msg.contains("correlation_id"),
                    "fresh correlation_id clause: {msg}"
                );
            }
        }
    }

    #[test]
    fn delegate_retry_names_delegate_to_agent_not_continue() {
        let msg = correlation_error_message(
            CorrelationFailureKind::Missing,
            CorrelationEntryPoint::DelegateToAgent,
        );
        let lower = msg.to_ascii_lowercase();
        assert!(
            lower.contains("delegate_to_agent"),
            "delegate retry text: {msg}"
        );
        assert!(
            !lower.contains("continue_delegation"),
            "delegate path must not name continue: {msg}"
        );
    }

    #[test]
    fn continue_retry_mentions_current_latest_terminal_and_status_reread() {
        let msg = correlation_error_message(
            CorrelationFailureKind::Timeout,
            CorrelationEntryPoint::ContinueDelegation,
        );
        let lower = msg.to_ascii_lowercase();
        assert!(
            lower.contains("continue_delegation"),
            "continue retry text: {msg}"
        );
        assert!(
            lower.contains("latest terminal") || lower.contains("current latest terminal"),
            "current latest terminal target: {msg}"
        );
        assert!(
            lower.contains("get_delegation_status"),
            "re-read via get_delegation_status: {msg}"
        );
        assert!(
            !lower.contains("delegate_to_agent"),
            "continue path must not name delegate: {msg}"
        );
    }
}

#[cfg(test)]
mod extract_tests {
    use super::extract_mandatory_profile_ids;

    #[test]
    fn extracts_profile_id_directive_and_uri_forms() {
        let a = "11111111-1111-4111-8111-111111111111";
        let b = "22222222-2222-4222-8222-222222222222";
        let noise = "33333333-3333-4333-8333-333333333333";
        let fenced = "44444444-4444-4444-8444-444444444444";
        let tilde = "55555555-5555-4555-8555-555555555555";
        let buried = "66666666-6666-4666-8666-666666666666";
        let open_link = "77777777-7777-4777-8777-777777777777";
        // Note: Rust `\` string continuations strip leading whitespace on the
        // next physical line, so leading-space cases must embed `\x20` explicitly.
        let text = format!(
            "Codeg mandatory delegation route: profile_id=\"{a}\" for @X\n\
also see [y](codeg://delegation-profile/code_buddy/{b})\n\
ignore bare codeg://delegation-profile/{noise}\n\
and prose profile_id=\"{noise}\" not on a directive line\n\
and malformed ](codeg://delegation-profile/{noise})\n\
and [broken] label](codeg://delegation-profile/{noise})\n\
and unterminated [open](codeg://delegation-profile/{open_link}\n\
see docs about Codeg mandatory delegation route profile_id=\"{buried}\" in prose\n\
\x20Codeg mandatory delegation route: profile_id=\"{buried}\" indented\n\
```\n\
[doc](codeg://delegation-profile/{fenced})\n\
Codeg mandatory delegation route: profile_id=\"{fenced}\"\n\
~~~\n\
still inside backtick fence: profile_id=\"{fenced}\"\n\
```\n\
~~~\n\
[tilde](codeg://delegation-profile/{tilde})\n\
~~~\n\
````\n\
```\n\
Codeg mandatory delegation route: profile_id=\"{fenced}\"\n\
```\n\
````\n"
        );
        let ids = extract_mandatory_profile_ids(&text);
        assert_eq!(ids, vec![a.to_string(), b.to_string()]);
    }
}
