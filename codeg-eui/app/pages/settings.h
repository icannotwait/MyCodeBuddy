#pragma once

#include "model.h"

#include <algorithm>
#include <map>
#include <optional>
#include <set>
#include <sstream>
#include <string>
#include <vector>

enum class Agent { Codex, Grok };

inline const char* agentWire(Agent agent) {
  return agent == Agent::Codex ? "codex" : "grok";
}

inline bool parseAgent(const std::string& wire, Agent* out) {
  if (wire == "codex") {
    *out = Agent::Codex;
    return true;
  }
  if (wire == "grok") {
    *out = Agent::Grok;
    return true;
  }
  return false;
}

struct SettingsState {
  Agent agent = Agent::Codex;
  bool enabled = true;
  bool available = false;
  std::string installedVersion;
  std::string modelProviderId;
  std::map<std::string, std::string> env;
  std::string configJson;
  std::string codexAuthJson;
  std::string codexConfigToml;
  std::string codexModelCatalog;
  std::string codexSandbox;     // JSON fragment for structured sandbox
  std::string codexApproval;
  std::string grokConfigToml;
  std::string grokStructured;  // JSON fragment
  std::string model;           // P1 structured
  std::string provider;        // P1
  std::string reasoning;       // P1
  // UI-only (never serialized)
  bool dirty = false;
  std::string uiTabLabel;
  std::optional<std::uint64_t> probeRequestId;
  std::optional<std::uint64_t> saveRequestId;

  static std::set<std::string> codexFacadeKeys() {
    return {
        "enabled",
        "env",
        "modelProviderId",
        "configJson",
        "codexAuthJson",
        "codexConfigToml",
        "codexModelCatalog",
        "codexSandbox",
    };
  }

  static std::set<std::string> grokFacadeKeys() {
    return {
        "enabled",
        "env",
        "modelProviderId",
        "configJson",
        "grokConfigToml",
        "grokStructured",
    };
  }

  static std::set<std::string> p1ExtraKeys() {
    return {"model", "provider", "reasoning", "approvalMode"};
  }

  std::string escapeJson(const std::string& in) const {
    std::string out;
    out.reserve(in.size() + 8);
    for (char c : in) {
      switch (c) {
        case '\\':
          out += "\\\\";
          break;
        case '"':
          out += "\\\"";
          break;
        case '\n':
          out += "\\n";
          break;
        case '\r':
          out += "\\r";
          break;
        case '\t':
          out += "\\t";
          break;
        default:
          out += c;
      }
    }
    return out;
  }

  std::string buildPatchJson() const {
    std::ostringstream oss;
    oss << '{';
    bool first = true;
    auto field = [&](const char* key, const std::string& value, bool raw) {
      if (!first) {
        oss << ',';
      }
      first = false;
      oss << '"' << key << "\":";
      if (raw) {
        oss << value;
      } else {
        oss << '"' << escapeJson(value) << '"';
      }
    };
    auto boolField = [&](const char* key, bool value) {
      if (!first) {
        oss << ',';
      }
      first = false;
      oss << '"' << key << "\":" << (value ? "true" : "false");
    };

    boolField("enabled", enabled);
    if (!env.empty()) {
      if (!first) {
        oss << ',';
      }
      first = false;
      oss << "\"env\":{";
      bool ef = true;
      for (const auto& entry : env) {
        if (!ef) {
          oss << ',';
        }
        ef = false;
        oss << '"' << escapeJson(entry.first) << "\":\""
            << escapeJson(entry.second) << '"';
      }
      oss << '}';
    }
    if (!modelProviderId.empty()) {
      field("modelProviderId", modelProviderId, false);
    }
    if (!configJson.empty()) {
      field("configJson", configJson, false);
    }

    if (agent == Agent::Codex) {
      if (!codexAuthJson.empty()) {
        field("codexAuthJson", codexAuthJson, false);
      }
      if (!codexConfigToml.empty()) {
        field("codexConfigToml", codexConfigToml, false);
      }
      if (!codexModelCatalog.empty()) {
        field("codexModelCatalog", codexModelCatalog, false);
      }
      if (!codexSandbox.empty()) {
        field("codexSandbox", codexSandbox, true);
      }
      if (!codexApproval.empty()) {
        field("approvalMode", codexApproval, false);
      }
    } else {
      if (!grokConfigToml.empty()) {
        field("grokConfigToml", grokConfigToml, false);
      }
      if (!grokStructured.empty()) {
        field("grokStructured", grokStructured, true);
      }
    }

    // P1 structured fields (optional)
    if (!model.empty()) {
      field("model", model, false);
    }
    if (!provider.empty()) {
      field("provider", provider, false);
    }
    if (!reasoning.empty()) {
      field("reasoning", reasoning, false);
    }

    oss << '}';
    return oss.str();
  }

