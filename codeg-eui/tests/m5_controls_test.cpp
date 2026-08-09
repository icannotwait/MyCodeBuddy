#include "pages/m5.h"
#include "test_harness.h"

#include <string>

TEST(M5Controls, stale_cancel_does_not_change_new_selection) {
  M5ControlsState state = streamingSelection("conn-a", 10);
  state.beginCancel(71);
  state.select("conn-b", 11);
  state.apply(cancelledStale(71));
  EXPECT_EQ(state.connectionId, std::string("conn-b"));
  EXPECT_FALSE(state.cancelPending());
}

TEST(M5Controls, stop_square_visibility_and_tooltip) {
  M5ControlsState state = streamingSelection("conn-a", 1);
  EXPECT_TRUE(state.stopVisible());
  EXPECT_TRUE(state.stopEnabled());
  EXPECT_EQ(std::string(M5ControlsState::stopTooltip()),
            std::string("Cancel active turn"));
  EXPECT_EQ(M5ControlsState::kStopSizePx, 36);
  state.beginCancel(3);
  EXPECT_FALSE(state.stopEnabled());
  EXPECT_TRUE(state.stopVisible());
}

TEST(M5Controls, hard_error_retains_partial_and_allows_new_session) {
  M5ControlsState state = streamingSelection("conn-a", 2);
  state.liveAssistant = "partial answer";
  state.apply(Completion{9, Operation::SendUserMessage, CompletionStatus::Error,
                         "", "agent exited"});
  EXPECT_EQ(state.liveAssistant, std::string("partial answer"));
  EXPECT_FALSE(state.streamActive);
  EXPECT_EQ(state.errorStrip, std::string("agent exited"));
  EXPECT_TRUE(state.canCreateSessionAfterError());
  EXPECT_EQ(state.rowStatus, std::string("error"));
}

TEST(M5Controls, row_status_active_streaming_error) {
  M5ControlsState state;
  state.select("c1", 1);
  EXPECT_EQ(state.rowStatus, std::string("active"));
  state.streamActive = true;
  state.recomputeRowStatus();
  EXPECT_EQ(state.rowStatus, std::string("streaming"));
}
