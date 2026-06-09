import { describe, expect, it } from "vitest"

import {
  isPlanLikeToolName,
  normalizePriority,
  normalizeStatus,
  parseTodosFromJson,
} from "./plan-parse"

describe("normalizeStatus", () => {
  it("maps common synonyms to the canonical status", () => {
    expect(normalizeStatus("completed")).toBe("completed")
    expect(normalizeStatus("done")).toBe("completed")
    expect(normalizeStatus("in_progress")).toBe("in_progress")
    expect(normalizeStatus("in-progress")).toBe("in_progress")
    expect(normalizeStatus("running")).toBe("in_progress")
    expect(normalizeStatus("active")).toBe("in_progress")
    expect(normalizeStatus("pending")).toBe("pending")
    expect(normalizeStatus("whatever")).toBe("pending")
    expect(normalizeStatus(undefined)).toBe("pending")
  })
})

describe("normalizePriority", () => {
  it("maps to high/medium/low with medium as default", () => {
    expect(normalizePriority("high")).toBe("high")
    expect(normalizePriority("urgent")).toBe("high")
    expect(normalizePriority("low")).toBe("low")
    expect(normalizePriority("medium")).toBe("medium")
    expect(normalizePriority("nonsense")).toBe("medium")
    expect(normalizePriority(undefined)).toBe("medium")
  })
})

describe("isPlanLikeToolName", () => {
  it("recognizes TodoWrite (any casing/separator) and plan-named tools", () => {
    expect(isPlanLikeToolName("TodoWrite")).toBe(true)
    expect(isPlanLikeToolName("todo_write")).toBe(true)
    expect(isPlanLikeToolName("update_plan")).toBe(true)
    expect(isPlanLikeToolName("exit_plan_mode")).toBe(true)
  })

  it("returns false for unrelated tools", () => {
    expect(isPlanLikeToolName("Bash")).toBe(false)
    expect(isPlanLikeToolName("read_file")).toBe(false)
  })
})

describe("parseTodosFromJson", () => {
  it("parses the `todos` array shape", () => {
    const input = JSON.stringify({
      todos: [
        { content: "Build the thing", status: "in_progress", priority: "high" },
        { content: "Ship it", status: "pending", priority: "low" },
      ],
    })
    expect(parseTodosFromJson(input)).toEqual([
      { content: "Build the thing", status: "in_progress", priority: "high" },
      { content: "Ship it", status: "pending", priority: "low" },
    ])
  })

  it("supports `entries` and `plan` array shapes", () => {
    expect(
      parseTodosFromJson(JSON.stringify({ entries: [{ content: "A" }] }))
    ).toEqual([{ content: "A", status: "pending", priority: "medium" }])
    expect(
      parseTodosFromJson(
        JSON.stringify({ plan: [{ step: "B", status: "done" }] })
      )
    ).toEqual([{ content: "B", status: "completed", priority: "medium" }])
  })

  it("derives content from step/title/name fallbacks and skips empty rows", () => {
    const input = JSON.stringify({
      todos: [{ title: "Titled" }, { name: "Named" }, { status: "pending" }],
    })
    expect(parseTodosFromJson(input)).toEqual([
      { content: "Titled", status: "pending", priority: "medium" },
      { content: "Named", status: "pending", priority: "medium" },
    ])
  })

  it("returns [] for invalid JSON or non-plan payloads", () => {
    expect(parseTodosFromJson("not json")).toEqual([])
    expect(parseTodosFromJson(JSON.stringify({ other: 1 }))).toEqual([])
    expect(parseTodosFromJson("")).toEqual([])
  })
})
