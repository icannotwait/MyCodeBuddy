//! Sealed launch manifest for one service member.
//!
//! Service mode starts from an empty capability set and then installs only the
//! roundtable allowlist. It does not read ordinary companion flags and delete
//! fields. A UI switch cannot hide a tool that is still in the set. The
//! canonical manifest hash joins qualification policy. Fake success is not a
//! certificate.

use std::collections::HashMap;
use std::sync::Mutex;

use roundtable_protocol::{canonical_bytes, ErrorCode, Fence, Hash256, RtResult, RuntimeTurnCompleted};
use serde::Serialize;
use serde_json::Value;

use super::companion::ATTEMPT_TOKEN_ENV;
use super::rt_error;
use super::tool_core::service_tool_names;

pub const MANIFEST_VERSION: &str = "launch_capability_manifest_v1";
pub const SERVICE_LAUNCHER: &str = "roundtable-service-launcher";
pub const SERVICE_ADAPTER: &str = "codex-acp@2.1.1";
pub const SERVICE_HELPER: &str = "codeg-mcp --service-roundtable";
pub const SERVICE_CONFIG: &str = "roundtable-service-config";
pub const SERVICE_MCP: &str = "roundtable";
pub const SERVICE_MOUNT: &str = "instance-socket";

/// Ordinary session toggles. Service launch reads this only to ignore it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrdinarySessionFlags {
    pub browser: bool,
    pub browser_eval: bool,
    pub computer: bool,
    pub computer_launch: bool,
    pub computer_clipboard: bool,
    pub delegation: bool,
    pub sessions: bool,
    pub ask: bool,
    pub feedback: bool,
    pub tasks: bool,
    pub automations: bool,
    pub taskboard: bool,
    pub unknown: usize,
}

impl OrdinarySessionFlags {
    pub fn from_names(names: &[&str]) -> Self {
        let mut flags = Self::default();
        for name in names {
            match *name {
                "browser" => flags.browser = true,
                "browser_eval" => flags.browser_eval = true,
                "computer" => flags.computer = true,
                "computer_launch" => flags.computer_launch = true,
                "computer_clipboard" => flags.computer_clipboard = true,
                "delegation" => flags.delegation = true,
                "sessions" => flags.sessions = true,
                "ask" => flags.ask = true,
                "feedback" => flags.feedback = true,
                "tasks" => flags.tasks = true,
                "automations" => flags.automations = true,
                "taskboard" => flags.taskboard = true,
                _ => flags.unknown = flags.unknown.saturating_add(1),
            }
        }
        flags
    }

    pub fn any_enabled(&self) -> bool {
        self.browser
            || self.browser_eval
            || self.computer
            || self.computer_launch
            || self.computer_clipboard
            || self.delegation
            || self.sessions
            || self.ask
            || self.feedback
            || self.tasks
            || self.automations
            || self.taskboard
            || self.unknown > 0
    }
}

/// Extra launch facts that must not widen the sealed set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestExtras {
    pub ordinary_companion_token: bool,
    pub legacy_feature_flags: Vec<String>,
    pub ui_hidden_tools: Vec<String>,
    pub global_config: bool,
}

/// Complete launch inventory. Names and destinations only; no secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchCapabilityManifestV1 {
    pub version: String,
    pub launcher: String,
    pub adapter: String,
    pub helper: String,
    pub config: String,
    pub environment_keys: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub tool_names: Vec<String>,
    pub native_fs: bool,
    pub native_shell: bool,
    pub native_subagent: bool,
    pub network_destinations: Vec<String>,
    pub mounts: Vec<String>,
    pub inherited_fds: Vec<String>,
}

impl LaunchCapabilityManifestV1 {
    pub fn canonical_hash(&self) -> RtResult<Hash256> {
        Ok(Hash256::sha256(&canonical_bytes(self)?))
    }
}

struct CapabilitySet {
    tools: Vec<String>,
    mcp_servers: Vec<String>,
    native_fs: bool,
    native_shell: bool,
    native_subagent: bool,
    network_destinations: Vec<String>,
    mounts: Vec<String>,
    inherited_fds: Vec<String>,
    environment_keys: Vec<String>,
}

impl CapabilitySet {
    fn empty() -> Self {
        Self {
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            native_fs: false,
            native_shell: false,
            native_subagent: false,
            network_destinations: Vec::new(),
            mounts: Vec::new(),
            inherited_fds: Vec::new(),
            environment_keys: Vec::new(),
        }
    }
}

/// Service inventory. Built from [`CapabilitySet::empty`], not from host flags.
pub fn sealed_service_manifest() -> LaunchCapabilityManifestV1 {
    let _empty = CapabilitySet::empty();
    LaunchCapabilityManifestV1 {
        version: MANIFEST_VERSION.to_string(),
        launcher: SERVICE_LAUNCHER.to_string(),
        adapter: SERVICE_ADAPTER.to_string(),
        helper: SERVICE_HELPER.to_string(),
        config: SERVICE_CONFIG.to_string(),
        environment_keys: vec![ATTEMPT_TOKEN_ENV.to_string()],
        mcp_servers: vec![SERVICE_MCP.to_string()],
        tool_names: service_tool_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        native_fs: false,
        native_shell: false,
        native_subagent: false,
        network_destinations: Vec::new(),
        mounts: vec![SERVICE_MOUNT.to_string()],
        inherited_fds: Vec::new(),
    }
}

