#pragma once

#include "model.h"

#include <optional>
#include <string>

// M5 cancel/session/error control state — pure, contracts-testable.
struct M5ControlsState {
  std::string connectionId;
  std::uint64_t selectionEpoch = 0;
  std::optional<std::uint64_t> cancelRequestId;
  bool streamActive = false;
  std::string liveAssistant;
  std::string errorStrip;
  std::string rowStatus;  // active | streaming | error | idle

  void beginCancel(std::uint64_t requestId) {
    cancelRequestId = requestId;
  }

  void select(const std::string& connectionId, std::uint64_t epoch) {
    this->connectionId = connectionId;
    selectionEpoch = epoch;
    // Switching sessions clears cancel pending for the new selection view.
    // Stale cancel completions still clear cancelPending without mutating id.
    streamActive = false;
    errorStrip.clear();
    recomputeRowStatus();
  }

  void apply(const Completion& completion) {
    if (completion.op != Operation::CancelActiveTurn) {
      if (completion.status == CompletionStatus::Error &&
          !completion.error.empty()) {
        // Terminal agent error retains partial assistant text.
        streamActive = false;
        errorStrip = AppModel::redactSecrets(completion.error);
        recomputeRowStatus();
      }
      return;
    }
    if (cancelRequestId && *cancelRequestId == completion.requestId) {
      cancelRequestId.reset();
    }
    // Stale cancel must not change the current selection.
    if (completion.status == CompletionStatus::Stale) {
      recomputeRowStatus();
      return;
    }
    if (completion.status == CompletionStatus::Ok ||
        completion.status == CompletionStatus::Cancelled) {
      streamActive = false;
    }
    recomputeRowStatus();
  }

  bool cancelPending() const { return cancelRequestId.has_value(); }

  bool stopVisible() const { return streamActive; }

  bool stopEnabled() const { return streamActive && !cancelPending(); }

  static const char* stopTooltip() { return "Cancel active turn"; }

  static constexpr int kStopSizePx = 36;

  bool canCreateSessionAfterError() const {
    return !errorStrip.empty() || !streamActive;
  }

  void recomputeRowStatus() {
    if (streamActive) {
      rowStatus = "streaming";
    } else if (!errorStrip.empty()) {
      rowStatus = "error";
    } else if (!connectionId.empty()) {
      rowStatus = "active";
    } else {
      rowStatus = "idle";
    }
  }
};

inline M5ControlsState streamingSelection(const std::string& connectionId,
                                          std::uint64_t epoch) {
  M5ControlsState state;
  state.connectionId = connectionId;
  state.selectionEpoch = epoch;
  state.streamActive = true;
  state.liveAssistant = "partial";
  state.recomputeRowStatus();
  return state;
}

inline Completion cancelledStale(std::uint64_t requestId) {
  return Completion{requestId, Operation::CancelActiveTurn,
                    CompletionStatus::Stale, "", ""};
}
