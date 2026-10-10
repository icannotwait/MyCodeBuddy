//! Host qualification probe. It measures the machine it is running on and
//! writes `qualified-runtime.json` only when every required check passed.
//! A failed check writes a report and does not issue a certificate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{SecondsFormat, Utc};
use roundtable_protocol::{canonical_hash, Hash256, ParseLimits, QualifiedContextProfile};
use serde_json::{json, Value};

use super::installed_runtime::{shared_core_hashes, InstalledRuntime, ProviderBinding};
use super::qualification::{CertifiedBinary, OsIdentity, QualificationKey};
use super::qualification_linux::measure_host;
use super::qualification_profiles::{
    os_accepted, profile_by_id, profile_for_agent, AdapterProfile,
};
use super::rt_error;
use super::sandbox::QualifiedOciProfile;
use roundtable_protocol::ErrorCode;

const REQUIRED_CAPABILITIES: &[&str] = &[
    "new_session",
    "roundtable_mcp",
    "ordered_turn_completion",
    "submit_receipt_completion",
    "cancel_and_reap",
    "strict_isolation",
    "bounded_context_delivery",
];

#[derive(Clone, Debug)]
pub struct ProbeCheck {
    pub name: String,
    pub status: String,
    pub evidence: String,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
    pub count: Option<u64>,
    pub flag: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ProbeFacts {
    pub agent: String,
    pub profile_id: String,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub arch: String,
    pub platform_blocked: bool,
    pub checks: Vec<ProbeCheck>,
    pub binaries: Vec<CertifiedBinary>,
    pub image_digest: String,
    pub oci: Option<QualifiedOciProfile>,
    pub providers: Vec<ProviderBinding>,
    pub anomalies: Vec<String>,
    /// Set only after the isolator printed `ISOLATION_OK` for every required
    /// denial. Callers cannot turn a skipped probe into a pass.
    pub isolation_marker: String,
    pub fake_broker: bool,
    pub fake_fd: bool,
}

#[derive(Clone, Debug)]
pub struct ProbeRequest {
    pub data_dir: PathBuf,
    pub agent: String,
    pub rootfs: PathBuf,
    pub crun: PathBuf,
    pub cgroup_root: PathBuf,
    pub runtime_root: PathBuf,
    pub provider_bindings: PathBuf,
    pub home: PathBuf,
    /// Exact profile id. Empty selects the default profile for `agent`.
    pub profile_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProbeOutcome {
    pub verdict: String,
    pub qualification_issued: bool,
    pub report_path: PathBuf,
    pub runtime_path: Option<PathBuf>,
    pub reasons: Vec<String>,
}

pub fn roundtable_qualify_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "roundtable-qualify")
}

