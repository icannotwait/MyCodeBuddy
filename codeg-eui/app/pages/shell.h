#pragma once

#include "model.h"

#include <algorithm>
#include <cstdint>
#include <string>

struct Rect {
  float x = 0;
  float y = 0;
  float width = 0;
  float height = 0;

  bool contains(float px, float py) const {
    return px >= x && py >= y && px < x + width && py < y + height;
  }
};

struct ShellLayout {
  Rect sidebar{};
  Rect header{};
  Rect content{};
  Rect composer{};
  Rect errorStrip{};

  static constexpr float kSidebarWidth = 248.0f;
  static constexpr float kHeaderHeight = 48.0f;
  static constexpr float kComposerHeight = 44.0f;
  static constexpr float kErrorStripHeight = 28.0f;
  static constexpr float kCardRadius = 8.0f;

  static ShellLayout calculate(float width, float height) {
    ShellLayout layout;
    layout.sidebar = {0, 0, kSidebarWidth, height};
    const float mainX = kSidebarWidth;
    const float mainW = std::max(0.0f, width - kSidebarWidth);
    layout.header = {mainX, 0, mainW, kHeaderHeight};
    layout.errorStrip = {mainX, kHeaderHeight, mainW, kErrorStripHeight};
    const float contentY = kHeaderHeight + kErrorStripHeight;
    const float contentH =
        std::max(0.0f, height - contentY - kComposerHeight);
    layout.content = {mainX, contentY, mainW, contentH};
    layout.composer = {mainX, height - kComposerHeight, mainW,
                       kComposerHeight};
    return layout;
  }

  bool overlaps() const {
    // Regions must not share interior area pairwise (edges may touch).
    auto overlap = [](const Rect& a, const Rect& b) {
      const float ax2 = a.x + a.width;
      const float ay2 = a.y + a.height;
      const float bx2 = b.x + b.width;
      const float by2 = b.y + b.height;
      return a.x < bx2 && ax2 > b.x && a.y < by2 && ay2 > b.y && a.width > 0 &&
             a.height > 0 && b.width > 0 && b.height > 0 &&
             !(ax2 == b.x || bx2 == a.x || ay2 == b.y || by2 == a.y);
    };
    // Sidebar may touch main columns on the edge (x equality) — allowed.
    // Header/content/composer stack vertically without interior overlap.
    if (overlap(header, content)) return true;
    if (overlap(header, composer)) return true;
    if (overlap(header, errorStrip)) return true;
    if (overlap(content, composer)) return true;
    if (overlap(errorStrip, content)) return true;
    if (overlap(errorStrip, composer)) return true;
    return false;
  }
};

class ShellPage {
 public:
  Route route() const { return route_; }
  void navigate(Route route) { route_ = route; }

  const ShellLayout& layout() const { return layout_; }

  void recompute(float width, float height) {
    layout_ = ShellLayout::calculate(width, height);
  }

  bool newSessionEnabled(const AppModel& model) const {
    return model.canCreateSession() &&
           !model.hasPending(Operation::CreateSession);
  }

  std::string statusLabel(const AppModel& model) const {
    if (model.snapshot.streamActive) {
      return "streaming";
    }
    if (!model.errorStrip.empty()) {
      return "error";
    }
    if (!model.currentConnectionId.empty()) {
      return "ready";
    }
    return "idle";
  }

 private:
  Route route_ = Route::Chat;
  ShellLayout layout_{};
};
