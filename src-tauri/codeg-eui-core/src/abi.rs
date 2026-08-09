use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::commands::{enqueue, CommandPayload, Operation};
use crate::model::{CodegEuiCompletion, CodegEuiSessionSummary, CodegEuiSlice, OwnedFrame};
use crate::runtime::RuntimeOwner;
use crate::{BootstrapError, DataRootError, EuiBootstrap, SharedModel};

pub const CODEG_EUI_API_VERSION: u32 = 1;
pub const CODEG_EUI_OK: i32 = 0;
pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
pub const CODEG_EUI_ERR_INVALID_UTF8: i32 = 3;
pub const CODEG_EUI_ERR_TOO_LARGE: i32 = 4;
pub const CODEG_EUI_ERR_QUEUE_FULL: i32 = 5;
pub const CODEG_EUI_ERR_WRONG_THREAD: i32 = 6;
pub const CODEG_EUI_ERR_PANIC: i32 = 7;
pub const CODEG_EUI_ERR_INTERNAL: i32 = 8;
pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;

pub const CODEG_EUI_MAX_PATH_BYTES: usize = 32_768;
pub const CODEG_EUI_MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const CODEG_EUI_MAX_SETTINGS_JSON_BYTES: usize = 2_097_152;
pub const CODEG_EUI_COMMAND_QUEUE_CAPACITY: usize = 256;
pub const CODEG_EUI_COMPLETION_CAPACITY: usize = 256;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LifecycleState {
    #[default]
    Uninitialized = 0,
    Starting = 1,
    Running = 2,
    Stopping = 3,
    Stopped = 4,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub selection_epoch: u64,
    pub sessions: *const CodegEuiSessionSummary,
    pub sessions_len: usize,
    pub connection_id: CodegEuiSlice,
    pub event_seq: u64,
    pub transcript_json: CodegEuiSlice,
    pub live_assistant: CodegEuiSlice,
    pub stream_active: u8,
    pub needs_resync: u8,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 5],
    pub error_strip: CodegEuiSlice,
    pub completions: *const CodegEuiCompletion,
    pub completions_len: usize,
    pub t0_ns: u64,
    pub t_first_token_ns: u64,
    pub t_end_ns: u64,
}

impl Default for CodegEuiFrame {
    fn default() -> Self {
        Self {
            api_version: 0,
            lifecycle_state: LifecycleState::Uninitialized as u32,
            generation: 0,
            selection_epoch: 0,
            sessions: std::ptr::null(),
            sessions_len: 0,
            connection_id: CodegEuiSlice::default(),
            event_seq: 0,
            transcript_json: CodegEuiSlice::default(),
            live_assistant: CodegEuiSlice::default(),
            stream_active: 0,
            needs_resync: 0,
            shutdown_ready: 0,
            _reserved: [0; 5],
            error_strip: CodegEuiSlice::default(),
            completions: std::ptr::null(),
            completions_len: 0,
            t0_ns: 0,
            t_first_token_ns: 0,
            t_end_ns: 0,
        }
    }
}

struct BridgeSlot {
    lifecycle: LifecycleState,
    ui_thread: Option<std::thread::ThreadId>,
    runtime: Option<RuntimeOwner>,
    model: SharedModel,
    last_frame: Option<OwnedFrame>,
    generation: u64,
    shutdown_ready_observed: bool,
}

impl Default for BridgeSlot {
    fn default() -> Self {
        Self {
            lifecycle: LifecycleState::Uninitialized,
            ui_thread: None,
            runtime: None,
            model: SharedModel::new(),
            last_frame: None,
            generation: 0,
            shutdown_ready_observed: false,
        }
    }
}

static BRIDGE: OnceLock<Mutex<BridgeSlot>> = OnceLock::new();

fn bridge() -> &'static Mutex<BridgeSlot> {
    BRIDGE.get_or_init(|| Mutex::new(BridgeSlot::default()))
}

