//! Strict local installation metadata. It references the existing qualification
//! report contract; installing configuration alone never grants qualification.
use super::qualification::QualificationKey;
use super::rt_error;
use super::sandbox::QualifiedOciProfile;
use roundtable_protocol::{canonical_hash, ErrorCode, Hash256, QualifiedContextProfile, RtResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderBinding {
    pub provider_ref: String,
    pub model: String,
    pub origin: String,
    pub credential_env: String,
    pub supported_efforts: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledRuntime {
    pub schema_version: u32,
    pub qualification_key: QualificationKey,
    pub report_path: PathBuf,
    pub report_sha256: Hash256,
    pub oci: QualifiedOciProfile,
    pub context_profile: QualifiedContextProfile,
    pub providers: Vec<ProviderBinding>,
}

impl InstalledRuntime {
    pub(crate) fn load(data_dir: &Path) -> RtResult<Self> {
        let path = data_dir.join("roundtable/qualified-runtime.json");
        let bytes = bounded_read(&path, 1_048_576)?;
        let value: Self = roundtable_protocol::decode_json(
            &bytes,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        if value.schema_version != 1
            || value.providers.is_empty()
            || !value.report_path.is_absolute()
        {
            return Err(unqualified("runtime_installation"));
        }
        // Parsing retains the pinned cleanup provider after report/core drift.
        // New turns still require verify_report + verify_host before admission.
        Ok(value)
    }

    /// Schema 1 is the original single Codex runtime. Schema 2 lists one
    /// installed runtime per adapter (`codex`, `grok`, `cursor`, `antigravity`).
    pub(crate) fn load_catalog(data_dir: &Path) -> RtResult<BTreeMap<String, Self>> {
        let path = data_dir.join("roundtable/qualified-runtime.json");
        let bytes = bounded_read(&path, 4_194_304)?;
        let value = roundtable_protocol::parse_strict_json(
            &bytes,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        if value.get("adapters").is_some() {
            if value["schema_version"] != 2 {
                return Err(unqualified("runtime_installation"));
            }
            let entries = value["adapters"]
                .as_array()
                .ok_or_else(|| unqualified("runtime_installation"))?;
            let mut adapters = BTreeMap::new();
            for entry in entries {
                let agent = entry["agent"]
                    .as_str()
                    .ok_or_else(|| unqualified("runtime_installation"))?;
                if !matches!(agent, "codex" | "grok" | "cursor" | "antigravity") {
                    return Err(unqualified("runtime_installation"));
                }
                let runtime: Self = serde_json::from_value(entry["runtime"].clone())
                    .map_err(|_| unqualified("runtime_installation"))?;
                if runtime.schema_version != 1
                    || runtime.providers.is_empty()
                    || !runtime.report_path.is_absolute()
                {
                    return Err(unqualified("runtime_installation"));
                }
                adapters.insert(agent.to_string(), runtime);
            }
            if adapters.is_empty() {
                return Err(unqualified("runtime_installation"));
            }
            return Ok(adapters);
        }
        let runtime = Self::load(data_dir)?;
        let mut adapters = BTreeMap::new();
        adapters.insert("codex".to_string(), runtime);
        Ok(adapters)
    }

    pub(crate) fn verify_host(&self) -> RtResult<()> {
        if !cfg!(target_os = "linux") {
            return Err(unqualified("platform_unqualified"));
        }
        let release = std::fs::read_to_string("/etc/os-release")
            .map_err(|_| unqualified("host_os_unknown"))?;
        let field = |key: &str| {
            release.lines().find_map(|line| {
                line.strip_prefix(&format!("{key}="))
                    .map(|value| value.trim_matches('"').to_owned())
            })
        };
        let version = format!(
            "{}-{}",
            field("ID").ok_or_else(|| unqualified("host_os_unknown"))?,
            field("VERSION_ID").ok_or_else(|| unqualified("host_os_unknown"))?
        );
        if self.qualification_key.os.name != "linux" || self.qualification_key.os.version != version
        {
            return Err(unqualified("host_os_changed"));
        }
        let report = roundtable_protocol::parse_strict_json(
            &bounded_read(&self.report_path, 1_048_576)?,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map_err(|_| unqualified("host_kernel_unknown"))?;
        verify_runtime_contract(
            &report,
            &serde_json::to_value(&self.providers)
                .map_err(|_| unqualified("provider_binding_invalid"))?,
            &version,
            kernel.trim(),
        )?;
        Ok(())
    }

    pub(crate) fn verify_report(&self) -> RtResult<()> {
        let bytes = bounded_read(&self.report_path, 1_048_576)?;
        if Hash256::sha256(&bytes) != self.report_sha256 {
            return Err(unqualified("qualification_report_changed"));
        }
        let report = roundtable_protocol::parse_strict_json(
            &bytes,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        if report["verdict"] != "passed"
            || report["g1_passed"] != true
            || report["qualification_issued"] != true
            || report["profile"]["platform_blocked"] != false
        {
            return Err(unqualified("certificate_not_passed"));
        }
        for field in ["missing", "anomalies"] {
            if report[field]
                .as_array()
                .is_none_or(|values| !values.is_empty())
            {
                return Err(unqualified("qualification_incomplete"));
            }
        }
        for field in ["experiment_id", "observed_at", "execution_tree_hash"] {
            if report[field].as_str().is_none_or(str::is_empty) {
                return Err(unqualified("qualification_incomplete"));
            }
        }
        if report["runtime_build_version"] != env!("CARGO_PKG_VERSION")
            || report["runtime_implementation_version"] != "roundtable-oci-acp-v1"
        {
            return Err(unqualified("runtime_version_changed"));
        }
        if report["provider_bindings_hash"] != json!(canonical_hash(&self.providers)?) {
            return Err(unqualified("provider_bindings_changed"));
        }
        if self.providers.iter().any(|provider| {
            provider.model.is_empty()
                || provider.provider_ref.is_empty()
                || provider.credential_env.is_empty()
        }) {
            return Err(unqualified("provider_binding_invalid"));
        }
        let profile = &report["profile"];
        let key = &self.qualification_key;
        for (name, value) in [
            ("os_name", json!(key.os.name)),
            ("os_version", json!(key.os.version)),
            ("adapter_version", json!(key.adapter_version)),
            ("isolator_version", json!(key.isolator_version)),
            ("image_digest", json!(key.image_digest)),
            ("policy_hash", json!(key.policy_hash)),
            ("tool_contract_hash", json!(key.tool_contract_hash)),
            ("core_hash", json!(key.core_hash)),
            ("plan_hash", json!(key.plan_hash)),
        ] {
            if profile[name] != value {
                return Err(unqualified("qualification_key_changed"));
            }
        }
        let binaries = profile["binaries"]
            .as_array()
            .ok_or_else(|| unqualified("qualification_binaries"))?;
        for binary in &key.binaries {
            if !binaries.iter().any(|entry| {
                entry["absolute_path"] == binary.absolute_path
                    && entry["version"] == binary.version
                    && entry["sha256"] == json!(binary.sha256)
            }) {
                return Err(unqualified("qualification_binaries"));
            }
        }
        for name in [
            "new_session",
            "roundtable_mcp",
            "ordered_turn_completion",
            "cancel_and_reap",
            "strict_isolation",
            "bounded_context_delivery",
        ] {
            if !report["required_capabilities"]
                .as_array()
                .is_some_and(|checks| {
                    checks
                        .iter()
                        .any(|check| check["name"] == name && check["status"] == "passed")
                })
            {
                return Err(unqualified("qualification_capability"));
            }
        }
        for field in [
            "private_events",
            "sidebar_discovery",
            "credentials_unreachable",
            "api_credential_scope",
            "binary_match",
            "endpoint_compatibility",
            "native_read_boundary",
            "companion_lifecycle",
        ] {
            if report[field] != "passed" {
                return Err(unqualified("qualification_capability"));
            }
        }
        for field in [
            "ordinary_sidebar_imports",
            "global_body_events",
            "idle_processes_after_reap",
        ] {
            if report[field].as_u64() != Some(0) {
                return Err(unqualified("qualification_leak"));
            }
        }
        for field in [
            "model_credential_material_in_sandbox",
            "model_credentials_visible_to_agent",
            "fake_broker_is_certificate",
            "fake_fd_is_certificate",
        ] {
            if report[field] != false {
                return Err(unqualified("qualification_leak"));
            }
        }
        for field in [
            "actual_resolved_binary_matches_certificate",
            "normal_completion_after_submit_receipt",
        ] {
            if report[field] != true {
                return Err(unqualified("qualification_capability"));
            }
        }
        let shared = shared_core_hashes();
        if report["shared_core_hashes"] != shared
            || profile["shared_core_hashes"] != shared
            || canonical_hash(&shared)? != key.core_hash
        {
            return Err(unqualified("qualification_core_changed"));
        }
        let base = self
            .report_path
            .parent()
            .ok_or_else(|| unqualified("qualification_evidence"))?;
        let traces = report["traces"]
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or_else(|| unqualified("qualification_evidence"))?;
        for trace in traces {
            let path = trace["path"]
                .as_str()
                .ok_or_else(|| unqualified("qualification_evidence"))?;
            let relative = super::snapshot::validate_relative_path(path)?;
            let trace_bytes = bounded_read(&base.join(relative), 16_777_216)?;
            if json!(Hash256::sha256(&trace_bytes)) != trace["sha256"] {
                return Err(unqualified("qualification_evidence_changed"));
            }
        }
        if report["probe_hashes"].as_array().is_none_or(Vec::is_empty) {
            return Err(unqualified("qualification_evidence"));
        }
        if self.context_profile.tokenizer_id != "utf8-byte-upper-bound-v1"
            || self.context_profile.tokenizer_hash != Hash256::sha256(b"utf8-byte-upper-bound-v1")
            || self.context_profile.proof_ref.is_empty()
        {
            return Err(unqualified("tokenizer_unqualified"));
        }
        super::request_accounting::enforce_profile_caps(&self.context_profile)?;
        if report["context_profile_hash"] != json!(canonical_hash(&self.context_profile)?) {
            return Err(unqualified("context_profile_changed"));
        }
        if super::sandbox::qualified_oci_profile_hash(&self.oci, key)? != key.plan_hash {
            return Err(unqualified("qualification_plan_changed"));
        }
        Ok(())
    }
}

pub(crate) fn shared_core_hashes() -> Value {
    let source = include_str!("../acp/connection.rs");
    let classifier = source
        .find("\nfn air_session_failure(")
        .and_then(|start| {
            source[start..]
                .find("\n/// Strict SemVer floor check:")
                .map(|end| &source[start..start + end])
        })
        // Moving or renaming a marker must broaden the binding, never omit it.
        .unwrap_or(source);
    // Bind the actual admission, durable completion/cleanup and privacy
    // implementations, not only the profile and ACP transport adapters.
    // Full external modules intentionally invalidate certificates on any
    // source change: an unrelated edit is safer than an omitted privacy path.
    let sources: &[(&str, &[u8])] = &[
        ("acceptance", include_bytes!("acceptance.rs")),
        ("acp_connection", include_bytes!("../acp/connection.rs")),
        (
            "acp_host_tools_policy",
            include_bytes!("../acp/host_tools_policy.rs"),
        ),
        ("acp_launch", include_bytes!("../acp/agent_process.rs")),
        ("acp_manager", include_bytes!("../acp/manager.rs")),
        ("actor", include_bytes!("actor.rs")),
        ("api", include_bytes!("api.rs")),
        ("authorization", include_bytes!("authorization.rs")),
        ("budget_ledger", include_bytes!("budget_ledger.rs")),
        (
            "canonical",
            include_bytes!("../../roundtable-protocol/src/canonical.rs"),
        ),
        ("capabilities", include_bytes!("capabilities.rs")),
        ("clock", include_bytes!("clock.rs")),
        ("companion", include_bytes!("companion.rs")),
        ("companion_entry", include_bytes!("../bin/codeg_mcp.rs")),
        (
            "companion_transport",
            include_bytes!("companion/transport.rs"),
        ),
        ("connection_purpose", include_bytes!("../auto_title/mod.rs")),
        ("control", include_bytes!("control.rs")),
        (
            "conversation_discovery",
            include_bytes!("../commands/conversations.rs"),
        ),
        (
            "delivery_encoder",
            include_bytes!("../../roundtable-protocol/src/context.rs"),
        ),
        ("diagnostics", include_bytes!("diagnostics.rs")),
        ("events", include_bytes!("events.rs")),
        ("feature_gate", include_bytes!("feature_gate.rs")),
        ("gateway", include_bytes!("gateway.rs")),
        ("ingress", include_bytes!("ingress.rs")),
        ("installed_runtime", include_bytes!("installed_runtime.rs")),
        (
            "internal_sessions",
            include_bytes!("../auto_title/internal_sessions.rs"),
        ),
        ("linux_oci", include_bytes!("sandbox/linux_oci.rs")),
        ("live_gateway", include_bytes!("live_gateway.rs")),
        ("live_runtime", include_bytes!("live_runtime.rs")),
        ("maintenance", include_bytes!("maintenance.rs")),
        ("mcp", include_bytes!("mcp.rs")),
        ("objects", include_bytes!("objects.rs")),
        ("owned_runtime", include_bytes!("owned_runtime.rs")),
        ("ownership", include_bytes!("ownership.rs")),
        ("paging", include_bytes!("paging.rs")),
        ("product", include_bytes!("product.rs")),
        (
            "protocol_admission",
            include_bytes!("../../roundtable-protocol/src/admission.rs"),
        ),
        (
            "protocol_budget",
            include_bytes!("../../roundtable-protocol/src/budget.rs"),
        ),
        (
            "protocol_completion",
            include_bytes!("../../roundtable-protocol/src/completion.rs"),
        ),
        (
            "protocol_dto",
            include_bytes!("../../roundtable-protocol/src/dto.rs"),
        ),
        (
            "protocol_lib",
            include_bytes!("../../roundtable-protocol/src/lib.rs"),
        ),
        (
            "protocol_model",
            include_bytes!("../../roundtable-protocol/src/model.rs"),
        ),
        (
            "protocol_projection",
            include_bytes!("../../roundtable-protocol/src/projection.rs"),
        ),
        (
            "protocol_strategy",
            include_bytes!("../../roundtable-protocol/src/strategy.rs"),
        ),
        (
            "protocol_usage",
            include_bytes!("../../roundtable-protocol/src/usage.rs"),
        ),
        ("qualification", include_bytes!("qualification.rs")),
        (
            "qualification_linux",
            include_bytes!("qualification_linux.rs"),
        ),
        (
            "qualification_probe",
            include_bytes!("qualification_probe.rs"),
        ),
        (
            "qualification_profiles",
            include_bytes!("qualification_profiles.rs"),
        ),
        ("recovery", include_bytes!("recovery.rs")),
        ("registry", include_bytes!("registry.rs")),
        ("relay", include_bytes!("relay.rs")),
        (
            "request_accounting",
            include_bytes!("request_accounting.rs"),
        ),
        ("resources", include_bytes!("resources.rs")),
        ("rollout", include_bytes!("rollout.rs")),
        (
            "roundtable_commands",
            include_bytes!("../commands/roundtable.rs"),
        ),
        (
            "roundtable_http",
            include_bytes!("../web/handlers/roundtable.rs"),
        ),
        ("runtime", include_bytes!("runtime.rs")),
        ("sandbox", include_bytes!("sandbox.rs")),
        ("schema", include_bytes!("schema.rs")),
        ("service", include_bytes!("service.rs")),
        ("snapshot", include_bytes!("snapshot.rs")),
        ("store", include_bytes!("store.rs")),
        ("tool_core", include_bytes!("tool_core.rs")),
        ("usage", include_bytes!("usage.rs")),
        (
            "validator",
            include_bytes!("../../roundtable-protocol/src/validation.rs"),
        ),
        ("web_event_bridge", include_bytes!("../web/event_bridge.rs")),
    ];
    let mut hashes = serde_json::Map::new();
    hashes.insert(
        "service_failure_classifier".into(),
        json!(Hash256::sha256(classifier.as_bytes())),
    );
    for (name, bytes) in sources {
        hashes.insert((*name).into(), json!(Hash256::sha256(bytes)));
    }
    Value::Object(hashes)
}
fn bounded_read(path: &Path, limit: u64) -> RtResult<Vec<u8>> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).map_err(|_| unqualified("qualification_artifact_missing"))?;
    if file
        .metadata()
        .map_err(|_| unqualified("qualification_artifact_missing"))?
        .len()
        > limit
    {
        return Err(unqualified("qualification_artifact_size"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unqualified("qualification_artifact_missing"))?;
    if bytes.len() as u64 > limit {
        return Err(unqualified("qualification_artifact_size"));
    }
    Ok(bytes)
}
fn unqualified(reason: &'static str) -> roundtable_protocol::RtError {
    rt_error(ErrorCode::CapabilityUnqualified, reason)
}

fn verify_runtime_contract(
    report: &Value,
    providers: &Value,
    os_version: &str,
    kernel: &str,
) -> RtResult<()> {
    if report["provider_bindings_hash"] != json!(canonical_hash(providers)?) {
        return Err(unqualified("provider_bindings_changed"));
    }
    if report["profile"]["os_name"] != "linux"
        || report["profile"]["os_version"] != os_version
        || report["host_kernel_release"] != kernel
        || report["host_arch"] != std::env::consts::ARCH
    {
        return Err(unqualified("host_os_changed"));
    }
    Ok(())
}
/// Verifies individual production report bindings, never grants a certificate.
#[cfg(any(test, feature = "test-utils"))]
pub fn verify_runtime_contract_fixture(
    report: &Value,
    providers: &Value,
    os_version: &str,
    kernel: &str,
) -> RtResult<()> {
    verify_runtime_contract(report, providers, os_version, kernel)
}
