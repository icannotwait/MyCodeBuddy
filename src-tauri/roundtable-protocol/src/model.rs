//! Closed protocol types for contract profile `roundtable_plan_1_2`.
//!
//! Profile numbers from the plan are suggestions. v1.1 hard limits stay in
//! [`v1_1`] and are not the creation-page preset.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical::map_serde;

pub const CONTRACT_PROFILE: &str = "roundtable_plan_1_2";
pub const SCHEMA_VERSION: u32 = 1;
/// Design §13.2 names no public code for this transport status.
pub const INACCESSIBLE_ROOM_HTTP_STATUS: u16 = 404;
pub const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
/// A03 read cursor lifetime. Not the preflight suggestion below.
pub const READ_CURSOR_TTL_SECONDS: u32 = 30 * 60;
pub const MAX_OBJECT_CHUNK_BYTES: u32 = 256 * 1024;
pub const MAX_OBJECT_CHUNK_ENVELOPE_BYTES: u32 = 1024 * 1024;

/// Plan §3 profile suggestions. Not v1.1 hard maxima and not measured speed.
pub mod profile_suggestions {
    pub const LABEL: &str = "suggestion_not_v1_1_hard_limit";
    pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
    pub const MAX_JSON_DEPTH: u32 = 32;
    pub const MAX_TOPIC_BYTES: usize = 16 * 1024;
    pub const MAX_ROLE_BYTES: usize = 8 * 1024;
    pub const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
    pub const MAX_SOURCE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
    pub const MAX_SOURCE_FILES: u32 = 1000;
    pub const SCRATCH_BYTES: u64 = 64 * 1024 * 1024;
    pub const LOG_BYTES_PER_BINDING: u64 = 8 * 1024 * 1024;
    pub const ROOM_IMMUTABLE_BYTES: u64 = 256 * 1024 * 1024;
    pub const DATA_DIR_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    pub const MAX_BUSINESS_EVENTS: u32 = 10_000;
    pub const RESERVED_TERMINAL_EVENTS: u32 = 128;
    pub const MAX_PARTICIPANTS: u32 = 16;
    pub const MAX_CRITIQUE_ROUNDS: u32 = 8;
    /// A02 wording: suggested confirmation lifetime, not a hard stop for a run.
    pub const PREFLIGHT_TTL_SECONDS: u32 = 30 * 60;
}

/// Design v1.1 limits that are normative, distinct from profile suggestions.
pub mod v1_1 {
    pub const MIN_PARTICIPANTS: u32 = 2;
    pub const MIN_CONCURRENCY: u32 = 1;
    pub const DEFAULT_RESULT_BYTES: u32 = 8 * 1024;
    pub const MAX_RESULT_BYTES: u32 = 64 * 1024;
    pub const DEFAULT_INTERJECTION_BYTES: u32 = 16 * 1024;
    pub const MAX_CLAIMS: usize = 20;
    pub const MAX_RESPONSES: usize = 30;
    pub const MAX_OPEN_QUESTIONS: usize = 20;
    pub const MAX_POSITION_CHANGES: usize = 20;
    pub const DEFAULT_PAGE_ITEMS: u32 = 100;
    pub const MAX_PAGE_ITEMS: u32 = 500;
    pub const MAX_PAGE_BYTES: u32 = 1024 * 1024;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitsOrigin {
    SuggestionNotV11HardLimit,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLimits {
    pub origin: LimitsOrigin,
    pub max_bytes: usize,
    pub max_depth: u32,
}

impl ParseLimits {
    pub fn suggested_profile() -> Self {
        Self {
            origin: LimitsOrigin::SuggestionNotV11HardLimit,
            max_bytes: profile_suggestions::MAX_REQUEST_BYTES,
            max_depth: profile_suggestions::MAX_JSON_DEPTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub origin: LimitsOrigin,
    pub max_participants: u32,
    pub max_critique_rounds: u32,
    pub max_topic_bytes: usize,
    pub max_role_bytes: usize,
    pub max_output_bytes: u32,
    pub max_source_files: u32,
    pub max_source_file_bytes: u64,
    pub max_source_total_bytes: u64,
}

impl ResourceLimits {
    pub fn suggested_profile() -> Self {
        Self {
            origin: LimitsOrigin::SuggestionNotV11HardLimit,
            max_participants: profile_suggestions::MAX_PARTICIPANTS,
            max_critique_rounds: profile_suggestions::MAX_CRITIQUE_ROUNDS,
            max_topic_bytes: profile_suggestions::MAX_TOPIC_BYTES,
            max_role_bytes: profile_suggestions::MAX_ROLE_BYTES,
            max_output_bytes: v1_1::MAX_RESULT_BYTES,
            max_source_files: profile_suggestions::MAX_SOURCE_FILES,
            max_source_file_bytes: profile_suggestions::MAX_SOURCE_FILE_BYTES,
            max_source_total_bytes: profile_suggestions::MAX_SOURCE_TOTAL_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdParseError;

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid id")
    }
}

impl std::error::Error for IdParseError {}

fn parse_canonical_uuid(text: &str) -> Result<uuid::Uuid, IdParseError> {
    if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(IdParseError);
    }
    let id = uuid::Uuid::parse_str(text).map_err(|_| IdParseError)?;
    if id.as_hyphenated().to_string() != text {
        return Err(IdParseError);
    }
    Ok(id)
}

macro_rules! uuid_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(uuid::Uuid);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0.as_hyphenated())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0.as_hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                parse_canonical_uuid(text).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(|_| de::Error::custom("invalid_id"))
            }
        }
    )+};
}

