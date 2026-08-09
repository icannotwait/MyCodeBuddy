#include "model.h"
#include "pages/shell.h"
#include "test_harness.h"

TEST(ShellLayout, remains_valid_at_minimum_supported_size) {
  const auto layout = ShellLayout::calculate(800, 600);
  EXPECT_EQ(layout.sidebar.width, 248.0f);
  EXPECT_EQ(layout.header.height, 48.0f);
  EXPECT_GE(layout.content.width, 0.0f);
  EXPECT_FALSE(layout.overlaps());
}

TEST(ShellLayout, remains_valid_at_desktop_size) {
  const auto layout = ShellLayout::calculate(1440, 900);
  EXPECT_EQ(layout.sidebar.width, 248.0f);
  EXPECT_EQ(layout.composer.height, 44.0f);
  EXPECT_GE(layout.content.height, 0.0f);
  EXPECT_FALSE(layout.overlaps());
}

TEST(ShellPage, navigation_between_chat_and_settings) {
  ShellPage page;
  EXPECT_EQ(static_cast<int>(page.route()), static_cast<int>(Route::Chat));
  page.navigate(Route::Settings);
  EXPECT_EQ(static_cast<int>(page.route()), static_cast<int>(Route::Settings));
  page.navigate(Route::Chat);
  EXPECT_EQ(static_cast<int>(page.route()), static_cast<int>(Route::Chat));
}

TEST(ShellPage, new_enablement_requires_workspace_and_agent) {
  ShellPage page;
  AppModel model;
  EXPECT_FALSE(page.newSessionEnabled(model));
  model.workspacePath = "/tmp/ws";
  model.selectedAgent = "codex";
  EXPECT_TRUE(page.newSessionEnabled(model));
  model.selectedAgent = "claude";
  EXPECT_FALSE(page.newSessionEnabled(model));
}

TEST(ShellPage, status_labels_stream_error_ready) {
  ShellPage page;
  AppModel model;
  EXPECT_EQ(page.statusLabel(model), std::string("idle"));
  model.currentConnectionId = "c1";
  EXPECT_EQ(page.statusLabel(model), std::string("ready"));
  model.snapshot.streamActive = true;
  EXPECT_EQ(page.statusLabel(model), std::string("streaming"));
  model.snapshot.streamActive = false;
  model.errorStrip = "boom";
  EXPECT_EQ(page.statusLabel(model), std::string("error"));
}
