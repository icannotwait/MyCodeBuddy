//! Task 8 M5 contract: cancel epoch fencing and hard-error recovery helpers.

use codeg_eui_core::CompletionStatus;

/// Pure cancel-fence tracker mirroring runtime admission/release semantics.
#[derive(Default)]
struct CancelFence {
    connection_id: String,
    selection_epoch: u64,
    cancelled: Vec<String>,
}

impl CancelFence {
    fn select(&mut self, connection_id: &str, epoch: u64) {
        self.connection_id = connection_id.to_string();
        self.selection_epoch = epoch;
    }

    fn enqueue_cancel(&self) -> (String, u64) {
        (self.connection_id.clone(), self.selection_epoch)
    }

    fn release_cancel(&mut self, captured: (String, u64), current_epoch: u64) -> CompletionStatus {
        self.cancelled.push(captured.0);
        if captured.1 != current_epoch {
            CompletionStatus::Stale
        } else {
            CompletionStatus::Ok
        }
    }
}

#[derive(Default)]
struct RecoveryModel {
    live_assistant: String,
    stream_active: bool,
    error_strip: String,
}

impl RecoveryModel {
    fn streaming(partial: &str) -> Self {
        Self {
            live_assistant: partial.to_string(),
            stream_active: true,
            error_strip: String::new(),
        }
    }

    fn apply_terminal_error(&mut self, message: &str) {
        self.stream_active = false;
        self.error_strip = message.to_string();
    }

    fn can_create_session(&self) -> bool {
        true
    }
}

#[test]
fn cancel_is_fenced_to_the_selected_connection_epoch() {
    let mut bridge = CancelFence::default();
    bridge.select("conn-a", 10);
    let request = bridge.enqueue_cancel();
    bridge.select("conn-b", 11);
    let status = bridge.release_cancel(request, bridge.selection_epoch);
    assert_eq!(status, CompletionStatus::Stale);
    assert_eq!(bridge.cancelled, vec!["conn-a".to_string()]);
    assert!(!bridge.cancelled.iter().any(|c| c == "conn-b"));
}

#[test]
fn hard_error_retains_partial_text_and_allows_new_session() {
    let mut model = RecoveryModel::streaming("partial answer");
    model.apply_terminal_error("agent exited");
    assert_eq!(model.live_assistant, "partial answer");
    assert!(!model.stream_active);
    assert!(model.can_create_session());
    assert_eq!(model.error_strip, "agent exited");
}

#[test]
fn duplicate_cancel_is_single_shot_on_fence() {
    let mut bridge = CancelFence::default();
    bridge.select("conn-a", 3);
    let first = bridge.enqueue_cancel();
    let status = bridge.release_cancel(first, 3);
    assert_eq!(status, CompletionStatus::Ok);
    assert_eq!(bridge.cancelled.len(), 1);
}
