//! Qualification path for the shared tool core.
//!
//! The encoder and validator are the production P02 and P04 functions.
//! [`InMemoryToolStore`] is only this path. A fake broker is not a certificate.

use std::str::FromStr;
use std::sync::Mutex;

use roundtable_protocol::{
    CandidateReceipt, DeliveryEncoder, ErrorCode, Hash256, ManifestId, PhaseId, PhaseKind,
    PhaseSnapshotV1, QualificationStatus, QualifiedContextProfile, ResultScope, Revision,
    RoleSnapshot, RtResult, SafeInt, SpeakerId, SubmissionDecision, TokenBound, ToolQuotaV1,
    SCHEMA_VERSION,
};
use serde_json::{json, Value};

use super::companion::{plan_service_launch, FakeBrokerTransport, ServiceLaunchInput};
use super::rt_error;
use super::tool_core::{
    dispatch_tool, service_result_schema, service_tool_schema, AttemptToken, InMemoryToolStore,
    RoundtableToolCall, TokenBinding, TokenRegistry, SERVICE_RESULT_SCHEMA_ID,
    SERVICE_TOOL_VERSION,
};

pub struct QualificationHarness {
    phase: PhaseSnapshotV1,
    role: RoleSnapshot,
    binding_id: roundtable_protocol::BindingId,
    profile: QualifiedContextProfile,
    result_scope: ResultScope,
    token: AttemptToken,
    scope: super::tool_core::AdmittedToolScope,
    store: InMemoryToolStore,
    delivered: Mutex<Option<CandidateReceipt>>,
    transport: FakeBrokerTransport,
}

impl QualificationHarness {
    pub fn open(billing_credential: &str) -> Self {
        let speaker = id::<SpeakerId>(8);
        let profile = QualifiedContextProfile::proposed(
            "test-tokenizer",
            Hash256::from_bytes([0x44; 32]),
            2_000_000,
            0,
            "proof-p07c",
        );
        let fence = roundtable_protocol::Fence {
            boot_epoch: roundtable_protocol::Epoch(7),
            run_epoch: roundtable_protocol::Epoch(7),
            phase_id: id::<PhaseId>(1),
            phase_revision: Revision(1),
            attempt_id: id(2),
            binding_id: id(3),
            incarnation: id(4),
            context_hash: Hash256::from_bytes([0x11; 32]),
            policy_hash: Hash256::from_bytes([0x22; 32]),
        };
        let result_scope = ResultScope {
            phase_kind: PhaseKind::Proposal,
            speaker_id: speaker,
            aliases: roundtable_protocol::VisibleAliases::default(),
            mandatory_targets: Vec::new(),
            published: roundtable_protocol::PublishedHistory::default(),
            quota_bytes: 8_192,
        };
        let binding = TokenBinding {
            attempt_id: fence.attempt_id,
            room_id: id(5),
            fence: fence.clone(),
            speaker_id: speaker,
            tool_version: SERVICE_TOOL_VERSION.to_string(),
            aliases: result_scope.aliases.clone(),
            result_scope: result_scope.clone(),
            evidence: std::collections::BTreeMap::new(),
            profile: profile.clone(),
        };
        let phase = PhaseSnapshotV1 {
            schema_version: SCHEMA_VERSION,
            phase_id: fence.phase_id,
            phase_index: 0,
            revision: Revision(1),
            kind: PhaseKind::Proposal,
            critique_round: None,
            config_version: Revision(1),
            question_version: Revision(1),
            interjection_version: Revision(1),
            source_manifest_id: id::<ManifestId>(6),
            source_manifest_hash: Hash256::from_bytes([0x11; 32]),
            published_messages: Vec::new(),
            members: Vec::new(),
            mandatory_targets: Vec::new(),
            policy_hash: fence.policy_hash,
            output_byte_limit: SafeInt(1024),
            tool_quota: ToolQuotaV1 {
                per_call_bytes: SafeInt(8_192),
                per_attempt_bytes: SafeInt(32_768),
            },
        };
        let role = RoleSnapshot {
            role: "member".to_string(),
            model: "model-a".to_string(),
            effort: "low".to_string(),
            provider_ref: "provider:test".to_string(),
            prompt_version: "prompt-1".to_string(),
            template_version: "template-1".to_string(),
            schema_id: SERVICE_RESULT_SCHEMA_ID.to_string(),
            schema_text: service_result_schema().to_string(),
            tool_version: SERVICE_TOOL_VERSION.to_string(),
            tool_text: service_tool_schema().to_string(),
        };
        let registry = TokenRegistry::new();
        let token = registry.issue(binding.clone());
        let plan = plan_service_launch(&ServiceLaunchInput {
            token: &token,
            socket_path: "roundtable-qualification",
            incarnation: &fence.incarnation.to_string(),
            billing_credential,
        });
        assert!(
            !plan.log.contains(billing_credential)
                && !plan.prompt.contains(billing_credential)
                && !plan.request_url.contains(billing_credential)
                && !plan
                    .sandbox_env
                    .values()
                    .any(|value| value.contains(billing_credential)),
            "billing credential entered the sandbox plan"
        );
        let scope = registry
            .admit(token.reveal_for_same_sandbox(), &binding, "submit_result")
            .expect("qualification token admits its own attempt");
        let transport =
            FakeBrokerTransport::open("roundtable-qualification", fence.incarnation.to_string());
        Self {
            phase,
            role,
            binding_id: fence.binding_id,
            profile,
            result_scope,
            token,
            scope,
            store: InMemoryToolStore::new(),
            delivered: Mutex::new(None),
            transport,
        }
    }