uuid_id!(
    RoomId,
    PrincipalId,
    ParticipantId,
    SpeakerId,
    PhaseId,
    TurnId,
    AttemptId,
    BindingId,
    IncarnationId,
    OperationId,
    RequestId,
    MessageId,
    ClaimId,
    ResponseId,
    EvidenceId,
    SnapshotId,
    ProjectionId,
    ManifestId,
    SubscriptionId,
);

/// Attempt-local 1..=64 ASCII letter, digit, `_`, or `-`. Not a UUID newtype.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubmissionId(String);

impl fmt::Debug for SubmissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SubmissionId({})", self.0)
    }
}

impl fmt::Display for SubmissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SubmissionId {
    type Err = IdParseError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let len = text.len();
        if (1..=64).contains(&len)
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            Ok(Self(text.to_string()))
        } else {
            Err(IdParseError)
        }
    }
}

impl Serialize for SubmissionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SubmissionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|_| de::Error::custom("invalid_id"))
    }
}

/// 32-byte SHA-256 value. Wire form is 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash256([u8; 32]);

impl fmt::Debug for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash256({})", self.to_hex())
    }
}

impl Hash256 {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn sha256(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    pub fn to_hex(self) -> String {
        crate::canonical::to_hex(&self.0)
    }
}

impl Serialize for Hash256 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_hash_hex(&text).ok_or_else(|| de::Error::custom("invalid_hash"))
    }
}

fn parse_hash_hex(text: &str) -> Option<Hash256> {
    if text.len() != 64
        || text
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(Hash256(bytes))
}

macro_rules! wire_u64 {
    ($($name:ident),+ $(,)?) => {$(
        /// Decimal string `0|[1-9][0-9]*` on the wire. Internal value is `u64`.
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                parse_wire_u64(&text)
                    .map(Self)
                    .ok_or_else(|| de::Error::custom("wire_u64"))
            }
        }
    )+};
}

wire_u64!(Seq, Revision, Epoch, DurationMs);

/// Monotonic sample. Not a wire type and not stored across processes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoMs(pub u64);

fn parse_wire_u64(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    text.parse::<u64>().ok()
}

/// JSON number in `0..=2^53-1`.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafeInt(pub u64);

impl fmt::Debug for SafeInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SafeInt({})", self.0)
    }
}

impl Serialize for SafeInt {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for SafeInt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value > MAX_SAFE_INTEGER {
            return Err(de::Error::custom("safe_integer"));
        }
        Ok(Self(value))
    }
}

pub fn de_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    match Option::<T>::deserialize(deserializer)? {
        Some(value) => Ok(Some(value)),
        None => Err(de::Error::custom("null_not_allowed")),
    }
}

/// Nullable field: the key must be present. JSON null becomes `None`.
pub fn de_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

fn default_one() -> u32 {
    1
}

fn default_output() -> SafeInt {
    SafeInt(u64::from(v1_1::DEFAULT_RESULT_BYTES))
}

fn default_interjection() -> SafeInt {
    SafeInt(u64::from(v1_1::DEFAULT_INTERJECTION_BYTES))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundtableConfigV1 {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub topic: String,
    /// Optional. Absent omits the key; JSON null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub display_name: Option<String>,
    pub workspace_id: String,
    pub source_refs: Vec<SourceRefV1>,
    pub participants: Vec<ParticipantV1>,
    pub moderator_ordinal: u32,
    pub strategy: StrategyV1,
    pub concurrency: u32,
    #[serde(default = "default_true")]
    pub strict_snapshot_v1: bool,
    pub budgets: BudgetsV1,
    pub timeouts: TimeoutsV1,
    pub quotas: QuotasV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRefV1 {
    pub snapshot_id: SnapshotId,
    /// Nullable background. The key is required; null and absent are different.
    #[serde(deserialize_with = "de_nullable")]
    pub base_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantV1 {
    pub ordinal: u32,
    pub role: String,
    pub provider_ref: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub effort: Option<String>,
    /// ACP agent for this seat. Absent means Codex, so older rooms keep working.
    /// `grok`, `cursor`, `antigravity`, and `code_buddy` select the other
    /// qualified adapters.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub agent: Option<String>,
}

/// Agents a roundtable seat may name. Absent is Codex.
pub fn is_roundtable_agent(agent: &str) -> bool {
    matches!(
        agent,
        "codex" | "grok" | "cursor" | "antigravity" | "code_buddy"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyType {
    PhasedRounds,
}

impl StrategyType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PhasedRounds => "phased_rounds",
        }
    }
}

impl<'de> Deserialize<'de> for StrategyType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        match text.as_str() {
            "phased_rounds" => Ok(Self::PhasedRounds),
            _ => Err(de::Error::custom("unknown_strategy")),
        }
    }
}

