#![cfg(feature = "ffi-test-hooks")]

use std::process::Command;
use std::time::Duration;

use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_send_user_message,
    codeg_eui_shutdown, enqueue_blocked_for_test, CodegEuiFrame, LifecycleState,
    CODEG_EUI_COMPLETION_CANCELLED, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_NOT_READY,
    CODEG_EUI_OK,
};

const CHILD_CASE: &str = "CODEG_EUI_SHUTDOWN_CONTRACT_CASE";
const CHILD_ROOT: &str = "CODEG_EUI_SHUTDOWN_CONTRACT_ROOT";

#[test]
fn stopping_poll_exposes_cancelled_completion_before_final_free() {
    if std::env::var_os(CHILD_CASE).is_none() {
        let root = tempfile::tempdir().expect("tempdir");
        let status = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "stopping_poll_exposes_cancelled_completion_before_final_free",
            ])
            .env(CHILD_CASE, "child")
            .env(CHILD_ROOT, root.path())
            .status()
            .expect("run isolated shutdown contract");
        assert!(status.success(), "isolated shutdown contract failed");
        return;
    }

    let root = std::env::var(CHILD_ROOT).expect("isolated root");
    assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);

    let request_id = enqueue_blocked_for_test().expect("blocked request accepted");
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    let mut rejected_id = 0;
    assert_eq!(
        codeg_eui_send_user_message(b"late".as_ptr(), 4, &mut rejected_id),
        CODEG_EUI_ERR_INVALID_STATE
    );
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);

    let mut cancelled_count = 0;
    let mut ready = false;
    for _ in 0..200 {
        let mut frame = CodegEuiFrame::default();
        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
        assert_eq!(frame.lifecycle_state, LifecycleState::Stopping as u32);
        if frame.completions_len > 0 {
            assert!(!frame.completions.is_null());
            let completions =
                unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
            cancelled_count += completions
                .iter()
                .filter(|completion| {
                    completion.request_id == request_id
                        && completion.status == CODEG_EUI_COMPLETION_CANCELLED
                })
                .count();
        }
        if frame.shutdown_ready == 1 {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(ready, "shutdown-ready frame was not observed");
    assert_eq!(cancelled_count, 1);
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
    let mut after = CodegEuiFrame::default();
    assert_eq!(codeg_eui_poll(&mut after), CODEG_EUI_ERR_INVALID_STATE);
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
}
