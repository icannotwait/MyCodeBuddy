use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_send_user_message,
    codeg_eui_set_workspace, codeg_eui_shutdown, CodegEuiCompletion, CodegEuiFrame,
    CodegEuiSessionSummary, CodegEuiSlice, CompletionStatus, LifecycleState, Operation,
    CODEG_EUI_API_VERSION, CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_INTERNAL,
    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_INVALID_UTF8, CODEG_EUI_ERR_NOT_READY,
    CODEG_EUI_ERR_NULL_POINTER, CODEG_EUI_ERR_PANIC, CODEG_EUI_ERR_QUEUE_FULL,
    CODEG_EUI_ERR_TOO_LARGE, CODEG_EUI_ERR_WRONG_THREAD, CODEG_EUI_MAX_MESSAGE_BYTES, CODEG_EUI_OK,
};

const CHILD_CASE: &str = "CODEG_EUI_BRIDGE_CONTRACT_CASE";
const CHILD_ROOT: &str = "CODEG_EUI_BRIDGE_CONTRACT_ROOT";

#[test]
fn complete_abi_layout_matches_v1() {
    assert_eq!(CODEG_EUI_OK, 0);
    assert_eq!(CODEG_EUI_ERR_INVALID_STATE, 1);
    assert_eq!(CODEG_EUI_ERR_NULL_POINTER, 2);
    assert_eq!(CODEG_EUI_ERR_INVALID_UTF8, 3);
    assert_eq!(CODEG_EUI_ERR_TOO_LARGE, 4);
    assert_eq!(CODEG_EUI_ERR_QUEUE_FULL, 5);
    assert_eq!(CODEG_EUI_ERR_WRONG_THREAD, 6);
    assert_eq!(CODEG_EUI_ERR_PANIC, 7);
    assert_eq!(CODEG_EUI_ERR_INTERNAL, 8);
    assert_eq!(CODEG_EUI_ERR_NOT_READY, 9);
    assert_eq!(std::mem::size_of::<LifecycleState>(), 4);
    assert_eq!(LifecycleState::Uninitialized as u32, 0);
    assert_eq!(LifecycleState::Starting as u32, 1);
    assert_eq!(LifecycleState::Running as u32, 2);
    assert_eq!(LifecycleState::Stopping as u32, 3);
    assert_eq!(LifecycleState::Stopped as u32, 4);
    assert_eq!(std::mem::size_of::<Operation>(), 4);
    assert_eq!(Operation::SetWorkspace as u32, 1);
    assert_eq!(Operation::CreateSession as u32, 2);
    assert_eq!(Operation::SelectSession as u32, 3);
    assert_eq!(Operation::SendUserMessage as u32, 4);
    assert_eq!(Operation::CancelActiveTurn as u32, 5);
    assert_eq!(Operation::GetAgentSettings as u32, 6);
    assert_eq!(Operation::SetAgentSettings as u32, 7);
    assert_eq!(Operation::ProbeAgent as u32, 8);
    assert_eq!(std::mem::size_of::<CompletionStatus>(), 4);
    assert_eq!(CompletionStatus::Ok as u32, 0);
    assert_eq!(CompletionStatus::Error as u32, 1);
    assert_eq!(CompletionStatus::Stale as u32, 2);
    assert_eq!(CompletionStatus::Cancelled as u32, 3);

    assert_eq!(std::mem::size_of::<CodegEuiSlice>(), 16);
    assert_eq!(std::mem::align_of::<CodegEuiSlice>(), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiSlice, ptr), 0);
    assert_eq!(std::mem::offset_of!(CodegEuiSlice, len), 8);

    assert_eq!(std::mem::size_of::<CodegEuiSessionSummary>(), 48);
    assert_eq!(std::mem::align_of::<CodegEuiSessionSummary>(), 8);
    assert_eq!(
        std::mem::offset_of!(CodegEuiSessionSummary, conversation_id),
        0
    );
    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, _reserved), 4);
    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, title), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, agent), 24);
    assert_eq!(
        std::mem::offset_of!(CodegEuiSessionSummary, updated_at_ms),
        40
    );

    assert_eq!(std::mem::size_of::<CodegEuiCompletion>(), 48);
    assert_eq!(std::mem::align_of::<CodegEuiCompletion>(), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, request_id), 0);
    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, op), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, status), 12);
    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, result_payload), 16);
    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, error), 32);

    assert_eq!(std::mem::size_of::<CodegEuiFrame>(), 160);
    assert_eq!(std::mem::align_of::<CodegEuiFrame>(), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, api_version), 0);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, lifecycle_state), 4);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, generation), 8);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, selection_epoch), 16);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, sessions), 24);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, sessions_len), 32);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, connection_id), 40);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, event_seq), 56);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, transcript_json), 64);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, live_assistant), 80);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, stream_active), 96);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, needs_resync), 97);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, shutdown_ready), 98);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, _reserved), 99);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, error_strip), 104);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, completions), 120);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, completions_len), 128);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t0_ns), 136);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t_first_token_ns), 144);
    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t_end_ns), 152);
}