impl Serialize for StrategyType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyV1 {
    #[serde(rename = "type")]
    pub type_name: StrategyType,
    #[serde(default = "default_one")]
    pub version: u32,
    pub critique_rounds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetsV1 {
    pub room_budget: DurationMs,
    pub phase_budget: DurationMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutsV1 {
    pub attempt_timeout: DurationMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotasV1 {
    #[serde(default = "default_output")]
    pub output_byte_limit: SafeInt,
    pub input_byte_limit: SafeInt,
    #[serde(default = "default_interjection")]
    pub interjection_byte_limit: SafeInt,
}

pub fn validate_config(config: &RoundtableConfigV1, limits: &ResourceLimits) -> RtResult<()> {
    if config.schema_version != SCHEMA_VERSION {
        return Err(RtError::from_reason(InternalReason::SchemaVersion));
    }
    if config.topic.is_empty() || config.topic.len() > limits.max_topic_bytes {
        return Err(RtError::from_reason(InternalReason::Topic));
    }
    if config.workspace_id.is_empty() {
        return Err(RtError::from_reason(InternalReason::Workspace));
    }
    let count = u32::try_from(config.participants.len()).unwrap_or(u32::MAX);
    if count < v1_1::MIN_PARTICIPANTS {
        return Err(RtError::from_reason(InternalReason::ParticipantCount));
    }
    if count > limits.max_participants {
        return Err(RtError::from_reason(InternalReason::ParticipantLimit));
    }
    let mut ordinals: Vec<u32> = config
        .participants
        .iter()
        .map(|item| item.ordinal)
        .collect();
    ordinals.sort_unstable();
    if ordinals
        .iter()
        .enumerate()
        .any(|(index, ordinal)| *ordinal != index as u32)
    {
        return Err(RtError::from_reason(InternalReason::ParticipantOrdinal));
    }
    if config.moderator_ordinal >= count {
        return Err(RtError::from_reason(InternalReason::ModeratorOrdinal));
    }
    for participant in &config.participants {
        if participant.role.is_empty() || participant.role.len() > limits.max_role_bytes {
            return Err(RtError::from_reason(InternalReason::Role));
        }
        if participant.provider_ref.is_empty() {
            return Err(RtError::from_reason(InternalReason::ProviderRef));
        }
        if participant
            .agent
            .as_deref()
            .is_some_and(|agent| !is_roundtable_agent(agent))
        {
            return Err(RtError::from_reason(InternalReason::UnknownAgent));
        }
    }
    if config.strategy.version != 1 {
        return Err(RtError::from_reason(InternalReason::StrategyVersion));
    }
    if config.strategy.critique_rounds > limits.max_critique_rounds {
        return Err(RtError::from_reason(InternalReason::CritiqueRounds));
    }
    if config.concurrency < v1_1::MIN_CONCURRENCY || config.concurrency > count {
        return Err(RtError::from_reason(InternalReason::Concurrency));
    }
    if !config.strict_snapshot_v1 {
        return Err(RtError::from_reason(InternalReason::StrictSnapshot));
    }
    let output_cap = u64::from(limits.max_output_bytes.min(v1_1::MAX_RESULT_BYTES));
    if config.quotas.output_byte_limit.0 == 0 || config.quotas.output_byte_limit.0 > output_cap {
        return Err(RtError::from_reason(InternalReason::OutputQuota));
    }
    if config.quotas.input_byte_limit.0 == 0 || config.quotas.interjection_byte_limit.0 == 0 {
        return Err(RtError::from_reason(InternalReason::InputQuota));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    Draft,
    Ready,
    Running,
    Pausing,
    Paused,
    Recovering,
    Stopping,
    Stopped,
    Failed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    Ready,
    Running,
    Closing,
    Published,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Reserved,
    Launching,
    Admitting,
    Admitted,
    Streaming,
    Validating,
    Accepted,
    Invalid,
    Failed,
    TimedOut,
    Interrupted,
    Uncertain,
}

/// Independent of [`AttemptState`]. `not_dispatched` is not a public state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMark {
    Cancelling,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    SynthesisFailed,
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    Proposal,
    Critique,
    Synthesis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Pause,
    Stop,
    RestartCurrent,
    RetrySynthesis,
    Recover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStep {
    Requested,
    Revoking,
    Cleaning,
    Applying,
    Done,
    Blocked,
}

macro_rules! closed_enum {
    ($ty:ty, $($variant:ident => $text:literal),+ $(,)?) => {
        impl $ty {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }
    };
}

closed_enum!(
    RoomState,
    Draft => "draft",
    Ready => "ready",
    Running => "running",
    Pausing => "pausing",
    Paused => "paused",
    Recovering => "recovering",
    Stopping => "stopping",
    Stopped => "stopped",
    Failed => "failed",
    Completed => "completed",
);
closed_enum!(
    PhaseState,
    Ready => "ready",
    Running => "running",
    Closing => "closing",
    Published => "published",
    Failed => "failed",
    Superseded => "superseded",
);
closed_enum!(
    AttemptState,
    Reserved => "reserved",
    Launching => "launching",
    Admitting => "admitting",
    Admitted => "admitted",
    Streaming => "streaming",
    Validating => "validating",
    Accepted => "accepted",
    Invalid => "invalid",
    Failed => "failed",
    TimedOut => "timed_out",
    Interrupted => "interrupted",
    Uncertain => "uncertain",
);
closed_enum!(RuntimeMark, Cancelling => "cancelling", Cleanup => "cleanup");
closed_enum!(
    PhaseKind,
    Proposal => "proposal",
    Critique => "critique",
    Synthesis => "synthesis",
);
closed_enum!(
    ControlKind,
    Pause => "pause",
    Stop => "stop",
    RestartCurrent => "restart_current",
    RetrySynthesis => "retry_synthesis",
    Recover => "recover",
);
closed_enum!(
    ControlStep,
    Requested => "requested",
    Revoking => "revoking",
    Cleaning => "cleaning",
    Applying => "applying",
    Done => "done",
    Blocked => "blocked",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArgument,
    Unauthenticated,
    Forbidden,
    RevisionConflict,
    IdempotencyConflict,
    CommandInProgress,
    ControlInProgress,
    InvalidState,
    PolicyUnenforceable,
    CapabilityUnqualified,
    ContextTooLarge,
    CapacityUnknown,
    InsufficientBudget,
    CannotReachQuorum,
    NoNextPhase,
    CapacityLimited,
    StorageUnavailable,
    RuntimeUnavailable,
}

closed_enum!(
    ErrorCode,
    InvalidArgument => "invalid_argument",
    Unauthenticated => "unauthenticated",
    Forbidden => "forbidden",
    RevisionConflict => "revision_conflict",
    IdempotencyConflict => "idempotency_conflict",
    CommandInProgress => "command_in_progress",
    ControlInProgress => "control_in_progress",
    InvalidState => "invalid_state",
    PolicyUnenforceable => "policy_unenforceable",
    CapabilityUnqualified => "capability_unqualified",
    ContextTooLarge => "context_too_large",
    CapacityUnknown => "capacity_unknown",
    InsufficientBudget => "insufficient_budget",
    CannotReachQuorum => "cannot_reach_quorum",
    NoNextPhase => "no_next_phase",
    CapacityLimited => "capacity_limited",
    StorageUnavailable => "storage_unavailable",
    RuntimeUnavailable => "runtime_unavailable",
);

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ErrorCode {
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|code| code.as_str() == text)
    }

    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidArgument => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden => 403,
            Self::RevisionConflict
            | Self::IdempotencyConflict
            | Self::CommandInProgress
            | Self::ControlInProgress
            | Self::InvalidState => 409,
            Self::PolicyUnenforceable
            | Self::CapabilityUnqualified
            | Self::ContextTooLarge
            | Self::CapacityUnknown
            | Self::InsufficientBudget
            | Self::CannotReachQuorum
            | Self::NoNextPhase => 422,
            Self::CapacityLimited => 429,
            Self::StorageUnavailable | Self::RuntimeUnavailable => 503,
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::CommandInProgress
                | Self::ControlInProgress
                | Self::CapacityLimited
                | Self::StorageUnavailable
                | Self::RuntimeUnavailable
        )
    }
}