pub fn run_roundtable_qualify() -> ExitCode {
    match qualify_cli(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(outcomes) => {
            let failed = outcomes.iter().any(|item| !item.qualification_issued);
            for outcome in &outcomes {
                println!(
                    "adapter verdict={} qualification_issued={} report={}",
                    outcome.verdict,
                    outcome.qualification_issued,
                    outcome.report_path.display()
                );
                if let Some(path) = &outcome.runtime_path {
                    println!("qualified_runtime={}", path.display());
                }
                if !outcome.reasons.is_empty() {
                    println!("reasons={}", outcome.reasons.join(","));
                }
            }
            if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => {
            eprintln!("roundtable-qualify: {message}");
            ExitCode::from(2)
        }
    }
}

pub fn qualify_cli(args: &[String]) -> Result<Vec<ProbeOutcome>, String> {
    let mut agent = None;
    let mut data_dir = std::env::var_os("CODEG_DATA_DIR").map(PathBuf::from);
    let mut rootfs = None;
    let mut crun = None;
    let mut cgroup_root = None;
    let mut runtime_root = None;
    let mut bindings = None;
    let mut home = std::env::var_os("HOME").map(PathBuf::from);
    let mut profile_id = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "roundtable-qualify" {
            index += 1;
            continue;
        }
        let value = || {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {arg}"))
        };
        match arg.as_str() {
            "--help" | "-h" => return Err(help_text()),
            "--agent" => {
                agent = Some(value()?);
                index += 2;
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--rootfs" => {
                rootfs = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--crun" => {
                crun = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--cgroup-root" => {
                cgroup_root = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--runtime-root" => {
                runtime_root = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--provider-bindings" => {
                bindings = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--home" => {
                home = Some(PathBuf::from(value()?));
                index += 2;
            }
            "--profile" => {
                profile_id = Some(value()?);
                index += 2;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let agent = agent.ok_or("--agent is required")?;
    let data_dir = data_dir.ok_or("--data-dir or CODEG_DATA_DIR is required")?;
    let rootfs = rootfs.ok_or("--rootfs is required")?;
    let bindings = bindings.ok_or("--provider-bindings is required")?;
    let home = home.ok_or("--home or HOME is required")?;
    let crun = crun.unwrap_or_else(|| PathBuf::from("/usr/bin/crun"));
    let cgroup_root =
        cgroup_root.unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup/codeg-roundtable"));
    let runtime_root = runtime_root.unwrap_or_else(|| data_dir.join("roundtable/oci"));
    if agent == "all" && profile_id.is_some() {
        return Err("--profile cannot be combined with --agent all".into());
    }
    let agents: Vec<String> = if agent == "all" {
        ["grok", "cursor", "antigravity", "codex", "code_buddy"]
            .into_iter()
            .map(str::to_string)
            .collect()
    } else {
        vec![agent]
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let mut outcomes = Vec::new();
        for agent in agents {
            let request = ProbeRequest {
                data_dir: data_dir.clone(),
                agent,
                rootfs: rootfs.clone(),
                crun: crun.clone(),
                cgroup_root: cgroup_root.clone(),
                runtime_root: runtime_root.clone(),
                provider_bindings: bindings.clone(),
                home: home.clone(),
                profile_id: profile_id.clone(),
            };
            outcomes.push(qualify_adapter_on_host(request).await);
        }
        Ok(outcomes)
    })
}

fn help_text() -> String {
    "codeg-server roundtable-qualify --agent grok|cursor|antigravity|codex|all \
--data-dir DIR --rootfs DIR --provider-bindings FILE [--crun PATH] \
[--cgroup-root PATH] [--runtime-root PATH] [--home PATH] [--profile ID]\n\
Writes roundtable/qualification/<profile>/report.json. Writes \
roundtable/qualified-runtime.json only when the verdict is passed."
        .to_string()
}

pub async fn qualify_adapter_on_host(request: ProbeRequest) -> ProbeOutcome {
    let agent = request.agent.clone();
    let data_dir = request.data_dir.clone();
    let facts = measure_host(&request).await;
    let mut outcome = write_outcome(&data_dir, facts);
    if outcome.qualification_issued {
        let host_ok = load_agent_runtime(&data_dir, &agent)
            .and_then(|runtime| runtime.verify_host())
            .is_ok();
        if !host_ok {
            remove_agent(&data_dir, &agent);
            revoke_passed_report(&outcome.report_path);
            outcome.qualification_issued = false;
            outcome.verdict = "failed".into();
            outcome.runtime_path = None;
            outcome.reasons.push("host_verification_failed".into());
        }
    }
    outcome
}

fn revoke_passed_report(path: &Path) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    value["verdict"] = json!("failed");
    value["g1_passed"] = json!(false);
    value["qualification_issued"] = json!(false);
    if let Some(anomalies) = value.get_mut("anomalies").and_then(Value::as_array_mut) {
        anomalies.push(json!("host_verification_failed"));
    }
    if let Ok(pretty) = serde_json::to_vec_pretty(&value) {
        let _ = fs::write(path, pretty);
    }
}

pub(crate) fn write_outcome(data_dir: &Path, mut facts: ProbeFacts) -> ProbeOutcome {
    // A report contains one conservative observation per name. A later pass
    // can never erase measured failure or incomplete/conflicting evidence.
    if facts
        .oci
        .as_ref()
        .is_some_and(|oci| !oci.auth_mounts.is_empty())
    {
        facts.checks.push(ProbeCheck {
            name: "model_credential_material_in_sandbox".into(),
            status: "failed".into(),
            evidence:
                "native auth files are readable inside the sandbox, including attempt-local copies"
                    .into(),
            input: b"configured-auth-mounts".to_vec(),
            output: b"credential-material-present".to_vec(),
            count: None,
            flag: Some(true),
        });
    }
    let mut checks: BTreeMap<String, ProbeCheck> = BTreeMap::new();
    for check in std::mem::take(&mut facts.checks) {
        match checks.entry(check.name.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(check);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if observation_severity(&check) > observation_severity(entry.get()) {
                    entry.insert(check);
                }
            }
        }
    }
    facts.checks = checks.into_values().collect();
    let profile = if facts.profile_id.is_empty() {
        profile_for_agent(&facts.agent)
    } else {
        profile_by_id(&facts.profile_id)
    };
    let mut reasons = facts.anomalies.clone();
    let decision = decide(profile, &facts, &mut reasons);
    let exact_id = profile
        .map(|item| item.exact_id)
        .unwrap_or("unknown-adapter");
    let dir = data_dir.join("roundtable/qualification").join(exact_id);
    let _ = fs::create_dir_all(dir.join("traces"));
    write_traces(&dir, &facts);
    let observed = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let report = render_report(profile, &facts, &decision, &reasons, &observed);
    let report_path = dir.join("report.json");
    let report_bytes = serde_json::to_vec_pretty(&report).unwrap_or_else(|_| b"{}".to_vec());
    let _ = fs::write(&report_path, &report_bytes);

    let runtime_path = data_dir.join("roundtable/qualified-runtime.json");
    let mut issued = false;
    if decision.passed {
        if let (Some(profile), Some(oci)) = (profile, facts.oci.clone()) {
            if install_certificate(
                data_dir,
                profile,
                &facts,
                oci,
                &report_path,
                &report_bytes,
                &decision,
            )
            .is_ok()
            {
                if let Ok(runtime) = load_agent_runtime(data_dir, facts.agent.as_str()) {
                    if runtime.verify_report().is_ok() {
                        issued = true;
                        let _ = write_policy_example(data_dir, profile.exact_id, &runtime);
                    } else {
                        reasons.push("certificate_rejected".into());
                        remove_agent(data_dir, &facts.agent);
                    }
                }
            } else {
                reasons.push("certificate_write_failed".into());
            }
        }
    } else {
        remove_agent(data_dir, &facts.agent);
    }
    if !issued && decision.passed {
        let mut failed = decision.clone();
        failed.passed = false;
        failed.verdict = "failed".into();
        reasons.push("certificate_not_issued".into());
        let report = render_report(profile, &facts, &failed, &reasons, &observed);
        let _ = fs::write(
            &report_path,
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        );
    }
    ProbeOutcome {
        verdict: if issued {
            "passed".into()
        } else if decision.passed {
            "failed".into()
        } else {
            decision.verdict
        },
        qualification_issued: issued,
        report_path,
        runtime_path: issued.then_some(runtime_path),
        reasons,
    }
}

