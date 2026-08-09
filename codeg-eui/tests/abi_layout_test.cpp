#include "codeg_eui_bridge.h"
#include "test_harness.h"

#include <cstddef>

static_assert(sizeof(CodegEuiLifecycleState) == 4,
              "CodegEuiLifecycleState ABI size drift");
static_assert(sizeof(CodegEuiOperation) == 4,
              "CodegEuiOperation ABI size drift");
static_assert(sizeof(CodegEuiCompletionStatus) == 4,
              "CodegEuiCompletionStatus ABI size drift");
static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI size drift");
static_assert(alignof(CodegEuiSlice) == 8,
              "CodegEuiSlice ABI alignment drift");
static_assert(offsetof(CodegEuiSlice, len) == 8,
              "CodegEuiSlice len offset drift");
static_assert(sizeof(CodegEuiSessionSummary) == 48,
              "CodegEuiSessionSummary ABI size drift");
static_assert(alignof(CodegEuiSessionSummary) == 8,
              "CodegEuiSessionSummary ABI alignment drift");
static_assert(offsetof(CodegEuiSessionSummary, conversation_id) == 0,
              "CodegEuiSessionSummary id offset drift");
static_assert(offsetof(CodegEuiSessionSummary, reserved) == 4,
              "CodegEuiSessionSummary reserved offset drift");
static_assert(offsetof(CodegEuiSessionSummary, title) == 8,
              "CodegEuiSessionSummary title offset drift");
static_assert(offsetof(CodegEuiSessionSummary, agent) == 24,
              "CodegEuiSessionSummary agent offset drift");
static_assert(offsetof(CodegEuiSessionSummary, updated_at_ms) == 40,
              "CodegEuiSessionSummary updated_at offset drift");
static_assert(sizeof(CodegEuiCompletion) == 48,
              "CodegEuiCompletion ABI size drift");
static_assert(alignof(CodegEuiCompletion) == 8,
              "CodegEuiCompletion ABI alignment drift");
static_assert(offsetof(CodegEuiCompletion, request_id) == 0,
              "CodegEuiCompletion request offset drift");
static_assert(offsetof(CodegEuiCompletion, op) == 8,
              "CodegEuiCompletion op offset drift");
static_assert(offsetof(CodegEuiCompletion, status) == 12,
              "CodegEuiCompletion status offset drift");
static_assert(offsetof(CodegEuiCompletion, result_payload) == 16,
              "CodegEuiCompletion result offset drift");
static_assert(offsetof(CodegEuiCompletion, error) == 32,
              "CodegEuiCompletion error offset drift");
static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI size drift");
static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame ABI alignment drift");
static_assert(offsetof(CodegEuiFrame, api_version) == 0,
              "CodegEuiFrame version offset drift");
static_assert(offsetof(CodegEuiFrame, lifecycle_state) == 4,
              "CodegEuiFrame lifecycle offset drift");
static_assert(offsetof(CodegEuiFrame, generation) == 8,
              "CodegEuiFrame generation offset drift");
static_assert(offsetof(CodegEuiFrame, selection_epoch) == 16,
              "CodegEuiFrame selection_epoch offset drift");
static_assert(offsetof(CodegEuiFrame, sessions) == 24,
              "CodegEuiFrame sessions offset drift");
static_assert(offsetof(CodegEuiFrame, sessions_len) == 32,
              "CodegEuiFrame sessions length offset drift");
static_assert(offsetof(CodegEuiFrame, connection_id) == 40,
              "CodegEuiFrame connection_id offset drift");
static_assert(offsetof(CodegEuiFrame, event_seq) == 56,
              "CodegEuiFrame event sequence offset drift");
static_assert(offsetof(CodegEuiFrame, transcript_json) == 64,
              "CodegEuiFrame transcript_json offset drift");
static_assert(offsetof(CodegEuiFrame, live_assistant) == 80,
              "CodegEuiFrame live_assistant offset drift");
static_assert(offsetof(CodegEuiFrame, stream_active) == 96,
              "CodegEuiFrame stream flag offset drift");
static_assert(offsetof(CodegEuiFrame, needs_resync) == 97,
              "CodegEuiFrame resync flag offset drift");
static_assert(offsetof(CodegEuiFrame, shutdown_ready) == 98,
              "CodegEuiFrame shutdown_ready offset drift");
static_assert(offsetof(CodegEuiFrame, reserved) == 99,
              "CodegEuiFrame reserved offset drift");
static_assert(offsetof(CodegEuiFrame, error_strip) == 104,
              "CodegEuiFrame error_strip offset drift");
static_assert(offsetof(CodegEuiFrame, completions) == 120,
              "CodegEuiFrame completions offset drift");
static_assert(offsetof(CodegEuiFrame, completions_len) == 128,
              "CodegEuiFrame completions length offset drift");
static_assert(offsetof(CodegEuiFrame, t0_ns) == 136,
              "CodegEuiFrame t0 offset drift");
static_assert(offsetof(CodegEuiFrame, t_first_token_ns) == 144,
              "CodegEuiFrame first token offset drift");
static_assert(offsetof(CodegEuiFrame, t_end_ns) == 152,
              "CodegEuiFrame end offset drift");

TEST(AbiLayout, matches_v1_size_alignment_and_offsets) {
    EXPECT_EQ(CODEG_EUI_API_VERSION, 1u);
    EXPECT_EQ(CODEG_EUI_OK, 0);
    EXPECT_EQ(CODEG_EUI_ERR_INVALID_STATE, 1);
    EXPECT_EQ(CODEG_EUI_ERR_NULL_POINTER, 2);
    EXPECT_EQ(CODEG_EUI_ERR_INVALID_UTF8, 3);
    EXPECT_EQ(CODEG_EUI_ERR_TOO_LARGE, 4);
    EXPECT_EQ(CODEG_EUI_ERR_QUEUE_FULL, 5);
    EXPECT_EQ(CODEG_EUI_ERR_WRONG_THREAD, 6);
    EXPECT_EQ(CODEG_EUI_ERR_PANIC, 7);
    EXPECT_EQ(CODEG_EUI_ERR_INTERNAL, 8);
    EXPECT_EQ(CODEG_EUI_ERR_NOT_READY, 9);
    EXPECT_EQ(CODEG_EUI_LIFECYCLE_STOPPED, 4);
    EXPECT_EQ(CODEG_EUI_OP_PROBE_AGENT, 8);
    EXPECT_EQ(CODEG_EUI_COMPLETION_CANCELLED, 3);
    EXPECT_EQ(sizeof(CodegEuiFrame), static_cast<std::size_t>(160));
    EXPECT_EQ(offsetof(CodegEuiFrame, generation), static_cast<std::size_t>(8));
    EXPECT_EQ(offsetof(CodegEuiFrame, shutdown_ready),
              static_cast<std::size_t>(98));
}
