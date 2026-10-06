//! Fake-isolator qualification reports. These tests never start crun.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use codeg_lib::roundtable::{
    assemble_probe_report_for_test, os_accepted, profile_for_agent,
    verify_installed_report_for_test, CertifiedBinary, ProbeCheck, ProbeFacts, ProviderBinding,
    QualifiedOciProfile,
};
use roundtable_protocol::Hash256;

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rt-probe-{nanos}-{}", std::process::id()));
    fs::create_dir_all(&path).expect("scratch");
    path
}

fn absolute(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn check(name: &str) -> ProbeCheck {
    let (count, flag) = match name {
        "sidebar_discovery" | "global_body_events" | "cancel_and_reap" => (Some(0), None),
        "model_credential_material_in_sandbox"
        | "model_credentials_visible_to_agent"
        | "native_read_boundary" => (None, Some(false)),
        "actual_binary_match" | "ordered_turn_completion" => (None, Some(true)),
        _ => (None, None),
    };
    ProbeCheck {
        name: name.to_string(),
        status: "passed".into(),
        evidence: format!("observed {name}"),
        input: name.as_bytes().to_vec(),
        output: b"ok".to_vec(),
        count,
        flag,
    }
}

fn passing(agent: &str, os_version: &str) -> ProbeFacts {
    let root = scratch();
    let binary = root.join(agent);
    fs::write(&binary, agent).expect("binary");
    let hash = Hash256::sha256(agent.as_bytes());
    let digest = Hash256::sha256(b"image");
    let binary_pin = |role: &str| CertifiedBinary {
        role: role.into(),
        absolute_path: absolute(&binary),
        version: format!("{agent}-1"),
        sha256: hash,
    };
    ProbeFacts {
        agent: agent.into(),
        os_name: "linux".into(),
        os_version: os_version.into(),
        kernel: "6.12.0-test".into(),
        arch: std::env::consts::ARCH.into(),
        platform_blocked: false,
        checks: [
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
            "companion_lifecycle",
        ]
        .into_iter()
        .map(check)
        .collect(),
        binaries: vec![binary_pin("crun"), binary_pin("cli"), binary_pin("mcp")],
        image_digest: format!("sha256:{}", digest.to_hex()),
        oci: Some(QualifiedOciProfile {
            runtime: binary_pin("crun"),
            rootfs: root.join("rootfs"),
            rootfs_sha256: digest,
            runtime_root: root.join("oci"),
            cgroup_root: root.join("cgroup"),
            cli_args: profile_for_agent(agent)
                .map(|profile| {
                    profile
                        .cli_args
                        .iter()
                        .map(|arg| (*arg).to_string())
                        .collect()
                })
                .unwrap_or_default(),
            service_socket: None,
            gateway_socket: None,
            auth_mounts: Vec::new(),
            container_env: BTreeMap::new(),
        }),
        providers: vec![ProviderBinding {
            provider_ref: format!("provider:{agent}"),
            model: format!("{agent}-model"),
            origin: "https://models.example".into(),
            credential_env: "ROUNDTABLE_PROVIDER_CREDENTIAL".into(),
            supported_efforts: vec!["low".into(), "high".into()],
        }],
        anomalies: Vec::new(),
        isolation_marker: "ISOLATION_OK".into(),
        fake_broker: false,
        fake_fd: false,
    }
}

#[test]
fn debian_13_is_a_profile_parameter_for_each_adapter() {
    for agent in ["codex", "grok", "cursor", "antigravity"] {
        let profile = profile_for_agent(agent).expect(agent);
        assert!(os_accepted(profile, "linux", "debian-12"));
        assert!(os_accepted(profile, "linux", "debian-13"));
        assert!(!os_accepted(profile, "linux", "debian-11"));
        assert!(profile.requires_acp_turn);
    }
    assert!(profile_for_agent("claude").is_none());
}

#[test]
fn fake_isolator_pass_writes_a_report_the_verifier_accepts() {
    let data = scratch();
    let facts = passing("grok", "debian-13");
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(outcome.qualification_issued, "{:?}", outcome.reasons);
    assert_eq!(outcome.verdict, "passed");
    let runtime = data.join("roundtable/qualified-runtime.json");
    let body: serde_json::Value =
        serde_json::from_slice(&fs::read(&runtime).expect("runtime")).expect("json");
    assert_eq!(body["schema_version"], 2);
    assert_eq!(body["adapters"][0]["agent"], "grok");
    verify_installed_report_for_test(&data, "grok").expect("verify_report");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&outcome.report_path).expect("report")).expect("json");
    assert_eq!(report["verdict"], "passed");
    assert_eq!(report["qualification_issued"], true);
    assert_eq!(report["profile"]["os_version"], "debian-13");
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn fake_isolator_failure_does_not_issue_a_certificate() {
    let data = scratch();
    let mut facts = passing("cursor", "debian-13");
    facts.isolation_marker.clear();
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(!outcome.qualification_issued);
    assert_ne!(outcome.verdict, "passed");
    assert!(outcome
        .reasons
        .iter()
        .any(|reason| reason == "strict_isolation"));
    assert!(!data.join("roundtable/qualified-runtime.json").exists());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&outcome.report_path).expect("report")).expect("json");
    assert_eq!(report["qualification_issued"], false);
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn unaccepted_os_fails_even_when_every_check_passed() {
    let data = scratch();
    let outcome = assemble_probe_report_for_test(&data, passing("antigravity", "debian-11"));
    assert!(!outcome.qualification_issued);
    assert!(outcome
        .reasons
        .iter()
        .any(|reason| reason == "host_os_not_accepted"));
    assert!(!data.join("roundtable/qualified-runtime.json").exists());
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn catalog_keeps_a_passed_adapter_when_another_fails() {
    let data = scratch();
    let first = assemble_probe_report_for_test(&data, passing("grok", "debian-13"));
    assert!(first.qualification_issued, "{:?}", first.reasons);
    let mut second = passing("cursor", "debian-13");
    second.fake_broker = true;
    let failed = assemble_probe_report_for_test(&data, second);
    assert!(!failed.qualification_issued);
    verify_installed_report_for_test(&data, "grok").expect("grok remains");
    assert!(verify_installed_report_for_test(&data, "cursor").is_err());
    let body: serde_json::Value = serde_json::from_slice(
        &fs::read(data.join("roundtable/qualified-runtime.json")).expect("runtime"),
    )
    .expect("json");
    let agents: Vec<_> = body["adapters"]
        .as_array()
        .expect("adapters")
        .iter()
        .filter_map(|entry| entry["agent"].as_str())
        .collect();
    assert_eq!(agents, vec!["grok"]);
    let _ = fs::remove_dir_all(&data);
}
