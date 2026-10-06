//! Fake-isolator qualification reports. These tests never start crun.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use codeg_lib::roundtable::{
    assemble_probe_report_for_test, os_accepted, profile_for_agent,
    verify_installed_report_for_test, CertifiedBinary, ProbeCheck, ProbeFacts, ProbeRequest,
    ProviderBinding, QualifiedOciProfile,
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
        "actual_binary_match" | "ordered_turn_completion" | "submit_receipt_completion" => (None, Some(true)),
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
fn live_container_spec_bind_mounts_the_slirp_resolver() {
    let root = scratch();
    let bin = root.join("slirp4netns");
    fs::write(&bin, b"not-executed").expect("fake slirp");
    let spec = codeg_lib::roundtable::live_slirp_document(&root, "attempt-1", &bin).expect("spec");
    let mounts = spec["mounts"].as_array().expect("mounts");
    let resolv = mounts
        .iter()
        .find(|mount| mount["destination"] == "/etc/resolv.conf")
        .expect("resolv mount");
    assert_eq!(resolv["type"], "bind");
    assert_eq!(
        resolv["options"],
        serde_json::json!(["bind", "ro", "nosuid", "nodev", "noexec"])
    );
    let source = resolv["source"].as_str().expect("source");
    assert_eq!(
        fs::read(source).expect("resolv bytes"),
        b"nameserver 10.0.2.3\n"
    );
    assert_eq!(
        spec["annotations"]["io.codeg.roundtable.network"],
        "slirp-egress-not-origin-filtered"
    );
    assert!(spec["hooks"].get("createRuntime").is_none());
    let hook = &spec["hooks"]["poststart"][0];
    assert_eq!(hook["args"][0], "slirp-hook.sh");
    assert_eq!(hook["args"][1], bin.to_string_lossy().as_ref());
    let script = fs::read_to_string(hook["path"].as_str().expect("hook path")).expect("script");
    assert_eq!(script, codeg_lib::roundtable::slirp_hook_script());
    assert!(script.contains("while kill -0"));
    assert!(script.contains("ready_byte") && script.contains("\"1\""));
    let names = spec["linux"]["seccomp"]["syscalls"][0]["names"]
        .as_array()
        .expect("names");
    for required in ["sendmmsg", "recvmmsg", "getitimer", "setitimer"] {
        assert!(names.iter().any(|name| name == required), "{required}");
    }
    let arch = if cfg!(target_arch = "aarch64") {
        "SCMP_ARCH_AARCH64"
    } else {
        "SCMP_ARCH_X86_64"
    };
    assert_eq!(spec["linux"]["seccomp"]["architectures"][0], arch);
    assert_eq!(spec["linux"]["seccomp"]["defaultAction"], "SCMP_ACT_ERRNO");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn grok_auth_refresh_stays_in_the_attempt_copy() {
    let root = scratch();
    let host = root.join("host");
    let attempt = root.join("attempt");
    fs::create_dir_all(host.join(".grok")).expect("grok home");
    let host_auth = host.join(".grok/auth.json");
    let original = br#"{"refresh_token":"host-token","access_token":"old"}"#;
    fs::write(&host_auth, original).expect("host auth");
    let copied =
        codeg_lib::roundtable::stage_attempt_auth(&attempt, &host_auth, "/rt-home/.grok/auth.json")
            .expect("copy");
    assert!(!copied, "grok auth is not a read-only bind");
    let scratch_auth = attempt.join(".grok/auth.json");
    assert_eq!(fs::read(&scratch_auth).expect("scratch"), original);
    fs::write(&scratch_auth, br#"{"refresh_token":"rotated"}"#).expect("refresh");
    assert_eq!(fs::read(&host_auth).expect("host unchanged"), original);

    let cursor = host.join(".cursor/auth.json");
    fs::create_dir_all(cursor.parent().expect("parent")).expect("cursor home");
    fs::write(&cursor, b"cursor-token").expect("cursor auth");
    let bind =
        codeg_lib::roundtable::stage_attempt_auth(&attempt, &cursor, "/rt-home/.cursor/auth.json")
            .expect("cursor bind");
    assert!(bind, "cursor auth stays a read-only bind");
    assert_eq!(
        fs::read(attempt.join(".cursor/auth.json")).expect("placeholder"),
        b""
    );
    assert_eq!(fs::read(&cursor).expect("cursor host"), b"cursor-token");

    #[cfg(unix)]
    {
        let link = root.join("link.json");
        std::os::unix::fs::symlink(&host_auth, &link).expect("symlink");
        assert!(codeg_lib::roundtable::stage_attempt_auth(
            &attempt,
            &link,
            "/rt-home/.grok/auth.json",
        )
        .is_err());
        assert_eq!(fs::read(&host_auth).expect("host after symlink"), original);
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn slirp_hook_backgrounds_and_joins_the_user_namespace() {
    let script = codeg_lib::roundtable::slirp_hook_script();
    assert!(script.contains("--userns-path="));
    assert!(script.contains("--netns-type=path"));
    assert!(script.contains("--ready-fd"));
    assert!(script.contains("--exit-fd=0"));
    assert!(!script.contains("kill \"$slirp\""));
    assert!(!script.contains("exec \"$1\""));
    assert!(script.contains("ready_byte"));
    assert!(script.contains("ready_byte") && script.contains("\"1\""));
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
fn crun_version_on_the_key_ignores_the_state_directory_error() {
    let version = codeg_lib::roundtable::probe_recorded_crun_version(
        "crun version 1.21\ncommit: abc\nspec: 1.0.0\n",
    );
    assert_eq!(version.as_deref(), Some("crun version 1.21"));
    assert!(
        codeg_lib::roundtable::probe_recorded_crun_version("Failed to get state directory\n")
            .is_none()
    );
    let mixed = codeg_lib::roundtable::probe_recorded_crun_version(
        "Failed to get state directory\ncrun version 1.21\n",
    );
    assert_eq!(mixed.as_deref(), Some("crun version 1.21"));
}

#[cfg(target_os = "linux")]
#[test]
fn acp_session_error_stops_slirp_and_deletes_the_container() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let dir = scratch();
    let log = dir.join("crun.log");
    let crun = dir.join("crun");
    let log_quoted = format!("'{}'", log.display().to_string().replace('\'', "'\\''"));
    fs::write(
        &crun,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log_quoted}\nexit 1\n"),
    )
    .expect("crun script");
    fs::set_permissions(&crun, fs::Permissions::from_mode(0o755)).expect("chmod");
    let runtime_root = dir.join("runtime");
    fs::create_dir_all(runtime_root.join("slirp-pids")).expect("pids");
    let mut slirp = Command::new("sleep")
        .env("CODEG_ROUNDTABLE_SLIRP_ROLE", "slirp")
        .env("CODEG_ROUNDTABLE_SLIRP_OWNER", "cq-acp-9")
        .env("CODEG_ROUNDTABLE_SLIRP_ROOT", &runtime_root)
        .arg("30")
        .spawn()
        .expect("stand-in slirp");
    write_owned_helper_pin(&runtime_root, "cq-acp-9", slirp.id());
    let request = ProbeRequest {
        data_dir: dir.clone(),
        agent: "cursor".into(),
        rootfs: dir.join("rootfs"),
        crun,
        cgroup_root: dir.join("cgroup"),
        runtime_root,
        provider_bindings: dir.join("bindings.json"),
        home: dir.join("home"),
        profile_id: None,
    };
    let error = codeg_lib::roundtable::probe_acp_exit_cleans_container(&request, "cq-acp-9")
        .expect_err("session/new");
    assert!(error.contains("session/new"), "{error}");
    let text = fs::read_to_string(&log).expect("crun log");
    assert!(text.contains("kill cq-acp-9 KILL"), "{text}");
    assert!(text.contains("delete cq-acp-9"), "{text}");
    let status = slirp.wait().expect("slirp wait");
    assert!(!status.success(), "slirp still running: {status}");
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn live_rpc_allows_submit_rejects_terminal_and_keeps_float_frames() {
    let seen = codeg_lib::roundtable::exercise_live_acp_rpc()
        .await
        .expect("live rpc");
    assert!(seen.terminal_disabled);
    assert!(seen.fs_read_disabled);
    assert!(seen.fs_write_disabled);
    assert!(seen.session_omits_capabilities);
    assert_eq!(seen.allow_option_id, "allow-submit");
    assert_eq!(seen.reject_option_id, "reject-terminal");
    assert!(!seen.saw_cancelled);
    assert_eq!(seen.stop_reason, "end_turn");
    assert!(
        seen.assistant_text.contains("partial answer"),
        "{}",
        seen.assistant_text
    );

    let frame = br#"{"jsonrpc":"2.0","method":"session/update","params":{"score":0.5}}"#;
    assert!(codeg_lib::roundtable::acp_frame_fixture(frame).is_ok());
    assert!(roundtable_protocol::parse_strict_json(
        frame,
        &roundtable_protocol::ParseLimits::suggested_profile(),
    )
    .is_err());
    let rejected = codeg_lib::roundtable::acp_frame_fixture(b"{").expect_err("truncated frame");
    assert_eq!(rejected.details.reason.as_deref(), Some("acp_frame"));

    let allow_always = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {"title": "mcp__roundtable__read_evidence"},
        "options": [
            {"optionId": "always", "kind": "allow_always", "name": "Always"},
            {"optionId": "reject", "kind": "reject_once", "name": "Reject"}
        ]
    }));
    // Persistent permission scope is not qualified; reject rather than grant it.
    assert_eq!(allow_always["result"]["outcome"]["optionId"], "reject");
    assert_eq!(allow_always["result"]["outcome"]["outcome"], "selected");

    let missing_reject = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {"title": "run_terminal_command", "rawInput": {"command": "echo submit_result"}},
        "options": [{"optionId": "once", "kind": "allow_once", "name": "Allow"}]
    }));
    assert_eq!(missing_reject["error"]["code"], -32601);
    assert!(!missing_reject.to_string().contains("cancelled"));

    let search = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {"name": "roundtable/search_evidence"},
        "options": [
            {"optionId": "allow-search", "kind": "allow_once", "name": "Allow"},
            {"optionId": "reject-search", "kind": "reject_once", "name": "Reject"}
        ]
    }));
    assert_eq!(search["result"]["outcome"]["optionId"], "allow-search");

    let grok_search = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {
            "title": "use_tool",
            "kind": "other",
            "rawInput": {
                "tool_name": "roundtable__search_evidence",
                "tool_input": {"file_alias": "e0", "query": "alpha", "limit": 5}
            }
        },
        "options": [
            {"optionId": "allow-grok", "kind": "allow_once", "name": "Allow"},
            {"optionId": "reject-grok", "kind": "reject_once", "name": "Reject"}
        ]
    }));
    assert_eq!(grok_search["result"]["outcome"]["optionId"], "allow-grok");
    assert!(!grok_search.to_string().contains("cancelled"));

    let terminal_text = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {
            "title": "run_terminal_command",
            "kind": "execute",
            "rawInput": {"command": "echo submit_result"}
        },
        "options": [
            {"optionId": "allow-term", "kind": "allow_once", "name": "Allow"},
            {"optionId": "reject-term", "kind": "reject_once", "name": "Reject"}
        ]
    }));
    assert_eq!(
        terminal_text["result"]["outcome"]["optionId"],
        "reject-term"
    );
    assert!(!terminal_text.to_string().contains("cancelled"));

    let sentence = codeg_lib::roundtable::permission_reply_fixture(serde_json::json!({
        "toolCall": {
            "title": "please call submit_result now",
            "rawInput": {"command": "echo submit_result && echo read_evidence"}
        },
        "options": [
            {"optionId": "allow-sentence", "kind": "allow_once", "name": "Allow"},
            {"optionId": "reject-sentence", "kind": "reject_once", "name": "Reject"}
        ]
    }));
    assert_eq!(sentence["result"]["outcome"]["optionId"], "reject-sentence");
}

