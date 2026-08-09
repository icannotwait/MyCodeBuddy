use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const CODEG_EUI_API_VERSION: u32 = 1;
pub const CODEG_EUI_OK: i32 = 0;
pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;

const LIFECYCLE_UNINITIALIZED: u32 = 0;
const LIFECYCLE_STARTING: u32 = 1;
const LIFECYCLE_RUNNING: u32 = 2;
const LIFECYCLE_STOPPING: u32 = 3;
const LIFECYCLE_STOPPED: u32 = 4;

static LIFECYCLE: AtomicU32 = AtomicU32::new(LIFECYCLE_UNINITIALIZED);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_READY: AtomicBool = AtomicBool::new(false);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 7],
}

fn ffi_status(operation: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(CODEG_EUI_ERR_INVALID_STATE)
}

#[no_mangle]
pub extern "C" fn codeg_eui_api_version() -> u32 {
    catch_unwind(|| CODEG_EUI_API_VERSION).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize) -> i32 {
    ffi_status(|| {
        if data_dir_utf8.is_null() && data_dir_len > 0 {
            return CODEG_EUI_ERR_NULL_POINTER;
        }

        let current = LIFECYCLE.load(Ordering::Acquire);
        if current != LIFECYCLE_UNINITIALIZED && current != LIFECYCLE_STOPPED {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        LIFECYCLE.store(LIFECYCLE_STARTING, Ordering::Release);
        GENERATION.store(0, Ordering::Release);
        SHUTDOWN_READY.store(false, Ordering::Release);
        LIFECYCLE.store(LIFECYCLE_RUNNING, Ordering::Release);
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
    ffi_status(|| {
        if out.is_null() {
            return CODEG_EUI_ERR_NULL_POINTER;
        }

        let lifecycle_state = LIFECYCLE.load(Ordering::Acquire);
        if lifecycle_state != LIFECYCLE_RUNNING && lifecycle_state != LIFECYCLE_STOPPING {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        let shutdown_ready = lifecycle_state == LIFECYCLE_STOPPING;
        let frame = CodegEuiFrame {
            api_version: CODEG_EUI_API_VERSION,
            lifecycle_state,
            generation: GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
            shutdown_ready: u8::from(shutdown_ready),
            _reserved: [0; 7],
        };

        // The caller owns `out` and must provide writable storage for one frame.
        unsafe { out.write(frame) };
        if shutdown_ready {
            SHUTDOWN_READY.store(true, Ordering::Release);
        }
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_begin_shutdown() -> i32 {
    ffi_status(|| {
        if LIFECYCLE
            .compare_exchange(
                LIFECYCLE_RUNNING,
                LIFECYCLE_STOPPING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return CODEG_EUI_ERR_INVALID_STATE;
        }
        SHUTDOWN_READY.store(false, Ordering::Release);
        CODEG_EUI_OK
    })
}

#[no_mangle]
pub extern "C" fn codeg_eui_shutdown() -> i32 {
    ffi_status(|| {
        if LIFECYCLE.load(Ordering::Acquire) != LIFECYCLE_STOPPING
            || !SHUTDOWN_READY.load(Ordering::Acquire)
        {
            return CODEG_EUI_ERR_INVALID_STATE;
        }

        SHUTDOWN_READY.store(false, Ordering::Release);
        LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
        CODEG_EUI_OK
    })
}