/// Internal diagnostics. These strings are `details.reason`, never public codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalReason {
    AttemptClosed,
    StaleFence,
    QueueRejected,
    SnapshotUnstable,
    ContextContractViolation,
    DuplicateKey,
    MaxDepth,
    NonFiniteNumber,
    FloatRejected,
    NegativeZero,
    UnknownField,
    UnknownStrategy,
    SafeInteger,
    NullNotAllowed,
    MissingField,
    WireU64,
    InvalidJson,
    InvalidNumber,
    LoneSurrogate,
    ByteLimit,
    InvalidUtf8,
    TrailingData,
    ParticipantCount,
    ParticipantLimit,
    ParticipantOrdinal,
    ModeratorOrdinal,
    Concurrency,
    CritiqueRounds,
    Topic,
    Workspace,
    Role,
    ProviderRef,
    StrategyVersion,
    StrictSnapshot,
    OutputQuota,
    InputQuota,
    SchemaVersion,
    ReconfirmationRequired,
    ResyncRequired,
    SubmissionConflict,
    ResultAlreadySealed,
    UnknownAgent,
}

impl InternalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AttemptClosed => "attempt_closed",
            Self::StaleFence => "stale_fence",
            Self::QueueRejected => "queue_rejected",
            Self::SnapshotUnstable => "snapshot_unstable",
            Self::ContextContractViolation => "context_contract_violation",
            Self::DuplicateKey => "duplicate_key",
            Self::MaxDepth => "max_depth",
            Self::NonFiniteNumber => "non_finite_number",
            Self::FloatRejected => "float_rejected",
            Self::NegativeZero => "negative_zero",
            Self::UnknownField => "unknown_field",
            Self::UnknownStrategy => "unknown_strategy",
            Self::SafeInteger => "safe_integer",
            Self::NullNotAllowed => "null_not_allowed",
            Self::MissingField => "missing_field",
            Self::WireU64 => "wire_u64",
            Self::InvalidJson => "invalid_json",
            Self::InvalidNumber => "invalid_number",
            Self::LoneSurrogate => "lone_surrogate",
            Self::ByteLimit => "byte_limit",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::TrailingData => "trailing_data",
            Self::ParticipantCount => "participant_count",
            Self::ParticipantLimit => "participant_limit",
            Self::ParticipantOrdinal => "participant_ordinal",
            Self::ModeratorOrdinal => "moderator_ordinal",
            Self::Concurrency => "concurrency",
            Self::CritiqueRounds => "critique_rounds",
            Self::Topic => "topic",
            Self::Workspace => "workspace",
            Self::Role => "role",
            Self::ProviderRef => "provider_ref",
            Self::StrategyVersion => "strategy_version",
            Self::StrictSnapshot => "strict_snapshot",
            Self::OutputQuota => "output_quota",
            Self::InputQuota => "input_quota",
            Self::SchemaVersion => "schema_version",
            Self::ReconfirmationRequired => "reconfirmation_required",
            Self::ResyncRequired => "resync_required",
            Self::SubmissionConflict => "submission_conflict",
            Self::ResultAlreadySealed => "result_already_sealed",
            Self::UnknownAgent => "unknown_agent",
        }
    }

    pub fn public_code(self) -> ErrorCode {
        match self {
            Self::AttemptClosed
            | Self::StaleFence
            | Self::QueueRejected
            | Self::ResultAlreadySealed => ErrorCode::InvalidState,
            Self::ContextContractViolation => ErrorCode::CapabilityUnqualified,
            Self::ReconfirmationRequired | Self::ResyncRequired => ErrorCode::InvalidState,
            Self::SubmissionConflict => ErrorCode::IdempotencyConflict,
            _ => ErrorCode::InvalidArgument,
        }
    }

    fn public_message(self) -> &'static str {
        match self.public_code() {
            ErrorCode::InvalidState => "The room cannot accept this command.",
            ErrorCode::CapabilityUnqualified => "The capability is not qualified.",
            ErrorCode::IdempotencyConflict => {
                "The idempotency key was reused for a different request."
            }
            _ => "The request is invalid.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldError {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetails {
    /// Nullable diagnostic. The key is required; null and absent differ.
    #[serde(deserialize_with = "de_nullable")]
    pub reason: Option<String>,
    pub field_errors: Vec<FieldError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(deny_unknown_fields)]
