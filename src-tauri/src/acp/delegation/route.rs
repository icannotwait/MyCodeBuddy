//! Pure managed delegation-route domain.
//!
//! Resolves an immutable `DelegationRoutePlan` from settings, origin, and
//! preflight inputs. Does not spawn processes, inject MCP, or touch settings
//! persistence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::models::AgentType;

/// Adapter contract version embedded in fingerprints and capability ads.
pub const ROUTE_ADAPTER_CONTRACT_VERSION: &str = "delegation-route-v1";

/// Pinned Codex CLI version covered by the route-adapter contract.
pub const PINNED_CODEX_CLI_VERSION: &str = "0.144.1";
/// Pinned Grok package version covered by the route-adapter contract.
pub const PINNED_GROK_VERSION: &str = "0.2.98";
/// Pinned CodeBuddy package version covered by the route-adapter contract.
pub const PINNED_CODEBUDDY_VERSION: &str = "2.118.2";
/// Pinned Claude Code product version covered by the route-adapter contract.
pub const PINNED_CLAUDE_CODE_VERSION: &str = "2.1.205";
/// Pinned Claude ACP wrapper version covered by the route-adapter contract.
pub const PINNED_CLAUDE_ACP_VERSION: &str = "0.58.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationRoutePolicy {
    Codeg,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationRouteSource {
    ForcedChild,
    SessionOverride,
    GlobalDefault,
    FeatureDisabled,
    SafeFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDegradedReason {
    NativeSuppressionUnsupported,
    NativeSuppressionInvalid,
    CompanionBinaryUnavailable,
    AgentMcpUnsupported,
    CompanionInitializationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationConnectionOrigin {
    Root,
    CodegChild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "adapter", rename_all = "snake_case")]
pub enum NativeSuppressionPlan {
    None,
    CodexMultiAgentFalse,
    GrokNoSubagents,
    CodeBuddyDisallowedTools { tools: Vec<String> },
    ClaudeDisallowedTools { tools: Vec<String> },
}

impl NativeSuppressionPlan {
    /// True when this plan actively suppresses native creation tools.
    pub fn suppresses_creation(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressionCapability {
    pub adapter_contract_version: String,
    pub failure: Option<RouteDegradedReason>,
}

impl SuppressionCapability {
    pub fn supported(adapter_contract_version: impl Into<String>) -> Self {
        Self {
            adapter_contract_version: adapter_contract_version.into(),
            failure: None,
        }
    }

    pub fn unsupported(reason: RouteDegradedReason) -> Self {
        Self {
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            failure: Some(reason),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouteResolutionInput {
    pub agent_type: AgentType,
    pub origin: DelegationConnectionOrigin,
    pub session_override: Option<DelegationRoutePolicy>,
    pub global_policy: DelegationRoutePolicy,
    pub delegation_enabled: bool,
    pub suppression: SuppressionCapability,
    pub agent_mcp_supported: bool,
    pub companion_binary_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRoutePlan {
    pub managed: bool,
    pub requested: DelegationRoutePolicy,
    pub effective: DelegationRoutePolicy,
    pub source: DelegationRouteSource,
    pub native_suppression: NativeSuppressionPlan,
    pub expose_codeg_delegation: bool,
    pub degraded_reason: Option<RouteDegradedReason>,
    pub adapter_contract_version: String,
    pub fingerprint: String,
}

impl DelegationRoutePlan {
    /// True when Codeg does not suppress native creation for this plan.
    pub fn native_creation_exposed(&self) -> bool {
        !self.native_suppression.suppresses_creation()
    }

    /// Rejects the forbidden mixed state on managed plans:
    /// `native_creation_exposed && expose_codeg_delegation`.
    pub fn assert_exclusive(&self) -> Result<(), RouteResolutionError> {
        if self.managed && self.native_creation_exposed() && self.expose_codeg_delegation {
            return Err(RouteResolutionError::MixedCreationSurfaces);
        }
        Ok(())
    }
}

/// Errors produced by pure route resolution (not process launch).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteResolutionError {
    #[error("route unavailable: {reason:?}")]
    RouteUnavailable { reason: RouteDegradedReason },
    #[error("managed plan exposes both native creation and codeg delegation")]
    MixedCreationSurfaces,
}

impl RouteResolutionError {
    /// Stable machine-readable code for wire/UI matching.
    pub fn stable_code(&self) -> &'static str {
        match self {
            Self::RouteUnavailable { .. } => "route_unavailable",
            // Wire taxonomy: invalid suppression / mixed creation surfaces.
            Self::MixedCreationSurfaces => "native_suppression_invalid",
        }
    }
}

/// Versioned canonical fields hashed into `DelegationRoutePlan.fingerprint`.
///
/// Source is deliberately excluded so inherited vs explicit same-policy
/// selections share a fingerprint.
#[derive(Serialize)]
struct RouteFingerprintPayload<'a> {
    v: u32,
    agent_type: AgentType,
    managed: bool,
    requested: DelegationRoutePolicy,
    effective: DelegationRoutePolicy,
    native_suppression: &'a NativeSuppressionPlan,
    expose_codeg_delegation: bool,
    adapter_contract_version: &'a str,
    degraded_reason: Option<RouteDegradedReason>,
}

/// Managed route contract applies only to these four Agent types.
pub fn is_managed_agent(agent_type: AgentType) -> bool {
    matches!(
        agent_type,
        AgentType::Codex | AgentType::Grok | AgentType::CodeBuddy | AgentType::ClaudeCode
    )
}

/// Canonical managed **host** suppression-contract version for an Agent type.
///
/// This is distinct from the ACP adapter/package `registry_version` (e.g. Codex
/// ACP `1.1.2`). Suppression pin checks use host contracts only.
pub fn managed_host_contract_version(agent_type: AgentType) -> Option<&'static str> {
    match agent_type {
        AgentType::Codex => Some(PINNED_CODEX_CLI_VERSION),
        AgentType::Grok => Some(PINNED_GROK_VERSION),
        AgentType::CodeBuddy => Some(PINNED_CODEBUDDY_VERSION),
        // Wrapper is the primary host contract; product pin is also accepted.
        AgentType::ClaudeCode => Some(PINNED_CLAUDE_ACP_VERSION),
        _ => None,
    }
}

/// Classification of whether a launch uses the managed host contract or an
/// unverified custom override. Built from persisted installed-version and
/// runtime-env facts only — never from a per-connect `--help`/version probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedHostContractFacts {
    /// Version string passed to [`suppression_capability`].
    pub contract_version: Option<String>,
    /// True when the launch uses an unverified custom path/version override.
    pub custom_executable: bool,
}

fn env_has_nonempty(runtime_env: &BTreeMap<String, String>, key: &str) -> bool {
    runtime_env.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case(key) && !v.trim().is_empty()
    })
}