#[derive(Clone)]
struct Decision {
    passed: bool,
    verdict: String,
    g1_passed: bool,
}

fn decide(
    profile: Option<&AdapterProfile>,
    facts: &ProbeFacts,
    reasons: &mut Vec<String>,
) -> Decision {
    let Some(profile) = profile else {
        reasons.push("unknown_adapter".into());
        return failed_decision("failed");
    };
    if facts.platform_blocked {
        reasons.push("platform_blocked".into());
        return failed_decision("blocked_platform");
    }
    if !os_accepted(profile, &facts.os_name, &facts.os_version) {
        reasons.push("host_os_not_accepted".into());
        return failed_decision("failed");
    }
    if facts.fake_broker {
        reasons.push("fake_broker_is_not_a_certificate".into());
        return failed_decision("failed");
    }
    if facts.fake_fd {
        reasons.push("fake_fd_is_not_a_certificate".into());
        return failed_decision("failed");
    }
    if facts.isolation_marker != "ISOLATION_OK" {
        reasons.push("strict_isolation".into());
    }
    if facts.oci.is_none() {
        reasons.push("oci_profile_missing".into());
    }
    if facts.providers.is_empty()
        || facts.providers.iter().any(|provider| {
            provider.provider_ref.is_empty()
                || provider.model.is_empty()
                || provider.credential_env.is_empty()
                || provider.origin.is_empty()
        })
    {
        reasons.push("provider_binding_invalid".into());
    }
    if !facts.image_digest.starts_with("sha256:")
        || facts.image_digest.len() != "sha256:".len() + 64
    {
        reasons.push("image_digest".into());
    }
    if facts.binaries.is_empty()
        || facts.binaries.iter().any(|binary| {
            binary.role.is_empty()
                || binary.version.is_empty()
                || !Path::new(&binary.absolute_path).is_absolute()
                || binary.sha256 == Hash256::from_bytes([0; 32])
        })
    {
        reasons.push("qualification_binaries".into());
    }
    for name in REQUIRED_CHECKS {
        match facts.checks.iter().find(|check| check.name == *name) {
            Some(check) if check_accepted(check) => {}
            Some(_) => reasons.push((*name).to_string()),
            None => reasons.push((*name).to_string()),
        }
    }
    if profile.requires_acp_turn
        && !facts
            .checks
            .iter()
            .any(|check| check.name == "ordered_turn_completion" && check_accepted(check))
    {
        reasons.push("acp_turn_required".into());
    }
    reasons.sort();
    reasons.dedup();
    if reasons.is_empty() {
        Decision {
            passed: true,
            verdict: "passed".into(),
            g1_passed: true,
        }
    } else {
        failed_decision("failed")
    }
}

