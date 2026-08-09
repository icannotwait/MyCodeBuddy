#include "pages/settings.h"
#include "test_harness.h"

#include <set>
#include <string>

TEST(SettingsState, save_payload_contains_only_facade_fields) {
  SettingsState state = codexSettingsFixture();
  // P0 fixture without P1-only projection keys for this assertion:
  state.model.clear();
  state.provider.clear();
  state.reasoning.clear();
  state.codexApproval.clear();
  const auto json = state.buildPatchJson();
  EXPECT_TRUE(jsonContainsKey(json, "enabled"));
  EXPECT_TRUE(hasOnlyKeys(json, SettingsState::codexFacadeKeys()));
  EXPECT_FALSE(json.find("OPENAI_API_KEY=test-secret") != std::string::npos);
  EXPECT_FALSE(jsonContainsKey(json, "uiTabLabel"));
  EXPECT_FALSE(jsonContainsKey(json, "dirty"));
}

TEST(SettingsState, serializes_p1_fields_without_ui_only_keys) {
  SettingsState state = codexSettingsFixture();
  const auto json = state.buildPatchJson();
  EXPECT_TRUE(jsonContainsKey(json, "model"));
  EXPECT_TRUE(jsonContainsKey(json, "provider"));
  EXPECT_TRUE(jsonContainsKey(json, "reasoning"));
  EXPECT_TRUE(jsonContainsKey(json, "approvalMode"));
  EXPECT_FALSE(jsonContainsKey(json, "uiTabLabel"));
  EXPECT_FALSE(jsonContainsKey(json, "dirty"));
  auto allowed = SettingsState::codexFacadeKeys();
  for (const auto& key : SettingsState::p1ExtraKeys()) {
    allowed.insert(key);
  }
  EXPECT_TRUE(hasOnlyKeys(json, allowed));
}

TEST(SettingsState, grok_payload_uses_grok_keys) {
  SettingsState state = grokSettingsFixture();
  const auto json = state.buildPatchJson();
  EXPECT_TRUE(jsonContainsKey(json, "enabled"));
  EXPECT_TRUE(jsonContainsKey(json, "grokConfigToml"));
  EXPECT_FALSE(jsonContainsKey(json, "codexAuthJson"));
}

TEST(SettingsState, unsupported_agent_rejected) {
  SettingsPage page;
  EXPECT_FALSE(page.acceptAgentWire("claude"));
  EXPECT_TRUE(page.acceptAgentWire("grok"));
  EXPECT_EQ(static_cast<int>(page.activeTab()), static_cast<int>(Agent::Grok));
}

TEST(SettingsState, probe_and_save_pending_ids) {
  SettingsState state;
  EXPECT_FALSE(state.probePending());
  state.beginProbe(11);
  state.beginSave(12);
  EXPECT_TRUE(state.probePending());
  EXPECT_TRUE(state.savePending());
  state.applyCompletion(
      Completion{11, Operation::ProbeAgent, CompletionStatus::Ok, "{}", ""});
  EXPECT_FALSE(state.probePending());
  EXPECT_TRUE(state.savePending());
  state.applyCompletion(Completion{12, Operation::SetAgentSettings,
                                   CompletionStatus::Ok, "{}", ""});
  EXPECT_FALSE(state.savePending());
}

TEST(SettingsState, credential_redaction_in_errors) {
  const auto redacted =
      AppModel::redactSecrets("fail OPENAI_API_KEY=test-secret trailing");
  EXPECT_TRUE(redacted.find("test-secret") == std::string::npos);
  EXPECT_TRUE(redacted.find("***") != std::string::npos);
}

TEST(SettingsState, raw_editor_round_trip_in_patch) {
  SettingsState state;
  state.agent = Agent::Codex;
  state.enabled = false;
  state.codexConfigToml = "model = \"o3\"\n";
  const auto json = state.buildPatchJson();
  EXPECT_TRUE(json.find("model = \\\"o3\\\"") != std::string::npos ||
              json.find("model = \"o3\"") != std::string::npos);
  EXPECT_TRUE(jsonContainsKey(json, "codexConfigToml"));
  EXPECT_TRUE(json.find("false") != std::string::npos);
}
