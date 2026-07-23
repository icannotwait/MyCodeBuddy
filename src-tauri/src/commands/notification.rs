//! Desktop system notifications (Tauri only).
//!
//! Optional `action_id` + `conversation_id` register a short-lived navigation
//! target. When the OS notification click callback fires (platforms that
//! support it), the host looks up the target and emits a frontend event:
//!
//! - event: `notification-navigate`
//! - payload: `{ kind: "conversation", conversationId: number }`
//!
//! Platforms without click actions still show the notification; the in-session
//! banner remains authoritative. Entries expire after 15 minutes or on fire.

#[cfg(feature = "tauri-runtime")]
use std::collections::HashMap;
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
) -> Result<(), AppCommandError> {
    // Register navigation target before showing so a fast click cannot race
    // past an empty map. Platforms without click actions still keep the entry
    // until fire/TTL; the in-session banner remains authoritative.
    if let (Some(ref aid), Some(cid)) = (action_id.as_ref(), conversation_id) {
        register_action(aid, cid);
    }

    #[cfg(target_os = "macos")]
    {
        let app_id = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            "app.mycodebuddy"
        };
        let _ = mac_notification_sys::set_application(app_id);

        // mac-notification-sys does not expose a reliable click callback on all
        // versions; show without navigation payload. Banner remains authoritative.
        let _ = mac_notification_sys::Notification::default()
            .title(&title)
            .message(&body)
            .send();
        let _ = app;
    }

    #[cfg(not(target_os = "macos"))]
    {
        use tauri_plugin_notification::NotificationExt;

        let _ = app.notification().builder().title(title).body(body).show();
        // Action remains registered for fire_notification_navigate / TTL.
        // OS-level click wiring is platform-specific; banner stays authoritative.
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
}
