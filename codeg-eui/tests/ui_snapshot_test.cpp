#include "codeg_eui_bridge.h"
#include "test_harness.h"
#include "ui_snapshot.h"

#include <stdexcept>
#include <string>

TEST(UiSnapshot, owns_frame_a_after_frame_b_and_shutdown) {
    std::string rustBacking = "frame-a";
    std::string sessionTitle = "Session A";
    std::string sessionAgent = "codex";
    std::string completionResult = "result-a";
    std::string completionError = "error-a";
    CodegEuiSessionSummary session{};
    session.conversation_id = 17;
    session.title = {
        reinterpret_cast<const std::uint8_t*>(sessionTitle.data()),
        sessionTitle.size(),
    };
    session.agent = {
        reinterpret_cast<const std::uint8_t*>(sessionAgent.data()),
        sessionAgent.size(),
    };
    session.updated_at_ms = 42;
    CodegEuiCompletion completion{};
    completion.request_id = 23;
    completion.op = CODEG_EUI_OP_SEND_USER_MESSAGE;
    completion.status = CODEG_EUI_COMPLETION_ERROR;
    completion.result_payload = {
        reinterpret_cast<const std::uint8_t*>(completionResult.data()),
        completionResult.size(),
    };
    completion.error = {
        reinterpret_cast<const std::uint8_t*>(completionError.data()),
        completionError.size(),
    };
    CodegEuiFrame frameA{};
    frameA.sessions = &session;
    frameA.sessions_len = 1;
    frameA.live_assistant = {
        reinterpret_cast<const std::uint8_t*>(rustBacking.data()),
        rustBacking.size(),
    };
    frameA.completions = &completion;
    frameA.completions_len = 1;

    const UiSnapshot copied = copy_frame(frameA);
    rustBacking = "frame-b";
    rustBacking.clear();
    sessionTitle.clear();
    sessionAgent.clear();
    completionResult.clear();
    completionError.clear();

    EXPECT_EQ(copied.liveAssistant, std::string("frame-a"));
    ASSERT_EQ(copied.sessions.size(), static_cast<std::size_t>(1));
    EXPECT_EQ(copied.sessions[0].conversationId, 17);
    EXPECT_EQ(copied.sessions[0].title, std::string("Session A"));
    EXPECT_EQ(copied.sessions[0].agent, std::string("codex"));
    ASSERT_EQ(copied.completions.size(), static_cast<std::size_t>(1));
    EXPECT_EQ(copied.completions[0].requestId, static_cast<std::uint64_t>(23));
    EXPECT_EQ(copied.completions[0].resultPayload, std::string("result-a"));
    EXPECT_EQ(copied.completions[0].error, std::string("error-a"));
}

TEST(UiSnapshot, validates_null_and_length_pairs) {
    CodegEuiFrame invalid{};
    invalid.live_assistant = {nullptr, 1};
    bool rejected = false;
    try {
        (void)copy_frame(invalid);
    } catch (const std::invalid_argument&) {
        rejected = true;
    }
    EXPECT_TRUE(rejected);

    CodegEuiFrame empty{};
    empty.live_assistant = {nullptr, 0};
    bool accepted = true;
    try {
        const UiSnapshot copied = copy_frame(empty);
        accepted = copied.liveAssistant.empty();
    } catch (...) {
        accepted = false;
    }
    EXPECT_TRUE(accepted);
}