#[test]
fn lifecycle_rejects_invalid_order_and_wrong_thread() {
    run_isolated("lifecycle", || {
        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
        assert_eq!(init(), CODEG_EUI_OK);
        assert_eq!(init(), CODEG_EUI_ERR_INVALID_STATE);
        assert_eq!(poll().lifecycle_state, LifecycleState::Running as u32);
        assert_eq!(
            std::thread::spawn(|| {
                let mut frame = CodegEuiFrame::default();
                codeg_eui_poll(&mut frame)
            })
            .join()
            .expect("wrong-thread poll joined"),
            CODEG_EUI_ERR_WRONG_THREAD
        );
        assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
        let mut request_id = 0;
        assert_eq!(
            codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut request_id),
            CODEG_EUI_ERR_INVALID_STATE
        );
        assert_eq!(
            codeg_eui_shutdown(),
            codeg_eui_core::CODEG_EUI_ERR_NOT_READY
        );
        assert_eq!(poll().lifecycle_state, LifecycleState::Stopping as u32);
        drain_until_ready();
        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
    });
}

#[test]
fn strings_reject_null_invalid_utf8_and_bounds_without_accepting_a_request() {
    run_isolated("input", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 91;
        assert_eq!(
            codeg_eui_send_user_message(std::ptr::null(), 1, &mut request_id),
            CODEG_EUI_ERR_NULL_POINTER
        );
        assert_eq!(request_id, 91);
        assert_eq!(
            codeg_eui_send_user_message([0xff].as_ptr(), 1, &mut request_id),
            CODEG_EUI_ERR_INVALID_UTF8
        );
        assert_eq!(request_id, 91);
        let oversized = vec![b'x'; CODEG_EUI_MAX_MESSAGE_BYTES + 1];
        assert_eq!(
            codeg_eui_send_user_message(oversized.as_ptr(), oversized.len(), &mut request_id,),
            CODEG_EUI_ERR_TOO_LARGE
        );
        assert_eq!(request_id, 91);
        assert_eq!(
            codeg_eui_send_user_message(b"x".as_ptr(), 1, std::ptr::null_mut()),
            CODEG_EUI_ERR_NULL_POINTER
        );
        assert!(copy_completions(&poll()).is_empty());
        complete_shutdown();
    });
}

#[test]
fn path_inputs_reject_embedded_nul_without_accepting_a_request() {
    run_isolated("path_nul", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut request_id = 91;
        let path = b"workspace\0suffix";
        assert_eq!(
            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
            CODEG_EUI_ERR_INVALID_STATE
        );
        assert_eq!(request_id, 91);
        assert!(copy_completions(&poll()).is_empty());
        complete_shutdown();
    });
}

#[test]
fn queue_rejects_the_257th_request_before_acceptance() {
    run_isolated("queue", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let mut ids = Vec::with_capacity(CODEG_EUI_COMPLETION_CAPACITY);
        for _ in 0..CODEG_EUI_COMPLETION_CAPACITY {
            let mut request_id = 0;
            assert_eq!(
                codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut request_id),
                CODEG_EUI_OK
            );
            ids.push(request_id);
        }
        assert!(ids.iter().all(|request_id| *request_id != 0));
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
        let max_request_id = *ids.iter().max().expect("accepted request IDs");

        let mut rejected_id = 777;
        assert_eq!(
            codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut rejected_id),
            CODEG_EUI_ERR_QUEUE_FULL
        );
        assert_eq!(rejected_id, 777);

        let seen = collect_completions(ids.len());
        assert_eq!(seen.len(), ids.len());
        assert_eq!(
            seen.iter()
                .map(|item| item.request_id)
                .collect::<HashSet<_>>(),
            ids.into_iter().collect::<HashSet<_>>()
        );
        complete_shutdown();

        assert_eq!(init(), CODEG_EUI_OK);
        let mut restarted_id = 0;
        assert_eq!(
            codeg_eui_send_user_message(b"restart".as_ptr(), 7, &mut restarted_id),
            CODEG_EUI_OK
        );
        assert!(restarted_id > max_request_id);
        complete_shutdown();
    });
}