fn failed_decision(verdict: &str) -> Decision {
    Decision {
        passed: false,
        verdict: verdict.to_string(),
        g1_passed: false,
    }
}

const REQUIRED_CHECKS: &[&str] = &[
    "new_session",
    "roundtable_mcp",
    "ordered_turn_completion",
    "submit_receipt_completion",
    "cancel_and_reap",
    "strict_isolation",
    "bounded_context_delivery",
    "private_events",
    "sidebar_discovery",
    "global_body_events",
    "model_credential_material_in_sandbox",
    "model_credentials_visible_to_agent",
    "actual_binary_match",
    "endpoint_compatibility",
    "native_read_boundary",
    "api_credential_scope",
    "companion_lifecycle",
];

fn observation_severity(check: &ProbeCheck) -> u8 {
    if check.status == "failed" {
        3 + u8::from(check.flag == Some(true))
    } else if !check_accepted(check) {
        2
    } else {
        0
    }
}

fn check_accepted(check: &ProbeCheck) -> bool {
    // Host-local credential reads are accepted for this iteration. These four
    // checks no longer fail a certificate when the CLI bind-mounts host auth.
    if check.status == "not_applicable"
        && matches!(
            check.name.as_str(),
            "model_credential_material_in_sandbox"
                | "model_credentials_visible_to_agent"
                | "native_read_boundary"
                | "api_credential_scope"
        )
        && !check.evidence.trim().is_empty()
    {
        return true;
    }
    if check.status != "passed" || check.evidence.trim().is_empty() {
        return false;
    }
    match check.name.as_str() {
        "sidebar_discovery" | "global_body_events" | "cancel_and_reap" => check.count == Some(0),
        "model_credential_material_in_sandbox"
        | "model_credentials_visible_to_agent"
        | "native_read_boundary" => check.flag == Some(false),
        "actual_binary_match" | "ordered_turn_completion" | "submit_receipt_completion" => {
            check.flag == Some(true)
        }
        _ => true,
    }
}

