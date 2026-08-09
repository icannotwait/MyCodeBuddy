#pragma once

#include "codeg_eui_bridge.h"
#include "model.h"
#include "ui_snapshot.h"

#include <chrono>
#include <cstdint>
#include <functional>
#include <string>

// Enqueue function signatures match the C ABI; injectables for tests.
struct BridgeEnqueueApi {
  std::function<int(const std::uint8_t*, std::size_t, std::uint64_t*)>
      setWorkspace;
  std::function<int(const std::uint8_t*, std::size_t, std::uint64_t*)>
      createSession;
  std::function<int(std::int32_t, std::uint64_t*)> selectSession;
  std::function<int(const std::uint8_t*, std::size_t, std::uint64_t*)>
      sendUserMessage;
  std::function<int(std::uint64_t*)> cancelActiveTurn;
  std::function<int(const std::uint8_t*, std::size_t, std::uint64_t*)>
      getAgentSettings;
  std::function<int(const std::uint8_t*, std::size_t, const std::uint8_t*,
                    std::size_t, std::uint64_t*)>
      setAgentSettings;
  std::function<int(const std::uint8_t*, std::size_t, std::uint64_t*)>
      probeAgent;
  std::function<int(CodegEuiFrame*)> poll;
};

// Production bindings — only referenced from native app translation units.
inline BridgeEnqueueApi productionBridgeApi() {
  BridgeEnqueueApi api;
  api.poll = [](CodegEuiFrame* out) { return codeg_eui_poll(out); };
  api.setWorkspace = [](const std::uint8_t* p, std::size_t n, std::uint64_t* id) {
    return codeg_eui_set_workspace(p, n, id);
  };
  api.createSession = [](const std::uint8_t* p, std::size_t n, std::uint64_t* id) {
    return codeg_eui_create_session(p, n, id);
  };
  api.selectSession = [](std::int32_t conversation, std::uint64_t* id) {
    return codeg_eui_select_session(conversation, id);
  };
  api.sendUserMessage = [](const std::uint8_t* p, std::size_t n,
                           std::uint64_t* id) {
    return codeg_eui_send_user_message(p, n, id);
  };
  api.cancelActiveTurn = [](std::uint64_t* id) {
    return codeg_eui_cancel_active_turn(id);
  };
  api.getAgentSettings = [](const std::uint8_t* p, std::size_t n,
                            std::uint64_t* id) {
    return codeg_eui_get_agent_settings(p, n, id);
  };
  api.setAgentSettings = [](const std::uint8_t* a, std::size_t an,
                            const std::uint8_t* j, std::size_t jn,
                            std::uint64_t* id) {
    return codeg_eui_set_agent_settings(a, an, j, jn, id);
  };
  api.probeAgent = [](const std::uint8_t* p, std::size_t n, std::uint64_t* id) {
    return codeg_eui_probe_agent(p, n, id);
  };
  return api;
}

class BridgeClient {
 public:
  explicit BridgeClient(AppModel& model, BridgeEnqueueApi api = {})
      : model_(model), api_(std::move(api)) {}

  bool pollIfDue(std::chrono::steady_clock::time_point now) {
    if (!api_.poll) {
      return false;
    }
    if (lastPoll_.time_since_epoch().count() != 0 && !pollDue(lastPoll_, now)) {
      return false;
    }
    CodegEuiFrame frame{};
    const int status = api_.poll(&frame);
    lastPoll_ = now;
    if (status != CODEG_EUI_OK) {
      model_.errorStrip = "poll failed: " + std::to_string(status);
      return false;
    }
    dispatch(copy_frame(frame));
    return true;
  }

  void dispatch(const UiSnapshot& frame) {
    for (const UiCompletion& raw : frame.completions) {
      const Completion completion{
          raw.requestId,
          static_cast<Operation>(raw.op),
          static_cast<CompletionStatus>(raw.status),
          raw.resultPayload,
          raw.error,
      };
      model_.apply(completion);
    }
    model_.selectionEpoch = frame.selectionEpoch;
    model_.currentConnectionId = frame.connectionId;
    if (!frame.errorStrip.empty()) {
      model_.errorStrip = AppModel::redactSecrets(frame.errorStrip);
    }
    model_.snapshot = frame;
    model_.lastAppliedGeneration = frame.generation;
    model_.hasAppliedFrame = true;
  }

  bool trackEnqueue(Operation op, int abiStatus, std::uint64_t requestId) {
    if (abiStatus != CODEG_EUI_OK) {
      model_.errorStrip = "enqueue rejected: " + std::to_string(abiStatus);
      return false;
    }
    if (requestId == 0) {
      model_.errorStrip = "enqueue rejected: zero request id";
      return false;
    }
    if (model_.pending.count(requestId) != 0) {
      model_.errorStrip = "duplicate pending request id";
      return false;
    }
    model_.pending.emplace(requestId,
                           PendingRequest{op, model_.selectionEpoch});
    return true;
  }

  bool enqueueSetWorkspace(const std::string& path) {
    if (!api_.setWorkspace) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.setWorkspace(
        reinterpret_cast<const std::uint8_t*>(path.data()), path.size(), &id);
    return trackEnqueue(Operation::SetWorkspace, status, id);
  }

  bool enqueueCreateSession(const std::string& agent) {
    if (!api_.createSession) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.createSession(
        reinterpret_cast<const std::uint8_t*>(agent.data()), agent.size(),
        &id);
    return trackEnqueue(Operation::CreateSession, status, id);
  }

  bool enqueueSelectSession(std::int32_t conversationId) {
    if (!api_.selectSession) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.selectSession(conversationId, &id);
    return trackEnqueue(Operation::SelectSession, status, id);
  }

  bool enqueueSend(const std::string& text) {
    if (!api_.sendUserMessage) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.sendUserMessage(
        reinterpret_cast<const std::uint8_t*>(text.data()), text.size(), &id);
    return trackEnqueue(Operation::SendUserMessage, status, id);
  }

  bool enqueueCancel() {
    if (!api_.cancelActiveTurn) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.cancelActiveTurn(&id);
    return trackEnqueue(Operation::CancelActiveTurn, status, id);
  }

  bool enqueueGetSettings(const std::string& agent) {
    if (!api_.getAgentSettings) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.getAgentSettings(
        reinterpret_cast<const std::uint8_t*>(agent.data()), agent.size(),
        &id);
    return trackEnqueue(Operation::GetAgentSettings, status, id);
  }

  bool enqueueSetSettings(const std::string& agent, const std::string& json) {
    if (!api_.setAgentSettings) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.setAgentSettings(
        reinterpret_cast<const std::uint8_t*>(agent.data()), agent.size(),
        reinterpret_cast<const std::uint8_t*>(json.data()), json.size(), &id);
    return trackEnqueue(Operation::SetAgentSettings, status, id);
  }

  bool enqueueProbe(const std::string& agent) {
    if (!api_.probeAgent) {
      return false;
    }
    std::uint64_t id = 0;
    const int status = api_.probeAgent(
        reinterpret_cast<const std::uint8_t*>(agent.data()), agent.size(),
        &id);
    return trackEnqueue(Operation::ProbeAgent, status, id);
  }

 private:
  AppModel& model_;
  BridgeEnqueueApi api_;
  std::chrono::steady_clock::time_point lastPoll_{};
};