/// Ordinary flags are accepted and ignored. They are not copied then stripped.
pub fn service_manifest_for_session(flags: &OrdinarySessionFlags) -> LaunchCapabilityManifestV1 {
    let _ignored = flags.any_enabled();
    let _empty = CapabilitySet::empty();
    sealed_service_manifest()
}

/// `HostToolsPolicy::Agent` withholds hosted fs/terminal and delegation only.
/// The agent's own shell and the remaining companion surface stay. That is not
/// the empty service set and cannot qualify.
pub fn host_tools_agent_manifest() -> LaunchCapabilityManifestV1 {
    debug_assert!(
        !crate::acp::host_tools_policy::HostToolsPolicy::Agent.hosts_channels(),
        "agent policy withholds hosted channels and still is not a service seal"
    );
    LaunchCapabilityManifestV1 {
        version: MANIFEST_VERSION.to_string(),
        launcher: "ordinary-acp".to_string(),
        adapter: SERVICE_ADAPTER.to_string(),
        helper: "codeg-mcp".to_string(),
        config: "host-tools-agent".to_string(),
        environment_keys: vec!["CODEG_ACP_HOST_TOOLS".to_string()],
        mcp_servers: vec!["codeg-mcp".to_string()],
        tool_names: vec![
            "check_user_feedback".to_string(),
            "ask_user_question".to_string(),
            "get_session_info".to_string(),
        ],
        native_fs: true,
        native_shell: true,
        native_subagent: false,
        network_destinations: vec!["https://api.openai.com".to_string()],
        mounts: vec!["host-home".to_string()],
        inherited_fds: vec!["stdin".to_string()],
    }
}

pub fn verify_service_manifest(
    manifest: &LaunchCapabilityManifestV1,
    extras: &ManifestExtras,
) -> RtResult<()> {
    if extras.ordinary_companion_token {
        return Err(rt_error(
            ErrorCode::Unauthenticated,
            "ordinary_companion_token",
        ));
    }
    if !extras.ui_hidden_tools.is_empty() {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "ui_switch_cannot_hide",
        ));
    }
    if !extras.legacy_feature_flags.is_empty() {
        return Err(rt_error(ErrorCode::Forbidden, "legacy_feature_flag"));
    }
    if extras.global_config || manifest.config != SERVICE_CONFIG {
        return Err(rt_error(ErrorCode::Forbidden, "global_config"));
    }
    if manifest.native_shell || !manifest.network_destinations.is_empty() {
        return Err(rt_error(ErrorCode::Forbidden, "native_shell_egress"));
    }
    let sealed = sealed_service_manifest();
    if manifest.mcp_servers != sealed.mcp_servers {
        return Err(rt_error(ErrorCode::Forbidden, "extra_mcp"));
    }
    if manifest.tool_names != sealed.tool_names
        || manifest.native_fs
        || manifest.native_subagent
        || manifest != &sealed
    {
        return Err(rt_error(ErrorCode::Forbidden, "unknown_capability"));
    }
    Ok(())
}

/// Host broker allowlist. Verification failure refuses even a known tool.
pub fn host_broker_call(
    manifest: &LaunchCapabilityManifestV1,
    extras: &ManifestExtras,
    tool: &str,
) -> RtResult<()> {
    verify_service_manifest(manifest, extras)?;
    if !service_tool_names().contains(&tool) || !manifest.tool_names.iter().any(|name| name == tool)
    {
        return Err(rt_error(ErrorCode::Forbidden, "tool_not_admitted"));
    }
    Ok(())
}

#[derive(Serialize)]
struct PolicyJoin<'a> {
    manifest_hash: &'a str,
    version: &'a str,
}