pub struct RtError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    /// Nullable. JSON null means no revision; omitting the key is rejected.
    #[serde(deserialize_with = "de_nullable")]
    pub current_revision: Option<Revision>,
    pub details: ErrorDetails,
}

pub type RtResult<T> = Result<T, RtError>;

impl RtError {
    pub fn from_reason(reason: InternalReason) -> Self {
        Self::with_field_errors(reason, Vec::new())
    }

    /// Duplicate-key failure. `path` is the JSONPath of the repeated key.
    pub fn duplicate_key(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::with_field_errors(
            InternalReason::DuplicateKey,
            vec![FieldError {
                path,
                reason: InternalReason::DuplicateKey.as_str().to_string(),
            }],
        )
    }

    fn with_field_errors(reason: InternalReason, field_errors: Vec<FieldError>) -> Self {
        let code = reason.public_code();
        Self {
            code,
            message: reason.public_message().to_string(),
            retryable: code.retryable(),
            current_revision: None,
            details: ErrorDetails {
                reason: Some(reason.as_str().to_string()),
                field_errors,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Mutation,
    Read,
}

pub struct CommandNameV1;

impl CommandNameV1 {
    pub const ALL: [&'static str; 18] = [
        "roundtable_preflight",
        "roundtable_create",
        "roundtable_update_draft",
        "roundtable_start",
        "roundtable_get",
        "roundtable_list",
        "roundtable_pause",
        "roundtable_resume",
        "roundtable_stop",
        "roundtable_interject",
        "roundtable_retry_synthesis",
        "roundtable_events",
        "roundtable_messages",
        "roundtable_evidence",
        "roundtable_operation",
        "roundtable_clone",
        "roundtable_attach",
        "roundtable_detach",
    ];

    /// Parallel to [`Self::ALL`]. Not a second name list.
    pub const CLASS: [CommandClass; 18] = [
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Read,
        CommandClass::Read,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Mutation,
        CommandClass::Read,
        CommandClass::Read,
        CommandClass::Read,
        CommandClass::Read,
        CommandClass::Mutation,
        CommandClass::Read,
        CommandClass::Read,
    ];
}

fn classified(class: CommandClass) -> &'static [&'static str] {
    static MUTATIONS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static READS: OnceLock<Vec<&'static str>> = OnceLock::new();
    let slot = if class == CommandClass::Mutation {
        &MUTATIONS
    } else {
        &READS
    };
    slot.get_or_init(|| {
        CommandNameV1::ALL
            .iter()
            .zip(CommandNameV1::CLASS)
            .filter_map(|(name, item)| (item == class).then_some(*name))
            .collect()
    })
    .as_slice()
}

pub fn mutation_methods() -> &'static [&'static str] {
    classified(CommandClass::Mutation)
}

pub fn read_methods() -> &'static [&'static str] {
    classified(CommandClass::Read)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorScope {
    SingleOperator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub kind: ClientKind,
    pub session_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Desktop,
    Web,
}

/// Trusted entry only. Request bodies have no principal field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorContext {
    principal_id: PrincipalId,
    scope: OperatorScope,
    client: ClientIdentity,
}

impl ActorContext {
    pub fn from_trusted_entry(
        principal_id: PrincipalId,
        scope: OperatorScope,
        client: ClientIdentity,
    ) -> Self {
        Self {
            principal_id,
            scope,
            client,
        }
    }

    pub fn principal_id(&self) -> PrincipalId {
        self.principal_id
    }

    pub fn scope(&self) -> OperatorScope {
        self.scope
    }