  bool probePending() const { return probeRequestId.has_value(); }
  bool savePending() const { return saveRequestId.has_value(); }

  void beginProbe(std::uint64_t requestId) { probeRequestId = requestId; }
  void beginSave(std::uint64_t requestId) { saveRequestId = requestId; }

  void applyCompletion(const Completion& completion) {
    if (probeRequestId && *probeRequestId == completion.requestId) {
      probeRequestId.reset();
    }
    if (saveRequestId && *saveRequestId == completion.requestId) {
      saveRequestId.reset();
    }
  }
};

class SettingsPage {
 public:
  SettingsState& codex() { return codex_; }
  SettingsState& grok() { return grok_; }
  Agent activeTab() const { return active_; }
  void setActiveTab(Agent agent) { active_ = agent; }

  SettingsState& active() {
    return active_ == Agent::Codex ? codex_ : grok_;
  }

  std::string buildPatchJson(Agent agent) const {
    return agent == Agent::Codex ? codex_.buildPatchJson()
                                 : grok_.buildPatchJson();
  }

  bool acceptAgentWire(const std::string& wire) {
    Agent agent{};
    if (!parseAgent(wire, &agent)) {
      return false;
    }
    active_ = agent;
    return true;
  }

 private:
  SettingsState codex_{Agent::Codex};
  SettingsState grok_{Agent::Grok};
  Agent active_ = Agent::Codex;
};

// Tiny JSON helpers for tests (not a full parser).
inline bool jsonContainsKey(const std::string& json, const std::string& key) {
  return json.find('"' + key + '"') != std::string::npos;
}

inline bool hasOnlyKeys(const std::string& json,
                        const std::set<std::string>& allowed) {
  // Scan for "key": patterns
  std::size_t pos = 0;
  while (pos < json.size()) {
    const std::size_t q1 = json.find('"', pos);
    if (q1 == std::string::npos) {
      break;
    }
    const std::size_t q2 = json.find('"', q1 + 1);
    if (q2 == std::string::npos) {
      break;
    }
    // Only top-level keys: preceded by { or , (ignoring whitespace)
    bool top = false;
    if (q1 == 0) {
      top = true;
    } else {
      std::size_t i = q1;
      while (i > 0) {
        --i;
        const char c = json[i];
        if (c == ' ' || c == '\n' || c == '\t' || c == '\r') {
          continue;
        }
        top = (c == '{' || c == ',');
        break;
      }
    }
    if (top && q2 + 1 < json.size()) {
      std::size_t colon = q2 + 1;
      while (colon < json.size() &&
             (json[colon] == ' ' || json[colon] == '\t')) {
        ++colon;
      }
      if (colon < json.size() && json[colon] == ':') {
        const std::string key = json.substr(q1 + 1, q2 - q1 - 1);
        // Nested object keys after env:{ are not top-level — detect depth.
        int depth = 0;
        for (std::size_t d = 0; d < q1; ++d) {
          if (json[d] == '{' || json[d] == '[') {
            ++depth;
          } else if (json[d] == '}' || json[d] == ']') {
            --depth;
          }
        }
        if (depth == 1 && allowed.count(key) == 0) {
          return false;
        }
      }
    }
    pos = q2 + 1;
  }
  return true;
}

inline SettingsState codexSettingsFixture() {
  SettingsState state;
  state.agent = Agent::Codex;
  state.enabled = true;
  state.available = true;
  state.codexAuthJson = "{}";
  state.codexConfigToml = "model = \"gpt-5\"\n";
  state.codexSandbox = "{\"mode\":\"workspace-write\"}";
  state.codexApproval = "on-request";
  state.env["OPENAI_API_KEY"] = "test-secret";
  state.model = "gpt-5";
  state.provider = "openai";
  state.reasoning = "medium";
  state.uiTabLabel = "Codex";
  state.dirty = true;
  return state;
}

inline SettingsState grokSettingsFixture() {
  SettingsState state;
  state.agent = Agent::Grok;
  state.enabled = true;
  state.grokConfigToml = "model = \"grok-3\"\n";
  state.grokStructured = "{\"temperature\":0.2}";
  state.model = "grok-3";
  state.reasoning = "high";
  return state;
}