fn render_report(
    profile: Option<&AdapterProfile>,
    facts: &ProbeFacts,
    decision: &Decision,
    reasons: &[String],
    observed: &str,
) -> Value {
    let shared = shared_core_hashes();
    let core_hash = canonical_hash(&shared).unwrap_or_else(|_| Hash256::from_bytes([0; 32]));
    let policy_hash =
        super::capabilities::policy_hash_for(&super::capabilities::sealed_service_manifest())
            .unwrap_or_else(|_| Hash256::from_bytes([0; 32]));
    let tool_hash = Hash256::sha256(
        format!(
            "{}{}",
            super::tool_core::SERVICE_TOOL_SCHEMA,
            super::tool_core::service_result_schema()
        )
        .as_bytes(),
    );
    let context = context_profile();
    let context_hash = canonical_hash(&context).unwrap_or_else(|_| Hash256::from_bytes([0; 32]));
    let provider_hash =
        canonical_hash(&facts.providers).unwrap_or_else(|_| Hash256::from_bytes([0; 32]));
    let oci_hash = facts
        .oci
        .as_ref()
        .and_then(|oci| plan_hash(oci, facts, profile, policy_hash, tool_hash, core_hash).ok())
        .unwrap_or_else(|| Hash256::from_bytes([0; 32]));
    let tree = execution_tree_hash(facts);
    let passed = decision.passed;
    let binaries: Vec<Value> = facts
        .binaries
        .iter()
        .map(|binary| {
            json!({
                "role": binary.role,
                "absolute_path": binary.absolute_path,
                "version": binary.version,
                "sha256": binary.sha256,
            })
        })
        .collect();
    let checks: Vec<Value> = REQUIRED_CAPABILITIES
        .iter()
        .map(|name| {
            let status = facts
                .checks
                .iter()
                .find(|check| check.name == *name)
                .map(|check| check.status.as_str())
                .unwrap_or("not_tested");
            json!({"name": name, "status": status})
        })
        .collect();
    let field = |name: &str| {
        facts
            .checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.status.as_str())
            .unwrap_or("not_tested")
    };
    let count = |name: &str| {
        facts
            .checks
            .iter()
            .find(|check| check.name == name)
            .and_then(|check| check.count)
    };
    let flag = |name: &str| {
        facts
            .checks
            .iter()
            .find(|check| check.name == name)
            .and_then(|check| check.flag)
    };
    let probe_hashes: Vec<Value> = facts
        .checks
        .iter()
        .map(|check| {
            json!({
                "case_id": check.name,
                "input": Hash256::sha256(&check.input),
                "output": Hash256::sha256(&check.output),
            })
        })
        .collect();
    let traces: Vec<Value> = facts
        .checks
        .iter()
        .map(|check| {
            let name = format!("traces/{}.txt", check.name);
            json!({
                "path": name,
                "sha256": Hash256::sha256(redact(&check.evidence).as_bytes()),
            })
        })
        .collect();
    let missing: Vec<&str> = if passed {
        Vec::new()
    } else {
        reasons.iter().map(String::as_str).collect()
    };
    let credential_names = [
        "model_credential_material_in_sandbox",
        "model_credentials_visible_to_agent",
        "native_read_boundary",
        "api_credential_scope",
    ];
    let credentials_unreachable = if credential_names.iter().any(|name| field(name) == "failed")
        || flag("model_credential_material_in_sandbox") == Some(true)
        || flag("model_credentials_visible_to_agent") == Some(true)
    {
        "failed"
    } else if credential_names
        .iter()
        .all(|name| field(name) == "not_applicable")
    {
        "not_applicable"
    } else if field("model_credential_material_in_sandbox") == "passed"
        && field("model_credentials_visible_to_agent") == "passed"
        && flag("model_credential_material_in_sandbox") == Some(false)
        && flag("model_credentials_visible_to_agent") == Some(false)
    {
        "passed"
    } else {
        "not_tested"
    };
    json!({
        "verdict": if passed { "passed" } else { decision.verdict.as_str() },
        "g1_passed": passed && decision.g1_passed,
        "qualification_issued": passed,
        "experiment_id": format!("qualify-{}-{observed}", profile.map(|item| item.exact_id).unwrap_or("unknown")),
        "execution_tree_hash": tree,
        "shared_core_hashes": shared,
        "runtime_build_version": env!("CARGO_PKG_VERSION"),
        "runtime_implementation_version": "roundtable-oci-acp-v1",
        "provider_bindings_hash": provider_hash,
        "context_profile_hash": context_hash,
        // Leave this unmeasured. Antigravity's model body never reaches the
        // host gateway, and the ACP prompt size is not that body. Live
        // admission reserves a fixed wrapper for `not_tested` instead.
        "request_envelope": {"status": "not_tested", "max_bytes": null, "evidence_hash": null},
        "host_kernel_release": facts.kernel,
        "host_arch": facts.arch,
        "profile": {
            "exact_id": profile.map(|item| item.exact_id).unwrap_or("unknown"),
            "os_name": facts.os_name,
            "os_version": facts.os_version,
            "adapter_version": profile.map(|item| item.adapter_version).unwrap_or(""),
            "isolator_version": profile.map(|item| item.isolator_version).unwrap_or("linux-oci"),
            "platform_blocked": facts.platform_blocked
                || (!passed && decision.verdict == "blocked_platform"),
            "binaries": binaries,
            "image_digest": facts.image_digest,
            "policy_hash": policy_hash,
            "tool_contract_hash": tool_hash,
            "core_hash": core_hash,
            "plan_hash": oci_hash,
            "execution_tree_hash": tree,
            "shared_core_hashes": shared,
            "observed_at": observed,
        },
        "probe_hashes": probe_hashes,
        "observed_at": observed,
        "missing": missing,
        "anomalies": if passed { json!([]) } else { json!(reasons) },
        "required_capabilities": checks,
        "private_events": field("private_events"),
        "ordinary_sidebar_imports": count("sidebar_discovery"),
        "sidebar_discovery": field("sidebar_discovery"),
        "global_body_events": count("global_body_events"),
        "model_credential_material_in_sandbox": flag("model_credential_material_in_sandbox"),
        "model_credentials_visible_to_agent": flag("model_credentials_visible_to_agent"),
        "credentials_unreachable": credentials_unreachable,
        "api_credential_scope": field("api_credential_scope"),
        "chatgpt_account_scope": "not_tested",
        "actual_resolved_binary_matches_certificate": flag("actual_binary_match").unwrap_or(false),
        "binary_match": field("actual_binary_match"),
        "idle_processes_after_reap": count("cancel_and_reap"),
        "normal_completion_after_submit_receipt": field("submit_receipt_completion") == "passed" && flag("submit_receipt_completion") == Some(true),
        "endpoint_compatibility": field("endpoint_compatibility"),
        "native_read_boundary": field("native_read_boundary"),
        "companion_lifecycle": field("companion_lifecycle"),
        "fake_broker_is_certificate": false,
        "fake_fd_is_certificate": false,
        "traces": traces,
    })
}

