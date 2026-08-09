#include "bridge/client.h"
#include "model.h"
#include "test_harness.h"
#include "ui_snapshot.h"

#include <chrono>
#include <cstdint>
#include <string>

TEST(BridgeClient, stale_completion_finishes_request_without_mutating_selection) {
  AppModel model;
  model.selectionEpoch = 4;
  model.pending.emplace(9, PendingRequest{Operation::SelectSession, 3});
  model.apply(Completion{9, Operation::SelectSession, CompletionStatus::Stale,
                         "{}", ""});
  EXPECT_TRUE(model.pending.empty());
  EXPECT_EQ(model.selectionEpoch, static_cast<std::uint64_t>(4));
  EXPECT_TRUE(model.currentConnectionId.empty());
}

TEST(BridgeClient, operation_mismatch_rejects_without_finishing) {
  AppModel model;
  model.pending.emplace(3, PendingRequest{Operation::SendUserMessage, 1});
  model.apply(Completion{3, Operation::SelectSession, CompletionStatus::Ok, "{}",
                         ""});
  EXPECT_EQ(model.pending.size(), static_cast<std::size_t>(1));
  EXPECT_EQ(model.errorStrip, std::string("completion operation mismatch"));
}

TEST(BridgeClient, unknown_completion_is_ignored) {
  AppModel model;
  model.pending.emplace(1, PendingRequest{Operation::ProbeAgent, 0});
  model.apply(Completion{99, Operation::ProbeAgent, CompletionStatus::Ok, "{}",
                         ""});
  EXPECT_EQ(model.pending.size(), static_cast<std::size_t>(1));
}

TEST(BridgeClient, enqueue_rejection_does_not_insert_pending) {
  AppModel model;
  BridgeEnqueueApi api;
  api.setWorkspace = [](const std::uint8_t*, std::size_t, std::uint64_t* id) {
    *id = 0;
    return CODEG_EUI_ERR_INVALID_STATE;
  };
  BridgeClient client(model, api);
  EXPECT_FALSE(client.enqueueSetWorkspace("/tmp/ws"));
  EXPECT_TRUE(model.pending.empty());
  EXPECT_FALSE(model.errorStrip.empty());
}

TEST(BridgeClient, accepted_enqueue_tracks_pending) {
  AppModel model;
  model.selectionEpoch = 7;
  BridgeEnqueueApi api;
  api.sendUserMessage = [](const std::uint8_t*, std::size_t, std::uint64_t* id) {
    *id = 42;
    return CODEG_EUI_OK;
  };
  BridgeClient client(model, api);
  EXPECT_TRUE(client.enqueueSend("hello"));
  ASSERT_EQ(model.pending.size(), static_cast<std::size_t>(1));
  EXPECT_EQ(model.pending.at(42).op, Operation::SendUserMessage);
  EXPECT_EQ(model.pending.at(42).selectionEpoch, static_cast<std::uint64_t>(7));
}

TEST(BridgeClient, dispatch_applies_completions_and_snapshot) {
  AppModel model;
  model.pending.emplace(5, PendingRequest{Operation::CreateSession, 1});
  BridgeClient client(model, {});
  UiSnapshot frame;
  frame.generation = 11;
  frame.selectionEpoch = 2;
  frame.connectionId = "conn-x";
  frame.errorStrip = "OPENAI_API_KEY=test-secret boom";
  frame.completions.push_back(
      {5, static_cast<std::uint32_t>(Operation::CreateSession),
       static_cast<std::uint32_t>(CompletionStatus::Ok), "{\"ok\":true}", ""});
  client.dispatch(frame);
  EXPECT_TRUE(model.pending.empty());
  EXPECT_EQ(model.selectionEpoch, static_cast<std::uint64_t>(2));
  EXPECT_EQ(model.currentConnectionId, std::string("conn-x"));
  EXPECT_EQ(model.snapshot.generation, static_cast<std::uint64_t>(11));
  EXPECT_TRUE(model.errorStrip.find("test-secret") == std::string::npos);
  EXPECT_TRUE(model.errorStrip.find("***") != std::string::npos);
}

TEST(BridgeClient, poll_respects_sixteen_ms_cadence) {
  AppModel model;
  int polls = 0;
  BridgeEnqueueApi api;
  api.poll = [&](CodegEuiFrame* out) {
    *out = CodegEuiFrame{};
    ++polls;
    return CODEG_EUI_OK;
  };
  BridgeClient client(model, api);
  const auto t0 = std::chrono::steady_clock::now();
  EXPECT_TRUE(client.pollIfDue(t0));
  EXPECT_FALSE(client.pollIfDue(t0 + std::chrono::milliseconds(15)));
  EXPECT_TRUE(client.pollIfDue(t0 + std::chrono::milliseconds(16)));
  EXPECT_EQ(polls, 2);
}