#[tokio::test]
async fn grok_use_tool_and_antigravity_submit_follow_the_schema() {
    let seen = codeg_lib::roundtable::exercise_schema_seat_rpc()
        .await
        .expect("schema seat");
    assert_eq!(seen.search_option_id, "allow-search");
    assert_eq!(seen.grok_submit_option_id, "allow-grok-submit");
    assert_eq!(seen.antigravity_option_id, "allow-antigravity");
    assert_eq!(seen.terminal_option_id, "reject-terminal");
    assert!(!seen.saw_cancelled_outcome);
    assert_eq!(seen.stop_reason, "end_turn");
    assert!(
        seen.repair_prompt.contains("roundtable__submit_result"),
        "{}",
        seen.repair_prompt
    );
    assert!(seen.grok_disallows_terminal);
    assert!(seen.grok_keeps_use_tool);

    let grok = codeg_lib::roundtable::session_params_fixture(
        codeg_lib::models::AgentType::Grok,
        &serde_json::json!([]),
    );
    let denied = grok["_meta"]["agentProfile"]["disallowedTools"]
        .as_array()
        .expect("denylist");
    assert!(denied.iter().any(|tool| tool == "run_terminal_command"));
    assert!(denied.iter().any(|tool| tool == "read_file"));
    assert!(denied
        .iter()
        .all(|tool| tool != "use_tool" && tool != "search_tool"));
    assert!(grok["_meta"]["agentProfile"].get("maxTurns").is_none());
    assert!(grok["_meta"]["agentProfile"]
        .get("permissionMode")
        .is_none());
    let antigravity = codeg_lib::roundtable::session_params_fixture(
        codeg_lib::models::AgentType::Antigravity,
        &serde_json::json!([]),
    );
    assert!(antigravity.get("_meta").is_none());

    let proposal =
        roundtable_protocol::submit_result_input_schema(roundtable_protocol::PhaseKind::Proposal);
    let required = proposal["properties"]["result"]["required"]
        .as_array()
        .expect("proposal required");
    assert!(required.iter().any(|field| field == "claims"));
    let example =
        roundtable_protocol::seat_schema_example(roundtable_protocol::PhaseKind::Proposal);
    assert!(example.contains("local_key"));
    let synthesis =
        roundtable_protocol::submit_result_input_schema(roundtable_protocol::PhaseKind::Synthesis);
    let properties = synthesis["properties"]["result"]["properties"]
        .as_object()
        .expect("synthesis properties");
    assert!(properties.contains_key("recommendation"));
    assert!(!properties.contains_key("speaker_id"));
    assert!(!properties.contains_key("coverage"));
}

