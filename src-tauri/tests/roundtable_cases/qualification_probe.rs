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
        profile_id: String::new(),
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

#[test]
fn antigravity_default_profile_is_1_3_0_and_mounts_the_token_file() {
    let profile = profile_for_agent("antigravity").expect("default");
    assert_eq!(profile.exact_id, "linux-antigravity-acp-1.3.0");
    assert!(profile.auth_files.iter().any(|file| {
        file.home_relative == ".gemini/antigravity-acp/acp_token.json" && file.required
    }));
    let pinned =
        codeg_lib::roundtable::profile_by_id("linux-antigravity-acp-1.2.1").expect("1.2.1");
    assert_eq!(pinned.agent, "antigravity");
    assert_eq!(pinned.version_needle, "1.2.1");
    assert!(os_accepted(pinned, "linux", "debian-13"));
}

#[test]
fn cursor_accepts_either_auth_json_and_pins_xdg() {
    let profile = profile_for_agent("cursor").expect("cursor");
    assert_eq!(profile.require_one_filename, Some("auth.json"));
    assert!(profile
        .container_env
        .iter()
        .any(|(key, value)| *key == "XDG_CONFIG_HOME" && *value == "/rt-home/.config"));
    assert!(profile
        .auth_files
        .iter()
        .any(|file| { file.home_relative == ".cursor/auth.json" && !file.required }));
    assert!(profile
        .auth_files
        .iter()
        .any(|file| { file.home_relative == ".config/cursor/auth.json" && !file.required }));
}

#[test]
fn isolation_script_treats_masked_kcore_as_denied_and_requires_sysfs() {
    let script = codeg_lib::roundtable::isolation_probe_script();
    assert!(script.contains("1:3"));
    assert!(!script.contains("if [ -r /proc/kcore ]; then fail"));
    assert!(script.contains("[ ! -d /sys/class/net ]"));
    assert!(script.contains("DENIED device"));
    assert!(script.contains("/dev/mem"));
}

#[test]
fn slirp_hook_backgrounds_and_joins_the_user_namespace() {
    let script = codeg_lib::roundtable::slirp_hook_script();
    assert!(script.contains("--userns-path="));
    assert!(script.contains("--netns-type=path"));
    assert!(script.contains("--ready-fd"));
    assert!(!script.contains("exec \"$1\""));
    assert!(script.contains("ready_byte"));
    assert!(script.contains("!= \"1\""));
    assert!(script.contains("while kill -0"));
    assert!(script.contains("sleep 0.2"));
    assert!(script.contains("exit 0"));
    assert_eq!(codeg_lib::roundtable::slirp_hook_phase(), "poststart");
}

#[test]
fn syscall_allowlist_covers_adapter_startup_and_still_denies_mount() {
    let names = codeg_lib::roundtable::syscall_allowlist();
    for required in [
        "getpgrp",
        "setpgid",
        "timerfd_create",
        "memfd_create",
        "statfs",
        "fstatfs",
        "inotify_init1",
        "inotify_add_watch",
        "membarrier",
        "sendmmsg",
        "recvmmsg",
        "getitimer",
        "setitimer",
    ] {
        assert!(names.contains(&required), "{required}");
    }
    for denied in [
        "mount",
        "umount2",
        "pivot_root",
        "unshare",
        "setns",
        "ptrace",
        "bpf",
        "perf_event_open",
        "init_module",
        "kexec_load",
    ] {
        assert!(!names.contains(&denied), "{denied}");
    }
}

#[test]
fn advertised_models_must_include_the_binding() {
    let session = serde_json::json!({
        "sessionId": "s",
        "currentModelId": "grok-4.6",
        "configOptions": [{
            "id": "model",
            "currentValue": "grok-4.6",
            "options": [
                {"value": "grok-4.6", "name": "Grok 4.6"},
                {"value": "grok-4", "name": "Grok 4"}
            ]
        }]
    });
    let ids = codeg_lib::roundtable::advertised_probe_models(&session);
    assert!(ids.contains("grok-4.6"));
    assert!(ids.contains("grok-4"));
    let empty =
        codeg_lib::roundtable::advertised_probe_models(&serde_json::json!({"sessionId": "s"}));
    assert!(empty.is_empty());
}

