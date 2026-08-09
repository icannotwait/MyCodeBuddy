#include "test_harness.h"

TEST(Harness, version_and_plan_assertions_are_available) {
    EXPECT_EQ(CODEG_EUI_TEST_HARNESS_VERSION, 1);
    EXPECT_TRUE(true);
    EXPECT_FALSE(false);
    EXPECT_GE(2, 1);
    ASSERT_EQ(4, 2 + 2);
}
