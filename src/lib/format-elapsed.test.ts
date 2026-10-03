import { describe, expect, it } from "vitest"
import {
  formatCompactElapsed,
  formatElapsedLabel,
  type ElapsedUnitTranslator,
} from "@/lib/format-elapsed"

// Minimal stand-in for the next-intl translator: mirrors the English
// `liveTurnStats.elapsed*` messages (`{value}h` / `{value}m` / `{value}s`).
const t: ElapsedUnitTranslator = (key, values) => {
  const unit =
    key === "elapsedHours" ? "h" : key === "elapsedMinutes" ? "m" : "s"
  return `${values.value}${unit}`
}

describe("formatElapsedLabel", () => {
  it("shows only seconds under a minute", () => {
    expect(formatElapsedLabel(0, t)).toBe("0s")
    expect(formatElapsedLabel(12_300, t)).toBe("12s")
    expect(formatElapsedLabel(59_999, t)).toBe("59s")
  })

  it("renders sub-second durations as 0s, matching the live tick", () => {
    expect(formatElapsedLabel(500, t)).toBe("0s")
  })

  it("shows minutes and seconds between one minute and one hour", () => {
    expect(formatElapsedLabel(60_000, t)).toBe("1m 0s")
    expect(formatElapsedLabel(90_000, t)).toBe("1m 30s")
    expect(formatElapsedLabel(3_599_000, t)).toBe("59m 59s")
  })

  it("shows hours, minutes, and seconds from one hour up", () => {
    expect(formatElapsedLabel(3_600_000, t)).toBe("1h 0m 0s")
    expect(formatElapsedLabel(3_661_000, t)).toBe("1h 1m 1s")
    expect(formatElapsedLabel(7_323_000, t)).toBe("2h 2m 3s")
  })

  it.each([NaN, Infinity, -Infinity])(
    "normalizes non-finite input %s to localized zero seconds",
    (duration) => {
      const localized: ElapsedUnitTranslator = (_key, { value }) =>
        `${value} 秒`
      expect(formatElapsedLabel(duration, localized)).toBe("0 秒")
    }
  )

  it("clamps negative input to zero", () => {
    expect(formatElapsedLabel(-100, t)).toBe("0s")
  })
})

describe("formatCompactElapsed", () => {
  it("floors seconds below one hour and omits zero seconds after minutes", () => {
    expect(formatCompactElapsed(500)).toBe("0s")
    expect(formatCompactElapsed(42_999)).toBe("42s")
    expect(formatCompactElapsed(60_000)).toBe("1m")
    expect(formatCompactElapsed(162_999)).toBe("2m42s")
    expect(formatCompactElapsed(3_599_999)).toBe("59m59s")
  })

  it("rounds total minutes up from one hour, including a carry", () => {
    expect(formatCompactElapsed(3_600_000)).toBe("1h")
    expect(formatCompactElapsed(5_641_000)).toBe("1h35m")
    expect(formatCompactElapsed(7_199_000)).toBe("2h")
  })

  it("normalizes negative and non-finite durations", () => {
    for (const duration of [-100, NaN, Infinity, -Infinity]) {
      expect(formatCompactElapsed(duration)).toBe("0s")
    }
  })
})