#[test]
fn rejected_frame_excerpt_drops_token_values() {
    let raw = br#"{"refresh_token":"super-secret-token-value-1234567890","access_token":"another-secret-value","score":1.5}"#;
    let excerpt = codeg_lib::roundtable::redact_untrusted_excerpt_fixture(raw);
    assert!(!excerpt.contains("super-secret"));
    assert!(!excerpt.contains("another-secret"));
    assert!(excerpt.contains("[redacted]"));
    assert!(excerpt.chars().count() <= 200);
}

#[cfg(unix)]
#[test]
fn reap_deletes_grok_auth_and_limits_old_run_directories() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::time::{Duration, UNIX_EPOCH};

    let root = scratch();
    let runs = root.join("runs");
    let keep = codeg_lib::roundtable::run_dir_retention_fixture();
    for index in 0..keep + 2 {
        let dir = runs.join(format!("run-{index:02}"));
        let auth = dir.join("scratch/rt-home/.grok/auth.json");
        fs::create_dir_all(auth.parent().expect("auth parent")).expect("run");
        fs::write(&auth, b"refresh-token-secret").expect("auth");
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).expect("mode");
        assert_eq!(
            fs::metadata(&auth).expect("meta").permissions().mode() & 0o777,
            0o600
        );
        fs::File::open(&dir)
            .expect("dir")
            .set_modified(UNIX_EPOCH + Duration::from_secs(index as u64))
            .expect("mtime");
    }
    let outside = root.join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("marker"), b"keep").expect("marker");
    symlink(&outside, runs.join("linked")).expect("symlink");

    let newest = format!("run-{:02}", keep + 1);
    // Only explicitly reaped attempts are retention candidates. Existing
    // directories alone do not prove that another participant has stopped.
    for index in 0..keep + 2 {
        codeg_lib::roundtable::retire_attempt_files_fixture(&root, &format!("run-{index:02}")).expect("retire");
    }
    let auth = runs.join(&newest).join("scratch/rt-home/.grok/auth.json");
    assert!(!auth.exists());
    assert!(!runs.join(&newest).join("scratch").exists());
    assert!(runs.join(&newest).is_dir());
    let remaining = fs::read_dir(&runs)
        .expect("runs")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().ok().is_some_and(|kind| kind.is_dir()))
        .count();
    assert_eq!(remaining, keep);
    assert!(!runs.join("run-00").exists());
    assert!(!runs.join("run-01").exists());
    assert_eq!(fs::read(outside.join("marker")).expect("marker"), b"keep");
    assert!(runs
        .join("linked")
        .symlink_metadata()
        .expect("link")
        .file_type()
        .is_symlink());
    codeg_lib::roundtable::retire_attempt_files_fixture(&root, &newest).expect("idempotent");

    let host = root.join("host-auth.json");
    fs::write(&host, b"host-secret").expect("host");
    let linked_auth = runs
        .join("symlink-auth")
        .join("scratch/rt-home/.grok/auth.json");
    fs::create_dir_all(linked_auth.parent().expect("parent")).expect("home");
    symlink(&host, &linked_auth).expect("auth link");
    codeg_lib::roundtable::retire_attempt_files_fixture(&root, "symlink-auth").expect("unlink");
    assert_eq!(fs::read(&host).expect("host remains"), b"host-secret");
    assert!(!linked_auth.exists());

    let host_home = root.join("host-home/.grok");
    fs::create_dir_all(&host_home).expect("host home");
    fs::write(host_home.join("auth.json"), b"real-host").expect("real");
    let scratch = runs.join("mid-link").join("scratch");
    fs::create_dir_all(&scratch).expect("scratch");
    symlink(root.join("host-home"), scratch.join("rt-home")).expect("home link");
    assert!(codeg_lib::roundtable::retire_attempt_files_fixture(&root, "mid-link").is_err());
    assert_eq!(
        fs::read(host_home.join("auth.json")).expect("host auth"),
        b"real-host"
    );

    let escaped = root.join("escaped");
    fs::create_dir_all(&escaped).expect("escaped");
    fs::write(escaped.join("marker"), b"stay").expect("stay");
    let bad = runs.join("bad-incarnation");
    fs::create_dir_all(&bad).expect("bad");
    symlink(&escaped, bad.join("scratch")).expect("scratch link");
    assert!(codeg_lib::roundtable::retire_attempt_files_fixture(&root, "bad-incarnation").is_err());
    assert_eq!(fs::read(escaped.join("marker")).expect("escaped"), b"stay");
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


