#include "codeg_eui_bridge.h"
#include "eui_neo.h"

namespace app {
namespace {

class BridgeLifecycle final {
public:
    BridgeLifecycle()
        : initStatus_(codeg_eui_init(nullptr, 0)) {}

    ~BridgeLifecycle() {
        if (initStatus_ != CODEG_EUI_OK ||
            codeg_eui_begin_shutdown() != CODEG_EUI_OK) {
            return;
        }

        CodegEuiFrame frame{};
        while (codeg_eui_poll(&frame) == CODEG_EUI_OK) {
            if (frame.shutdown_ready == 1) {
                (void)codeg_eui_shutdown();
                return;
            }
        }
    }

    int poll(CodegEuiFrame& frame) const {
        if (initStatus_ != CODEG_EUI_OK) {
            return initStatus_;
        }
        return codeg_eui_poll(&frame);
    }

private:
    int initStatus_;
};

BridgeLifecycle& bridge() {
    static BridgeLifecycle value;
    return value;
}

}  // namespace

const DslAppConfig& dslAppConfig() {
    (void)bridge();
    static const DslAppConfig config = DslAppConfig{}
        .title("Codeg EUI Spike")
        .pageId("codeg_eui_spike")
        .clearColor({0.055f, 0.062f, 0.075f, 1.0f})
        .windowSize(1180, 760)
        .fps(60.0);
    return config;
}

void compose(eui::Ui& ui, const eui::Screen& screen) {
    CodegEuiFrame frame{};
    (void)bridge().poll(frame);

    ui.stack("hello.root")
        .size(screen.width, screen.height)
        .align(eui::Align::CENTER, eui::Align::CENTER)
        .content([&] {
            ui.text("hello.title")
                .size(420.0f, 48.0f)
                .text("Codeg EUI / bridge v1")
                .fontSize(30.0f)
                .lineHeight(40.0f)
                .color({0.94f, 0.96f, 0.98f, 1.0f})
                .build();
        })
        .build();
}

}  // namespace app
