import { describe, expect, it } from "vitest"
import { formatRelativeTime, mapFileStatus, parseDate } from "./git-log-format"
import type { GitLogTimeKey } from "./git-log-format"

const t = (key: GitLogTimeKey, values?: { count: number }) =>
  `${key}:${values?.count ?? 0}`

describe("formatRelativeTime", () => {
  it("returns the raw string for an unparseable date", () => {
    expect(formatRelativeTime("not-a-date", t)).toBe("not-a-date")
  })

  it("buckets recent timestamps into just-now / minutes / hours / days", () => {
    const now = Date.now()
    expect(formatRelativeTime(new Date(now - 10_000).toISOString(), t)).toBe(
      "time.justNow:0"
    )
    expect(
      formatRelativeTime(new Date(now - 5 * 60_000).toISOString(), t)
    ).toBe("time.minsAgo:5")
    expect(
      formatRelativeTime(new Date(now - 3 * 3_600_000).toISOString(), t)
    ).toBe("time.hoursAgo:3")
    expect(
      formatRelativeTime(new Date(now - 2 * 86_400_000).toISOString(), t)
    ).toBe("time.daysAgo:2")
  })

  it("collapses timestamps older than a month into months", () => {
    const old = new Date(Date.now() - 65 * 86_400_000).toISOString()
    expect(formatRelativeTime(old, t)).toBe("time.monthsAgo:2")
  })
})

describe("parseDate", () => {
  it("parses valid dates and rejects invalid ones", () => {
    expect(parseDate("2024-01-01T00:00:00Z")).toBeInstanceOf(Date)
    expect(parseDate("nope")).toBeNull()
  })
})

describe("mapFileStatus", () => {
  it("maps git status letters to change kinds", () => {
    expect(mapFileStatus("A")).toBe("added")
    expect(mapFileStatus("D")).toBe("deleted")
    expect(mapFileStatus("R100")).toBe("renamed")
    expect(mapFileStatus("M")).toBe("modified")
    expect(mapFileStatus("x")).toBe("modified")
  })
})