#[cfg(unix)]
#[test]
fn retirement_pruning_preserves_other_unretired_attempts() {
    use std::time::UNIX_EPOCH;
    let root = scratch();
    let runs = root.join("runs");
    let active = runs.join("old-active/scratch/rt-home/.grok/auth.json");
    fs::create_dir_all(active.parent().unwrap()).unwrap();
    fs::write(&active, b"controlled-active-auth").unwrap();
    fs::File::open(runs.join("old-active")).unwrap().set_modified(UNIX_EPOCH).unwrap();
    // A different attempt may be preparing before its scratch is created.
    fs::create_dir_all(runs.join("old-preparing")).unwrap();
    fs::File::open(runs.join("old-preparing")).unwrap().set_modified(UNIX_EPOCH).unwrap();
    // A stale retirement marker cannot authorize a newly live scratch tree.
    fs::create_dir_all(runs.join("stale-marker/scratch")).unwrap();
    codeg_lib::roundtable::retire_attempt_files_fixture(&root, "stale-marker").unwrap();
    fs::create_dir_all(runs.join("stale-marker/scratch")).unwrap();
    fs::write(runs.join("stale-marker/scratch/keep"), b"new active data").unwrap();
    let keep = codeg_lib::roundtable::run_dir_retention_fixture();
    for index in 0..keep + 3 {
        let name = format!("retired-{index:02}");
        fs::create_dir_all(runs.join(&name).join("scratch")).unwrap();
        codeg_lib::roundtable::retire_attempt_files_fixture(&root, &name).unwrap();
    }
    assert_eq!(fs::read(&active).unwrap(), b"controlled-active-auth");
    assert!(runs.join("old-preparing").is_dir());
    assert_eq!(fs::read(runs.join("stale-marker/scratch/keep")).unwrap(), b"new active data");
    let retired = fs::read_dir(&runs).unwrap().flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with("retired-")).count();
    assert_eq!(retired, keep, "retention bounds only proven retired attempts");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ordinary_acp_completion_without_a_submit_receipt_cannot_certify() {
    let data = scratch();
    let mut facts = passing("grok", "debian-13");
    facts.checks.retain(|check| check.name != "submit_receipt_completion");
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(!outcome.qualification_issued, "pong is not a submit receipt");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    assert_eq!(report["normal_completion_after_submit_receipt"], false);
    let _ = fs::remove_dir_all(data);
}

