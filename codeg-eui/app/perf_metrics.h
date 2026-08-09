#pragma once

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

inline constexpr double LONG_FRAME_MS = 50.0;

struct PerfSummary {
  double firstPresentedLatencyMs = 0;
  double frameIntervalP95Ms = 0;
  double longFrameThresholdMs = LONG_FRAME_MS;
  std::uint32_t longFrameCount = 0;
  std::vector<double> activeIntervalsMs;
};

inline double nearestRankP95(std::vector<double> intervals) {
  if (intervals.empty()) {
    return 0;
  }
  std::sort(intervals.begin(), intervals.end());
  const std::size_t n = intervals.size();
  const std::size_t index =
      static_cast<std::size_t>(std::ceil(0.95 * static_cast<double>(n))) - 1;
  return intervals[std::min(index, n - 1)];
}

// frames: absolute timestamps in ms (or any unit consistent with t0/first/end).
// Active intervals sample from the frame that records firstPresented through end.
inline PerfSummary summarizeFrames(const std::vector<double>& frames,
                                   double t0,
                                   double firstPresented,
                                   double end) {
  PerfSummary summary;
  summary.longFrameThresholdMs = LONG_FRAME_MS;
  summary.firstPresentedLatencyMs = firstPresented - t0;

  std::vector<double> active;
  for (std::size_t i = 0; i + 1 < frames.size(); ++i) {
    const double a = frames[i];
    const double b = frames[i + 1];
    if (a < firstPresented || b > end) {
      // Keep intervals fully inside [firstPresented, end], and the first
      // interval that starts at firstPresented.
      if (!(a >= firstPresented && b <= end)) {
        continue;
      }
    }
    if (a >= firstPresented && b <= end) {
      active.push_back(b - a);
    }
  }
  // Include boundary: intervals where a >= firstPresented && a < end && b <= end
  // Recompute cleanly:
  active.clear();
  for (std::size_t i = 0; i + 1 < frames.size(); ++i) {
    const double a = frames[i];
    const double b = frames[i + 1];
    if (a >= firstPresented && b <= end && a < end) {
      active.push_back(b - a);
    }
  }
  summary.activeIntervalsMs = active;
  summary.frameIntervalP95Ms = nearestRankP95(active);
  for (double interval : active) {
    if (interval > LONG_FRAME_MS) {
      ++summary.longFrameCount;
    }
  }
  return summary;
}

// EUI post-presentation proxy: arm on compose with assistant text; mark on
// the first subsequent onFrame after present.
class PresentationClock {
 public:
  void onCompose(bool hasAssistantText, std::uint64_t /*composeNs*/) {
    if (hasAssistantText && !firstPresentedNs_ && !armed_) {
      armed_ = true;
    }
  }

  void onPresentedForTest(std::uint64_t /*presentNs*/) {
    // Present does not mark — only the next onFrame does.
    presentedSeen_ = true;
  }

  void onFrame(std::uint64_t frameNs) {
    if (armed_ && presentedSeen_ && !firstPresentedNs_) {
      firstPresentedNs_ = frameNs;
      armed_ = false;
    }
    if (firstPresentedNs_) {
      frameTimestamps_.push_back(frameNs);
    }
  }

  std::optional<std::uint64_t> firstPresentedNs() const {
    return firstPresentedNs_;
  }

  const std::vector<std::uint64_t>& frameTimestamps() const {
    return frameTimestamps_;
  }

 private:
  bool armed_ = false;
  bool presentedSeen_ = false;
  std::optional<std::uint64_t> firstPresentedNs_;
  std::vector<std::uint64_t> frameTimestamps_;
};

inline std::uint64_t ns(std::uint64_t value) { return value; }
