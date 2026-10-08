//! P08 adapter-qualification report checker.
//!
//! This module does not spawn a CLI, install a runtime, log in, or call a
//! model. [`qualify_adapter`] only judges an evidence record. `passed` is
//! possible only when every required check has real evidence. A label, a fake
//! broker, or a fake FD is not that evidence.
//!
//! The product probe is tested separately. This host's archived verdict
//! is `blocked_platform` / `not_tested` and is not G1.

use std::collections::BTreeSet;
use std::path::Path;

use roundtable_protocol::{Hash256, RtResult};
use serde::{Deserialize, Serialize};

const CAPABILITIES: &[&str] = &[
    "new_session",
    "roundtable_mcp",
    "ordered_turn_completion",
    "cancel_and_reap",
    "strict_isolation",
    "bounded_context_delivery",
];

/// Checks that must be evidenced before a report can pass. ChatGPT account
/// scope is recorded separately and may stay `not_tested`.
const REQUIRED_CASES: &[&str] = &[
    "new_session",
    "roundtable_mcp",
    "ordered_turn_completion",
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
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    NotTested,
    Passed,
    Failed,
    Expired,
    BlockedPlatform,
    CapacityUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedCoreHashes {
    pub canonical: Option<Hash256>,
    pub delivery_encoder: Option<Hash256>,
    pub validator: Option<Hash256>,
    pub tool_core: Option<Hash256>,
    pub request_accounting: Option<Hash256>,
    pub ingress: Option<Hash256>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedBinary {
    pub role: String,
    pub absolute_path: Option<String>,
    pub version: Option<String>,
    pub sha256: Option<Hash256>,
}

/// Exact component set the report is about. Missing hashes stay absent.
/// They are not filled with a placeholder digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationProfile {
    pub exact_id: String,
    pub os_name: String,
    pub os_version: String,
    pub adapter_version: String,
    pub isolator_version: String,
    pub host_os_name: String,
    pub host_os_version: String,
    pub platform_blocked: bool,
    pub binaries: Vec<ReportedBinary>,
    pub image_digest: Option<String>,
    pub policy_hash: Option<Hash256>,
    pub tool_contract_hash: Option<Hash256>,
    pub core_hash: Option<Hash256>,
    pub plan_hash: Option<Hash256>,
    pub execution_tree_hash: Option<Hash256>,
    pub shared_core_hashes: SharedCoreHashes,
    pub observed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualificationApproval {
    pub experiment_id: String,
    pub allowed_models: Vec<String>,
    pub allowed_fixtures: Vec<String>,
    pub attempt_limit: u32,
    pub spend_limit: u64,
    pub valid_until: String,
    pub approved: bool,
    pub expired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualificationCase {
    pub id: String,
    pub status: CheckStatus,
    pub evidence: Option<String>,
    pub probe_input_hash: Option<Hash256>,
    pub probe_output_hash: Option<Hash256>,
    pub observed_at: Option<String>,
    pub trace: Option<String>,
    pub count: Option<u64>,
    pub flag: Option<bool>,
    pub fake_broker: bool,
    pub fake_fd: bool,
    pub uses_env_token: bool,
    pub mounted_socket: bool,
    pub eof_lifecycle: bool,
    pub anomaly: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCheck {
    pub name: String,
    pub status: CheckStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeHash {
    pub case_id: String,
    pub input: Hash256,
    pub output: Hash256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactedTrace {
    pub case_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationReport {
    pub verdict: CheckStatus,
    pub g1_passed: bool,
    pub qualification_issued: bool,
    pub experiment_id: Option<String>,
    pub execution_tree_hash: Option<Hash256>,
    pub shared_core_hashes: SharedCoreHashes,
    pub profile: QualificationProfile,
    pub probe_hashes: Vec<ProbeHash>,
    pub observed_at: Option<String>,
    pub missing: Vec<String>,
    pub anomalies: Vec<String>,
    pub required_capabilities: Vec<CapabilityCheck>,
    pub private_events: CheckStatus,
    pub ordinary_sidebar_imports: Option<u64>,
    pub sidebar_discovery: CheckStatus,
    pub global_body_events: Option<u64>,
    pub model_credential_material_in_sandbox: Option<bool>,
    pub model_credentials_visible_to_agent: Option<bool>,
    pub credentials_unreachable: CheckStatus,
    pub api_credential_scope: CheckStatus,
    pub chatgpt_account_scope: CheckStatus,
    pub actual_resolved_binary_matches_certificate: Option<bool>,
    pub binary_match: CheckStatus,
    pub idle_processes_after_reap: Option<u64>,
    pub normal_completion_after_submit_receipt: Option<bool>,
    pub endpoint_compatibility: CheckStatus,
    pub native_read_boundary: CheckStatus,
    pub companion_lifecycle: CheckStatus,
    pub fake_broker_is_certificate: bool,
    pub fake_fd_is_certificate: bool,
    pub traces: Vec<RedactedTrace>,
}

#[derive(Clone, Copy)]
enum Measure {
    Pass,
    CountZero,
    /// `Some(false)` is the passing measurement.
    FlagClear,
    /// `Some(true)` is the passing measurement.
    FlagSet,
}

struct CaseEval {
    status: CheckStatus,
    accepted: bool,
    count: Option<u64>,
    flag: Option<bool>,
    hard_fail: bool,
    capacity_unknown: bool,
    expired: bool,
}

pub async fn qualify_adapter(
    profile: &QualificationProfile,
    approval: &QualificationApproval,
    cases: &[QualificationCase],
) -> RtResult<QualificationReport> {
    Ok(judge(profile, approval, cases))
}

fn judge(
    profile: &QualificationProfile,
    approval: &QualificationApproval,
    cases: &[QualificationCase],
) -> QualificationReport {
    let mut stripped = false;
    let profile = redact_profile(profile, &mut stripped);
    let approval = redact_approval(approval, &mut stripped);
    let cases: Vec<QualificationCase> = cases
        .iter()
        .map(|case| redact_case(case, &mut stripped))
        .collect();

    let mut missing = Vec::new();
    let mut anomalies = BTreeSet::new();
    if stripped {
        anomalies.insert("private_material_stripped".to_string());
    }
    if profile.platform_blocked {
        anomalies.insert("platform_blocked".to_string());
        if profile.binaries.is_empty()
            || profile
                .binaries
                .iter()
                .all(|binary| published_hash(binary.sha256).is_none())
        {
            anomalies.insert("binary_hashes_not_measured".to_string());
        }
    }
    if approval.expired {
        anomalies.insert("approval_expired".to_string());
    }
    if !approval.approved {
        anomalies.insert("approval_absent".to_string());
    } else if !approval_is_valid(&approval) {
        anomalies.insert("approval_incomplete".to_string());
    }
    if cases.iter().any(|case| case.fake_broker) {
        anomalies.insert("fake_broker_is_not_a_certificate".to_string());
    }
    if cases.iter().any(|case| case.fake_fd) {
        anomalies.insert("fake_fd_is_not_a_certificate".to_string());
    }

    push_profile_gaps(&profile, &mut missing);
    if !approval_is_valid(&approval) {
        missing.push("approval".to_string());
    }

    let mut hard_fail = false;
    let mut capacity_unknown = false;
    let mut expired = approval.expired;
    let mut evals = Vec::new();
    for id in REQUIRED_CASES {
        let measure = measure_for(id);
        let eval = eval_case(&cases, id, measure, &mut anomalies);
        if eval.hard_fail {
            hard_fail = true;
        } else if !eval.accepted {
            missing.push((*id).to_string());
        }
        if eval.capacity_unknown {
            capacity_unknown = true;
        }
        if eval.expired {
            expired = true;
        }
        evals.push(eval);
    }
    let chatgpt = eval_recorded_scope(&cases, "chatgpt_account_scope", &mut anomalies);
    if !chatgpt.accepted && chatgpt.status != CheckStatus::NotTested {
        missing.push("chatgpt_account_scope".to_string());
    } else if chatgpt.status == CheckStatus::NotTested && !chatgpt.accepted {
        // A present not_tested record is kept. An absent scope is not.
        if !scope_is_recorded(&cases, "chatgpt_account_scope") {
            missing.push("chatgpt_account_scope".to_string());
        }
    }

    for case in &cases {
        if let Some(text) = &case.anomaly {
            if !text.is_empty() {
                anomalies.insert(text.clone());
            }
        }
    }

    let verdict = if hard_fail {
        CheckStatus::Failed
    } else if profile.platform_blocked {
        CheckStatus::BlockedPlatform
    } else if expired {
        CheckStatus::Expired
    } else if capacity_unknown {
        CheckStatus::CapacityUnknown
    } else if missing.is_empty() {
        CheckStatus::Passed
    } else {
        CheckStatus::NotTested
    };
    let passed = verdict == CheckStatus::Passed;
    let by_id = |name: &str| {
        REQUIRED_CASES
            .iter()
            .position(|id| *id == name)
            .map(|index| &evals[index])
            .expect("required case")
    };

    let required_capabilities = CAPABILITIES
        .iter()
        .map(|name| CapabilityCheck {
            name: (*name).to_string(),
            status: by_id(name).status,
        })
        .collect();
    let material = by_id("model_credential_material_in_sandbox");
    let visible = by_id("model_credentials_visible_to_agent");
    let sidebar = by_id("sidebar_discovery");
    let global_body = by_id("global_body_events");
    let binary = by_id("actual_binary_match");
    let cancel = by_id("cancel_and_reap");
    let ordered = by_id("ordered_turn_completion");
    let endpoint = by_id("endpoint_compatibility");
    let native = by_id("native_read_boundary");
    let private_events = by_id("private_events");
    let api_scope = by_id("api_credential_scope");
    let companion = by_id("roundtable_mcp");

    QualificationReport {
        verdict,
        g1_passed: passed,
        qualification_issued: passed,
        experiment_id: non_empty(&approval.experiment_id),
        execution_tree_hash: published_hash(profile.execution_tree_hash),
        shared_core_hashes: publish_cores(&profile.shared_core_hashes),
        profile: profile.clone(),
        probe_hashes: probe_hashes(&cases),
        observed_at: non_empty(profile.observed_at.as_deref().unwrap_or("")),
        missing,
        anomalies: anomalies.into_iter().collect(),
        required_capabilities,
        private_events: private_events.status,
        ordinary_sidebar_imports: sidebar.count,
        sidebar_discovery: sidebar.status,
        global_body_events: global_body.count,
        model_credential_material_in_sandbox: material.flag,
        model_credentials_visible_to_agent: visible.flag,
        credentials_unreachable: combine_status(material, visible),
        api_credential_scope: api_scope.status,
        chatgpt_account_scope: chatgpt.status,
        actual_resolved_binary_matches_certificate: binary.flag,
        binary_match: binary.status,
        idle_processes_after_reap: cancel.count,
        normal_completion_after_submit_receipt: ordered.flag,
        endpoint_compatibility: endpoint.status,
        native_read_boundary: native.status,
        companion_lifecycle: companion.status,
        fake_broker_is_certificate: false,
        fake_fd_is_certificate: false,
        traces: traces(&cases),
    }
}

fn measure_for(id: &str) -> Measure {
    match id {
        "sidebar_discovery" | "global_body_events" | "cancel_and_reap" => Measure::CountZero,
        "model_credential_material_in_sandbox"
        | "model_credentials_visible_to_agent"
        | "native_read_boundary" => Measure::FlagClear,
        "actual_binary_match" | "ordered_turn_completion" => Measure::FlagSet,
        _ => Measure::Pass,
    }
}

fn eval_case(
    cases: &[QualificationCase],
    id: &str,
    measure: Measure,
    anomalies: &mut BTreeSet<String>,
) -> CaseEval {
    let found = match locate(cases, id, anomalies) {
        Located::None => {
            return unevidenced(CheckStatus::NotTested);
        }
        Located::Duplicate => {
            return unevidenced(CheckStatus::NotTested);
        }
        Located::One(case) => case,
    };
    if found.fake_broker || found.fake_fd {
        return unevidenced(CheckStatus::NotTested);
    }
    if found.status == CheckStatus::CapacityUnknown && evidenced(found) {
        let mut eval = unevidenced(CheckStatus::CapacityUnknown);
        eval.capacity_unknown = true;
        return eval;
    }
    if found.status == CheckStatus::Expired && evidenced(found) {
        let mut eval = unevidenced(CheckStatus::Expired);
        eval.expired = true;
        return eval;
    }
    if !fully_measured(found) {
        return unevidenced(CheckStatus::NotTested);
    }
    if found.status == CheckStatus::Failed {
        return failed_measurement(found);
    }
    if id == "roundtable_mcp" && !companion_lifecycle_ok(found) {
        anomalies.insert("companion_lifecycle_incomplete".to_string());
        return unevidenced(CheckStatus::NotTested);
    }
    match measure {
        Measure::Pass => {
            if found.status == CheckStatus::Passed {
                accepted(found)
            } else {
                unevidenced(CheckStatus::NotTested)
            }
        }
        Measure::CountZero => match found.count {
            Some(0) if found.status == CheckStatus::Passed => accepted(found),
            Some(count) if count > 0 => failed_measurement(found),
            _ => unevidenced(CheckStatus::NotTested),
        },
        Measure::FlagClear => match found.flag {
            Some(false) if found.status == CheckStatus::Passed => accepted(found),
            Some(true) => failed_measurement(found),
            _ => unevidenced(CheckStatus::NotTested),
        },
        Measure::FlagSet => match found.flag {
            Some(true) if found.status == CheckStatus::Passed => accepted(found),
            Some(false) => failed_measurement(found),
            _ => unevidenced(CheckStatus::NotTested),
        },
    }
}

fn eval_recorded_scope(
    cases: &[QualificationCase],
    id: &str,
    anomalies: &mut BTreeSet<String>,
) -> CaseEval {
    match locate(cases, id, anomalies) {
        Located::None | Located::Duplicate => unevidenced(CheckStatus::NotTested),
        Located::One(case) if case.fake_broker || case.fake_fd => {
            unevidenced(CheckStatus::NotTested)
        }
        Located::One(case) if case.status == CheckStatus::NotTested && evidenced(case) => {
            unevidenced(CheckStatus::NotTested)
        }
        Located::One(case) if fully_measured(case) && case.status == CheckStatus::Passed => {
            accepted(case)
        }
        Located::One(case) if fully_measured(case) && case.status == CheckStatus::Failed => {
            failed_measurement(case)
        }
        Located::One(_) => unevidenced(CheckStatus::NotTested),
    }
}

fn scope_is_recorded(cases: &[QualificationCase], id: &str) -> bool {
    match cases.iter().filter(|case| case.id == id).count() {
        1 => cases.iter().any(|case| {
            case.id == id
                && case.status == CheckStatus::NotTested
                && evidenced(case)
                && !case.fake_broker
                && !case.fake_fd
        }),
        _ => false,
    }
}

enum Located<'a> {
    None,
    One(&'a QualificationCase),
    Duplicate,
}

fn locate<'a>(
    cases: &'a [QualificationCase],
    id: &str,
    anomalies: &mut BTreeSet<String>,
) -> Located<'a> {
    let mut matched = cases.iter().filter(|case| case.id == id);
    match (matched.next(), matched.next()) {
        (None, _) => Located::None,
        (Some(case), None) => Located::One(case),
        (Some(_), Some(_)) => {
            anomalies.insert(format!("duplicate_case:{id}"));
            Located::Duplicate
        }
    }
}

fn unevidenced(status: CheckStatus) -> CaseEval {
    CaseEval {
        status,
        accepted: false,
        count: None,
        flag: None,
        hard_fail: false,
        capacity_unknown: false,
        expired: false,
    }
}

fn accepted(case: &QualificationCase) -> CaseEval {
    CaseEval {
        status: CheckStatus::Passed,
        accepted: true,
        count: case.count,
        flag: case.flag,
        hard_fail: false,
        capacity_unknown: false,
        expired: false,
    }
}

fn failed_measurement(case: &QualificationCase) -> CaseEval {
    CaseEval {
        status: CheckStatus::Failed,
        accepted: false,
        count: case.count,
        flag: case.flag,
        hard_fail: true,
        capacity_unknown: false,
        expired: false,
    }
}

fn evidenced(case: &QualificationCase) -> bool {
    case.evidence
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
}

fn fully_measured(case: &QualificationCase) -> bool {
    evidenced(case)
        && published_hash(case.probe_input_hash).is_some()
        && published_hash(case.probe_output_hash).is_some()
        && case
            .observed_at
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
        && !case.fake_broker
        && !case.fake_fd
}

fn companion_lifecycle_ok(case: &QualificationCase) -> bool {
    case.uses_env_token && case.mounted_socket && case.eof_lifecycle && !case.fake_fd
}

fn combine_status(material: &CaseEval, visible: &CaseEval) -> CheckStatus {
    if material.hard_fail || visible.hard_fail {
        CheckStatus::Failed
    } else if material.accepted && visible.accepted {
        CheckStatus::Passed
    } else {
        CheckStatus::NotTested
    }
}

fn push_profile_gaps(profile: &QualificationProfile, missing: &mut Vec<String>) {
    for (name, value) in [
        ("exact_id", profile.exact_id.as_str()),
        ("os_name", profile.os_name.as_str()),
        ("os_version", profile.os_version.as_str()),
        ("adapter_version", profile.adapter_version.as_str()),
        ("isolator_version", profile.isolator_version.as_str()),
        ("host_os_name", profile.host_os_name.as_str()),
        ("host_os_version", profile.host_os_version.as_str()),
    ] {
        if value.trim().is_empty() {
            missing.push(name.to_string());
        }
    }
    if profile
        .image_digest
        .as_deref()
        .is_none_or(|digest| !image_digest_complete(digest))
    {
        missing.push("image_digest".to_string());
    }
    for (name, hash) in [
        ("policy_hash", profile.policy_hash),
        ("tool_contract_hash", profile.tool_contract_hash),
        ("core_hash", profile.core_hash),
        ("plan_hash", profile.plan_hash),
        ("execution_tree_hash", profile.execution_tree_hash),
    ] {
        if published_hash(hash).is_none() {
            missing.push(name.to_string());
        }
    }
    for (name, hash) in [
        ("canonical", profile.shared_core_hashes.canonical),
        (
            "delivery_encoder",
            profile.shared_core_hashes.delivery_encoder,
        ),
        ("validator", profile.shared_core_hashes.validator),
        ("tool_core", profile.shared_core_hashes.tool_core),
        (
            "request_accounting",
            profile.shared_core_hashes.request_accounting,
        ),
        ("ingress", profile.shared_core_hashes.ingress),
    ] {
        if published_hash(hash).is_none() {
            missing.push(format!("shared_core:{name}"));
        }
    }
    if profile.binaries.is_empty() {
        missing.push("binaries".to_string());
    }
    for binary in &profile.binaries {
        if binary_gap(binary) {
            if binary.role.trim().is_empty() {
                missing.push("binary".to_string());
            } else {
                missing.push(format!("binary:{}", binary.role.trim()));
            }
        }
    }
    if profile
        .observed_at
        .as_deref()
        .is_none_or(|text| text.trim().is_empty())
    {
        missing.push("observed_at".to_string());
    }
}

fn binary_gap(binary: &ReportedBinary) -> bool {
    binary.role.trim().is_empty()
        || binary
            .absolute_path
            .as_deref()
            .is_none_or(|path| !Path::new(path).is_absolute())
        || binary
            .version
            .as_deref()
            .is_none_or(|version| version.trim().is_empty())
        || published_hash(binary.sha256).is_none()
}

fn approval_is_valid(approval: &QualificationApproval) -> bool {
    approval.approved
        && !approval.expired
        && !approval.experiment_id.trim().is_empty()
        && !approval.allowed_models.is_empty()
        && approval
            .allowed_models
            .iter()
            .all(|model| !model.trim().is_empty())
        && !approval.allowed_fixtures.is_empty()
        && approval
            .allowed_fixtures
            .iter()
            .all(|fixture| !fixture.trim().is_empty())
        && approval.attempt_limit >= 1
        && !approval.valid_until.trim().is_empty()
}

fn publish_cores(hashes: &SharedCoreHashes) -> SharedCoreHashes {
    SharedCoreHashes {
        canonical: published_hash(hashes.canonical),
        delivery_encoder: published_hash(hashes.delivery_encoder),
        validator: published_hash(hashes.validator),
        tool_core: published_hash(hashes.tool_core),
        request_accounting: published_hash(hashes.request_accounting),
        ingress: published_hash(hashes.ingress),
    }
}

fn published_hash(hash: Option<Hash256>) -> Option<Hash256> {
    hash.filter(|value| !is_zero(*value))
}

fn is_zero(hash: Hash256) -> bool {
    hash == Hash256::from_bytes([0; 32])
}

fn image_digest_complete(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn probe_hashes(cases: &[QualificationCase]) -> Vec<ProbeHash> {
    cases
        .iter()
        .filter_map(|case| {
            Some(ProbeHash {
                case_id: case.id.clone(),
                input: published_hash(case.probe_input_hash)?,
                output: published_hash(case.probe_output_hash)?,
            })
        })
        .collect()
}

fn traces(cases: &[QualificationCase]) -> Vec<RedactedTrace> {
    cases
        .iter()
        .filter_map(|case| {
            let text = case.trace.clone()?;
            if text.trim().is_empty() {
                None
            } else {
                Some(RedactedTrace {
                    case_id: case.id.clone(),
                    text,
                })
            }
        })
        .collect()
}

fn redact_profile(profile: &QualificationProfile, stripped: &mut bool) -> QualificationProfile {
    let mut profile = profile.clone();
    redact_in_place(&mut profile.exact_id, stripped);
    redact_in_place(&mut profile.os_name, stripped);
    redact_in_place(&mut profile.os_version, stripped);
    redact_in_place(&mut profile.adapter_version, stripped);
    redact_in_place(&mut profile.isolator_version, stripped);
    redact_in_place(&mut profile.host_os_name, stripped);
    redact_in_place(&mut profile.host_os_version, stripped);
    redact_opt_in_place(&mut profile.image_digest, stripped);
    redact_opt_in_place(&mut profile.observed_at, stripped);
    for binary in &mut profile.binaries {
        redact_in_place(&mut binary.role, stripped);
        redact_opt_in_place(&mut binary.absolute_path, stripped);
        redact_opt_in_place(&mut binary.version, stripped);
    }
    profile
}

fn redact_approval(approval: &QualificationApproval, stripped: &mut bool) -> QualificationApproval {
    let mut approval = approval.clone();
    redact_in_place(&mut approval.experiment_id, stripped);
    redact_in_place(&mut approval.valid_until, stripped);
    approval.allowed_models = redact_list(&approval.allowed_models, stripped);
    approval.allowed_fixtures = redact_list(&approval.allowed_fixtures, stripped);
    approval
}

fn redact_case(case: &QualificationCase, stripped: &mut bool) -> QualificationCase {
    let mut case = case.clone();
    redact_in_place(&mut case.id, stripped);
    redact_opt_in_place(&mut case.evidence, stripped);
    redact_opt_in_place(&mut case.observed_at, stripped);
    redact_opt_in_place(&mut case.trace, stripped);
    redact_opt_in_place(&mut case.anomaly, stripped);
    case
}

fn redact_list(values: &[String], stripped: &mut bool) -> Vec<String> {
    values
        .iter()
        .map(|value| redact_string(value, stripped))
        .filter(|value| !value.is_empty())
        .collect()
}

fn redact_in_place(slot: &mut String, stripped: &mut bool) {
    let next = redact_string(slot, stripped);
    *slot = next;
}

fn redact_opt_in_place(slot: &mut Option<String>, stripped: &mut bool) {
    let next = slot
        .as_deref()
        .map(|value| redact_text(value, stripped))
        .filter(|value| !value.trim().is_empty());
    *slot = next;
}

fn redact_string(value: &str, stripped: &mut bool) -> String {
    redact_text(value, stripped).trim().to_string()
}

fn redact_text(input: &str, stripped: &mut bool) -> String {
    let mut out = String::new();
    for line in input.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        if line_is_private(body) {
            *stripped = true;
            continue;
        }
        out.push_str(line);
    }
    out
}

fn line_is_private(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("-----begin ")
        || lower.contains("sk-")
    {
        return true;
    }
    let trimmed = lower.trim_start();
    if trimmed.starts_with("env:") || trimmed.starts_with("export ") {
        return true;
    }
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    if value.trim().is_empty() {
        return false;
    }
    let key_lower = key.to_ascii_lowercase();
    let key_token = key_lower
        .rsplit([' ', ',', '{', '"'])
        .next()
        .unwrap_or(key_lower.as_str());
    key_token.contains("token")
        || key_token.contains("secret")
        || key_token.contains("password")
        || key_token.contains("api_key")
        || key_token.contains("apikey")
}

fn hash_byte(byte: u8) -> Hash256 {
    Hash256::from_bytes([byte; 32])
}

fn empty_cores() -> SharedCoreHashes {
    SharedCoreHashes {
        canonical: None,
        delivery_encoder: None,
        validator: None,
        tool_core: None,
        request_accounting: None,
        ingress: None,
    }
}

fn unknown_binary(role: &str) -> ReportedBinary {
    ReportedBinary {
        role: role.to_string(),
        absolute_path: None,
        version: None,
        sha256: None,
    }
}

/// Candidate profile that this Windows host did not execute.
/// Binary digests stay absent on purpose.
fn this_host_profile() -> QualificationProfile {
    QualificationProfile {
        exact_id: "linux-codex-2.1.1".to_string(),
        os_name: "linux".to_string(),
        os_version: "debian-12".to_string(),
        adapter_version: "codex-acp@2.1.1".to_string(),
        isolator_version: "linux-oci".to_string(),
        host_os_name: "windows".to_string(),
        host_os_version: "Windows 11 Pro 10.0.26200.9457".to_string(),
        platform_blocked: true,
        binaries: vec![
            unknown_binary("crun"),
            unknown_binary("codex"),
            unknown_binary("node"),
            unknown_binary("adapter"),
            unknown_binary("mcp"),
        ],
        image_digest: None,
        policy_hash: None,
        tool_contract_hash: None,
        core_hash: None,
        plan_hash: None,
        execution_tree_hash: None,
        shared_core_hashes: empty_cores(),
        observed_at: None,
    }
}

fn absent_approval() -> QualificationApproval {
    QualificationApproval {
        experiment_id: String::new(),
        allowed_models: Vec::new(),
        allowed_fixtures: Vec::new(),
        attempt_limit: 0,
        spend_limit: 0,
        valid_until: String::new(),
        approved: false,
        expired: false,
    }
}

fn blank_case(id: &str) -> QualificationCase {
    QualificationCase {
        id: id.to_string(),
        status: CheckStatus::NotTested,
        evidence: None,
        probe_input_hash: None,
        probe_output_hash: None,
        observed_at: None,
        trace: None,
        count: None,
        flag: None,
        fake_broker: false,
        fake_fd: false,
        uses_env_token: false,
        mounted_socket: false,
        eof_lifecycle: false,
        anomaly: None,
    }
}

fn recorded_not_tested(id: &str) -> QualificationCase {
    let mut case = blank_case(id);
    case.evidence = Some("not_tested".to_string());
    case
}

fn this_host_cases() -> Vec<QualificationCase> {
    vec![
        recorded_not_tested("endpoint_compatibility"),
        recorded_not_tested("native_read_boundary"),
        recorded_not_tested("api_credential_scope"),
        recorded_not_tested("chatgpt_account_scope"),
    ]
}

fn pinned_path(role: &str) -> String {
    if cfg!(windows) {
        format!(r"C:\qual\{role}.exe")
    } else {
        format!("/usr/local/libexec/codeg/{role}")
    }
}

fn synthetic_profile() -> QualificationProfile {
    QualificationProfile {
        exact_id: "synthetic-complete-evidence".to_string(),
        os_name: "linux".to_string(),
        os_version: "debian-12".to_string(),
        adapter_version: "codex-acp@2.1.1".to_string(),
        isolator_version: "linux-oci-1".to_string(),
        host_os_name: "synthetic".to_string(),
        host_os_version: "fixture".to_string(),
        platform_blocked: false,
        binaries: vec![
            measured_binary("crun", 1),
            measured_binary("codex", 2),
            measured_binary("node", 3),
            measured_binary("adapter", 4),
            measured_binary("mcp", 5),
        ],
        image_digest: Some(format!("sha256:{}", hash_byte(6).to_hex())),
        policy_hash: Some(hash_byte(7)),
        tool_contract_hash: Some(hash_byte(8)),
        core_hash: Some(hash_byte(9)),
        plan_hash: Some(hash_byte(10)),
        execution_tree_hash: Some(hash_byte(11)),
        shared_core_hashes: SharedCoreHashes {
            canonical: Some(hash_byte(12)),
            delivery_encoder: Some(hash_byte(13)),
            validator: Some(hash_byte(14)),
            tool_core: Some(hash_byte(15)),
            request_accounting: Some(hash_byte(16)),
            ingress: Some(hash_byte(17)),
        },
        observed_at: Some("1970-01-01T00:00:00Z".to_string()),
    }
}

fn measured_binary(role: &str, byte: u8) -> ReportedBinary {
    ReportedBinary {
        role: role.to_string(),
        absolute_path: Some(pinned_path(role)),
        version: Some(format!("{role}-pinned")),
        sha256: Some(hash_byte(byte)),
    }
}

fn synthetic_approval() -> QualificationApproval {
    QualificationApproval {
        experiment_id: "synthetic-evidence-fixture".to_string(),
        allowed_models: vec!["fixture-model".to_string()],
        allowed_fixtures: vec!["synthetic-non-sensitive".to_string()],
        attempt_limit: 1,
        spend_limit: 0,
        valid_until: "2099-01-01T00:00:00Z".to_string(),
        approved: true,
        expired: false,
    }
}

fn measured_case(id: &str, index: u8) -> QualificationCase {
    let mut case = blank_case(id);
    case.status = CheckStatus::Passed;
    case.evidence = Some(format!("synthetic-fixture:{id}"));
    case.probe_input_hash = Some(hash_byte(100 + index));
    case.probe_output_hash = Some(hash_byte(150 + index));
    case.observed_at = Some("1970-01-01T00:00:00Z".to_string());
    case
}

fn complete_cases() -> Vec<QualificationCase> {
    let mut cases = vec![
        measured_case("new_session", 1),
        measured_case("roundtable_mcp", 2),
        measured_case("ordered_turn_completion", 3),
        measured_case("cancel_and_reap", 4),
        measured_case("strict_isolation", 5),
        measured_case("bounded_context_delivery", 6),
        measured_case("private_events", 7),
        measured_case("sidebar_discovery", 8),
        measured_case("global_body_events", 9),
        measured_case("model_credential_material_in_sandbox", 10),
        measured_case("model_credentials_visible_to_agent", 11),
        measured_case("actual_binary_match", 12),
        measured_case("endpoint_compatibility", 13),
        measured_case("native_read_boundary", 14),
        measured_case("api_credential_scope", 15),
        measured_case("chatgpt_account_scope", 16),
    ];
    edit(&mut cases, "roundtable_mcp", |case| {
        case.uses_env_token = true;
        case.mounted_socket = true;
        case.eof_lifecycle = true;
    });
    edit(&mut cases, "ordered_turn_completion", |case| {
        case.flag = Some(true);
    });
    edit(&mut cases, "cancel_and_reap", |case| {
        case.count = Some(0);
    });
    edit(&mut cases, "sidebar_discovery", |case| {
        case.count = Some(0);
    });
    edit(&mut cases, "global_body_events", |case| {
        case.count = Some(0);
    });
    edit(&mut cases, "model_credential_material_in_sandbox", |case| {
        case.flag = Some(false);
    });
    edit(&mut cases, "model_credentials_visible_to_agent", |case| {
        case.flag = Some(false);
    });
    edit(&mut cases, "actual_binary_match", |case| {
        case.flag = Some(true);
    });
    edit(&mut cases, "native_read_boundary", |case| {
        case.flag = Some(false);
    });
    edit(&mut cases, "chatgpt_account_scope", |case| {
        case.status = CheckStatus::NotTested;
        case.evidence = Some("account authentication was not measured".to_string());
        case.probe_input_hash = None;
        case.probe_output_hash = None;
        case.flag = None;
    });
    cases
}

fn edit(cases: &mut [QualificationCase], id: &str, change: impl FnOnce(&mut QualificationCase)) {
    let case = cases
        .iter_mut()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("missing fixture case {id}"));
    change(case);
}

fn without(cases: &[QualificationCase], id: &str) -> Vec<QualificationCase> {
    cases.iter().filter(|case| case.id != id).cloned().collect()
}

fn contains_sha256_hex(text: &str) -> bool {
    let mut run = 0;
    for byte in text.bytes() {
        if byte.is_ascii_hexdigit() {
            run += 1;
            if run >= 64 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[tokio::test(flavor = "current_thread")]
async fn qualification_report_requires_all_evidence() {
    let profile = synthetic_profile();
    let approval = synthetic_approval();
    let complete = complete_cases();
    let baseline = qualify_adapter(&profile, &approval, &complete)
        .await
        .expect("checker");
    // This pass judges a complete synthetic evidence record. It is not the
    // archived linux-codex-2.1.1 verdict and it does not run the adapter.
    assert_eq!(baseline.verdict, CheckStatus::Passed);
    assert!(baseline.g1_passed);
    assert_ne!(baseline.profile.exact_id, "linux-codex-2.1.1");
    assert!(baseline
        .required_capabilities
        .iter()
        .all(|check| check.status == CheckStatus::Passed));
    assert_eq!(baseline.model_credentials_visible_to_agent, Some(false));
    assert_eq!(baseline.model_credential_material_in_sandbox, Some(false));
    assert_eq!(baseline.idle_processes_after_reap, Some(0));
    assert_eq!(baseline.normal_completion_after_submit_receipt, Some(true));
    assert_eq!(baseline.ordinary_sidebar_imports, Some(0));
    assert_eq!(baseline.global_body_events, Some(0));
    assert_eq!(
        baseline.actual_resolved_binary_matches_certificate,
        Some(true)
    );
    assert_eq!(baseline.chatgpt_account_scope, CheckStatus::NotTested);
    assert!(!baseline.fake_broker_is_certificate);
    assert!(!baseline.fake_fd_is_certificate);

    for omitted in CAPABILITIES {
        let report = qualify_adapter(&profile, &approval, &without(&complete, omitted))
            .await
            .expect("checker");
        assert_ne!(report.verdict, CheckStatus::Passed, "{omitted}");
        assert!(
            report.missing.iter().any(|item| item == omitted),
            "{omitted} missing={:?}",
            report.missing
        );
        let check = report
            .required_capabilities
            .iter()
            .find(|check| check.name == *omitted)
            .expect("capability row");
        assert_ne!(check.status, CheckStatus::Passed, "{omitted}");
    }

    for omitted in [
        "private_events",
        "sidebar_discovery",
        "global_body_events",
        "model_credential_material_in_sandbox",
        "model_credentials_visible_to_agent",
        "actual_binary_match",
    ] {
        let report = qualify_adapter(&profile, &approval, &without(&complete, omitted))
            .await
            .expect("checker");
        assert!(report
            .required_capabilities
            .iter()
            .all(|check| check.status == CheckStatus::Passed));
        assert_ne!(report.verdict, CheckStatus::Passed, "{omitted}");
        assert!(report.missing.iter().any(|item| item == omitted));
    }
    let sidebar = qualify_adapter(
        &profile,
        &approval,
        &without(&complete, "sidebar_discovery"),
    )
    .await
    .expect("checker");
    assert!(sidebar.ordinary_sidebar_imports.is_none());
    assert_ne!(sidebar.sidebar_discovery, CheckStatus::Passed);
    let global = qualify_adapter(
        &profile,
        &approval,
        &without(&complete, "global_body_events"),
    )
    .await
    .expect("checker");
    assert!(global.global_body_events.is_none());
    let hidden = qualify_adapter(
        &profile,
        &approval,
        &without(&complete, "model_credentials_visible_to_agent"),
    )
    .await
    .expect("checker");
    assert!(hidden.model_credentials_visible_to_agent.is_none());
    assert_ne!(hidden.credentials_unreachable, CheckStatus::Passed);

    for name in [
        "canonical",
        "delivery_encoder",
        "validator",
        "tool_core",
        "request_accounting",
        "ingress",
    ] {
        let mut dropped = profile.clone();
        match name {
            "canonical" => dropped.shared_core_hashes.canonical = None,
            "delivery_encoder" => dropped.shared_core_hashes.delivery_encoder = None,
            "validator" => dropped.shared_core_hashes.validator = None,
            "tool_core" => dropped.shared_core_hashes.tool_core = None,
            "request_accounting" => dropped.shared_core_hashes.request_accounting = None,
            "ingress" => dropped.shared_core_hashes.ingress = None,
            _ => unreachable!("shared core name"),
        }
        let report = qualify_adapter(&dropped, &approval, &complete)
            .await
            .expect("checker");
        assert!(report
            .required_capabilities
            .iter()
            .all(|check| check.status == CheckStatus::Passed));
        assert_ne!(report.verdict, CheckStatus::Passed, "{name}");
        assert!(report
            .missing
            .iter()
            .any(|item| item == &format!("shared_core:{name}")));
    }

    let mut no_tree = profile.clone();
    no_tree.execution_tree_hash = None;
    let no_tree = qualify_adapter(&no_tree, &approval, &complete)
        .await
        .expect("checker");
    assert!(no_tree.execution_tree_hash.is_none());
    assert_ne!(no_tree.verdict, CheckStatus::Passed);

    let mut zero_tree = profile.clone();
    zero_tree.execution_tree_hash = Some(Hash256::from_bytes([0; 32]));
    let zero_tree = qualify_adapter(&zero_tree, &approval, &complete)
        .await
        .expect("checker");
    assert!(zero_tree.execution_tree_hash.is_none());
    assert_ne!(zero_tree.verdict, CheckStatus::Passed);

    let mut faked = complete.clone();
    for case in &mut faked {
        case.fake_broker = true;
    }
    let faked = qualify_adapter(&profile, &approval, &faked)
        .await
        .expect("checker");
    assert_ne!(faked.verdict, CheckStatus::Passed);
    assert!(!faked.fake_broker_is_certificate);
    assert!(faked
        .anomalies
        .iter()
        .any(|item| item == "fake_broker_is_not_a_certificate"));
    assert!(faked
        .required_capabilities
        .iter()
        .all(|check| check.status != CheckStatus::Passed));

    let mut fake_fd = complete.clone();
    edit(&mut fake_fd, "roundtable_mcp", |case| {
        case.fake_fd = true;
    });
    let fake_fd = qualify_adapter(&profile, &approval, &fake_fd)
        .await
        .expect("checker");
    assert_ne!(fake_fd.verdict, CheckStatus::Passed);
    assert!(!fake_fd.fake_fd_is_certificate);
    assert_ne!(fake_fd.companion_lifecycle, CheckStatus::Passed);
    assert!(fake_fd
        .anomalies
        .iter()
        .any(|item| item == "fake_fd_is_not_a_certificate"));

    let mut endpoint = complete.clone();
    edit(&mut endpoint, "endpoint_compatibility", |case| {
        case.status = CheckStatus::NotTested;
    });
    let endpoint = qualify_adapter(&profile, &approval, &endpoint)
        .await
        .expect("checker");
    assert_eq!(endpoint.endpoint_compatibility, CheckStatus::NotTested);
    assert_ne!(endpoint.verdict, CheckStatus::Passed);

    let mut endpoint_failed = complete.clone();
    edit(&mut endpoint_failed, "endpoint_compatibility", |case| {
        case.status = CheckStatus::Failed;
    });
    let endpoint_failed = qualify_adapter(&profile, &approval, &endpoint_failed)
        .await
        .expect("checker");
    assert_eq!(endpoint_failed.verdict, CheckStatus::Failed);
    assert_eq!(endpoint_failed.endpoint_compatibility, CheckStatus::Failed);

    let mut native = complete.clone();
    edit(&mut native, "native_read_boundary", |case| {
        case.flag = Some(true);
    });
    let native = qualify_adapter(&profile, &approval, &native)
        .await
        .expect("checker");
    assert_eq!(native.verdict, CheckStatus::Failed);
    assert_eq!(native.native_read_boundary, CheckStatus::Failed);
    assert_eq!(native.model_credentials_visible_to_agent, Some(false));

    let mut native_untested = complete.clone();
    edit(&mut native_untested, "native_read_boundary", |case| {
        case.status = CheckStatus::NotTested;
        case.flag = None;
        case.probe_input_hash = None;
        case.probe_output_hash = None;
    });
    let native_untested = qualify_adapter(&profile, &approval, &native_untested)
        .await
        .expect("checker");
    assert_eq!(native_untested.native_read_boundary, CheckStatus::NotTested);
    assert_ne!(native_untested.verdict, CheckStatus::Passed);

    let mut blocked = profile.clone();
    blocked.platform_blocked = true;
    let blocked = qualify_adapter(&blocked, &approval, &complete)
        .await
        .expect("checker");
    assert_eq!(blocked.verdict, CheckStatus::BlockedPlatform);
    assert!(!blocked.g1_passed);

    let mut unknown = complete.clone();
    edit(&mut unknown, "bounded_context_delivery", |case| {
        case.status = CheckStatus::CapacityUnknown;
        case.evidence = Some("capacity source unavailable".to_string());
        case.probe_input_hash = None;
        case.probe_output_hash = None;
    });
    let unknown = qualify_adapter(&profile, &approval, &unknown)
        .await
        .expect("checker");
    assert_eq!(unknown.verdict, CheckStatus::CapacityUnknown);
    assert_ne!(unknown.verdict, CheckStatus::Passed);

    let mut expired = approval.clone();
    expired.expired = true;
    let expired = qualify_adapter(&profile, &expired, &complete)
        .await
        .expect("checker");
    assert_eq!(expired.verdict, CheckStatus::Expired);
    assert!(!expired.g1_passed);
}

#[tokio::test(flavor = "current_thread")]
async fn private_material_is_stripped_before_the_report() {
    let profile = synthetic_profile();
    let approval = synthetic_approval();
    let mut cases = complete_cases();
    edit(&mut cases, "new_session", |case| {
        case.trace = Some(
            "Authorization: Bearer super-secret-value\nCODEG_TOKEN=super-secret-value\nenv: HOME=/Users/private\nkept-line\n".to_string(),
        );
    });
    let report = qualify_adapter(&profile, &approval, &cases)
        .await
        .expect("checker");
    let raw = serde_json::to_string(&report).expect("json");
    assert!(!raw.contains("super-secret-value"));
    assert!(!raw.contains("/Users/private"));
    assert!(!raw.contains("Authorization"));
    assert!(!raw.contains("CODEG_TOKEN"));
    assert!(raw.contains("kept-line"));
    assert!(report
        .anomalies
        .iter()
        .any(|item| item == "private_material_stripped"));
    assert_eq!(report.verdict, CheckStatus::Passed);

    edit(&mut cases, "strict_isolation", |case| {
        case.evidence = Some("Authorization: Bearer super-secret-value".to_string());
    });
    let stripped = qualify_adapter(&profile, &approval, &cases)
        .await
        .expect("checker");
    let raw = serde_json::to_string(&stripped).expect("json");
    assert!(!raw.contains("super-secret-value"));
    assert_ne!(stripped.verdict, CheckStatus::Passed);
    assert!(stripped
        .missing
        .iter()
        .any(|item| item == "strict_isolation"));
}

#[tokio::test(flavor = "current_thread")]
async fn archived_linux_codex_report_is_not_passed() {
    let produced = qualify_adapter(&this_host_profile(), &absent_approval(), &this_host_cases())
        .await
        .expect("checker");
    let raw = include_str!("../../../docs/roundtable/qualification/linux-codex-2.1.1/report.json");
    let archived: QualificationReport = serde_json::from_str(raw).expect("archive parses");
    assert_eq!(produced, archived);
    assert_eq!(archived.verdict, CheckStatus::BlockedPlatform);
    assert!(!archived.g1_passed);
    assert!(!archived.qualification_issued);
    assert_eq!(archived.endpoint_compatibility, CheckStatus::NotTested);
    assert_eq!(archived.native_read_boundary, CheckStatus::NotTested);
    assert!(archived.ordinary_sidebar_imports.is_none());
    assert_ne!(archived.sidebar_discovery, CheckStatus::Passed);
    assert!(archived.global_body_events.is_none());
    assert!(archived.model_credential_material_in_sandbox.is_none());
    assert!(archived.model_credentials_visible_to_agent.is_none());
    assert_ne!(archived.credentials_unreachable, CheckStatus::Passed);
    assert_eq!(archived.api_credential_scope, CheckStatus::NotTested);
    assert_eq!(archived.chatgpt_account_scope, CheckStatus::NotTested);
    assert!(archived
        .actual_resolved_binary_matches_certificate
        .is_none());
    assert_ne!(archived.binary_match, CheckStatus::Passed);
    assert!(archived.execution_tree_hash.is_none());
    assert!(archived.shared_core_hashes.canonical.is_none());
    assert!(archived.shared_core_hashes.delivery_encoder.is_none());
    assert!(archived.shared_core_hashes.validator.is_none());
    assert!(archived.shared_core_hashes.tool_core.is_none());
    assert!(archived.shared_core_hashes.request_accounting.is_none());
    assert!(archived.shared_core_hashes.ingress.is_none());
    assert!(archived.probe_hashes.is_empty());
    assert!(archived.idle_processes_after_reap.is_none());
    assert!(archived.normal_completion_after_submit_receipt.is_none());
    assert!(!archived.fake_broker_is_certificate);
    assert!(!archived.fake_fd_is_certificate);
    assert!(archived
        .profile
        .binaries
        .iter()
        .all(|binary| binary.sha256.is_none() && binary.absolute_path.is_none()));
    assert!(!raw.contains("\"passed\""));
    assert!(!raw.contains("Authorization"));
    assert!(!raw.contains("Bearer"));
    assert!(!contains_sha256_hex(raw));
    for name in CAPABILITIES {
        assert!(archived.missing.iter().any(|item| item == name));
        let check = archived
            .required_capabilities
            .iter()
            .find(|check| check.name == *name)
            .expect("capability row");
        assert_eq!(check.status, CheckStatus::NotTested);
    }
}

/// The product probe must not invent a certificate when this host has no
/// working isolator. A real pass is `codeg-server roundtable-qualify` on a
/// prepared Debian host, not this test.
#[tokio::test(flavor = "current_thread")]
async fn live_probe_fails_closed_without_an_isolator() {
    let root = std::env::temp_dir().join(format!(
        "rt-qualify-closed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("rootfs")).expect("rootfs");
    let bindings = root.join("bindings.json");
    std::fs::write(
        &bindings,
        r#"[{"provider_ref":"provider:grok","model":"grok-4","origin":"https://api.x.ai","credential_env":"XAI_API_KEY","supported_efforts":[]}]"#,
    )
    .expect("bindings");
    let outcome =
        codeg_lib::roundtable::qualify_adapter_on_host(codeg_lib::roundtable::ProbeRequest {
            data_dir: root.join("data"),
            agent: "grok".into(),
            rootfs: root.join("rootfs"),
            crun: root.join("missing-crun"),
            cgroup_root: root.join("missing-cgroup"),
            runtime_root: root.join("oci"),
            provider_bindings: bindings,
            home: root.join("home"),
            profile_id: None,
        })
        .await;
    assert!(!outcome.qualification_issued);
    assert_ne!(outcome.verdict, "passed");
    assert!(!root.join("data/roundtable/qualified-runtime.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}