fn installed_matches_managed_pin(agent_type: AgentType, installed: &str) -> bool {
    match agent_type {
        AgentType::Grok => installed == PINNED_GROK_VERSION,
        AgentType::CodeBuddy => installed == PINNED_CODEBUDDY_VERSION,
        AgentType::ClaudeCode => {
            installed == PINNED_CLAUDE_ACP_VERSION || installed == PINNED_CLAUDE_CODE_VERSION
        }
        // Codex host contract is CLI pin, not installed ACP adapter version.
        AgentType::Codex => installed == PINNED_CODEX_CLI_VERSION,
        _ => false,
    }
}

/// Classify managed-host contract identity from installed-version + runtime env.
///
/// Rules:
/// - **Codex:** explicit non-empty `CODEX_PATH` in user/runtime config → custom;
///   otherwise managed host contract is the pinned CLI `0.144.1` (never ACP
///   adapter `registry_version`).
/// - **Grok / CodeBuddy / Claude:** an installed version that differs from the
///   managed pin(s) → custom; missing installed version is treated as managed
///   (uses the host contract pin). Claude accepts wrapper `0.58.1` or product
///   `2.1.205`.
pub fn classify_managed_host_contract(
    agent_type: AgentType,
    installed_version: Option<&str>,
    runtime_env: &BTreeMap<String, String>,
) -> ManagedHostContractFacts {
    if !is_managed_agent(agent_type) {
        return ManagedHostContractFacts {
            contract_version: None,
            custom_executable: false,
        };
    }

    match agent_type {
        AgentType::Codex => {
            let custom =
                env_has_nonempty(runtime_env, crate::acp::codex_cli::CODEX_PATH_ENV);
            ManagedHostContractFacts {
                contract_version: Some(PINNED_CODEX_CLI_VERSION.to_string()),
                custom_executable: custom,
            }
        }
        AgentType::Grok | AgentType::CodeBuddy | AgentType::ClaudeCode => {
            match installed_version.map(str::trim).filter(|s| !s.is_empty()) {
                None => ManagedHostContractFacts {
                    contract_version: managed_host_contract_version(agent_type)
                        .map(str::to_string),
                    custom_executable: false,
                },
                Some(installed) => {
                    let custom = !installed_matches_managed_pin(agent_type, installed);
                    ManagedHostContractFacts {
                        contract_version: Some(installed.to_string()),
                        custom_executable: custom,
                    }
                }
            }
        }
        _ => ManagedHostContractFacts {
            contract_version: None,
            custom_executable: false,
        },
    }
}

