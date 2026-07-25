//! Desktop system notifications (Tauri only).
//!
//! Optional `action_id` + `conversation_id` register a short-lived navigation
//! target **only on platforms that can fire a click callback**. When the OS
//! notification click callback fires, the host looks up the target and emits a
//! frontend event:
//!
//! - event: `notification-navigate`
//! - payload: `{ kind: "conversation", conversationId: number }`
//!
//! Platforms without click actions (currently Windows via notify-rust /
//! tauri-plugin-notification) omit registration cleanly; the in-session banner
//! remains authoritative. Entries expire after 15 minutes or on fire.
//!
//! Optional `dedupe_key` is a host-side once-per-key gate so multi-window
//! renderers cannot each surface the same watchdog notice.

#[cfg(feature = "tauri-runtime")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "tauri-runtime")]
use std::sync::{LazyLock, Mutex};
#[cfg(feature = "tauri-runtime")]
use std::time::{Duration, Instant};

#[cfg(feature = "tauri-runtime")]
use serde::Serialize;
#[cfg(feature = "tauri-runtime")]
use tauri::{AppHandle, Emitter};

use crate::app_error::AppCommandError;

/// Frontend event name for notification click navigation.
pub const NOTIFICATION_NAVIGATE_EVENT: &str = "notification-navigate";

/// TTL for pending notification action targets (15 minutes).
#[cfg(feature = "tauri-runtime")]
const ACTION_TTL: Duration = Duration::from_secs(15 * 60);

/// Cap host dedupe set growth (many leases / versions over a long session).
#[cfg(feature = "tauri-runtime")]
const DEDUPE_CAP: usize = 512;

/// Whether this desktop build can wire OS notification clicks to
/// [`fire_notification_navigate`]. Windows notify-rust has no click callback.
#[cfg(feature = "tauri-runtime")]
pub const NOTIFICATION_CLICK_NAVIGATION_SUPPORTED: bool = cfg!(any(
    target_os = "macos",
    all(unix, not(target_os = "macos"))
));

#[cfg(feature = "tauri-runtime")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationNavigatePayload {
    pub kind: &'static str,
    pub conversation_id: i64,
}

#[cfg(feature = "tauri-runtime")]
struct PendingAction {
    conversation_id: i64,
    registered_at: Instant,
}

#[cfg(feature = "tauri-runtime")]
static PENDING_ACTIONS: LazyLock<Mutex<HashMap<String, PendingAction>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(feature = "tauri-runtime")]
static NOTIFICATION_DEDUPE: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Register (or refresh) an opaque `action_id` → conversation target.
#[cfg(feature = "tauri-runtime")]
fn register_action(action_id: &str, conversation_id: i64) {
    if action_id.is_empty() {
        return;
    }
    let mut map = PENDING_ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    prune_expired_locked(&mut map);
    map.insert(
        action_id.to_string(),
        PendingAction {
            conversation_id,
            registered_at: Instant::now(),
        },
    );
}

#[cfg(feature = "tauri-runtime")]
fn prune_expired_locked(map: &mut HashMap<String, PendingAction>) {
    map.retain(|_, v| v.registered_at.elapsed() < ACTION_TTL);
}

/// Look up and remove an action target. Returns the conversation id if found
/// and not expired.
#[cfg(feature = "tauri-runtime")]
pub fn take_action_target(action_id: &str) -> Option<i64> {
    let mut map = PENDING_ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    prune_expired_locked(&mut map);
    map.remove(action_id).map(|p| p.conversation_id)
}

/// Host once-per-key gate. Returns `true` if this is the first claim.
#[cfg(feature = "tauri-runtime")]
fn claim_notification_dedupe(key: &str) -> bool {
    if key.is_empty() {
        return true;
    }
    let mut set = NOTIFICATION_DEDUPE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if set.contains(key) {
        return false;
    }
    if set.len() >= DEDUPE_CAP {
        // Drop an arbitrary entry to bound memory (order is not load-bearing).
        if let Some(first) = set.iter().next().cloned() {
            set.remove(&first);
        }
    }
    set.insert(key.to_string());
    true
}

/// Register navigation target only when the OS path can fire a click.
/// Returns the action id to wire into the platform callback, if any.
#[cfg(feature = "tauri-runtime")]
fn maybe_register_click_target(
    action_id: Option<&str>,
    conversation_id: Option<i64>,
) -> Option<String> {
    if !NOTIFICATION_CLICK_NAVIGATION_SUPPORTED {
        // Unsupported platforms: omit the target cleanly (banner authoritative).
        return None;
    }
    let (Some(aid), Some(cid)) = (action_id, conversation_id) else {
        return None;
    };
    if aid.is_empty() {
        return None;
    }
    register_action(aid, cid);
    Some(aid.to_string())
}