#[test]
fn a_later_failed_observation_dominates_an_earlier_pass() {
    let data = scratch();
    let mut facts = passing("grok", "debian-13");
    let mut failure = check("model_credential_material_in_sandbox");
    failure.status = "failed".into();
    failure.flag = Some(true);
    facts.checks.push(failure);
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(!outcome.qualification_issued, "conflicting evidence cannot pass");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    assert_eq!(report["model_credential_material_in_sandbox"], true);
    assert_eq!(report["credentials_unreachable"], "failed");
    let _ = fs::remove_dir_all(data);
}

#[test]
fn mounted_auth_material_cannot_be_reported_as_absent() {
    let data = scratch();
    let mut facts = passing("grok", "debian-13");
    facts.oci.as_mut().unwrap().auth_mounts.push(serde_json::from_value(serde_json::json!({
        "source": data.join("fake-auth.json"),
        "destination": "/rt-home/.grok/auth.json",
    })).unwrap());
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(!outcome.qualification_issued, "read-only binds and writable copies both expose bytes");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    assert_eq!(report["model_credential_material_in_sandbox"], true);
    assert_eq!(report["credentials_unreachable"], "failed");
    let _ = fs::remove_dir_all(data);
}

#[test]
fn qualification_source_semantics_are_bound_into_certificates() {
    let data = scratch();
    let outcome = assemble_probe_report_for_test(&data, passing("grok", "debian-13"));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    for name in ["qualification_linux", "qualification_probe", "qualification_profiles", "service_failure_classifier"] {
        assert!(!report["shared_core_hashes"][name].is_null(), "{name}");
    }
    let _ = fs::remove_dir_all(data);
}