#[test]
fn model_check_uses_the_provider_for_the_agent() {
    let providers = vec![
        ProviderBinding {
            provider_ref: "provider:grok".into(),
            model: "grok-4.6".into(),
            origin: "https://api.x.ai".into(),
            credential_env: "XAI_API_KEY".into(),
            supported_efforts: vec!["low".into()],
        },
        ProviderBinding {
            provider_ref: "provider:antigravity".into(),
            model: "gemini-3.8-flash-high".into(),
            origin: "https://cloudcode-pa.googleapis.com".into(),
            credential_env: "GEMINI_API_KEY".into(),
            supported_efforts: vec!["low".into()],
        },
    ];
    let session = serde_json::json!({
        "currentModelId": "gemini-3.8-flash-high",
        "models": [{"modelId": "gemini-3.8-flash-high"}]
    });
    let matched =
        codeg_lib::roundtable::probe_model_binding_error("antigravity", &session, &providers);
    assert!(matched.is_none(), "{matched:?}");
    let mismatch = codeg_lib::roundtable::probe_model_binding_error("grok", &session, &providers)
        .expect("grok model is not advertised");
    assert!(mismatch.contains("grok-4.6"), "{mismatch}");
}

#[test]
fn reap_treats_a_missing_container_as_reaped_only_when_the_cgroup_is_gone() {
    let gone =
        "cannot open directory '/var/lib/codeg/state/cq-isolation': No such file or directory";
    assert!(codeg_lib::roundtable::probe_reap_classification(false, gone, false, false).is_ok());
    let still_there = codeg_lib::roundtable::probe_reap_classification(false, gone, false, true)
        .expect_err("cgroup remains");
    assert!(still_there.contains("cgroup"), "{still_there}");
    let state_remains = codeg_lib::roundtable::probe_reap_classification(false, gone, true, false)
        .expect_err("state remains");
    assert!(!state_remains.is_empty());
    let other =
        codeg_lib::roundtable::probe_reap_classification(false, "permission denied", false, false)
            .expect_err("unrelated delete failure");
    assert!(other.contains("permission denied"), "{other}");
    assert!(codeg_lib::roundtable::probe_reap_classification(true, "", false, false).is_ok());
    let leftover = codeg_lib::roundtable::probe_reap_classification(true, "", false, true)
        .expect_err("successful delete left a cgroup");
    assert!(leftover.contains("cgroup"), "{leftover}");
}

#[test]
fn scratch_home_is_per_adapter_and_per_run_and_removed() {
    let root = scratch();
    let grok = codeg_lib::roundtable::probe_scratch_home(&root, "grok", "cq-egress");
    let again = codeg_lib::roundtable::probe_scratch_home(&root, "grok", "cq-egress");
    let cursor = codeg_lib::roundtable::probe_scratch_home(&root, "cursor", "cq-egress");
    assert_ne!(grok, again);
    assert!(grok.starts_with(root.join("home-upper").join("grok")));
    assert!(cursor.starts_with(root.join("home-upper").join("cursor")));
    assert!(grok
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("cq-egress-"));
    assert!(!grok.ends_with("cq-egress"));
    fs::create_dir_all(&grok).expect("home");
    fs::write(grok.join("marker"), b"x").expect("marker");
    codeg_lib::roundtable::remove_probe_scratch_home(&grok);
    assert!(!grok.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cgroup_delegation_names_the_launcher_child_when_the_root_is_not_a_cgroup() {
    let dir = scratch();
    let reason = codeg_lib::roundtable::cgroup_delegation_failure(&dir).expect("not a cgroup");
    assert!(
        reason.contains("launcher") || reason.contains("cgroup.controllers"),
        "{reason}"
    );
    let _ = fs::remove_dir_all(&dir);
}