/// Emit `notification-navigate` for a previously registered action (or no-op).
#[cfg(feature = "tauri-runtime")]
pub fn fire_notification_navigate(app: &AppHandle, action_id: &str) {
    let Some(conversation_id) = take_action_target(action_id) else {
        return;
    };
    let payload = NotificationNavigatePayload {
        kind: "conversation",
        conversation_id,
    };
    let _ = app.emit(NOTIFICATION_NAVIGATE_EVENT, payload);
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn send_notification(
    #[allow(unused_variables)] app: AppHandle,
    title: String,
    body: String,
    action_id: Option<String>,
    conversation_id: Option<i64>,
    dedupe_key: Option<String>,
) -> Result<(), AppCommandError> {
    if let Some(ref key) = dedupe_key {
        if !claim_notification_dedupe(key) {
            return Ok(());
        }
    }

    // Register before showing so a fast click cannot race past an empty map.
    // Unsupported platforms never register (omit target cleanly).
    let click_action = maybe_register_click_target(action_id.as_deref(), conversation_id);

    #[cfg(target_os = "macos")]
    {
        let app_id = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            "app.mycodebuddy"
        };
        let _ = mac_notification_sys::set_application(app_id);

        if let Some(aid) = click_action {
            // wait_for_click blocks until interaction/timeout — run off the
            // async runtime so the invoke returns immediately.
            let app_for_click = app.clone();
            std::thread::spawn(move || {
                let response = mac_notification_sys::Notification::default()
                    .title(&title)
                    .message(&body)
                    .wait_for_click(true)
                    .send();
                match response {
                    Ok(mac_notification_sys::NotificationResponse::Click)
                    | Ok(mac_notification_sys::NotificationResponse::ActionButton(_)) => {
                        fire_notification_navigate(&app_for_click, &aid);
                    }
                    _ => {}
                }
            });
        } else {
            let _ = mac_notification_sys::Notification::default()
                .title(&title)
                .message(&body)
                .send();
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(aid) = click_action {
            let app_for_click = app.clone();
            std::thread::spawn(move || {
                // XDG: notify-rust exposes wait_for_action; the Tauri plugin
                // path does not, so use notify-rust directly for click wiring.
                match notify_rust::Notification::new()
                    .summary(&title)
                    .body(&body)
                    .action("default", "Open")
                    .show()
                {
                    Ok(handle) => {
                        handle.wait_for_action(|action| {
                            if action != "__closed" {
                                fire_notification_navigate(&app_for_click, &aid);
                            }
                        });
                    }
                    Err(_) => {
                        // Fall back to plugin show without navigation.
                        use tauri_plugin_notification::NotificationExt;
                        let _ = app_for_click
                            .notification()
                            .builder()
                            .title(title)
                            .body(body)
                            .show();
                    }
                }
            });
        } else {
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title(title).body(body).show();
        }
    }

    #[cfg(target_os = "windows")]
    {
        // notify-rust / tauri-plugin-notification have no click callback on
        // Windows. Do not register a target; banner remains authoritative.
        let _ = click_action;
        use tauri_plugin_notification::NotificationExt;
        let _ = app.notification().builder().title(title).body(body).show();
    }

    Ok(())
}

#[cfg(all(feature = "tauri-runtime", test))]
mod tests {
    use super::*;

    #[test]
    fn registers_and_takes_action_once() {
        let id = format!("test-action-{}", std::process::id());
        register_action(&id, 42);
        assert_eq!(take_action_target(&id), Some(42));
        assert_eq!(take_action_target(&id), None);
    }

    #[test]
    fn empty_action_id_is_ignored() {
        register_action("", 7);
        assert_eq!(take_action_target(""), None);
    }

    #[test]
    fn prune_does_not_drop_fresh_entries() {
        let id = format!("fresh-{}", std::process::id());
        register_action(&id, 99);
        let mut map = PENDING_ACTIONS.lock().unwrap();
        prune_expired_locked(&mut map);
        assert!(map.contains_key(&id));
        drop(map);
        assert_eq!(take_action_target(&id), Some(99));
    }

    #[test]
    fn command_payload_registers_when_click_supported() {
        // Mirrors the register branch of send_notification for platforms that
        // can fire fire_notification_navigate.
        let id = format!("payload-{}", std::process::id());
        let registered = maybe_register_click_target(Some(&id), Some(55));
        if NOTIFICATION_CLICK_NAVIGATION_SUPPORTED {
            assert_eq!(registered.as_deref(), Some(id.as_str()));
            assert_eq!(take_action_target(&id), Some(55));
        } else {
            // Windows: omit target cleanly — no registration.
            assert_eq!(registered, None);
            assert_eq!(take_action_target(&id), None);
        }
    }

    #[test]
    fn unsupported_or_partial_payload_omits_registration() {
        let id = format!("partial-{}", std::process::id());
        assert_eq!(maybe_register_click_target(Some(&id), None), None);
        assert_eq!(maybe_register_click_target(None, Some(1)), None);
        assert_eq!(maybe_register_click_target(Some(""), Some(1)), None);
        assert_eq!(take_action_target(&id), None);
    }

    #[test]
    fn fire_notification_navigate_consumes_registered_action() {
        let id = format!("fire-{}", std::process::id());
        register_action(&id, 88);
        // Without an AppHandle we still verify the lookup half of the callback
        // path: fire_notification_navigate starts with take_action_target.
        assert_eq!(take_action_target(&id), Some(88));
        assert_eq!(take_action_target(&id), None);
    }

    #[test]
    fn host_dedupe_claims_once_per_key() {
        let key = format!("lease-dedupe-{}:1", std::process::id());
        assert!(claim_notification_dedupe(&key));
        assert!(!claim_notification_dedupe(&key));
        // Empty key does not gate.
        assert!(claim_notification_dedupe(""));
        assert!(claim_notification_dedupe(""));
    }
}
