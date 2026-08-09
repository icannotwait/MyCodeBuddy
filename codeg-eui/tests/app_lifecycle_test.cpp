#include "model.h"
#include "test_harness.h"
#include "ui_snapshot.h"

#include <chrono>
#include <cstring>
#include <string>
#include <vector>

namespace {

struct FakeBridgeApi final : BridgeApi {
  std::vector<CodegEuiFrame> frames;
  std::size_t index = 0;
  std::vector<std::string> calls;
  // Own completion storage so slice pointers stay valid.
  std::vector<CodegEuiCompletion> ownedCompletions;
  std::vector<std::string> ownedPayloads;

  int beginShutdown() override {
    calls.push_back("begin_shutdown");
    return CODEG_EUI_OK;
  }

  int poll(CodegEuiFrame* out) override {
    calls.push_back("poll");
    if (index >= frames.size()) {
      return CODEG_EUI_ERR_INVALID_STATE;
    }
    *out = frames[index++];
    return CODEG_EUI_OK;
  }

  int shutdown() override {
    calls.push_back("shutdown");
    return CODEG_EUI_OK;
  }
};

CodegEuiFrame stoppingFrame(const std::vector<std::uint64_t>& requestIds,
                            bool ready) {
  CodegEuiFrame frame{};
  frame.lifecycle_state = CODEG_EUI_LIFECYCLE_STOPPING;
  frame.shutdown_ready = ready ? 1 : 0;
  return frame;
}

// Helper that mutates the fake API to attach a completion on the next frame.
void attachCancelled(FakeBridgeApi& api,
                     CodegEuiFrame& frame,
                     std::uint64_t requestId) {
  api.ownedPayloads.emplace_back();
  CodegEuiCompletion c{};
  c.request_id = requestId;
  c.op = CODEG_EUI_OP_CANCEL_ACTIVE_TURN;
  c.status = CODEG_EUI_COMPLETION_CANCELLED;
  c.result_payload = {nullptr, 0};
  c.error = {nullptr, 0};
  api.ownedCompletions.push_back(c);
  frame.completions = api.ownedCompletions.data() +
                      (api.ownedCompletions.size() - 1);
  frame.completions_len = 1;
}

}  // namespace

TEST(ShutdownDriver, dispatches_stopping_completions_before_final_free) {
  FakeBridgeApi api;
  CodegEuiFrame first = stoppingFrame({}, false);
  attachCancelled(api, first, 41);
  // re-point after potential realloc — store single completion stably
  static CodegEuiCompletion completion{};
  completion.request_id = 41;
  completion.op = CODEG_EUI_OP_CANCEL_ACTIVE_TURN;
  completion.status = CODEG_EUI_COMPLETION_CANCELLED;
  first.completions = &completion;
  first.completions_len = 1;
  CodegEuiFrame second = stoppingFrame({}, true);
  api.frames = {first, second};

  // Use a driver that records through the same call list as FakeBridgeApi.
  class RecordingDriver {
   public:
    explicit RecordingDriver(FakeBridgeApi& api) : api_(api) {}
    void drainAndShutdown() {
      (void)api_.beginShutdown();
      for (;;) {
        CodegEuiFrame raw{};
        (void)api_.poll(&raw);
        UiSnapshot frame = copy_frame(raw);
        for (const auto& c : frame.completions) {
          api_.calls.push_back("dispatch:" + std::to_string(c.requestId));
        }
        if (frame.shutdownReady) {
          break;
        }
      }
      (void)api_.shutdown();
    }

   private:
    FakeBridgeApi& api_;
  };

  RecordingDriver driver(api);
  driver.drainAndShutdown();
  EXPECT_EQ(api.calls.size(), static_cast<std::size_t>(5));
  EXPECT_EQ(api.calls[0], std::string("begin_shutdown"));
  EXPECT_EQ(api.calls[1], std::string("poll"));
  EXPECT_EQ(api.calls[2], std::string("dispatch:41"));
  EXPECT_EQ(api.calls[3], std::string("poll"));
  EXPECT_EQ(api.calls[4], std::string("shutdown"));
}

TEST(ShutdownDriver, production_driver_matches_order) {
  FakeBridgeApi api;
  static CodegEuiCompletion completion{};
  completion.request_id = 41;
  completion.op = CODEG_EUI_OP_CANCEL_ACTIVE_TURN;
  completion.status = CODEG_EUI_COMPLETION_CANCELLED;
  CodegEuiFrame first = stoppingFrame({}, false);
  first.completions = &completion;
  first.completions_len = 1;
  api.frames = {first, stoppingFrame({}, true)};

  ShutdownDriver driver(api);
  EXPECT_TRUE(driver.drainAndShutdown(std::chrono::milliseconds(1000)));
  const auto& calls = driver.calls();
  EXPECT_EQ(calls.size(), static_cast<std::size_t>(5));
  EXPECT_EQ(calls[0], std::string("begin_shutdown"));
  EXPECT_EQ(calls[2], std::string("dispatch:41"));
  EXPECT_EQ(calls[4], std::string("shutdown"));
}

TEST(SmokeFrameExit, invalid_values_disable_hook) {
  EXPECT_FALSE(SmokeFrameExit::parseEnv(nullptr).exitAfterFrames.has_value());
  EXPECT_FALSE(SmokeFrameExit::parseEnv("").exitAfterFrames.has_value());
  EXPECT_FALSE(SmokeFrameExit::parseEnv("0").exitAfterFrames.has_value());
  EXPECT_FALSE(SmokeFrameExit::parseEnv("abc").exitAfterFrames.has_value());
  EXPECT_FALSE(SmokeFrameExit::parseEnv("1x").exitAfterFrames.has_value());
}

TEST(SmokeFrameExit, counts_only_after_shell_and_closes_on_n) {
  auto smoke = SmokeFrameExit::parseEnv("3");
  EXPECT_TRUE(smoke.exitAfterFrames.has_value());
  EXPECT_EQ(*smoke.exitAfterFrames, static_cast<std::uint64_t>(3));
  EXPECT_FALSE(smoke.onFrameCallback());
  smoke.noteShellComposed();
  EXPECT_FALSE(smoke.onFrameCallback());
  EXPECT_FALSE(smoke.onFrameCallback());
  EXPECT_TRUE(smoke.onFrameCallback());
  EXPECT_TRUE(smoke.closeRequested);
  EXPECT_FALSE(smoke.onFrameCallback());
}

TEST(PollCadence, requires_sixteen_ms) {
  const auto t0 = std::chrono::steady_clock::time_point{};
  EXPECT_FALSE(pollDue(t0, t0 + std::chrono::milliseconds(15)));
  EXPECT_TRUE(pollDue(t0, t0 + std::chrono::milliseconds(16)));
}
