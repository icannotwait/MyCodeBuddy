#include "bridge/client.h"
#include "bridge/codeg_eui_bridge.h"
#include "model.h"
#include "pages/chat.h"
#include "pages/settings.h"
#include "pages/shell.h"
#include "ui_snapshot.h"

#include "eui_neo.h"

#include <chrono>
#include <cstdlib>
#include <string>

#if defined(EUI_WINDOW_BACKEND_GLFW) || defined(GLFW_INCLUDE_NONE) || 1
#include <GLFW/glfw3.h>
#endif

namespace app {
namespace {

class BridgeLifecycle final : public BridgeApi {
 public:
  BridgeLifecycle() : initStatus_(codeg_eui_init(nullptr, 0)) {}

  ~BridgeLifecycle() override {
    if (initStatus_ != CODEG_EUI_OK) {
      return;
    }
    ShutdownDriver driver(*this);
    (void)driver.drainAndShutdown();
  }

  int beginShutdown() override { return codeg_eui_begin_shutdown(); }
  int poll(CodegEuiFrame* out) override {
    if (initStatus_ != CODEG_EUI_OK) {
      return initStatus_;
    }
    return codeg_eui_poll(out);
  }
  int shutdown() override { return codeg_eui_shutdown(); }

  int initStatus() const { return initStatus_; }

