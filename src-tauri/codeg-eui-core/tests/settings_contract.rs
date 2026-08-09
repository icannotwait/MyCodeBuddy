use std::process::Command;
use std::thread;
use std::time::Duration;

use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_get_agent_settings, codeg_eui_init, codeg_eui_poll,
    codeg_eui_probe_agent, codeg_eui_set_agent_settings, codeg_eui_shutdown, CodegEuiCompletion,
    CodegEuiFrame, CodegEuiSlice, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_TOO_LARGE,
    CODEG_EUI_MAX_SETTINGS_JSON_BYTES, CODEG_EUI_OK,
};
use serde_json::Value;

const CASE_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_CASE";
const ROOT_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_ROOT";

#[test]
fn malformed_patch_is_rejected_before_acceptance() {
    run_isolated("malformed", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 1234;
        let agent = b"codex";
        let malformed = br#"{"enabled":true,"unknown":1}"#;
        assert_eq!(
            codeg_eui_set_agent_settings(
                agent.as_ptr(),
                agent.len(),
                malformed.as_ptr(),
                malformed.len(),
                &mut request_id,
            ),
            CODEG_EUI_ERR_INVALID_STATE
        );
        assert_eq!(request_id, 1234);
        complete_shutdown();
    });
}

#[test]
fn oversized_patch_is_rejected_before_acceptance() {
    run_isolated("oversized", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 1234;
        let oversized = vec![b' '; CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1];
        assert_eq!(
            codeg_eui_set_agent_settings(
                b"codex".as_ptr(),
                5,
                oversized.as_ptr(),
                oversized.len(),
                &mut request_id,
            ),
            CODEG_EUI_ERR_TOO_LARGE
        );
        assert_eq!(request_id, 1234);
        complete_shutdown();
    });
}

#[test]
fn unsupported_agent_completes_with_an_error() {
    run_isolated("unsupported", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let agent = b"claude_code";
        let mut request_id = 0;
        assert_eq!(
            codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
            CODEG_EUI_OK
        );

        let completion = wait_for_completion(request_id);
        assert_eq!(completion.status, 1);
        assert!(completion.result_payload.is_empty());
        assert!(String::from_utf8_lossy(&completion.error).contains("unsupported EUI agent"));
        complete_shutdown();
    });
}

#[test]
fn probe_result_arrives_through_the_public_abi() {
    run_isolated("probe", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 0;
        assert_eq!(
            codeg_eui_probe_agent(b"codex".as_ptr(), 5, &mut request_id),
            CODEG_EUI_OK
        );

        let completion = wait_for_completion(request_id);
        assert_completion_ok(&completion);
        let probe: Value =
            serde_json::from_slice(&completion.result_payload).expect("probe completion JSON");
        assert!(probe["launchable"].is_boolean());
        assert!(probe["message"].is_string());
        assert!(probe["installedVersion"].is_null() || probe["installedVersion"].is_string());
        complete_shutdown();
    });
}

#[test]
fn settings_get_result_arrives_through_poll_completion() {
    run_isolated("get", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 0;
        let agent = b"codex";
        assert_eq!(
            codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
            CODEG_EUI_OK
        );
        let completion = wait_for_completion(request_id);
        assert_eq!(completion.request_id, request_id);
        assert_eq!(completion.status, 0);
        assert!(!completion.result_payload.is_empty());
        complete_shutdown();
    });
}

