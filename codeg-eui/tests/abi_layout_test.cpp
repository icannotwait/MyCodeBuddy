#include "codeg_eui_bridge.h"
#include "test_harness.h"

#include <cstddef>

static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI size drift");
static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame ABI alignment drift");
static_assert(offsetof(CodegEuiFrame, generation) == 8,
              "CodegEuiFrame generation offset drift");
static_assert(offsetof(CodegEuiFrame, shutdown_ready) == 16,
              "CodegEuiFrame shutdown_ready offset drift");

TEST(AbiLayout, matches_v1_size_alignment_and_offsets) {
    EXPECT_EQ(CODEG_EUI_API_VERSION, 1u);
    EXPECT_EQ(sizeof(CodegEuiFrame), static_cast<std::size_t>(24));
    EXPECT_EQ(offsetof(CodegEuiFrame, generation), static_cast<std::size_t>(8));
    EXPECT_EQ(offsetof(CodegEuiFrame, shutdown_ready),
              static_cast<std::size_t>(16));
}
