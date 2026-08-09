#include "codeg_eui_bridge.h"
#include "test_harness.h"

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <string>
#include <thread>
#include <vector>

#include <unistd.h>

namespace {

struct Completion {
    std::uint64_t requestId;
    std::uint32_t status;
};

void appendCopiedCompletions(const CodegEuiFrame& frame,
                             std::vector<Completion>& target) {
    if (frame.completions_len == 0) {
        EXPECT_TRUE(frame.completions == nullptr);
        return;
    }
    ASSERT_EQ(frame.completions == nullptr, false);
    for (std::size_t index = 0; index < frame.completions_len; ++index) {
        const CodegEuiCompletion& completion = frame.completions[index];
        target.push_back({completion.request_id, completion.status});
    }
}

std::size_t countCompletion(const std::vector<Completion>& values,
                            std::uint64_t requestId,
                            std::uint32_t status) {
    std::size_t count = 0;
    for (const Completion& completion : values) {
        if (completion.requestId == requestId && completion.status == status) {
            ++count;
        }
    }
    return count;
}

}  // namespace

TEST(ShutdownDrain, exposes_cancelled_completion_before_final_free) {
    const std::filesystem::path root =
        std::filesystem::temp_directory_path() /
        ("codeg-eui-shutdown-drain-" + std::to_string(getpid()));
    std::filesystem::remove_all(root);
    const std::string rootString = root.string();

    ASSERT_EQ(codeg_eui_init(
                  reinterpret_cast<const std::uint8_t*>(rootString.data()),
                  rootString.size()),
              CODEG_EUI_OK);
    std::uint64_t requestId = 0;
    ASSERT_EQ(codeg_eui_test_enqueue_blocked(&requestId), CODEG_EUI_OK);
    ASSERT_EQ(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);

    std::vector<Completion> seen;
    bool ready = false;
    for (int attempt = 0; attempt < 200; ++attempt) {
        CodegEuiFrame frame{};
        ASSERT_EQ(codeg_eui_poll(&frame), CODEG_EUI_OK);
        appendCopiedCompletions(frame, seen);
        if (frame.shutdown_ready == 1) {
            ready = true;
            break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }

    EXPECT_TRUE(ready);
    ASSERT_EQ(countCompletion(
                  seen, requestId, CODEG_EUI_COMPLETION_CANCELLED),
              static_cast<std::size_t>(1));
    ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_OK);
    std::filesystem::remove_all(root);
}