/// Derive suppression capability from host-contract classification.
pub fn resolve_managed_host_suppression(
    agent_type: AgentType,
    installed_version: Option<&str>,
    runtime_env: &BTreeMap<String, String>,
) -> SuppressionCapability {
    let facts = classify_managed_host_contract(agent_type, installed_version, runtime_env);
    suppression_capability(
        agent_type,
        facts.contract_version.as_deref(),
        facts.custom_executable,
    )
}

/// Exact capability facts captured once at launch and reused for every later
/// comparison re-resolution. Never synthesize optimistic facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCapabilitySnapshot {
    pub suppression: SuppressionCapability,
    pub agent_mcp_supported: bool,
    pub companion_binary_available: bool,
}

impl RouteCapabilitySnapshot {
    /// Build capability facts from installed-version, runtime env, MCP meta,
    /// and companion preflight. Call only after `runtime_env` is available.
    pub fn from_launch_facts(
        agent_type: AgentType,
        installed_version: Option<&str>,
        runtime_env: &BTreeMap<String, String>,
        agent_mcp_supported: bool,
        companion_binary_available: bool,
    ) -> Self {
        Self {
            suppression: resolve_managed_host_suppression(
                agent_type,
                installed_version,
                runtime_env,
            ),
            agent_mcp_supported,
            companion_binary_available,
        }
    }

    /// Optimistic supported capability — tests only.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn test_supported() -> Self {
        Self {
            suppression: SuppressionCapability::supported(ROUTE_ADAPTER_CONTRACT_VERSION),
            agent_mcp_supported: true,
            companion_binary_available: true,
        }
    }

    /// Degraded unsupported suppression capability — tests only.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn test_suppression_unsupported() -> Self {
        Self {
            suppression: SuppressionCapability::unsupported(
                RouteDegradedReason::NativeSuppressionUnsupported,
            ),
            agent_mcp_supported: true,
            companion_binary_available: true,
        }
    }
}

/// Pinned capability table for the four managed platforms.
///
/// `version` is the **host contract / package version under test**, not the
/// ACP adapter install id unless they are the same product. `custom_executable`
/// is true when the user points at an unverified custom binary; that path is
/// always unsupported without a per-connect `--help` probe.
pub fn suppression_capability(
    agent_type: AgentType,
    version: Option<&str>,
    custom_executable: bool,
) -> SuppressionCapability {
    if !is_managed_agent(agent_type) || custom_executable {
        return SuppressionCapability::unsupported(
            RouteDegradedReason::NativeSuppressionUnsupported,
        );
    }

    let Some(version) = version else {
        return SuppressionCapability::unsupported(
            RouteDegradedReason::NativeSuppressionUnsupported,
        );
    };

    let compatible = match agent_type {
        AgentType::Codex => version == PINNED_CODEX_CLI_VERSION,
        AgentType::Grok => version == PINNED_GROK_VERSION,
        AgentType::CodeBuddy => version == PINNED_CODEBUDDY_VERSION,
        AgentType::ClaudeCode => {
            version == PINNED_CLAUDE_ACP_VERSION || version == PINNED_CLAUDE_CODE_VERSION
        }
        _ => false,
    };

    if compatible {
        SuppressionCapability::supported(ROUTE_ADAPTER_CONTRACT_VERSION)
    } else {
        SuppressionCapability::unsupported(RouteDegradedReason::NativeSuppressionUnsupported)
    }
}