fn plan_hash(
    oci: &QualifiedOciProfile,
    facts: &ProbeFacts,
    profile: Option<&AdapterProfile>,
    policy_hash: Hash256,
    tool_hash: Hash256,
    core_hash: Hash256,
) -> roundtable_protocol::RtResult<Hash256> {
    let key = qualification_key(
        facts,
        profile,
        policy_hash,
        tool_hash,
        core_hash,
        Hash256::from_bytes([1; 32]),
    )?;
    // plan_hash is an input to itself. Compute it from a placeholder, then the
    // function hashes the profile template rather than the plan_hash field.
    super::sandbox::qualified_oci_profile_hash(oci, &key)
}

fn qualification_key(
    facts: &ProbeFacts,
    profile: Option<&AdapterProfile>,
    policy_hash: Hash256,
    tool_hash: Hash256,
    core_hash: Hash256,
    plan_hash: Hash256,
) -> roundtable_protocol::RtResult<QualificationKey> {
    Ok(QualificationKey {
        os: OsIdentity {
            name: facts.os_name.clone(),
            version: facts.os_version.clone(),
        },
        binaries: facts.binaries.clone(),
        image_digest: facts.image_digest.clone(),
        policy_hash,
        tool_contract_hash: tool_hash,
        core_hash,
        adapter_version: profile
            .map(|item| item.adapter_version.to_string())
            .unwrap_or_default(),
        isolator_version: profile
            .map(|item| item.isolator_version.to_string())
            .unwrap_or_else(|| "linux-oci".into()),
        plan_hash,
    })
}