#[test]
fn frame_bytes_survive_enqueue_and_failed_poll_then_transfer_once() {
    run_isolated("frame", || {
        assert_eq!(init(), CODEG_EUI_OK);
        let frame_a = poll();
        let generation_a = frame_a.generation;

        let mut request_id = 0;
        assert_eq!(
            codeg_eui_send_user_message(b"frame-a".as_ptr(), 7, &mut request_id),
            CODEG_EUI_OK
        );
        assert_eq!(frame_a.generation, generation_a);

        let frame_b = poll_until_completion(request_id);
        let completion_b = unsafe {
            std::slice::from_raw_parts(frame_b.completions, frame_b.completions_len)
                .iter()
                .find(|completion| completion.request_id == request_id)
                .copied()
                .expect("completion in frame B")
        };
        let expected_error = copy_slice(completion_b.error);
        assert!(!expected_error.is_empty());

        let mut later_id = 0;
        assert_eq!(
            codeg_eui_send_user_message(b"later".as_ptr(), 5, &mut later_id),
            CODEG_EUI_OK
        );
        assert_eq!(copy_slice(completion_b.error), expected_error);

        assert_eq!(
            std::thread::spawn(|| {
                let mut frame = CodegEuiFrame::default();
                codeg_eui_poll(&mut frame)
            })
            .join()
            .expect("failed poll joined"),
            CODEG_EUI_ERR_WRONG_THREAD
        );
        assert_eq!(copy_slice(completion_b.error), expected_error);

        let frame_c = poll();
        assert!(copy_completions(&frame_c)
            .iter()
            .all(|completion| completion.request_id != request_id));
        complete_shutdown();
    });
}

#[derive(Debug)]
struct CompletionCopy {
    request_id: u64,
}

fn run_isolated(case: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
        body();
        return;
    }
    if std::env::var_os(CHILD_CASE).is_some() {
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", std::thread::current().name().expect("test name")])
        .env(CHILD_CASE, case)
        .env(CHILD_ROOT, root.path())
        .status()
        .expect("run isolated bridge contract");
    assert!(status.success(), "isolated bridge case {case} failed");
}

fn init() -> i32 {
    let root = std::env::var(CHILD_ROOT).expect("isolated root");
    codeg_eui_init(root.as_ptr(), root.len())
}

fn poll() -> CodegEuiFrame {
    let mut frame = CodegEuiFrame::default();
    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
    assert_eq!(frame.api_version, CODEG_EUI_API_VERSION);
    frame
}

fn poll_until_completion(request_id: u64) -> CodegEuiFrame {
    for _ in 0..200 {
        let frame = poll();
        if copy_completions(&frame)
            .iter()
            .any(|completion| completion.request_id == request_id)
        {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("request {request_id} did not complete");
}

fn collect_completions(expected: usize) -> Vec<CompletionCopy> {
    let mut seen = Vec::new();
    for _ in 0..200 {
        seen.extend(copy_completions(&poll()));
        if seen.len() == expected {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("observed {} of {expected} completions", seen.len());
}

fn copy_completions(frame: &CodegEuiFrame) -> Vec<CompletionCopy> {
    if frame.completions_len == 0 {
        assert!(frame.completions.is_null());
        return Vec::new();
    }
    assert!(!frame.completions.is_null());
    unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) }
        .iter()
        .map(|completion| CompletionCopy {
            request_id: completion.request_id,
        })
        .collect()
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
    drain_until_ready();
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
}

fn drain_until_ready() {
    for _ in 0..200 {
        if poll().shutdown_ready == 1 {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("shutdown did not become ready");
}
