//! Windows taskbar awaiting-reply badge.
//!
//! Public scheduling facade always compiles (`schedule_from_emitter`). Count
//! and icon helpers only compile for tests or Windows desktop (`tauri-runtime`).

// Dead-code free on server / non-Windows: count+icon only compile for tests or Windows desktop.
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
mod count;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
mod icon;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
pub use count::count_awaiting_reply;
#[cfg(any(test, all(feature = "tauri-runtime", target_os = "windows")))]
pub use icon::render_badge_icon;

use crate::web::event_bridge::EventEmitter;

/// Always present (server-safe no-op until Task 2 fills Windows body).
pub fn schedule_from_emitter(_emitter: &EventEmitter) {}