 private:
  int initStatus_;
};

struct Host {
  BridgeLifecycle lifecycle;
  AppModel model;
  BridgeClient client{model, productionBridgeApi()};
  ShellPage shell;
  ChatPage chat;
  SettingsPage settings;
  SmokeFrameExit smoke;
  std::uint64_t lastGeneration = 0;
  bool nonblankShell = false;
};

Host& host() {
  static Host value;
  return value;
}

void paintErrorStrip(eui::Ui& ui, const ShellLayout& layout, const std::string& text) {
  if (text.empty()) {
    return;
  }
  ui.stack("error.strip")
      .position(layout.errorStrip.x, layout.errorStrip.y)
      .size(layout.errorStrip.width, layout.errorStrip.height)
      .background({0.35f, 0.12f, 0.12f, 1.0f})
      .content([&] {
        ui.text("error.strip.text")
            .size(layout.errorStrip.width - 16.0f, layout.errorStrip.height)
            .text(text)
            .fontSize(13.0f)
            .color({0.98f, 0.85f, 0.85f, 1.0f})
            .build();
      })
      .build();
}

void paintShell(eui::Ui& ui, float width, float height, Host& h) {
  h.shell.recompute(width, height);
  const ShellLayout& layout = h.shell.layout();
  const auto& model = h.model;

  // Sidebar
  ui.stack("shell.sidebar")
      .position(layout.sidebar.x, layout.sidebar.y)
      .size(layout.sidebar.width, layout.sidebar.height)
      .background({0.07f, 0.08f, 0.10f, 1.0f})
      .content([&] {
        ui.text("shell.brand")
            .position(12, 12)
            .size(220, 28)
            .text("Codeg EUI")
            .fontSize(18.0f)
            .color({0.94f, 0.96f, 0.98f, 1.0f})
            .build();

        const bool newEnabled = h.shell.newSessionEnabled(model);
        ui.button("shell.new")
            .position(12, 52)
            .size(224, 36)
            .text("New session")
            .enabled(newEnabled)
            .onClick([&] {
              if (newEnabled) {
                (void)h.client.enqueueCreateSession(model.selectedAgent);
              }
            })
            .build();

        ui.input("shell.workspace")
            .position(12, 100)
            .size(224, 32)
            .value(model.workspacePath)
            .placeholder("Workspace path")
            .onSubmit([&](const std::string& path) {
              h.model.workspacePath = path;
              (void)h.client.enqueueSetWorkspace(path);
            })
            .build();

        ui.button("shell.agent.codex")
            .position(12, 144)
            .size(108, 32)
            .text("Codex")
            .onClick([&] { h.model.selectedAgent = "codex"; })
            .build();
        ui.button("shell.agent.grok")
            .position(128, 144)
            .size(108, 32)
            .text("Grok")
            .onClick([&] { h.model.selectedAgent = "grok"; })
            .build();

        float y = 196;
        for (const auto& session : model.snapshot.sessions) {
          const std::string id =
              "shell.session." + std::to_string(session.conversationId);
          ui.button(id.c_str())
              .position(12, y)
              .size(224, 36)
              .text(session.title.empty()
                        ? ("#" + std::to_string(session.conversationId))
                        : session.title)
              .onClick([&, cid = session.conversationId] {
                h.model.selectedConversationId = cid;
                (void)h.client.enqueueSelectSession(cid);
              })
              .build();
          y += 40;
        }

        ui.button("shell.nav.chat")
            .position(12, layout.sidebar.height - 96)
            .size(224, 36)
            .text("Chat")
            .onClick([&] { h.shell.navigate(Route::Chat); })
            .build();
        ui.button("shell.nav.settings")
            .position(12, layout.sidebar.height - 52)
            .size(224, 36)
            .text("Settings")
            .onClick([&] { h.shell.navigate(Route::Settings); })
            .build();
      })
      .build();

  // Header
  ui.stack("shell.header")
      .position(layout.header.x, layout.header.y)
      .size(layout.header.width, layout.header.height)
      .background({0.09f, 0.10f, 0.12f, 1.0f})
      .content([&] {
        const std::string status = h.shell.statusLabel(model);
        ui.text("shell.header.status")
            .position(16, 12)
            .size(layout.header.width - 32, 24)
            .text("Status: " + status + " · agent " + model.selectedAgent)
            .fontSize(14.0f)
            .color(status == "error" ? eui::Color{0.95f, 0.45f, 0.45f, 1.0f}
                   : status == "streaming"
                       ? eui::Color{0.45f, 0.85f, 0.55f, 1.0f}
                       : eui::Color{0.82f, 0.86f, 0.90f, 1.0f})
            .build();
      })
      .build();

  paintErrorStrip(ui, layout, model.errorStrip);

  if (h.shell.route() == Route::Settings) {
    ui.stack("settings.root")
        .position(layout.content.x, layout.content.y)
        .size(layout.content.width, layout.content.height)
        .background({0.06f, 0.07f, 0.09f, 1.0f})
        .content([&] {
          ui.text("settings.title")
              .position(16, 16)
              .size(400, 28)
              .text("Settings (Grok / Codex P0)")
              .fontSize(18.0f)
              .color({0.94f, 0.96f, 0.98f, 1.0f})
              .build();
          ui.button("settings.tab.codex")
              .position(16, 56)
              .size(120, 32)
              .text("Codex")
              .onClick([&] { h.settings.setActiveTab(Agent::Codex); })
              .build();
          ui.button("settings.tab.grok")
              .position(144, 56)
              .size(120, 32)
              .text("Grok")
              .onClick([&] { h.settings.setActiveTab(Agent::Grok); })
              .build();
          auto& active = h.settings.active();
          ui.button("settings.probe")
              .position(16, 104)
              .size(120, 32)
              .text(active.probePending() ? "Probing…" : "Probe")
              .enabled(!active.probePending())
              .onClick([&] {
                if (h.client.enqueueProbe(agentWire(h.settings.activeTab()))) {
                  for (const auto& e : h.model.pending) {
                    if (e.second.op == Operation::ProbeAgent) {
                      active.beginProbe(e.first);
                    }
                  }
                }
              })
              .build();
          ui.button("settings.save")
              .position(148, 104)
              .size(120, 32)
              .text(active.savePending() ? "Saving…" : "Save")
              .enabled(!active.savePending())
              .onClick([&] {
                const std::string json =
                    h.settings.buildPatchJson(h.settings.activeTab());
                if (h.client.enqueueSetSettings(
                        agentWire(h.settings.activeTab()), json)) {
                  for (const auto& e : h.model.pending) {
                    if (e.second.op == Operation::SetAgentSettings) {
                      active.beginSave(e.first);
                    }
                  }
                }
              })
              .build();
          ui.text("settings.hint")
              .position(16, 156)
              .size(layout.content.width - 32, 80)
              .text("Facade fields only. Secrets never shown in the error strip.")
              .fontSize(13.0f)
              .color({0.7f, 0.74f, 0.78f, 1.0f})
              .build();
        })
        .build();
  } else {
    // Chat transcript
    const auto lines = ChatState::projectTranscript(
        model.snapshot.transcriptJson, model.snapshot.liveAssistant);
    ui.stack("chat.root")
        .position(layout.content.x, layout.content.y)
        .size(layout.content.width, layout.content.height)
        .background({0.055f, 0.062f, 0.075f, 1.0f})
        .content([&] {
          float y = 12;
          int index = 0;
          for (const auto& line : lines) {
            const std::string key = "chat.line." + std::to_string(index++);
            std::string body;
            if (line.role == "tool") {
              body = ChatState::projectToolLine(line.toolName, line.toolStatus);
            } else if (line.role == "user") {
              body = line.text;
            } else {
              body = line.text;
            }
            ui.text(key.c_str())
                .position(16, y)
                .size(layout.content.width - 32, 48)
                .text((line.role == "user" ? "You: " : line.role == "tool" ? "" : "Agent: ") +
                      body)
                .fontSize(14.0f)
                .color({0.90f, 0.92f, 0.94f, 1.0f})
                .build();
            y += 52;
          }
          if (lines.empty()) {
            ui.text("chat.empty")
                .position(16, 16)
                .size(layout.content.width - 32, 40)
                .text("Select a workspace and session, then send a message.")
                .fontSize(14.0f)
                .color({0.65f, 0.70f, 0.75f, 1.0f})
                .build();
          }
        })
        .build();

    // Composer
    ui.stack("chat.composer")
        .position(layout.composer.x, layout.composer.y)
        .size(layout.composer.width, layout.composer.height)
        .background({0.08f, 0.09f, 0.11f, 1.0f})
        .content([&] {
          const bool canSend = h.chat.state().sendEnabled(model) ||
                               (!h.chat.state().composer.empty() && model.canSend() &&
                                !h.chat.state().sendRequestId.has_value());
          ui.input("chat.input")
              .position(12, 6)
              .size(layout.composer.width - 120, 32)
              .value(h.chat.state().composer)
              .placeholder("Message")
              .onChange([&](const std::string& value) {
                h.chat.state().composer = value;
              })
              .build();
          ui.button("chat.send")
              .position(layout.composer.width - 100, 6)
              .size(88, 32)
              .text("Send")
              .enabled(canSend || !h.chat.state().composer.empty())
              .onClick([&] {
                (void)h.chat.trySend(h.model, [&](const std::string& text) {
                  return h.client.enqueueSend(text);
                });
              })
              .build();
        })
        .build();
  }

  h.nonblankShell = true;
  h.smoke.noteShellComposed();
}

}  // namespace

const DslAppConfig& dslAppConfig() {
  (void)host();
  static const DslAppConfig config = DslAppConfig{}
                                         .title("Codeg EUI Spike")
                                         .pageId("codeg_eui")
                                         .clearColor({0.055f, 0.062f, 0.075f, 1.0f})
                                         .windowSize(1180, 760)
                                         .fps(60.0);
  return config;
}

void compose(eui::Ui& ui, const eui::Screen& screen) {
  Host& h = host();
  if (h.lifecycle.initStatus() != CODEG_EUI_OK && h.model.errorStrip.empty()) {
    h.model.errorStrip =
        "bridge init failed: " + std::to_string(h.lifecycle.initStatus());
  }

  static bool smokeParsed = false;
  if (!smokeParsed) {
    h.smoke = SmokeFrameExit::parseEnv(
        std::getenv("CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES"));
    smokeParsed = true;
  }

  h.client.pollIfDue(std::chrono::steady_clock::now());

  // Complete settings pending when completions arrive
  for (const auto& raw : h.model.snapshot.completions) {
    Completion c{raw.requestId, static_cast<Operation>(raw.op),
                 static_cast<CompletionStatus>(raw.status), raw.resultPayload,
                 raw.error};
    h.settings.codex().applyCompletion(c);
    h.settings.grok().applyCompletion(c);
    h.chat.state().onSendCompleted(c);
  }

  paintShell(ui, screen.width, screen.height, h);

  // Persistent 1x1 ticker for smoke frame counting (post-shell).
  ui.stack("smoke.ticker")
      .position(0, 0)
      .size(1, 1)
      .onFrame([&] {
        if (h.smoke.onFrameCallback()) {
          if (GLFWwindow* window = glfwGetCurrentContext()) {
            glfwSetWindowShouldClose(window, GLFW_TRUE);
          }
        }
      })
      .build();
}

}  // namespace app