/// Resolve the immutable route plan for a connection before process launch.
pub fn resolve_route(
    input: RouteResolutionInput,
) -> Result<DelegationRoutePlan, RouteResolutionError> {
    if !is_managed_agent(input.agent_type) {
        return Ok(unmanaged_legacy_plan(&input));
    }

    let (requested, preference_source) = match input.origin {
        DelegationConnectionOrigin::CodegChild => (
            DelegationRoutePolicy::Codeg,
            DelegationRouteSource::ForcedChild,
        ),
        DelegationConnectionOrigin::Root => {
            if let Some(over) = input.session_override {
                (over, DelegationRouteSource::SessionOverride)
            } else {
                (input.global_policy, DelegationRouteSource::GlobalDefault)
            }
        }
    };

    // Forced children always keep Codeg effective (with optional zero exposure).
    // Roots honor the master switch: requested records selection, effective is
    // native with FeatureDisabled when the switch is off.
    let (effective, source) = match input.origin {
        DelegationConnectionOrigin::CodegChild => (
            DelegationRoutePolicy::Codeg,
            DelegationRouteSource::ForcedChild,
        ),
        DelegationConnectionOrigin::Root => {
            if !input.delegation_enabled {
                (
                    DelegationRoutePolicy::Native,
                    DelegationRouteSource::FeatureDisabled,
                )
            } else {
                (requested, preference_source)
            }
        }
    };

    if effective == DelegationRoutePolicy::Native {
        return Ok(finish_plan(
            input.agent_type,
            DelegationRoutePlan {
                managed: true,
                requested,
                effective: DelegationRoutePolicy::Native,
                source,
                native_suppression: NativeSuppressionPlan::None,
                expose_codeg_delegation: false,
                degraded_reason: None,
                adapter_contract_version: input.suppression.adapter_contract_version.clone(),
                fingerprint: String::new(),
            },
        ));
    }

    // effective == Codeg
    let expose_codeg_delegation = match input.origin {
        // Master switch off on a forced child: suppress native, expose nothing.
        DelegationConnectionOrigin::CodegChild => input.delegation_enabled,
        DelegationConnectionOrigin::Root => true,
    };

    if let Some(reason) = input.suppression.failure {
        return preflight_failure(&input, requested, reason);
    }

    // Companion/MCP are only required when Codeg delegation is actually exposed.
    if expose_codeg_delegation {
        if !input.companion_binary_available {
            return preflight_failure(
                &input,
                requested,
                RouteDegradedReason::CompanionBinaryUnavailable,
            );
        }
        if !input.agent_mcp_supported {
            return preflight_failure(
                &input,
                requested,
                RouteDegradedReason::AgentMcpUnsupported,
            );
        }
    }

    Ok(finish_plan(
        input.agent_type,
        DelegationRoutePlan {
            managed: true,
            requested,
            effective: DelegationRoutePolicy::Codeg,
            source,
            native_suppression: codeg_suppression_plan(input.agent_type),
            expose_codeg_delegation,
            degraded_reason: None,
            adapter_contract_version: input.suppression.adapter_contract_version.clone(),
            fingerprint: String::new(),
        },
    ))
}

/// Build a fresh native plan from a prior Codeg-oriented plan (root-only path).
///
/// Transforms only a **managed**, **effective-Codeg** plan whose suppression is
/// a typed non-`None` managed variant (from which agent identity is recovered
/// for the fingerprint). Unmanaged, already-native, feature-disabled,
/// already-fallback, or `None`-suppression inputs return `plan` unchanged so
/// misuse is idempotent and never invents an `AgentType`.
pub fn safe_native_fallback(
    plan: &DelegationRoutePlan,
    reason: RouteDegradedReason,
) -> DelegationRoutePlan {
    let Some(agent_type) = agent_type_from_suppression(&plan.native_suppression) else {
        return plan.clone();
    };
    if !plan.managed || plan.effective != DelegationRoutePolicy::Codeg {
        return plan.clone();
    }

    finish_plan(
        agent_type,
        DelegationRoutePlan {
            managed: plan.managed,
            requested: plan.requested,
            effective: DelegationRoutePolicy::Native,
            source: DelegationRouteSource::SafeFallback,
            native_suppression: NativeSuppressionPlan::None,
            expose_codeg_delegation: false,
            degraded_reason: Some(reason),
            adapter_contract_version: plan.adapter_contract_version.clone(),
            fingerprint: String::new(),
        },
    )
}

fn preflight_failure(
    input: &RouteResolutionInput,
    requested: DelegationRoutePolicy,
    reason: RouteDegradedReason,
) -> Result<DelegationRoutePlan, RouteResolutionError> {
    match input.origin {
        DelegationConnectionOrigin::Root => {
            // Fresh native plan: keep requested selection, drop suppression.
            Ok(finish_plan(
                input.agent_type,
                DelegationRoutePlan {
                    managed: true,
                    requested,
                    effective: DelegationRoutePolicy::Native,
                    source: DelegationRouteSource::SafeFallback,
                    native_suppression: NativeSuppressionPlan::None,
                    expose_codeg_delegation: false,
                    degraded_reason: Some(reason),
                    adapter_contract_version: input.suppression.adapter_contract_version.clone(),
                    fingerprint: String::new(),
                },
            ))
        }
        DelegationConnectionOrigin::CodegChild => {
            Err(RouteResolutionError::RouteUnavailable { reason })
        }
    }
}