/// Version plus canonical manifest hash. This is the service policy hash.
pub fn policy_hash_for(manifest: &LaunchCapabilityManifestV1) -> RtResult<Hash256> {
    let manifest_hash = manifest.canonical_hash()?.to_hex();
    let bytes = canonical_bytes(&PolicyJoin {
        manifest_hash: &manifest_hash,
        version: manifest.version.as_str(),
    })?;
    Ok(Hash256::sha256(&bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureSource {
    Update,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFailure {
    pub id: String,
    pub revision: u64,
    pub severity: String,
    pub source: FailureSource,
}

/// Typed sessionFailure classification. The parser lives in `acp::connection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureClassification {
    pub records: Vec<ParsedFailure>,
    pub incompatible: bool,
    pub http_status: Option<u16>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureObservation {
    pub fence: Fence,
    pub turn_generation: u64,
    pub classification: FailureClassification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTurnDecision {
    pub accepted: bool,
    pub certificate: bool,
    pub blocked: bool,
    pub adapter_incompatible: bool,
    pub finish_reason: String,
    pub candidate_id: Option<String>,
    pub blocking_ids: Vec<String>,
    pub warning_ids: Vec<String>,
    pub sources: Vec<FailureSource>,
    pub stop_reason: Option<String>,
    pub reason: Option<String>,
}

impl ServiceTurnDecision {
    /// Existing public code only. `adapter_incompatible` is a reason, not a code.
    pub fn public_code(&self) -> Option<ErrorCode> {
        if self.adapter_incompatible {
            Some(ErrorCode::CapabilityUnqualified)
        } else {
            None
        }
    }
}

pub fn classify_service_failure(
    meta: Option<&serde_json::Map<String, Value>>,
    source: FailureSource,
    stop_reason: Option<&str>,
    http_status: Option<u16>,
) -> FailureClassification {
    crate::acp::connection::classify_service_failure(meta, source, stop_reason, http_status)
}

pub(crate) fn decide_service_turn(
    fence: &Fence,
    turn_generation: u64,
    observations: &[FailureObservation],
    completed: Option<&RuntimeTurnCompleted>,
    protocol_error: Option<&str>,
    staged_candidate_id: Option<String>,
) -> ServiceTurnDecision {
    let mut blocking_ids = std::collections::BTreeSet::new();
    let mut warning_ids = std::collections::BTreeSet::new();
    let mut sources = Vec::new();
    let mut incompatible = false;
    let mut http_block = false;
    let mut stop_reason = None;
    for observation in observations {
        if observation.fence != *fence || observation.turn_generation != turn_generation {
            continue;
        }
        let class = &observation.classification;
        if stop_reason.is_none() {
            stop_reason = class.stop_reason.clone();
        }
        if class.incompatible {
            incompatible = true;
        }
        if class.http_status.is_some_and(|status| status >= 400) {
            http_block = true;
        }
        for record in &class.records {
            if !sources.contains(&record.source) {
                sources.push(record.source);
            }
            if record.severity == "warning" {
                warning_ids.insert(record.id.clone());
            } else {
                blocking_ids.insert(record.id.clone());
            }
        }
    }
    for id in &blocking_ids {
        warning_ids.remove(id);
    }
    let blocked = incompatible || http_block || !blocking_ids.is_empty();
    let reason = if incompatible {
        Some("adapter_incompatible".to_string())
    } else if http_block {
        Some("http_status".to_string())
    } else if !blocking_ids.is_empty() {
        Some("session_failure".to_string())
    } else if completed.is_none() {
        Some(protocol_error.unwrap_or("no_submission").to_string())
    } else {
        None
    };
    let finish_reason = if blocked {
        reason.clone().unwrap_or_else(|| "session_failure".to_string())
    } else if completed.is_some() {
        "normal".to_string()
    } else {
        reason.clone().unwrap_or_else(|| "no_submission".to_string())
    };
    let candidate_id = completed
        .and_then(|done| done.candidate_id.clone())
        .or(staged_candidate_id);
    ServiceTurnDecision {
        accepted: false,
        certificate: false,
        blocked,
        adapter_incompatible: incompatible,
        finish_reason,
        candidate_id,
        blocking_ids: blocking_ids.into_iter().collect(),
        warning_ids: warning_ids.into_iter().collect(),
        sources,
        stop_reason,
        reason,
    }
}

fn recorded_failures() -> &'static Mutex<HashMap<String, Vec<FailureObservation>>> {
    static FAILURES: std::sync::OnceLock<Mutex<HashMap<String, Vec<FailureObservation>>>> =
        std::sync::OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn record_service_carrier(
    connection_id: &str,
    turn_generation: Option<u64>,
    meta: Option<&serde_json::Map<String, Value>>,
    source: FailureSource,
    stop_reason: Option<&str>,
    http_status: Option<u16>,
) {
    let Some((fence, bound_turn)) = super::ingress::route_attribution(connection_id) else {
        return;
    };
    let classification = classify_service_failure(meta, source, stop_reason, http_status);
    let mut map = recorded_failures()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    map.entry(connection_id.to_string())
        .or_default()
        .push(FailureObservation {
            fence,
            turn_generation: turn_generation.unwrap_or(bound_turn),
            classification,
        });
}

pub fn record_service_update(
    connection_id: &str,
    turn_generation: Option<u64>,
    meta: Option<&serde_json::Map<String, Value>>,
) {
    record_service_carrier(
        connection_id,
        turn_generation,
        meta,
        FailureSource::Update,
        None,
        None,
    );
}

pub fn record_service_response(
    connection_id: &str,
    turn_generation: Option<u64>,
    meta: Option<&serde_json::Map<String, Value>>,
    stop_reason: &str,
) {
    record_service_carrier(
        connection_id,
        turn_generation,
        meta,
        FailureSource::Response,
        Some(stop_reason),
        None,
    );
}

pub fn drain_recorded_failures(connection_id: &str) -> Vec<FailureObservation> {
    recorded_failures()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .remove(connection_id)
        .unwrap_or_default()
}

pub fn service_client_capabilities_value() -> Value {
    crate::acp::connection::service_client_capabilities_value()
}
