//! Command DTOs. Bodies do not carry `principal_id`.

use serde::{Deserialize, Serialize};

use crate::model::{
    de_optional, EvidenceId, ManifestId, ObjectRefV1, OperationId, ProjectionId, RequestId,
    Revision, RoomId, RoundtableConfigV1, SafeInt, Seq, SubscriptionId,
};

fn default_false() -> bool {
    false
}

fn default_protocol_version() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub room_id: Option<RoomId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub revision: Option<Revision>,
    pub config: RoundtableConfigV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub request_id: RequestId,
    pub config: RoundtableConfigV1,
    /// Explicit workspace-relative files to freeze locally before confirmation.
    /// Omitted/empty means a topic-only room; never traverse the whole workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_source_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDraftRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    pub config: RoundtableConfigV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    /// A02 plan field. Absent is allowed; null is not.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub confirmed_preflight_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetRequest {
    pub room_id: RoomId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub read: Option<GetReadV1>,
}

/// A03 discriminated read. The object has exactly one variant key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum GetReadV1 {
    #[serde(rename = "projection")]
    Projection {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "de_optional"
        )]
        projection_id: Option<ProjectionId>,
    },
    #[serde(rename = "object")]
    Object {
        object_ref: ObjectRefV1,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "de_optional"
        )]
        cursor: Option<String>,
    },
    #[serde(rename = "usage")]
    Usage {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "de_optional"
        )]
        after_usage_version: Option<Revision>,
    },
    #[serde(rename = "metrics")]
    Metrics {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    pub workspace_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub cursor: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub limit: Option<SafeInt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub concurrency: Option<u32>,
    pub recovery_consent: bool,
    /// Bind each paid restart to the recipients and evidence just disclosed.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_optional")]
    pub confirmed_preflight_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    #[serde(default = "default_false")]
    pub force_latest: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterjectMode {
    NextPhase,
    RestartCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterjectRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    pub text: String,
    pub mode: InterjectMode,
    /// Bind each paid restart to the recipients and evidence just disclosed.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_optional")]
    pub confirmed_preflight_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySynthesisRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    /// Bind each paid restart to the recipients and evidence just disclosed.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_optional")]
    pub confirmed_preflight_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsRequest {
    pub room_id: RoomId,
    pub after_seq: Seq,
    pub through_seq: Seq,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesRequest {
    pub room_id: RoomId,
    pub manifest_id: ManifestId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRequest {
    pub room_id: RoomId,
    pub evidence_id: EvidenceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub room_id: RoomId,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    pub carry_published_context: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub config_override: Option<RoundtableConfigV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    pub subscription_id: SubscriptionId,
    pub room_id: RoomId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub since_seq: Option<Seq>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub projection_hash: Option<String>,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetachRequest {
    pub subscription_id: SubscriptionId,
    pub room_id: RoomId,
}