fn unmanaged_legacy_plan(input: &RouteResolutionInput) -> DelegationRoutePlan {
    // Unmanaged Agents keep legacy exposure (master switch only) and no
    // managed suppression / selector / hard-exclusion claim.
    let (source, expose) = if input.delegation_enabled {
        (DelegationRouteSource::GlobalDefault, true)
    } else {
        (DelegationRouteSource::FeatureDisabled, false)
    };

    finish_plan(
        input.agent_type,
        DelegationRoutePlan {
            managed: false,
            requested: DelegationRoutePolicy::Native,
            effective: DelegationRoutePolicy::Native,
            source,
            native_suppression: NativeSuppressionPlan::None,
            expose_codeg_delegation: expose,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: String::new(),
        },
    )
}

fn codeg_suppression_plan(agent_type: AgentType) -> NativeSuppressionPlan {
    match agent_type {
        AgentType::Codex => NativeSuppressionPlan::CodexMultiAgentFalse,
        AgentType::Grok => NativeSuppressionPlan::GrokNoSubagents,
        AgentType::CodeBuddy => NativeSuppressionPlan::CodeBuddyDisallowedTools {
            tools: vec!["Agent".into(), "Task".into()],
        },
        AgentType::ClaudeCode => NativeSuppressionPlan::ClaudeDisallowedTools {
            tools: vec!["Agent".into(), "Task".into()],
        },
        _ => NativeSuppressionPlan::None,
    }
}

/// Recover the managed Agent type from a typed Codeg suppression variant.
/// Returns `None` for `NativeSuppressionPlan::None` — callers must not invent
/// a stand-in agent identity.
fn agent_type_from_suppression(plan: &NativeSuppressionPlan) -> Option<AgentType> {
    match plan {
        NativeSuppressionPlan::CodexMultiAgentFalse => Some(AgentType::Codex),
        NativeSuppressionPlan::GrokNoSubagents => Some(AgentType::Grok),
        NativeSuppressionPlan::CodeBuddyDisallowedTools { .. } => Some(AgentType::CodeBuddy),
        NativeSuppressionPlan::ClaudeDisallowedTools { .. } => Some(AgentType::ClaudeCode),
        NativeSuppressionPlan::None => None,
    }
}

fn finish_plan(agent_type: AgentType, mut plan: DelegationRoutePlan) -> DelegationRoutePlan {
    plan.fingerprint = compute_fingerprint(agent_type, &plan);
    plan
}