fn lock_bridge() -> MutexGuard<'static, BridgeSlot> {
    bridge().lock().unwrap_or_else(|error| error.into_inner())
}

fn ffi_guard(body: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(code) => code,
        Err(_) => {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                record_panic_diagnostic("Rust panic contained at codeg-eui ABI");
            }));
            CODEG_EUI_ERR_PANIC
        }
    }
}

fn record_panic_diagnostic(message: &str) {
    eprintln!("{message}");
    lock_bridge()
        .model
        .set_error_strip(message.as_bytes().to_vec());
}

fn ensure_ui_thread(slot: &BridgeSlot) -> Result<(), i32> {
    if slot
        .ui_thread
        .as_ref()
        .is_some_and(|thread| *thread != std::thread::current().id())
    {
        Err(CODEG_EUI_ERR_WRONG_THREAD)
    } else {
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn codeg_eui_api_version() -> u32 {
    catch_unwind(|| CODEG_EUI_API_VERSION).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize) -> i32 {
    let status = ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_ui_thread(&slot) {
            return error;
        }
        if !matches!(
            slot.lifecycle,
            LifecycleState::Uninitialized | LifecycleState::Stopped
        ) {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        slot.lifecycle = LifecycleState::Starting;
        slot.generation = 0;
        slot.shutdown_ready_observed = false;
        slot.last_frame = None;

        let argument_root = match parse_data_root_argument(data_dir_utf8, data_dir_len) {
            Ok(argument_root) => argument_root,
            Err(error) => {
                slot.lifecycle = LifecycleState::Stopped;
                return error;
            }
        };
        let bootstrap = match EuiBootstrap::start_with_data_root_argument(argument_root) {
            Ok(bootstrap) => bootstrap,
            Err(BootstrapError::DataRoot(DataRootError::AlreadyPinned { .. })) => {
                slot.lifecycle = LifecycleState::Stopped;
                return CODEG_EUI_ERR_INVALID_STATE;
            }
            Err(_) => {
                slot.lifecycle = LifecycleState::Stopped;
                return CODEG_EUI_ERR_INTERNAL;
            }
        };

        let model = SharedModel::new();
        slot.runtime = Some(RuntimeOwner::start(bootstrap, model.clone()));
        slot.model = model;
        slot.ui_thread = Some(std::thread::current().id());
        slot.lifecycle = LifecycleState::Running;
        CODEG_EUI_OK
    });
    if status != CODEG_EUI_OK {
        let mut slot = lock_bridge();
        if slot.lifecycle == LifecycleState::Starting {
            slot.runtime = None;
            slot.last_frame = None;
            slot.lifecycle = LifecycleState::Stopped;
        }
    }
    status
}

#[no_mangle]
pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_ui_thread(&slot) {
            return error;
        }
        if out.is_null() {
            return CODEG_EUI_ERR_NULL_POINTER;
        }
        if !matches!(
            slot.lifecycle,
            LifecycleState::Running | LifecycleState::Stopping
        ) {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        let generation = match slot.generation.checked_add(1) {
            Some(generation) => generation,
            None => return CODEG_EUI_ERR_INTERNAL,
        };
        let stopping = slot.lifecycle == LifecycleState::Stopping;
        let quiesced = match slot.runtime.as_ref() {
            Some(runtime) => runtime.quiesced_flag(),
            None => return CODEG_EUI_ERR_INTERNAL,
        };
        let (owned_frame, shutdown_ready) = slot.model.build_frame(stopping, &quiesced);
        let frame = owned_frame.as_abi(slot.lifecycle, generation, shutdown_ready);

        slot.generation = generation;
        slot.last_frame = Some(owned_frame);
        unsafe { out.write(frame) };
        if shutdown_ready {
            slot.shutdown_ready_observed = true;
        }
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_begin_shutdown() -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_ui_thread(&slot) {
            return error;
        }
        if slot.lifecycle != LifecycleState::Running {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        slot.lifecycle = LifecycleState::Stopping;
        slot.shutdown_ready_observed = false;
        match slot.runtime.as_mut() {
            Some(runtime) => runtime.begin_shutdown(),
            None => return CODEG_EUI_ERR_INTERNAL,
        }
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_shutdown() -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_ui_thread(&slot) {
            return error;
        }
        if slot.lifecycle != LifecycleState::Stopping {
            return CODEG_EUI_ERR_INVALID_STATE;
        }
        if !slot.shutdown_ready_observed {
            return CODEG_EUI_ERR_NOT_READY;
        }

        let runtime = match slot.runtime.take() {
            Some(runtime) => runtime,
            None => return CODEG_EUI_ERR_INTERNAL,
        };
        runtime.join();
        slot.last_frame = None;
        slot.lifecycle = LifecycleState::Stopped;
        slot.shutdown_ready_observed = false;
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_set_workspace(
    path_utf8: *const u8,
    path_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    enqueue_path(
        path_utf8,
        path_len,
        CODEG_EUI_MAX_PATH_BYTES,
        out_request_id,
        Operation::SetWorkspace,
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_create_session(
    agent_utf8: *const u8,
    agent_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    enqueue_path(
        agent_utf8,
        agent_len,
        CODEG_EUI_MAX_PATH_BYTES,
        out_request_id,
        Operation::CreateSession,
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_select_session(conversation_id: i32, out_request_id: *mut u64) -> i32 {
    enqueue_payload(
        out_request_id,
        Operation::SelectSession,
        CommandPayload::SelectSession(conversation_id),
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_send_user_message(
    text_utf8: *const u8,
    text_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    enqueue_utf8(
        text_utf8,
        text_len,
        CODEG_EUI_MAX_MESSAGE_BYTES,
        out_request_id,
        Operation::SendUserMessage,
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_cancel_active_turn(out_request_id: *mut u64) -> i32 {
    enqueue_payload(
        out_request_id,
        Operation::CancelActiveTurn,
        CommandPayload::Empty,
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_get_agent_settings(
    agent_utf8: *const u8,
    agent_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    enqueue_path(
        agent_utf8,
        agent_len,
        CODEG_EUI_MAX_PATH_BYTES,
        out_request_id,
        Operation::GetAgentSettings,
    )
}

#[no_mangle]
pub extern "C" fn codeg_eui_set_agent_settings(
    agent_utf8: *const u8,
    agent_len: usize,
    json_utf8: *const u8,
    json_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_running(&slot) {
            return error;
        }
        if out_request_id.is_null() {
            return CODEG_EUI_ERR_NULL_POINTER;
        }
        let agent = match copy_utf8(agent_utf8, agent_len, CODEG_EUI_MAX_PATH_BYTES) {
            Ok(agent) => agent,
            Err(error) => return error,
        };
        if agent.contains(&0) {
            return CODEG_EUI_ERR_INVALID_STATE;
        }
        let json = match copy_utf8(json_utf8, json_len, CODEG_EUI_MAX_SETTINGS_JSON_BYTES) {
            Ok(json) => json,
            Err(error) => return error,
        };
        accept_and_write(
            &mut slot,
            out_request_id,
            Operation::SetAgentSettings,
            CommandPayload::AgentSettings { agent, json },
        )
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_probe_agent(
    agent_utf8: *const u8,
    agent_len: usize,
    out_request_id: *mut u64,
) -> i32 {
    enqueue_path(
        agent_utf8,
        agent_len,
        CODEG_EUI_MAX_PATH_BYTES,
        out_request_id,
        Operation::ProbeAgent,
    )
}

#[doc(hidden)]
pub fn enqueue_blocked_for_test() -> Result<u64, i32> {
    let mut request_id = 0;
    let code = enqueue_payload(
        &mut request_id,
        Operation::SendUserMessage,
        CommandPayload::Blocked,
    );
    if code == CODEG_EUI_OK {
        Ok(request_id)
    } else {
        Err(code)
    }
}

#[cfg(feature = "ffi-test-hooks")]
#[no_mangle]
pub extern "C" fn codeg_eui_test_enqueue_blocked(out_request_id: *mut u64) -> i32 {
    enqueue_payload(
        out_request_id,
        Operation::SendUserMessage,
        CommandPayload::Blocked,
    )
}

fn enqueue_utf8(
    ptr: *const u8,
    len: usize,
    max_len: usize,
    out_request_id: *mut u64,
    op: Operation,
) -> i32 {
    enqueue_utf8_with_policy(ptr, len, max_len, out_request_id, op, false)
}

fn enqueue_path(
    ptr: *const u8,
    len: usize,
    max_len: usize,
    out_request_id: *mut u64,
    op: Operation,
) -> i32 {
    enqueue_utf8_with_policy(ptr, len, max_len, out_request_id, op, true)
}

fn enqueue_utf8_with_policy(
    ptr: *const u8,
    len: usize,
    max_len: usize,
    out_request_id: *mut u64,
    op: Operation,
    reject_nul: bool,
) -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_running(&slot) {
            return error;
        }
        if out_request_id.is_null() {
            return CODEG_EUI_ERR_NULL_POINTER;
        }
        let value = match copy_utf8(ptr, len, max_len) {
            Ok(value) => value,
            Err(error) => return error,
        };
        if reject_nul && value.contains(&0) {
            return CODEG_EUI_ERR_INVALID_STATE;
        }
        accept_and_write(&mut slot, out_request_id, op, CommandPayload::Utf8(value))
    })
}

fn enqueue_payload(out_request_id: *mut u64, op: Operation, payload: CommandPayload) -> i32 {
    ffi_guard(|| {
        let mut slot = lock_bridge();
        if let Err(error) = ensure_running(&slot) {
            return error;
        }
        if out_request_id.is_null() {
            return CODEG_EUI_ERR_NULL_POINTER;
        }
        accept_and_write(&mut slot, out_request_id, op, payload)
    })
}

fn ensure_running(slot: &BridgeSlot) -> Result<(), i32> {
    ensure_ui_thread(slot)?;
    if slot.lifecycle == LifecycleState::Running {
        Ok(())
    } else {
        Err(CODEG_EUI_ERR_INVALID_STATE)
    }
}

fn accept_and_write(
    slot: &mut BridgeSlot,
    out_request_id: *mut u64,
    op: Operation,
    payload: CommandPayload,
) -> i32 {
    let runtime = match slot.runtime.as_ref() {
        Some(runtime) => runtime,
        None => return CODEG_EUI_ERR_INTERNAL,
    };
    match enqueue(runtime, &slot.model, op, payload) {
        Ok(request_id) => {
            unsafe { out_request_id.write(request_id.get()) };
            CODEG_EUI_OK
        }
        Err(error) => error,
    }
}

fn copy_utf8(ptr: *const u8, len: usize, max_len: usize) -> Result<Vec<u8>, i32> {
    if len > max_len {
        return Err(CODEG_EUI_ERR_TOO_LARGE);
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(CODEG_EUI_ERR_NULL_POINTER);
    }

    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_UTF8)?;
    Ok(bytes.to_vec())
}

fn parse_data_root_argument(
    data_dir_utf8: *const u8,
    data_dir_len: usize,
) -> Result<Option<PathBuf>, i32> {
    let bytes = copy_utf8(data_dir_utf8, data_dir_len, CODEG_EUI_MAX_PATH_BYTES)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.contains(&0) {
        return Err(CODEG_EUI_ERR_INVALID_STATE);
    }
    Ok(Some(PathBuf::from(
        String::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_UTF8)?,
    )))
}

#[cfg(test)]
mod tests {
    use super::{ffi_guard, CODEG_EUI_ERR_PANIC};

    #[test]
    fn ffi_guard_contains_panics() {
        assert_eq!(ffi_guard(|| panic!("contained")), CODEG_EUI_ERR_PANIC);
    }
}