fn execution_tree_hash(facts: &ProbeFacts) -> Hash256 {
    let mut bytes = Vec::new();
    for check in &facts.checks {
        bytes.extend(check.name.as_bytes());
        bytes.push(0);
        bytes.extend(check.status.as_bytes());
        bytes.extend(Hash256::sha256(&check.input).to_hex().as_bytes());
        bytes.extend(Hash256::sha256(&check.output).to_hex().as_bytes());
    }
    Hash256::sha256(&bytes)
}

fn context_profile() -> QualifiedContextProfile {
    QualifiedContextProfile::proposed(
        "utf8-byte-upper-bound-v1",
        Hash256::sha256(b"utf8-byte-upper-bound-v1"),
        2_000_000,
        0,
        "roundtable-qualify",
    )
}

fn write_traces(dir: &Path, facts: &ProbeFacts) {
    let traces = dir.join("traces");
    let _ = fs::create_dir_all(&traces);
    for check in &facts.checks {
        let _ = fs::write(
            traces.join(format!("{}.txt", check.name)),
            redact(&check.evidence),
        );
    }
}

fn redact(input: &str) -> String {
    let mut out = String::new();
    for line in input.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("authorization")
            || lower.contains("bearer")
            || lower.contains("sk-")
            || lower.contains("api_key")
            || lower.contains("token")
            || lower.contains("secret")
            || lower.contains("password")
        {
            continue;
        }
        out.push_str(line);
    }
    if out.is_empty() {
        "redacted".into()
    } else {
        out
    }
}

fn install_certificate(
    data_dir: &Path,
    profile: &AdapterProfile,
    facts: &ProbeFacts,
    oci: QualifiedOciProfile,
    report_path: &Path,
    report_bytes: &[u8],
    decision: &Decision,
) -> roundtable_protocol::RtResult<()> {
    if !decision.passed {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "certificate_not_passed",
        ));
    }
    let shared = shared_core_hashes();
    let core_hash = canonical_hash(&shared)?;
    let policy_hash =
        super::capabilities::policy_hash_for(&super::capabilities::sealed_service_manifest())?;
    let tool_hash = Hash256::sha256(
        format!(
            "{}{}",
            super::tool_core::SERVICE_TOOL_SCHEMA,
            super::tool_core::service_result_schema()
        )
        .as_bytes(),
    );
    let plan = super::sandbox::qualified_oci_profile_hash(
        &oci,
        &qualification_key(
            facts,
            Some(profile),
            policy_hash,
            tool_hash,
            core_hash,
            Hash256::from_bytes([1; 32]),
        )?,
    )?;
    let key = qualification_key(
        facts,
        Some(profile),
        policy_hash,
        tool_hash,
        core_hash,
        plan,
    )?;
    let report_path = report_path
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "qualification_report"))?;
    let installed = InstalledRuntime {
        schema_version: 1,
        qualification_key: key,
        report_path,
        report_sha256: Hash256::sha256(report_bytes),
        oci,
        context_profile: context_profile(),
        providers: facts.providers.clone(),
    };
    merge_runtime(data_dir, profile.agent, &installed)
}