fn compute_fingerprint(agent_type: AgentType, plan: &DelegationRoutePlan) -> String {
    let payload = RouteFingerprintPayload {
        v: 1,
        agent_type,
        managed: plan.managed,
        requested: plan.requested,
        effective: plan.effective,
        native_suppression: &plan.native_suppression,
        expose_codeg_delegation: plan.expose_codeg_delegation,
        adapter_contract_version: &plan.adapter_contract_version,
        degraded_reason: plan.degraded_reason,
    };
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Placeholder plan for test connections that do not exercise route reuse.
/// Matches `AcpLaunchInputs::with_placeholder_route` so session-id reuse tests
/// that rebuild launch inputs with that helper remain compatible.
#[cfg(any(test, feature = "test-utils"))]
pub fn test_empty_route_plan() -> DelegationRoutePlan {
    resolve_route(RouteResolutionInput {
        agent_type: crate::models::AgentType::ClaudeCode,
        origin: DelegationConnectionOrigin::Root,
        session_override: None,
        global_policy: DelegationRoutePolicy::Codeg,
        delegation_enabled: false,
        suppression: SuppressionCapability::supported(ROUTE_ADAPTER_CONTRACT_VERSION),
        agent_mcp_supported: true,
        companion_binary_available: true,
    })
    .expect("feature-disabled native plan must resolve")
}

/// Deterministic comparison fingerprint used by staleness refresh helpers.
///
/// Must use the **exact** [`RouteCapabilitySnapshot`] captured at launch so
/// degraded launch fingerprints and later preference-only comparisons stay
/// consistent. Never synthesize optimistic supported/true/true facts.
pub fn comparison_route_fingerprint(
    agent_type: AgentType,
    origin: DelegationConnectionOrigin,
    session_override: Option<DelegationRoutePolicy>,
    global_policy: DelegationRoutePolicy,
    delegation_enabled: bool,
    capability: &RouteCapabilitySnapshot,
) -> String {
    resolve_route(RouteResolutionInput {
        agent_type,
        origin,
        session_override,
        global_policy,
        delegation_enabled,
        suppression: capability.suppression.clone(),
        agent_mcp_supported: capability.agent_mcp_supported,
        companion_binary_available: capability.companion_binary_available,
    })
    .map(|p| p.fingerprint)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(agent_type: AgentType) -> RouteResolutionInput {
        RouteResolutionInput {
            agent_type,
            origin: DelegationConnectionOrigin::Root,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression: SuppressionCapability::supported("route-v1"),
            agent_mcp_supported: true,
            companion_binary_available: true,
        }
    }

    #[test]
    fn managed_agents_default_to_one_codeg_creation_surface() {
        for agent in [
            AgentType::Codex,
            AgentType::Grok,
            AgentType::CodeBuddy,
            AgentType::ClaudeCode,
        ] {
            let plan = resolve_route(input(agent)).expect("managed route");
            assert!(plan.managed);
            assert_eq!(plan.requested, DelegationRoutePolicy::Codeg);
            assert_eq!(plan.effective, DelegationRoutePolicy::Codeg);
            assert_eq!(plan.source, DelegationRouteSource::GlobalDefault);
            assert!(plan.native_suppression.suppresses_creation());
            assert!(plan.expose_codeg_delegation);
            assert!(!plan.native_creation_exposed());
            plan.assert_exclusive().expect("exclusive");
        }
    }

    #[test]
    fn override_feature_gate_child_and_fallback_have_stable_precedence() {
        let mut root = input(AgentType::Grok);
        root.session_override = Some(DelegationRoutePolicy::Native);
        root.suppression = SuppressionCapability::unsupported(
            RouteDegradedReason::NativeSuppressionUnsupported,
        );
        root.agent_mcp_supported = false;
        root.companion_binary_available = false;
        let native = resolve_route(root.clone()).expect("native override");
        assert_eq!(native.effective, DelegationRoutePolicy::Native);
        assert_eq!(native.source, DelegationRouteSource::SessionOverride);
        assert!(!native.expose_codeg_delegation);
        assert!(!native.native_suppression.suppresses_creation());

        root.session_override = None;
        root.suppression = SuppressionCapability::supported("route-v1");
        root.agent_mcp_supported = true;
        root.companion_binary_available = true;
        root.delegation_enabled = false;
        let disabled = resolve_route(root).expect("disabled root");
        assert_eq!(disabled.requested, DelegationRoutePolicy::Codeg);
        assert_eq!(disabled.effective, DelegationRoutePolicy::Native);
        assert_eq!(disabled.source, DelegationRouteSource::FeatureDisabled);

        let mut child = input(AgentType::Grok);
        child.origin = DelegationConnectionOrigin::CodegChild;
        child.global_policy = DelegationRoutePolicy::Native;
        child.delegation_enabled = false;
        child.agent_mcp_supported = false;
        child.companion_binary_available = false;
        let forced = resolve_route(child).expect("forced child");
        assert_eq!(forced.requested, DelegationRoutePolicy::Codeg);
        assert_eq!(forced.effective, DelegationRoutePolicy::Codeg);
        assert_eq!(forced.source, DelegationRouteSource::ForcedChild);
        assert!(forced.native_suppression.suppresses_creation());
        assert!(!forced.expose_codeg_delegation);

        let fallback = safe_native_fallback(
            &resolve_route(input(AgentType::Grok)).unwrap(),
            RouteDegradedReason::CompanionBinaryUnavailable,
        );
        assert_eq!(fallback.requested, DelegationRoutePolicy::Codeg);
        assert_eq!(fallback.effective, DelegationRoutePolicy::Native);
        assert_eq!(fallback.source, DelegationRouteSource::SafeFallback);
        assert!(!fallback.native_suppression.suppresses_creation());
        assert!(!fallback.expose_codeg_delegation);
    }

    #[test]
    fn child_rejects_missing_capability_and_fingerprint_ignores_source_only() {
        let mut child = input(AgentType::ClaudeCode);
        child.origin = DelegationConnectionOrigin::CodegChild;
        child.suppression = SuppressionCapability::unsupported(
            RouteDegradedReason::NativeSuppressionUnsupported,
        );
        assert_eq!(
            resolve_route(child).unwrap_err().stable_code(),
            "route_unavailable"
        );

        let inherited = resolve_route(input(AgentType::Codex)).unwrap();
        let mut explicit_input = input(AgentType::Codex);
        explicit_input.session_override = Some(DelegationRoutePolicy::Codeg);
        let explicit = resolve_route(explicit_input).unwrap();
        assert_ne!(inherited.source, explicit.source);
        assert_eq!(inherited.fingerprint, explicit.fingerprint);

        let mut native_input = input(AgentType::Codex);
        native_input.global_policy = DelegationRoutePolicy::Native;
        let native = resolve_route(native_input).unwrap();
        assert_ne!(inherited.fingerprint, native.fingerprint);
    }

    #[test]
    fn codex_host_contract_ignores_acp_adapter_registry_version() {
        let empty = BTreeMap::new();
        // Built-in managed path: no CODEX_PATH override → pinned CLI contract.
        let facts = classify_managed_host_contract(AgentType::Codex, Some("1.1.2"), &empty);
        assert!(!facts.custom_executable);
        assert_eq!(
            facts.contract_version.as_deref(),
            Some(PINNED_CODEX_CLI_VERSION)
        );
        let suppression = resolve_managed_host_suppression(AgentType::Codex, Some("1.1.2"), &empty);
        assert!(suppression.failure.is_none());

        // Child with managed host contract must resolve Codeg, not RouteUnavailable.
        let child = resolve_route(RouteResolutionInput {
            agent_type: AgentType::Codex,
            origin: DelegationConnectionOrigin::CodegChild,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression,
            agent_mcp_supported: true,
            companion_binary_available: true,
        })
        .expect("codex child with host contract must resolve");
        assert_eq!(child.effective, DelegationRoutePolicy::Codeg);
        assert_eq!(child.source, DelegationRouteSource::ForcedChild);
    }

    #[test]
    fn codex_custom_path_and_non_pin_install_are_unsupported() {
        let mut env = BTreeMap::new();
        env.insert(
            crate::acp::codex_cli::CODEX_PATH_ENV.to_string(),
            "C:\\custom\\codex.exe".into(),
        );
        let custom = resolve_managed_host_suppression(AgentType::Codex, None, &env);
        assert_eq!(
            custom.failure,
            Some(RouteDegradedReason::NativeSuppressionUnsupported)
        );

        let root = resolve_route(RouteResolutionInput {
            agent_type: AgentType::Codex,
            origin: DelegationConnectionOrigin::Root,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression: custom.clone(),
            agent_mcp_supported: true,
            companion_binary_available: true,
        })
        .expect("root falls back");
        assert_eq!(root.source, DelegationRouteSource::SafeFallback);
        assert_eq!(root.effective, DelegationRoutePolicy::Native);
        assert_eq!(
            root.degraded_reason,
            Some(RouteDegradedReason::NativeSuppressionUnsupported)
        );

        let child_err = resolve_route(RouteResolutionInput {
            agent_type: AgentType::Codex,
            origin: DelegationConnectionOrigin::CodegChild,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression: custom,
            agent_mcp_supported: true,
            companion_binary_available: true,
        })
        .expect_err("child unavailable");
        assert_eq!(child_err.stable_code(), "route_unavailable");

        // Grok/CodeBuddy non-pin installed version is custom.
        let empty = BTreeMap::new();
        let grok = classify_managed_host_contract(AgentType::Grok, Some("0.1.0"), &empty);
        assert!(grok.custom_executable);
        let cap = resolve_managed_host_suppression(AgentType::Grok, Some("0.1.0"), &empty);
        assert_eq!(
            cap.failure,
            Some(RouteDegradedReason::NativeSuppressionUnsupported)
        );
    }

    #[test]
    fn capability_snapshot_keeps_degraded_launch_and_comparison_consistent() {
        let capability = RouteCapabilitySnapshot::test_suppression_unsupported();
        let plan = resolve_route(RouteResolutionInput {
            agent_type: AgentType::Codex,
            origin: DelegationConnectionOrigin::Root,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression: capability.suppression.clone(),
            agent_mcp_supported: capability.agent_mcp_supported,
            companion_binary_available: capability.companion_binary_available,
        })
        .expect("root degraded plan");
        assert_eq!(plan.source, DelegationRouteSource::SafeFallback);

        let comparison = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            None,
            DelegationRoutePolicy::Codeg,
            true,
            &capability,
        );
        assert_eq!(
            comparison, plan.fingerprint,
            "stale comparison must reuse launch capability snapshot"
        );

        // Optimistic supported facts would diverge — prove we do not use them.
        let optimistic = RouteCapabilitySnapshot::test_supported();
        let optimistic_fp = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            None,
            DelegationRoutePolicy::Codeg,
            true,
            &optimistic,
        );
        assert_ne!(optimistic_fp, plan.fingerprint);
    }

    #[test]
    fn preflight_failure_falls_back_for_root_and_rejects_child() {
        for reason in [
            RouteDegradedReason::NativeSuppressionUnsupported,
            RouteDegradedReason::NativeSuppressionInvalid,
            RouteDegradedReason::CompanionBinaryUnavailable,
            RouteDegradedReason::AgentMcpUnsupported,
        ] {
            let mut root = input(AgentType::CodeBuddy);
            if reason == RouteDegradedReason::CompanionBinaryUnavailable {
                root.companion_binary_available = false;
            } else if reason == RouteDegradedReason::AgentMcpUnsupported {
                root.agent_mcp_supported = false;
            } else {
                root.suppression = SuppressionCapability::unsupported(reason);
            }
            let fallback = resolve_route(root).expect("root safe fallback");
            assert_eq!(fallback.source, DelegationRouteSource::SafeFallback);
            assert_eq!(fallback.degraded_reason, Some(reason));
            fallback.assert_exclusive().unwrap();

            let mut child = input(AgentType::CodeBuddy);
            child.origin = DelegationConnectionOrigin::CodegChild;
            if reason == RouteDegradedReason::CompanionBinaryUnavailable {
                child.companion_binary_available = false;
            } else if reason == RouteDegradedReason::AgentMcpUnsupported {
                child.agent_mcp_supported = false;
            } else {
                child.suppression = SuppressionCapability::unsupported(reason);
            }
            assert_eq!(
                resolve_route(child).unwrap_err().stable_code(),
                "route_unavailable"
            );
        }
    }

    #[test]
    fn safe_native_fallback_noops_non_codeg_and_transforms_managed_codeg_once() {
        let reason = RouteDegradedReason::CompanionBinaryUnavailable;

        // Already-native managed plan: must remain exactly unchanged.
        let mut native_input = input(AgentType::Grok);
        native_input.global_policy = DelegationRoutePolicy::Native;
        let native = resolve_route(native_input).expect("native plan");
        assert_eq!(native.effective, DelegationRoutePolicy::Native);
        assert_eq!(native.native_suppression, NativeSuppressionPlan::None);
        let after_native = safe_native_fallback(&native, reason);
        assert_eq!(
            after_native, native,
            "already-native plan must not be mutated or re-fingerprinted"
        );

        // Feature-disabled (native effective, None suppression).
        let mut disabled_input = input(AgentType::Grok);
        disabled_input.delegation_enabled = false;
        let disabled = resolve_route(disabled_input).expect("feature-disabled plan");
        assert_eq!(disabled.source, DelegationRouteSource::FeatureDisabled);
        let after_disabled = safe_native_fallback(&disabled, reason);
        assert_eq!(after_disabled, disabled);

        // Unmanaged / None-suppression plan.
        let unmanaged = resolve_route(input(AgentType::OpenCode)).expect("unmanaged");
        assert!(!unmanaged.managed);
        assert_eq!(unmanaged.native_suppression, NativeSuppressionPlan::None);
        let after_unmanaged = safe_native_fallback(&unmanaged, reason);
        assert_eq!(
            after_unmanaged, unmanaged,
            "unmanaged None-suppression plan must not invent an agent fingerprint"
        );

        // Valid managed Codeg plan still transforms exactly once.
        let codeg = resolve_route(input(AgentType::Grok)).expect("codeg plan");
        assert_eq!(codeg.effective, DelegationRoutePolicy::Codeg);
        assert!(codeg.native_suppression.suppresses_creation());
        let once = safe_native_fallback(&codeg, reason);
        assert!(once.managed);
        assert_eq!(once.requested, DelegationRoutePolicy::Codeg);
        assert_eq!(once.effective, DelegationRoutePolicy::Native);
        assert_eq!(once.source, DelegationRouteSource::SafeFallback);
        assert_eq!(once.degraded_reason, Some(reason));
        assert!(!once.native_suppression.suppresses_creation());
        assert!(!once.expose_codeg_delegation);
        assert_ne!(once.fingerprint, codeg.fingerprint);
        assert_ne!(once, codeg);

        // Already-fallback: second call is a pure no-op (idempotent).
        let twice = safe_native_fallback(&once, RouteDegradedReason::AgentMcpUnsupported);
        assert_eq!(
            twice, once,
            "already-fallback plan must stay unchanged (no second fallback)"
        );
    }
}