#[test]
fn qualification_core_key_set_covers_admission_completion_cleanup_and_privacy() {
    let data = scratch();
    let outcome = assemble_probe_report_for_test(&data, passing("grok", "debian-13"));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    let hashes = report["shared_core_hashes"].as_object().unwrap();
    let actual: std::collections::BTreeSet<_> = hashes.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<_> = [
        "acceptance",
        "acp_connection",
        "acp_host_tools_policy",
        "acp_launch",
        "acp_manager",
        "actor",
        "api",
        "authorization",
        "budget_ledger",
        "canonical",
        "capabilities",
        "clock",
        "companion",
        "companion_entry",
        "companion_transport",
        "connection_purpose",
        "control",
        "conversation_discovery",
        "delivery_encoder",
        "diagnostics",
        "events",
        "feature_gate",
        "gateway",
        "ingress",
        "installed_runtime",
        "internal_sessions",
        "linux_oci",
        "live_gateway",
        "live_runtime",
        "maintenance",
        "mcp",
        "objects",
        "owned_runtime",
        "ownership",
        "paging",
        "product",
        "protocol_admission",
        "protocol_budget",
        "protocol_completion",
        "protocol_dto",
        "protocol_lib",
        "protocol_model",
        "protocol_projection",
        "protocol_strategy",
        "protocol_usage",
        "qualification",
        "qualification_linux",
        "qualification_probe",
        "qualification_profiles",
        "recovery",
        "registry",
        "relay",
        "request_accounting",
        "resources",
        "rollout",
        "roundtable_commands",
        "roundtable_http",
        "runtime",
        "sandbox",
        "schema",
        "service",
        "service_failure_classifier",
        "snapshot",
        "store",
        "tool_core",
        "usage",
        "validator",
        "web_event_bridge",
    ].into_iter().collect();
    assert_eq!(actual, expected, "security-relevant implementation sources must remain certificate-bound");
    for (key, bytes) in [
        ("mcp", include_bytes!("../../src/roundtable/mcp.rs").as_slice()),
        ("resources", include_bytes!("../../src/roundtable/resources.rs").as_slice()),
        ("acp_connection", include_bytes!("../../src/acp/connection.rs").as_slice()),
        ("internal_sessions", include_bytes!("../../src/auto_title/internal_sessions.rs").as_slice()),
        ("protocol_completion", include_bytes!("../../roundtable-protocol/src/completion.rs").as_slice()),
    ] {
        assert_eq!(hashes[key], serde_json::json!(Hash256::sha256(bytes)), "{key}");
    }
    let _ = fs::remove_dir_all(data);
}

