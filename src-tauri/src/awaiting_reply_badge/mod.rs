//! Windows taskbar awaiting-reply badge.
//!
//! Public scheduling facade always compiles (`schedule_from_emitter`). Count,
//! icon, and apply-state helpers only compile for tests or Windows desktop
//! (`tauri-runtime`).

// Dead-code free on server / non-Windows: count+icon+sync_state only compile
// for tests or Windows desktop.
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
mod count;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
mod icon;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
mod sync_state;
#[cfg(test)]
mod hooks_tests;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
pub use count::count_awaiting_reply;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
pub use icon::render_badge_icon;

use crate::web::event_bridge::EventEmitter;

// ---------------------------------------------------------------------------
// Test recorder (cfg(test) only — keep imports off production / clippy paths)
// ---------------------------------------------------------------------------

#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(test)]
static SCHEDULE_CALLS: AtomicU32 = AtomicU32::new(0);
// tokio mutex: hook tests hold the guard across `.await` (clippy::await_holding_lock
// rejects std::sync::Mutex guards across await points).
// Used by Task 3 hook tests; defined here so schedule bodies compile under cfg(test).
#[cfg(test)]
static HOOK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
pub(crate) async fn hook_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    HOOK_TEST_LOCK.lock().await
}

#[cfg(test)]
pub fn reset_schedule_calls() {
    SCHEDULE_CALLS.store(0, Ordering::SeqCst);
}

#[cfg(test)]
pub fn schedule_call_count() -> u32 {
    SCHEDULE_CALLS.load(Ordering::SeqCst)
}

#[cfg(test)]
fn record_schedule() {
    SCHEDULE_CALLS.fetch_add(1, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Windows desktop: AppHandle store, apply, schedule
// ---------------------------------------------------------------------------

#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
use std::sync::OnceLock;

#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
use sync_state::{BadgeApplyError, BadgeApplyState};

#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
static STATE: OnceLock<BadgeApplyState> = OnceLock::new();

/// Store process-level AppHandle for lifecycle paths without a live emitter.
/// Ignore if already set (idempotent).
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// Resolve the process-level AppHandle clone, if registered.
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
pub fn try_app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}

/// Lifecycle no-emitter secondary schedule (end-turn / orphan reconcile).
///
/// Records under `cfg(test)` even when no AppHandle is stored, so hook tests
/// can assert without a Tauri runtime. Production calls `schedule_from_app`
/// only when a handle was registered at setup.
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
pub fn notify_after_lifecycle_write_no_emitter() {
    #[cfg(test)]
    record_schedule();
    if let Some(app) = try_app_handle() {
        // schedule_from_app also records under cfg(test) — count may be 2.
        // Tests assert schedule_call_count() >= 1 after the helper returns.
        schedule_from_app(&app);
    }
}

/// Schedule a detached badge sync from a Tauri `AppHandle`.
///
/// Uses `tauri::async_runtime::spawn` (never bare `tokio::spawn`) so it is safe
/// from setup callbacks that have no current tokio runtime.
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
pub fn schedule_from_app(app: &tauri::AppHandle) {
    #[cfg(test)]
    record_schedule();

    use tauri::Manager;

    let Some(db) = app.try_state::<crate::db::AppDatabase>() else {
        tracing::warn!("[awaiting_reply_badge] AppDatabase not managed; skip schedule");
        return;
    };
    let conn = db.conn.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = sync_once(app, &conn).await;
        match result {
            Ok(()) | Err(BadgeApplyError::MissingMainWindow) => {}
            Err(e) => {
                tracing::warn!(error = ?e, "[awaiting_reply_badge] sync failed");
            }
        }
    });
}

#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
async fn sync_once(
    app: tauri::AppHandle,
    conn: &sea_orm::DatabaseConnection,
) -> Result<(), BadgeApplyError> {
    let state = STATE.get_or_init(BadgeApplyState::new);
    state
        .sync_with_count(
            || async {
                count_awaiting_reply(conn)
                    .await
                    .map_err(|e| BadgeApplyError::Count(e.to_string()))
            },
            |icon| {
                let app = app.clone();
                async move { apply_overlay_on_main(app, icon).await }
            },
            render_badge_icon,
        )
        .await
}

/// Apply (or clear) the overlay icon on the main window via main-thread hop.
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
async fn apply_overlay_on_main(
    app: tauri::AppHandle,
    icon: Option<(Vec<u8>, u32, u32)>,
) -> Result<(), BadgeApplyError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Clone for the main-thread closure; `run_on_main_thread` borrows `&self`.
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let result = (|| {
            use tauri::Manager;
            let Some(win) = app_for_main.get_webview_window("main") else {
                return Err(BadgeApplyError::MissingMainWindow);
            };
            let image = match icon {
                None => None,
                Some((rgba, w, h)) => Some(tauri::image::Image::new_owned(rgba, w, h)),
            };
            win.set_overlay_icon(image)
                .map_err(|e| BadgeApplyError::Setter(e.to_string()))
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| BadgeApplyError::Enqueue(e.to_string()))?;

    match rx.await {
        Ok(inner) => inner,
        Err(_) => Err(BadgeApplyError::ApplyChannelClosed),
    }
}

/// Always present (server-safe). On Windows+tauri, delegates Tauri emitters to
/// `schedule_from_app`; otherwise no-op. Under `cfg(test)`, always records.
pub fn schedule_from_emitter(emitter: &EventEmitter) {
    #[cfg(test)]
    record_schedule();

    #[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
    {
        if let EventEmitter::Tauri(app) = emitter {
            schedule_from_app(app);
            return;
        }
    }
    let _ = emitter;
}