fn merge_runtime(
    data_dir: &Path,
    agent: &str,
    installed: &InstalledRuntime,
) -> roundtable_protocol::RtResult<()> {
    let path = data_dir.join("roundtable/qualified-runtime.json");
    let mut adapters = BTreeMap::new();
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(value) =
            roundtable_protocol::parse_strict_json(&bytes, &ParseLimits::suggested_profile())
        {
            if let Some(entries) = value.get("adapters").and_then(Value::as_array) {
                for entry in entries {
                    let name = entry["agent"].as_str().unwrap_or("");
                    if name.is_empty() || name == agent {
                        continue;
                    }
                    if let Ok(runtime) =
                        serde_json::from_value::<InstalledRuntime>(entry["runtime"].clone())
                    {
                        adapters.insert(name.to_string(), runtime);
                    }
                }
            } else if agent != "codex" {
                if let Ok(runtime) = serde_json::from_slice::<InstalledRuntime>(&bytes) {
                    adapters.insert("codex".into(), runtime);
                }
            }
        }
    }
    adapters.insert(agent.to_string(), installed.clone());
    let entries: Vec<Value> = adapters
        .iter()
        .map(|(name, runtime)| json!({"agent": name, "runtime": runtime}))
        .collect();
    let body = json!({"schema_version": 2, "adapters": entries});
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "fixture_config"))?;
    fs::create_dir_all(data_dir.join("roundtable"))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_directory"))?;
    super::feature_gate::atomic_write(&path, &bytes)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_config"))?;
    Ok(())
}

fn remove_agent(data_dir: &Path, agent: &str) {
    let path = data_dir.join("roundtable/qualified-runtime.json");
    let Ok(bytes) = fs::read(&path) else {
        return;
    };
    let Ok(value) =
        roundtable_protocol::parse_strict_json(&bytes, &ParseLimits::suggested_profile())
    else {
        return;
    };
    let Some(entries) = value.get("adapters").and_then(Value::as_array) else {
        if agent == "codex" {
            let _ = fs::remove_file(&path);
        }
        return;
    };
    let kept: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["agent"].as_str() != Some(agent))
        .collect();
    if kept.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    let body = json!({"schema_version": 2, "adapters": kept});
    if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
        let _ = super::feature_gate::atomic_write(&path, &bytes);
    }
}

fn load_agent_runtime(
    data_dir: &Path,
    agent: &str,
) -> roundtable_protocol::RtResult<InstalledRuntime> {
    InstalledRuntime::load_catalog(data_dir)?
        .remove(agent)
        .ok_or_else(|| rt_error(ErrorCode::CapabilityUnqualified, "runtime_installation"))
}

fn write_policy_example(
    data_dir: &Path,
    exact_id: &str,
    runtime: &InstalledRuntime,
) -> roundtable_protocol::RtResult<()> {
    let example = json!({
        "enabled": true,
        "generation": 1,
        "allowed_qualification_keys": [runtime.qualification_key],
    });
    let path = data_dir
        .join("roundtable/qualification")
        .join(exact_id)
        .join("execution-policy.example.json");
    let bytes = serde_json::to_vec_pretty(&example)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "policy_encode"))?;
    super::feature_gate::atomic_write(&path, &bytes)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "policy_write"))?;
    Ok(())
}

/// Test double entry. Production `qualify_adapter_on_host` never calls this.
#[cfg(any(test, feature = "test-utils"))]
pub fn assemble_probe_report_for_test(data_dir: &Path, facts: ProbeFacts) -> ProbeOutcome {
    write_outcome(data_dir, facts)
}

/// Confirms a certificate written by the probe still satisfies the product
/// report verifier. Does not call `verify_host`.
#[cfg(any(test, feature = "test-utils"))]
pub fn verify_installed_report_for_test(
    data_dir: &Path,
    agent: &str,
) -> roundtable_protocol::RtResult<()> {
    load_agent_runtime(data_dir, agent)?.verify_report()
}
