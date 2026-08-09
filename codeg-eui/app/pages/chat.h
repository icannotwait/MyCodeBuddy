#pragma once

#include "model.h"

#include <chrono>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <vector>

enum class Generation { Delta, TurnEnd };

struct TranscriptLine {
  std::string role;  // user | assistant | tool
  std::string text;
  std::string toolName;
  std::string toolStatus;
};

struct ChatState {
  std::string composer;
  std::optional<std::uint64_t> sendRequestId;
  bool followBottom = true;
  float scrollOffset = 0;
  float contentHeight = 0;
  float viewportHeight = 0;
  std::string markdownSource;
  std::chrono::steady_clock::time_point lastMarkdownBuild{};
  bool hasMarkdownBuild = false;

  static constexpr double kMarkdownThrottleMs = 75.0;
  static constexpr float kBottomFollowPx = 48.0f;

  bool sendEnabled(const AppModel& model) const {
    if (composer.empty()) {
      return false;
    }
    if (sendRequestId.has_value()) {
      return false;
    }
    return model.canSend();
  }

  // Elapsed-since-last-build helper used by contracts tests.
  bool shouldRebuildMarkdown(std::chrono::milliseconds elapsed,
                             Generation generation) {
    if (generation == Generation::TurnEnd) {
      return true;
    }
    // First rebuild at 0 ms of a stream; subsequent deltas require 75 ms.
    if (!hasMarkdownBuild) {
      if (elapsed.count() >= 0) {
        hasMarkdownBuild = true;
        return true;
      }
      return false;
    }
    return elapsed.count() >= static_cast<std::int64_t>(kMarkdownThrottleMs);
  }

  bool shouldRebuildMarkdown(std::chrono::steady_clock::time_point now,
                             Generation generation) {
    if (!hasMarkdownBuild) {
      return true;
    }
    if (generation == Generation::TurnEnd) {
      return true;
    }
    const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
        now - lastMarkdownBuild);
    return elapsed.count() >= static_cast<std::int64_t>(kMarkdownThrottleMs);
  }

  void noteMarkdownBuilt(std::chrono::steady_clock::time_point now) {
    lastMarkdownBuild = now;
    hasMarkdownBuild = true;
  }

  void onSendAccepted(std::uint64_t requestId) {
    sendRequestId = requestId;
    composer.clear();
  }

  void onSendRejected() {}

  void onSendCompleted(const Completion& completion) {
    if (sendRequestId && *sendRequestId == completion.requestId) {
      sendRequestId.reset();
    }
  }

  bool nearBottom() const {
    if (contentHeight <= viewportHeight) {
      return true;
    }
    return (contentHeight - viewportHeight - scrollOffset) <= kBottomFollowPx;
  }

  void onScroll(float offset, float contentH, float viewportH) {
    scrollOffset = offset;
    contentHeight = contentH;
    viewportHeight = viewportH;
    followBottom = nearBottom();
  }

  static std::string projectToolLine(const std::string& name,
                                     const std::string& status) {
    return "tool: " + name + " - " + status;
  }

  static std::vector<TranscriptLine> projectTranscript(
      const std::string& transcriptJson,
      const std::string& liveAssistant) {
    std::vector<TranscriptLine> lines;
    if (!transcriptJson.empty()) {
      std::size_t start = 0;
      while (start < transcriptJson.size()) {
        std::size_t end = transcriptJson.find('\n', start);
        if (end == std::string::npos) {
          end = transcriptJson.size();
        }
        const std::string line = transcriptJson.substr(start, end - start);
        start = end + 1;
        if (line.rfind("user: ", 0) == 0) {
          lines.push_back({"user", line.substr(6), "", ""});
        } else if (line.rfind("assistant: ", 0) == 0) {
          lines.push_back({"assistant", line.substr(11), "", ""});
        } else if (line.rfind("tool: ", 0) == 0) {
          const std::string rest = line.substr(6);
          const auto bar = rest.find('|');
          if (bar != std::string::npos) {
            lines.push_back(
                {"tool", "", rest.substr(0, bar), rest.substr(bar + 1)});
          }
        }
      }
    }
    if (!liveAssistant.empty()) {
      lines.push_back({"assistant", liveAssistant, "", ""});
    }
    return lines;
  }
};

class ChatPage {
 public:
  ChatState& state() { return state_; }
  const ChatState& state() const { return state_; }

  bool trySend(AppModel& model,
               const std::function<bool(const std::string&)>& enqueueSend) {
    if (!state_.sendEnabled(model)) {
      return false;
    }
    const std::string text = state_.composer;
    if (enqueueSend(text)) {
      std::uint64_t id = 0;
      for (const auto& entry : model.pending) {
        if (entry.second.op == Operation::SendUserMessage) {
          id = entry.first;
        }
      }
      state_.onSendAccepted(id);
      return true;
    }
    state_.onSendRejected();
    return false;
  }

 private:
  ChatState state_;
};