    pub fn client(&self) -> &ClientIdentity {
        &self.client
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub boot_epoch: Epoch,
    pub run_epoch: Epoch,
    pub phase_id: PhaseId,
    pub phase_revision: Revision,
    pub attempt_id: AttemptId,
    pub binding_id: BindingId,
    pub incarnation: IncarnationId,
    pub context_hash: Hash256,
    pub policy_hash: Hash256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceOwner {
    pub room_id: RoomId,
    pub attempt_id: AttemptId,
    pub boot_epoch: Epoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTreeProof {
    pub instance_id: String,
    pub incarnation: IncarnationId,
    pub process_tree_empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupProof {
    pub process: ProcessTreeProof,
    pub mailbox_empty: bool,
    pub tools_drained: bool,
    pub ingress_drained: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermitReleaseProof {
    pub lease_id: String,
    pub proofs: BTreeMap<IncarnationId, CleanupProof>,
}

impl PermitReleaseProof {
    pub fn releases(&self, lease_incarnations: &[IncarnationId]) -> bool {
        if self.proofs.len() != lease_incarnations.len() {
            return false;
        }
        lease_incarnations.iter().all(|id| {
            self.proofs.get(id).is_some_and(|proof| {
                proof.process.incarnation == *id
                    && proof.process.process_tree_empty
                    && proof.mailbox_empty
                    && proof.tools_drained
                    && proof.ingress_drained
            })
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationStatus {
    NotTested,
    Passed,
    Failed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationCertificateV1 {
    pub status: QualificationStatus,
    pub keys: Vec<String>,
    /// Nullable. A missing report is explicit null, not an omitted key.
    #[serde(deserialize_with = "de_nullable")]
    pub report_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityPolicyV1 {
    pub max_output_bytes: SafeInt,
    pub max_input_bytes: SafeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPolicyV1 {
    pub service_owned: bool,
    pub interactive_permissions_deny: bool,
    pub host_fs: bool,
    pub host_terminal: bool,
    pub companion_groups: Vec<String>,
    pub ordinary_conversation_import: bool,
    pub automatic_title: bool,
}

/// Certificate plus capacity and launch policy. Not constructed from a brand name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifiedProfile {
    pub certificate: QualificationCertificateV1,
    pub capacity: CapacityPolicyV1,
    pub launch: LaunchPolicyV1,
}

impl QualifiedProfile {
    pub fn from_parts(
        certificate: QualificationCertificateV1,
        capacity: CapacityPolicyV1,
        launch: LaunchPolicyV1,
    ) -> Self {
        Self {
            certificate,
            capacity,
            launch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingIntent {
    pub phase_id: PhaseId,
    pub speaker_id: SpeakerId,
    pub ordinal: u32,
    pub is_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolBarrierV1 {
    pub drained: bool,
}

/// Not an accepted turn. A09 outcome fields are not part of this frozen wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTurnCompleted {
    pub fence: Fence,
    pub finish_reason: String,
    pub ingress_watermark: Seq,
    pub tool_barrier: ToolBarrierV1,
    /// Optional. Omitted when absent; JSON null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub candidate_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateState {
    Staged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReceipt {
    pub submission_id: SubmissionId,
    pub payload_hash: Hash256,
    pub candidate_id: String,
    pub state: CandidateState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationAck {
    pub request_id: RequestId,
    pub accepted: bool,
    /// Nullable. Null means accepted without an operation id.
    #[serde(deserialize_with = "de_nullable")]
    pub operation_id: Option<OperationId>,
    pub room_id: RoomId,
    pub revision: Revision,
    pub run_epoch: Epoch,
    pub last_seq: Seq,
    pub status: RoomState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlOperationV1 {
    pub operation_id: OperationId,
    pub kind: ControlKind,
    pub step: ControlStep,
    pub room_id: RoomId,
    pub target_phase_id: PhaseId,
    pub target_revision: Revision,
    pub run_epoch: Epoch,
    /// Nullable. Null means this step has no successor phase.
    #[serde(deserialize_with = "de_nullable")]
    pub successor_phase_id: Option<PhaseId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Proposal,
    Critique,
    Abstain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    Support,
    Challenge,
    Clarify,
    Revise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Normal,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimV1 {
    pub local_key: String,
    pub text: String,
    pub evidence_aliases: Vec<String>,
    pub confidence: Confidence,
}

impl ClaimV1 {
    pub fn basic(local_key: String) -> Self {
        Self {
            local_key,
            text: "claim".to_string(),
            evidence_aliases: Vec::new(),
            confidence: Confidence::Low,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseV1 {
    /// Optional. Omitted when absent; JSON null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub target_claim_alias: Option<String>,
    /// Optional. Omitted when absent; JSON null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub target_response_alias: Option<String>,
    pub stance: Stance,
    pub priority: Priority,
    pub text: String,
    pub evidence_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionChangeV1 {
    pub own_prior_claim_alias: String,
    pub new_local_claim_key: String,
    pub reason: String,
    pub trigger_response_aliases: Vec<String>,
}

/// Model result. Identity ids are not fields the model fills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberResultV1 {
    pub kind: MemberKind,
    pub summary: String,
    pub claims: Vec<ClaimV1>,
    #[serde(default)]
    pub responses: Vec<ResponseV1>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub position_changes: Vec<PositionChangeV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub reason: Option<String>,
}

impl MemberResultV1 {
    pub fn proposal_skeleton() -> Self {
        Self {
            kind: MemberKind::Proposal,
            summary: String::new(),
            claims: Vec::new(),
            responses: Vec::new(),
            open_questions: Vec::new(),
            position_changes: Vec::new(),
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasKind {
    Message,
    Claim,
    Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasRefV1 {
    pub kind: AliasKind,
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConclusionV1 {
    pub text: String,
    pub aliases: Vec<AliasRefV1>,
    pub inference: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgreementLevel {
    ExplicitAgreement,
    CompatiblePositions,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsensusItemV1 {
    pub text: String,
    pub agreement_level: AgreementLevel,
    pub aliases: Vec<AliasRefV1>,
    pub supporter_aliases: Vec<String>,
    pub support_response_aliases: Vec<String>,
    pub inference: bool,
}

/// Service-owned coverage. Nullable on the moderator result; the model does not fill it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCoverageV1 {
    pub succeeded: u32,
    pub absent: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeratorKind {
    Synthesis,
}

/// Stored moderator result. `speaker_id` is host-assigned and non-empty.
/// `coverage` is nullable and explicit on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeratorResultV1 {
    pub kind: ModeratorKind,
    pub speaker_id: SpeakerId,
    pub recommendation: ConclusionV1,
    pub alternatives: Vec<ConclusionV1>,
    pub consensus_items: Vec<ConsensusItemV1>,
    pub disagreements: Vec<ConclusionV1>,
    pub risks: Vec<ConclusionV1>,
    pub decision_requests: Vec<ConclusionV1>,
    #[serde(deserialize_with = "de_nullable")]
    pub coverage: Option<ServiceCoverageV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseRefV1 {
    pub phase_id: PhaseId,
    pub index: u32,
    pub revision: Revision,
    pub kind: PhaseKind,
    pub state: PhaseState,
}

/// A04 internal timing ledger. Prepaid ticks do not emit a business event.
/// Monotonic fields are skipped so they cannot cross a process or the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeLedger {
    pub ledger_seq: Seq,
    pub remaining_room_ms: DurationMs,
    pub remaining_phase_ms: DurationMs,
    #[serde(skip)]
    pub prepaid_until: MonoMs,
    #[serde(skip)]
    pub last_sample_mono: MonoMs,
}

/// Body hashed by [`ProjectionRef`]. The projection hash is not a field here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBodyV1 {
    pub schema_version: u32,
    pub room_id: RoomId,
    pub revision: Revision,
    pub run_epoch: Epoch,
    pub last_seq: Seq,
    pub status: RoomState,
    #[serde(deserialize_with = "de_nullable")]
    pub blocked_reason: Option<BlockedReason>,
    pub config_hash: Hash256,
    #[serde(deserialize_with = "de_nullable")]
    pub moderator_speaker_id: Option<SpeakerId>,
    pub phase_refs: Vec<PhaseRefV1>,
    /// Ordered immutable membership at this watermark, including staged results.
    pub messages: Vec<PublishedMessageRef>,
    pub evidence_manifests: Vec<ManifestId>,
    pub replay: ProjectionReplayV1,
    /// Persistent budget sample at this business commit. Not a start authorization.
    pub ledger_seq: Seq,
    pub sampled_active_ms: DurationMs,
    pub sampled_at_utc: String,
}

/// Durable business state only: no process handles, credentials or tool tokens.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionReplayV1 {
    #[serde(deserialize_with = "de_nullable")]
    pub config: Option<RoundtableConfigV1>,
    pub boot_epoch: Epoch,
    #[serde(deserialize_with = "de_nullable")]
    pub current_phase_id: Option<PhaseId>,
    #[serde(deserialize_with = "de_nullable")]
    pub active_control_id: Option<OperationId>,
    #[serde(deserialize_with = "de_nullable")]
    pub result_quality: Option<String>,
    pub speakers: Vec<ProjectionSpeakerV1>,
    pub turns: Vec<ProjectionTurnV1>,
    pub attempts: Vec<ProjectionAttemptV1>,
    pub bindings: Vec<ProjectionBindingV1>,
    pub control_operations: Vec<ProjectionControlV1>,
    pub inputs: Vec<ProjectionInputV1>,
    pub budget: ProjectionBudgetV1,
    #[serde(deserialize_with = "de_nullable")]
    pub coverage: Option<crate::CoverageV1>,
    pub message_memberships: Vec<MessageMembershipV1>,
    pub evidence: Vec<ProjectionEvidenceV1>,
    pub source_manifests: Vec<SourceManifestRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSpeakerV1 {
    pub speaker_id: SpeakerId,
    pub ordinal: u32,
    pub role: String,
    pub provider_ref: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionTurnV1 {
    pub turn_id: TurnId,
    pub phase_id: PhaseId,
    pub speaker_id: SpeakerId,
    pub status: String,
    #[serde(deserialize_with = "de_nullable")]
    pub accepted_attempt_id: Option<AttemptId>,
    pub admitted_attempt_count: SafeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionAttemptV1 {
    pub attempt_id: AttemptId,
    pub turn_id: TurnId,
    pub attempt_no: u32,
    pub binding_id: BindingId,
    pub state: String,
    pub dispatch_state: String,
    pub cleanup_state: String,
    pub residual_remote_work: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBindingV1 {
    pub binding_id: BindingId,
    pub speaker_id: SpeakerId,
    pub generation: Revision,
    pub state: String,
    pub context_state: String,
    #[serde(deserialize_with = "de_nullable")]
    pub retire_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionControlV1 {
    pub operation_id: OperationId,
    pub kind: ControlKind,
    pub step: ControlStep,
    #[serde(deserialize_with = "de_nullable")]
    pub target_phase_id: Option<PhaseId>,
    #[serde(deserialize_with = "de_nullable")]
    pub target_revision: Option<Revision>,
    pub requested_epoch: Epoch,
    pub status: String,
    #[serde(deserialize_with = "de_nullable")]
    pub blocked_reason: Option<String>,
    #[serde(deserialize_with = "de_nullable")]
    pub successor_phase_id: Option<PhaseId>,
    #[serde(deserialize_with = "de_nullable")]
    pub superseded_by: Option<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionInputV1 {
    pub input_id: String,
    pub text: String,
    pub mode: String,
    pub accepted_seq: Seq,
    pub target_phase_index: u32,
    #[serde(deserialize_with = "de_nullable")]
    pub applied_phase_id: Option<PhaseId>,
    #[serde(deserialize_with = "de_nullable")]
    pub applied_seq: Option<Seq>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBudgetV1 {
    pub remaining_active_ms: DurationMs,
    pub admitted_attempts: SafeInt,
    pub reservations: Vec<ProjectionReservationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionReservationV1 {
    pub reservation_id: String,
    pub purpose: String,
    pub amount_ms: DurationMs,
    pub amount_bytes: SafeInt,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageVisibility {
    Staged,
    Published,
    Void,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageMembershipV1 {
    pub message_id: MessageId,
    pub membership_version: Revision,
    pub visibility: MessageVisibility,
    #[serde(deserialize_with = "de_nullable")]
    pub published_seq: Option<Seq>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionEvidenceV1 {
    pub evidence_id: EvidenceId,
    pub manifest_id: ManifestId,
    pub owner_speaker_id: SpeakerId,
    pub content_hash: Hash256,
    /// Canonical evidence metadata and excerpt at the fixed projection.
    pub body_hash: Hash256,
    #[serde(deserialize_with = "de_nullable")]
    pub published_seq: Option<Seq>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestRefV1 {
    pub manifest_id: ManifestId,
    pub hash: Hash256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRef {
    pub id: ProjectionId,
    pub hash: Hash256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionV1 {
    pub projection_ref: ProjectionRef,
    pub body: ProjectionBodyV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedMessageRef {
    pub message_id: MessageId,
    pub hash: Hash256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderedMemberV1 {
    pub ordinal: u32,
    pub speaker_id: SpeakerId,
    pub participant_id: ParticipantId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MandatoryTargetV1 {
    pub speaker_id: SpeakerId,
    pub claim_id: ClaimId,
    /// Nullable. Null targets the claim without a response id.
    #[serde(deserialize_with = "de_nullable")]
    pub response_id: Option<ResponseId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolQuotaV1 {
    pub per_call_bytes: SafeInt,
    pub per_attempt_bytes: SafeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseSnapshotV1 {
    pub schema_version: u32,
    pub phase_id: PhaseId,
    pub phase_index: u32,
    pub revision: Revision,
    pub kind: PhaseKind,
    /// Optional. Present only for a critique round; null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub critique_round: Option<u32>,
    pub config_version: Revision,
    pub question_version: Revision,
    pub interjection_version: Revision,
    pub source_manifest_id: ManifestId,
    pub source_manifest_hash: Hash256,
    pub published_messages: Vec<PublishedMessageRef>,
    pub members: Vec<OrderedMemberV1>,
    pub mandatory_targets: Vec<MandatoryTargetV1>,
    pub policy_hash: Hash256,
    pub output_byte_limit: SafeInt,
    pub tool_quota: ToolQuotaV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryManifestV1 {
    pub schema_version: u32,
    pub public_view_hash: Hash256,
    pub role_hash: Hash256,
    pub prompt_version: String,
    pub template_version: String,
    pub schema_id: String,
    pub tool_version: String,
    pub output_byte_limit: SafeInt,
    pub model: String,
    pub effort: String,
    pub provider_ref: String,
    pub binding_id: BindingId,
    /// Nullable. Null means this delivery has no prior cursor.
    #[serde(deserialize_with = "de_nullable")]
    pub prior_cursor: Option<String>,
    pub prompt_hash: Hash256,
    pub prompt_bytes: SafeInt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextFreshness {
    Fresh,
    Known,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextStateV1 {
    pub freshness: ContextFreshness,
    pub delivered_prompt_bytes: SafeInt,
    pub tool_return_bytes: SafeInt,
    /// Nullable. Null means the hidden limit is unknown, not omitted.
    #[serde(deserialize_with = "de_nullable")]
    pub cli_hidden_context_limit: Option<SafeInt>,
    pub compression_signal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageManifestV1 {
    pub manifest_id: ManifestId,
    pub entries: Vec<PublishedMessageRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Projection,
    Message,
    Manifest,
    Diagnostic,
    SourceExcerpt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRefV1 {
    pub object_id: String,
    pub kind: ObjectKind,
    pub content_hash: Hash256,
    pub total_bytes: SafeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableEnvelopeV1 {
    pub schema_version: u32,
    pub object_ref: ObjectRefV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewFrameV1 {
    pub subscription_id: SubscriptionId,
    pub room_id: RoomId,
    pub speaker_id: SpeakerId,
    pub attempt_id: AttemptId,
    pub incarnation: IncarnationId,
    pub run_epoch: Epoch,
    pub phase_revision: Revision,
    /// Optional preview cursor. Omitted when absent; null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub chunk_seq: Option<Seq>,
    /// Optional. Omitted when absent; null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub first_chunk_seq: Option<Seq>,
    /// Optional. Omitted when absent; null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub last_chunk_seq: Option<Seq>,
    /// Optional. Omitted when absent; null is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_optional"
    )]
    pub reset_baseline_seq: Option<Seq>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRecipientV1 {
    pub provider_ref: String,
    pub provider_config_version: String,
    pub endpoint_origin: String,
    pub model: String,
    pub effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRecordV1 {
    pub preflight_id: String,
    pub principal_id: PrincipalId,
    /// Nullable. A config estimate with no room keeps explicit null.
    #[serde(deserialize_with = "de_nullable")]
    pub room_id: Option<RoomId>,
    /// Nullable. Paired with `room_id`; absent is not null.
    #[serde(deserialize_with = "de_nullable")]
    pub revision: Option<Revision>,
    pub config_hash: Hash256,
    pub source_manifest_id: ManifestId,
    pub source_manifest_hash: Hash256,
    pub recipients: Vec<ResolvedRecipientV1>,
    pub policy_hash: Hash256,
    pub qualification_keys: Vec<String>,
    pub limits_hash: Hash256,
    pub created_at: String,
    pub expires_at: String,
    pub confirmable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedCountV1 {
    pub key: String,
    pub value: SafeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageViewV1 {
    pub usage_version: Revision,
    pub measurements: Vec<NamedCountV1>,
    pub totals: Vec<NamedCountV1>,
    pub unknown_count: SafeInt,
    #[serde(default)]
    pub confirmed_output_tokens: Option<SafeInt>,
    #[serde(default)]
    pub unknown_total: bool,
    #[serde(default)]
    pub uncertain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsViewV1 {
    pub uncertain_count: SafeInt,
    pub cleanup_overrun_ms: DurationMs,
    pub admission_rejections_by_reason: BTreeMap<String, SafeInt>,
    pub event_count: SafeInt,
    pub reserved_event_slots: SafeInt,
    pub storage_used_bytes: SafeInt,
    pub storage_reserved_bytes: SafeInt,
    pub unknown_measurements: SafeInt,
}

pub fn canonical_hash<T: Serialize>(value: &T) -> RtResult<Hash256> {
    Ok(Hash256::sha256(&crate::canonical::canonical_bytes(value)?))
}

pub fn decode_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    limits: &ParseLimits,
) -> RtResult<T> {
    let value = crate::canonical::parse_strict_json(bytes, limits)?;
    serde_json::from_value(value).map_err(map_serde)
}
