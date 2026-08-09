#pragma once

#include "ui_snapshot.h"

#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <functional>
#include <optional>
#include <string>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

enum class Operation : std::uint32_t {
  SetWorkspace = 1,
  CreateSession = 2,
  SelectSession = 3,
  SendUserMessage = 4,
  CancelActiveTurn = 5,
  GetAgentSettings = 6,
  SetAgentSettings = 7,
  ProbeAgent = 8,
};

enum class CompletionStatus : std::uint32_t {
  Ok = 0,
  Error = 1,
  Stale = 2,
  Cancelled = 3,
};

struct Completion {
  std::uint64_t requestId{};
  Operation op{};
  CompletionStatus status{};
  std::string resultPayload;
  std::string error;
};

struct PendingRequest {
  Operation op{};
  std::uint64_t selectionEpoch{};
};

enum class Route { Chat, Settings };

struct AppModel {
  UiSnapshot snapshot{};
  std::uint64_t selectionEpoch = 0;
  std::string currentConnectionId;
  std::string workspacePath;
  std::string selectedAgent = "codex";
  std::int32_t selectedConversationId = 0;
  std::unordered_map<std::uint64_t, PendingRequest> pending;
  std::string errorStrip;
  std::uint64_t lastAppliedGeneration = 0;
  bool hasAppliedFrame = false;
  Route route = Route::Chat;

  void apply(const Completion& completion) {
    const auto it = pending.find(completion.requestId);
    if (it == pending.end()) {
      return;
    }
    if (it->second.op != completion.op) {
      errorStrip = "completion operation mismatch";
      return;
    }
    pending.erase(it);

    if (completion.status == CompletionStatus::Stale ||
        completion.status == CompletionStatus::Cancelled) {
      return;
    }

    if (completion.status == CompletionStatus::Error) {
      if (!completion.error.empty()) {
        errorStrip = redactSecrets(completion.error);
      }
      return;
    }
  }

  void applyFrame(UiSnapshot frame) {
    for (const UiCompletion& raw : frame.completions) {
      apply(Completion{
          raw.requestId,
          static_cast<Operation>(raw.op),
          static_cast<CompletionStatus>(raw.status),
          raw.resultPayload,
          raw.error,
      });
    }
    selectionEpoch = frame.selectionEpoch;
    currentConnectionId = frame.connectionId;
    if (!frame.errorStrip.empty()) {
      errorStrip = redactSecrets(frame.errorStrip);
    }
    snapshot = std::move(frame);
    lastAppliedGeneration = snapshot.generation;
    hasAppliedFrame = true;
  }

  bool hasPending(Operation op) const {
    for (const auto& entry : pending) {
      if (entry.second.op == op) {
        return true;
      }
    }
    return false;
  }

  bool canCreateSession() const {
    return !workspacePath.empty() &&
           (selectedAgent == "codex" || selectedAgent == "grok");
  }

  bool canSend() const {
    return !workspacePath.empty() && selectedConversationId > 0 &&
           (selectedAgent == "codex" || selectedAgent == "grok") &&
           !hasPending(Operation::SendUserMessage) &&
           !snapshot.streamActive;
  }

  static std::string redactSecrets(std::string text) {
    const char* keys[] = {
        "OPENAI_API_KEY=", "API_KEY=", "api_key=", "Bearer ", "sk-",
    };
    for (const char* key : keys) {
      const std::size_t pos = text.find(key);
      if (pos != std::string::npos) {
        const std::size_t start = pos + std::char_traits<char>::length(key);
        std::size_t end = start;
        while (end < text.size() && text[end] != ' ' && text[end] != '\n' &&
               text[end] != '"' && text[end] != ',') {
          ++end;
        }
        if (end > start) {
          text.replace(start, end - start, "***");
        }
      }
    }
    return text;
  }
};

struct BridgeApi {
  virtual ~BridgeApi() = default;
  virtual int beginShutdown() = 0;
  virtual int poll(CodegEuiFrame* out) = 0;
  virtual int shutdown() = 0;
};

class ShutdownDriver {
 public:
  explicit ShutdownDriver(BridgeApi& api) : api_(api) {}

  bool drainAndShutdown(
      std::chrono::milliseconds deadline = std::chrono::milliseconds(5000),
      std::function<void(const UiSnapshot&)> onFrame = {}) {
    calls_.clear();
    const int begin = api_.beginShutdown();
    calls_.push_back("begin_shutdown");
    if (begin != CODEG_EUI_OK) {
      return false;
    }

    const auto start = std::chrono::steady_clock::now();
    for (;;) {
      if (std::chrono::steady_clock::now() - start > deadline) {
        return false;
      }
      CodegEuiFrame raw{};
      const int status = api_.poll(&raw);
      calls_.push_back("poll");
      if (status != CODEG_EUI_OK) {
        return false;
      }
      UiSnapshot frame = copy_frame(raw);
      for (const UiCompletion& c : frame.completions) {
        calls_.push_back("dispatch:" + std::to_string(c.requestId));
      }
      if (onFrame) {
        onFrame(frame);
      }
      if (frame.shutdownReady) {
        break;
      }
      std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }
    const int shut = api_.shutdown();
    calls_.push_back("shutdown");
    return shut == CODEG_EUI_OK;
  }

  const std::vector<std::string>& calls() const { return calls_; }

 private:
  BridgeApi& api_;
  std::vector<std::string> calls_;
};

struct SmokeFrameExit {
  std::optional<std::uint64_t> exitAfterFrames;
  std::uint64_t framesAfterShell = 0;
  bool shellComposed = false;
  bool closeRequested = false;

  static SmokeFrameExit parseEnv(const char* value) {
    SmokeFrameExit smoke;
    if (value == nullptr || value[0] == '\0') {
      return smoke;
    }
    char* end = nullptr;
    const unsigned long long parsed = std::strtoull(value, &end, 10);
    if (end == value || (end && *end != '\0') || parsed == 0) {
      return smoke;
    }
    smoke.exitAfterFrames = static_cast<std::uint64_t>(parsed);
    return smoke;
  }

  void noteShellComposed() { shellComposed = true; }

  bool onFrameCallback() {
    if (!exitAfterFrames || !shellComposed || closeRequested) {
      return false;
    }
    ++framesAfterShell;
    if (framesAfterShell >= *exitAfterFrames) {
      closeRequested = true;
      return true;
    }
    return false;
  }
};

inline bool pollDue(std::chrono::steady_clock::time_point last,
                    std::chrono::steady_clock::time_point now) {
  return now - last >= std::chrono::milliseconds(16);
}
