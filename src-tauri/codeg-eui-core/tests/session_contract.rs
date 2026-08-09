use std::process::Command;
use std::time::Duration;

use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_select_session,
    codeg_eui_set_workspace, codeg_eui_shutdown, CodegEuiCompletion, CodegEuiFrame, CodegEuiSlice,
    CompletionStatus, Operation, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
};

const CHILD_CASE: &str = "CODEG_EUI_SESSION_CONTRACT_CASE";
const CHILD_ROOT: &str = "CODEG_EUI_SESSION_CONTRACT_ROOT";
const CHILD_WORKSPACE: &str = "CODEG_EUI_SESSION_CONTRACT_WORKSPACE";

#[test]
fn workspace_selection_uses_the_canonical_directory_and_advances_the_epoch() {
    run_isolated("workspace", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let workspace =
            std::fs::canonicalize(std::env::var(CHILD_WORKSPACE).expect("isolated workspace path"))
                .expect("canonical workspace");
        let path = workspace.to_string_lossy();
        let mut request_id = 0;

        assert_eq!(
            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
            CODEG_EUI_OK
        );

        let frame = poll_until_completion(request_id);
        let completion = completion_for(&frame, request_id);
        assert_eq!(completion.op, Operation::SetWorkspace as u32);
        assert_eq!(completion.status, CompletionStatus::Ok as u32);
        let payload: serde_json::Value =
            serde_json::from_slice(&copy_slice(completion.result_payload))
                .expect("workspace completion JSON");
        assert_eq!(payload["path"], path.as_ref());
        assert!(payload["folderId"].as_i64().is_some_and(|id| id > 0));
        assert_eq!(payload["sessions"], serde_json::json!([]));
        assert_eq!(frame.selection_epoch, 1);
        assert_eq!(frame.sessions_len, 0);
        complete_shutdown();
    });
}

#[test]
fn non_directory_workspace_terminalizes_as_an_error() {
    run_isolated("workspace_file", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let workspace = std::env::var(CHILD_WORKSPACE).expect("isolated workspace path");
        let file = std::path::Path::new(&workspace).join("not-a-directory.txt");
        std::fs::write(&file, b"fixture").expect("workspace file fixture");
        let path = file.to_string_lossy();
        let mut request_id = 0;

        assert_eq!(
            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
            CODEG_EUI_OK
        );

        let frame = poll_until_completion(request_id);
        let completion = completion_for(&frame, request_id);
        assert_eq!(completion.status, CompletionStatus::Error as u32);
        assert!(copy_slice(completion.result_payload).is_empty());
        assert!(!copy_slice(completion.error).is_empty());
        assert_eq!(frame.selection_epoch, 1);
        assert_eq!(frame.sessions_len, 0);
        complete_shutdown();
    });
}

#[test]
fn invalid_conversation_id_is_rejected_before_acceptance() {
    run_isolated("invalid_conversation", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 91;
        assert_eq!(
            codeg_eui_select_session(0, &mut request_id),
            CODEG_EUI_ERR_INVALID_STATE
        );
        assert_eq!(request_id, 91);
        assert!(completions(&poll()).is_empty());
        complete_shutdown();
    });
}

fn run_isolated(case: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
        body();
        return;
    }
    if std::env::var_os(CHILD_CASE).is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("data root");
    let workspace = tempfile::tempdir().expect("workspace root");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", std::thread::current().name().expect("test name")])
        .env(CHILD_CASE, case)
        .env(CHILD_ROOT, root.path())
        .env(CHILD_WORKSPACE, workspace.path())
        .status()
        .expect("run isolated session contract");
    assert!(status.success(), "isolated session case {case} failed");
}

fn init() -> i32 {
    let root = std::env::var(CHILD_ROOT).expect("isolated root");
    codeg_eui_init(root.as_ptr(), root.len())
}

fn poll() -> CodegEuiFrame {
    let mut frame = CodegEuiFrame::default();
    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
    frame
}

fn poll_until_completion(request_id: u64) -> CodegEuiFrame {
    for _ in 0..400 {
        let frame = poll();
        if completions(&frame)
            .iter()
            .any(|completion| completion.request_id == request_id)
        {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("request {request_id} did not complete");
}

fn completion_for(frame: &CodegEuiFrame, request_id: u64) -> CodegEuiCompletion {
    completions(frame)
        .iter()
        .find(|completion| completion.request_id == request_id)
        .copied()
        .expect("completion for request")
}

fn completions(frame: &CodegEuiFrame) -> &[CodegEuiCompletion] {
    if frame.completions_len == 0 {
        assert!(frame.completions.is_null());
        return &[];
    }
    assert!(!frame.completions.is_null());
    unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) }
}

fn copy_slice(slice: CodegEuiSlice) -> Vec<u8> {
    if slice.len == 0 {
        assert!(slice.ptr.is_null());
        return Vec::new();
    }
    assert!(!slice.ptr.is_null());
    unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) }.to_vec()
}

fn complete_shutdown() {
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    for _ in 0..400 {
        if poll().shutdown_ready == 1 {
            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("shutdown did not become ready");
}