#[test]
fn successful_delete_with_leftover_state_is_not_cleanup_proof() {
    assert!(codeg_lib::roundtable::probe_reap_classification(true, "", true, false).is_err());
}

#[test]
fn unmeasured_credential_visibility_is_unknown_rather_than_a_synthetic_fact() {
    let data = scratch();
    let mut facts = passing("grok", "debian-13");
    facts.checks.retain(|check| check.name != "model_credentials_visible_to_agent");
    let outcome = assemble_probe_report_for_test(&data, facts);
    assert!(!outcome.qualification_issued);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(outcome.report_path).unwrap()).unwrap();
    assert!(report["model_credentials_visible_to_agent"].is_null());
    assert_eq!(report["credentials_unreachable"], "not_tested");
    let _ = fs::remove_dir_all(data);
}

#[cfg(target_os = "linux")]
#[test]
fn probe_cleanup_cannot_leave_a_slirp_helper_that_ignores_term() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let runtime_root = root.path().join("runtime");
    fs::create_dir_all(runtime_root.join("slirp-pids")).unwrap();
    let ready = root.path().join("helper-ready");
    let mut helper = std::process::Command::new("sh")
        .env("CODEG_ROUNDTABLE_SLIRP_ROLE", "slirp")
        .env("CODEG_ROUNDTABLE_SLIRP_OWNER", "cq-acp-review")
        .env("CODEG_ROUNDTABLE_SLIRP_ROOT", &runtime_root).arg("-c")
        .arg(format!("trap '' TERM; touch '{}'; exec sleep 30", ready.display()))
        .spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    write_owned_helper_pin(&runtime_root, "cq-acp-review", helper.id());
    let crun = root.path().join("fake-crun");
    fs::write(&crun, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&crun, fs::Permissions::from_mode(0o700)).unwrap();
    let request = ProbeRequest {
        data_dir: root.path().to_path_buf(), agent: "grok".into(), rootfs: root.path().join("rootfs"),
        crun, cgroup_root: root.path().join("fake-cgroup"), runtime_root,
        provider_bindings: root.path().join("bindings.json"), home: root.path().join("home"), profile_id: None,
    };
    assert!(codeg_lib::roundtable::probe_acp_exit_cleans_container(&request, "cq-acp-review").is_err());
    let ended = helper.try_wait().unwrap().is_some();
    let _ = helper.kill();
    let _ = helper.wait();
    assert!(ended, "cleanup reported completion while the network helper survived");
}

#[cfg(target_os = "linux")]
#[test]
fn stale_slirp_pidfile_cannot_signal_an_unrelated_process() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let runtime_root = root.path().join("runtime");
    fs::create_dir_all(runtime_root.join("slirp-pids")).unwrap();
    let mut unrelated = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    write_owned_helper_pin(&runtime_root, "cq-acp-stale", unrelated.id());
    let crun = root.path().join("fake-crun");
    fs::write(&crun, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&crun, fs::Permissions::from_mode(0o700)).unwrap();
    let request = ProbeRequest {
        data_dir: root.path().to_path_buf(), agent: "grok".into(), rootfs: root.path().join("rootfs"),
        crun, cgroup_root: root.path().join("fake-cgroup"), runtime_root,
        provider_bindings: root.path().join("bindings.json"), home: root.path().join("home"), profile_id: None,
    };
    assert!(codeg_lib::roundtable::probe_acp_exit_cleans_container(&request, "cq-acp-stale").is_err());
    std::thread::sleep(std::time::Duration::from_millis(30));
    let survived = unrelated.try_wait().unwrap().is_none();
    let _ = unrelated.kill();
    let _ = unrelated.wait();
    assert!(survived, "an unowned stale PID was signaled");
}

#[cfg(target_os = "linux")]
fn write_owned_helper_pin(runtime_root: &Path, id: &str, pid: u32) {
    use std::os::unix::fs::PermissionsExt;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat.rsplit_once(") ").unwrap().1.split_whitespace().nth(19).unwrap();
    let path = runtime_root.join("slirp-pids").join(format!("{id}.pid"));
    fs::write(&path, format!("slirp {pid} {start}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for (suffix, body) in [("pid.lock", ""), ("pid.state", "running\n")] {
        let state = path.with_extension(suffix);
        fs::write(&state, body).unwrap();
        fs::set_permissions(state, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
