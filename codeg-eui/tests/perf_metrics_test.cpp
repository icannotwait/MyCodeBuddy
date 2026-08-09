#include "perf_metrics.h"
#include "test_harness.h"

#include <vector>

TEST(PerfMetrics, uses_first_presentation_and_fixed_threshold) {
  const auto run = summarizeFrames({0, 16, 32, 92, 108}, 0, 16, 108);
  EXPECT_EQ(run.firstPresentedLatencyMs, 16.0);
  EXPECT_EQ(run.frameIntervalP95Ms, 60.0);
  EXPECT_EQ(run.longFrameThresholdMs, 50.0);
  EXPECT_EQ(run.longFrameCount, static_cast<std::uint32_t>(1));
}

TEST(PresentationClock, eui_marks_on_update_after_eligible_present) {
  PresentationClock clock;
  clock.onCompose(true, ns(10));
  EXPECT_FALSE(clock.firstPresentedNs().has_value());
  clock.onPresentedForTest(ns(12));
  EXPECT_FALSE(clock.firstPresentedNs().has_value());
  clock.onFrame(ns(16));
  EXPECT_TRUE(clock.firstPresentedNs().has_value());
  EXPECT_EQ(*clock.firstPresentedNs(), ns(16));
  clock.onFrame(ns(32));
  EXPECT_EQ(*clock.firstPresentedNs(), ns(16));
}

TEST(PerfMetrics, empty_frames_are_zero) {
  const auto run = summarizeFrames({}, 0, 0, 0);
  EXPECT_EQ(run.longFrameCount, static_cast<std::uint32_t>(0));
  EXPECT_EQ(run.frameIntervalP95Ms, 0.0);
}