#[test]
fn codex_and_grok_settings_round_trip_through_native_files() {
    run_isolated("round_trip", || {
        assert_eq!(init(), CODEG_EUI_OK);

        let codex_patch = br#"{
            "enabled":true,
            "env":{"OPENAI_API_KEY":"test-key"},
            "codexAuthJson":"{\"OPENAI_API_KEY\":\"test-key\"}",
            "codexConfigToml":"model = \"gpt-5\"\napproval_policy = \"never\"\n"
        }"#;
        let mut codex_set_id = 0;
        assert_eq!(
            codeg_eui_set_agent_settings(
                b"codex".as_ptr(),
                5,
                codex_patch.as_ptr(),
                codex_patch.len(),
                &mut codex_set_id,
            ),
            CODEG_EUI_OK
        );
        let codex_set = wait_for_completion(codex_set_id);
        assert_completion_ok(&codex_set);

        let codex = get_settings("codex");
        assert_eq!(codex["agentType"], "codex");
        assert_eq!(codex["enabled"], true);
        assert_eq!(codex["env"]["OPENAI_API_KEY"], "test-key");
        assert_eq!(
            codex["codexConfigToml"],
            "model = \"gpt-5\"\napproval_policy = \"never\"\n"
        );
        assert_eq!(codex["codexAuthJson"], r#"{"OPENAI_API_KEY":"test-key"}"#);
        assert!(codex["grokConfigToml"].is_null());

        let codex_home = std::env::var("CODEX_HOME").expect("isolated CODEX_HOME");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&codex_home).join("config.toml"))
                .expect("Codex config.toml"),
            codex["codexConfigToml"].as_str().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&codex_home).join("auth.json"))
                .expect("Codex auth.json"),
            codex["codexAuthJson"].as_str().unwrap()
        );

        let grok_patch = br#"{
            "grokConfigToml":"[ui]\npermission_mode = \"default\"\n",
            "grokStructured":{"defaultReasoningEffort":"high","permissionMode":"plan"}
        }"#;
        let mut grok_set_id = 0;
        assert_eq!(
            codeg_eui_set_agent_settings(
                b"grok".as_ptr(),
                4,
                grok_patch.as_ptr(),
                grok_patch.len(),
                &mut grok_set_id,
            ),
            CODEG_EUI_OK
        );
        let grok_set = wait_for_completion(grok_set_id);
        assert_completion_ok(&grok_set);

        let grok = get_settings("grok");
        assert_eq!(grok["agentType"], "grok");
        assert_eq!(grok["grokSettings"]["default_reasoning_effort"], "high");
        assert_eq!(grok["grokSettings"]["permission_mode"], "plan");
        assert!(grok["codexConfigToml"].is_null());
        let grok_toml = grok["grokConfigToml"].as_str().expect("Grok raw TOML");
        assert!(grok_toml.contains("default_reasoning_effort = \"high\""));
        assert!(grok_toml.contains("permission_mode = \"plan\""));

        let grok_home = std::env::var("GROK_HOME").expect("isolated GROK_HOME");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&grok_home).join("config.toml"))
                .expect("Grok config.toml"),
            grok_toml
        );

        complete_shutdown();
    });
}

fn run_isolated(case: &str, body: impl FnOnce()) {
    if std::env::var(CASE_ENV).as_deref() == Ok(case) {
        body();
        return;
    }
    if std::env::var_os(CASE_ENV).is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let status = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", thread::current().name().expect("test name")])
        .env(CASE_ENV, case)
        .env(ROOT_ENV, root.path())
        .env("CODEX_HOME", root.path().join("codex"))
        .env("GROK_HOME", root.path().join("grok"))
        .status()
        .expect("run isolated settings contract");
    assert!(status.success(), "isolated case {case} failed");
}

fn init() -> i32 {
    let root = std::env::var(ROOT_ENV).expect("isolated root");
    codeg_eui_init(root.as_ptr(), root.len())
}

fn poll() -> CodegEuiFrame {
    let mut frame = CodegEuiFrame::default();
    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
    frame
}

#[derive(Debug)]
struct OwnedCompletion {
    request_id: u64,
    status: u32,
    result_payload: Vec<u8>,
    error: Vec<u8>,
}

impl OwnedCompletion {
    fn copy_from(completion: CodegEuiCompletion) -> Self {
        Self {
            request_id: completion.request_id,
            status: completion.status,
            result_payload: copy_slice(completion.result_payload),
            error: copy_slice(completion.error),
        }
    }
}

fn copy_slice(slice: CodegEuiSlice) -> Vec<u8> {
    if slice.len == 0 {
        return Vec::new();
    }
    assert!(
        !slice.ptr.is_null(),
        "non-empty ABI slice must have a pointer"
    );
    unsafe { std::slice::from_raw_parts(slice.ptr, slice.len).to_vec() }
}

fn get_settings(agent: &str) -> Value {
    let mut request_id = 0;
    assert_eq!(
        codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
        CODEG_EUI_OK
    );
    let completion = wait_for_completion(request_id);
    assert_completion_ok(&completion);
    serde_json::from_slice(&completion.result_payload).expect("settings completion JSON")
}

fn assert_completion_ok(completion: &OwnedCompletion) {
    assert_eq!(
        completion.status,
        0,
        "completion failed: {}",
        String::from_utf8_lossy(&completion.error)
    );
    assert!(!completion.result_payload.is_empty());
}

fn wait_for_completion(request_id: u64) -> OwnedCompletion {
    for _ in 0..200 {
        let frame = poll();
        if frame.completions_len > 0 {
            let completions =
                unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
            if let Some(completion) = completions
                .iter()
                .find(|completion| completion.request_id == request_id)
            {
                return OwnedCompletion::copy_from(*completion);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("request {request_id} did not complete");
}

fn complete_shutdown() {
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    for _ in 0..200 {
        if poll().shutdown_ready == 1 {
            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("shutdown did not become ready");
}
