#include "model.h"
#include "pages/chat.h"
#include "test_harness.h"

#include <chrono>
#include <string>

namespace {
std::chrono::milliseconds ms(int value) {
  return std::chrono::milliseconds(value);
}
}  // namespace

TEST(ChatState, throttles_streaming_markdown_but_flushes_turn_end) {
  ChatState state;
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(0), Generation::Delta));
  EXPECT_FALSE(state.shouldRebuildMarkdown(ms(74), Generation::Delta));
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(75), Generation::Delta));
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(76), Generation::TurnEnd));
}

TEST(ChatState, send_enablement_and_composer_clear_on_accept) {
  AppModel model;
  model.workspacePath = "/ws";
  model.selectedConversationId = 3;
  model.selectedAgent = "grok";
  ChatState state;
  state.composer = "hi";
  EXPECT_TRUE(state.sendEnabled(model));
  state.onSendAccepted(9);
  EXPECT_TRUE(state.composer.empty());
  EXPECT_FALSE(state.sendEnabled(model));
  state.composer = "again";
  EXPECT_FALSE(state.sendEnabled(model));
  state.onSendCompleted(
      Completion{9, Operation::SendUserMessage, CompletionStatus::Ok, "", ""});
  EXPECT_TRUE(state.sendEnabled(model));
}

TEST(ChatState, retain_composer_on_reject) {
  ChatState state;
  state.composer = "keep me";
  state.onSendRejected();
  EXPECT_EQ(state.composer, std::string("keep me"));
}

TEST(ChatState, tool_and_user_projection) {
  EXPECT_EQ(ChatState::projectToolLine("bash", "completed"),
            std::string("tool: bash - completed"));
  const auto lines = ChatState::projectTranscript(
      "user: hello\ntool: bash|running\nassistant: partial\n", "live tail");
  EXPECT_EQ(lines.size(), static_cast<std::size_t>(4));
  EXPECT_EQ(lines[0].role, std::string("user"));
  EXPECT_EQ(lines[0].text, std::string("hello"));
  EXPECT_EQ(lines[1].role, std::string("tool"));
  EXPECT_EQ(lines[1].toolName, std::string("bash"));
  EXPECT_EQ(lines[3].text, std::string("live tail"));
}

TEST(ChatState, bottom_follow_threshold) {
  ChatState state;
  state.onScroll(0, 1000, 400);
  EXPECT_FALSE(state.followBottom);
  state.onScroll(560, 1000, 400);  // 1000-400-560=40 <= 48
  EXPECT_TRUE(state.followBottom);
}

TEST(ChatPage, try_send_wires_enqueue) {
  AppModel model;
  model.workspacePath = "/ws";
  model.selectedConversationId = 1;
  model.selectedAgent = "codex";
  ChatPage page;
  page.state().composer = "ping";
  bool called = false;
  const bool ok = page.trySend(model, [&](const std::string& text) {
    called = text == "ping";
    model.pending.emplace(77,
                          PendingRequest{Operation::SendUserMessage, 0});
    return true;
  });
  EXPECT_TRUE(ok);
  EXPECT_TRUE(called);
  EXPECT_TRUE(page.state().composer.empty());
  EXPECT_TRUE(page.state().sendRequestId.has_value());
  EXPECT_EQ(*page.state().sendRequestId, static_cast<std::uint64_t>(77));
}