    pub fn phase(&self) -> &PhaseSnapshotV1 {
        &self.phase
    }

    pub fn role(&self) -> &RoleSnapshot {
        &self.role
    }

    pub fn binding_id(&self) -> &roundtable_protocol::BindingId {
        &self.binding_id
    }

    pub fn profile(&self) -> &QualifiedContextProfile {
        &self.profile
    }

    pub fn result_scope(&self) -> &ResultScope {
        &self.result_scope
    }

    pub fn sandbox_visible_token(&self) -> &str {
        self.token.reveal_for_same_sandbox()
    }

    pub fn store_is_durable(&self) -> bool {
        false
    }

    pub fn delivery_manifest(
        &self,
        tokens: &dyn TokenBound,
    ) -> RtResult<roundtable_protocol::DeliveryManifestV1> {
        DeliveryEncoder::encode(
            &self.phase,
            &self.role,
            &self.binding_id,
            tokens,
            &self.profile,
        )
    }

    pub async fn submit_result(
        &self,
        submission_id: &str,
        result: &Value,
    ) -> RtResult<SubmissionDecision> {
        let response = dispatch_tool(
            &self.scope,
            RoundtableToolCall {
                name: "submit_result".to_string(),
                arguments: json!({
                    "submission_id": submission_id,
                    "result": result,
                }),
            },
            &self.store,
        )
        .await?;
        response
            .decision
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "decision_missing"))
    }

    pub fn tool_calls(&self) -> u32 {
        self.scope.tool_calls()
    }

    pub fn tool_reply_bytes(&self) -> u64 {
        self.scope.tool_reply_bytes()
    }

    pub fn sealed_receipt(&self) -> Option<CandidateReceipt> {
        self.scope.sealed_receipt()
    }

    pub fn deliver_receipt_to_cli(&self, receipt: &CandidateReceipt) -> RtResult<()> {
        match self.sealed_receipt() {
            Some(sealed) if &sealed == receipt => {
                *self.delivered.lock().expect("delivered") = Some(sealed);
                Ok(())
            }
            _ => Err(rt_error(ErrorCode::InvalidState, "receipt_not_at_cli")),
        }
    }

    pub fn allow_normal_completion(&self) -> RtResult<()> {
        let delivered = self.delivered.lock().expect("delivered").clone();
        match (delivered, self.sealed_receipt()) {
            (Some(delivered), Some(sealed)) if delivered == sealed => Ok(()),
            _ => Err(rt_error(ErrorCode::InvalidState, "receipt_not_at_cli")),
        }
    }

    pub fn close_fake_broker(&self) {
        self.transport.close();
    }

    pub fn certificate_status(&self) -> QualificationStatus {
        QualificationStatus::NotTested
    }

    pub fn fake_transport_is_certificate(&self) -> bool {
        self.transport.is_certificate()
    }
}

fn id<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("00000000-0000-4000-8000-{n:012x}")
        .parse()
        .expect("id")
}
