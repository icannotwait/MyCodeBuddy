import { describe, expect, it } from "vitest"

import {
  classifyToolKind,
  isAgentLikeToolName,
  TOOL_KIND_ORDER,
  type ToolKindLabel,
} from "./tool-kind-classifier"

describe("classifyToolKind", () => {
  it.each([
    ["grep", "search"],
    ["GLOB", "search"],
    ["list_files", "search"],
    ["bash", "command"],
    ["execute_command", "command"],
    ["read", "read"],
    ["Read File", "read"],
    ["view", "read"],
    ["edit", "edit"],
    ["NotebookEdit", "edit"],
    ["replace_in_file", "edit"],
    ["webfetch", "fetch"],
    ["browser_action", "fetch"],
    ["think", "think"],
    ["EnterPlanMode", "think"],
    ["TodoWrite", "todo"],
    ["update_todo_list", "todo"],
    ["task", "task"],
    ["new_task", "task"],
    ["skill", "task"],
    ["memory_recall", "memory"],
    ["MEMORY_RECALL", "memory"],
    ["my_custom_tool", "other"],
    ["", "other"],
  ] as const)("classifies %s as %s", (name, expected) => {
    expect(classifyToolKind(name)).toBe(expected as ToolKindLabel)
  })

  it("normalizes whitespace and case", () => {
    expect(classifyToolKind("  Grep  ")).toBe("search")
    expect(classifyToolKind("BASH")).toBe("command")
  })
})

describe("isAgentLikeToolName", () => {
  it("matches the agent tool exactly", () => {
    expect(isAgentLikeToolName("agent")).toBe(true)
    expect(isAgentLikeToolName("AGENT")).toBe(true)
    expect(isAgentLikeToolName("  agent ")).toBe(true)
  })

  it("matches delegate_to_agent across host naming conventions", () => {
    expect(isAgentLikeToolName("delegate_to_agent")).toBe(true)
    // Claude Code style
    expect(isAgentLikeToolName("mcp__codeg-delegate__delegate_to_agent")).toBe(
      true
    )
    expect(isAgentLikeToolName("mcp__codeg__delegate_to_agent")).toBe(true)
    // Codex live ACP style (server/tool)
    expect(isAgentLikeToolName("codeg-delegate/delegate_to_agent")).toBe(true)
    // Dot- and colon-separated forms other hosts may emit
    expect(isAgentLikeToolName("codeg-delegate.delegate_to_agent")).toBe(true)
    expect(isAgentLikeToolName("codeg-delegate:delegate_to_agent")).toBe(true)
  })

  it("does not match other tools", () => {
    expect(isAgentLikeToolName("task")).toBe(false)
    expect(isAgentLikeToolName("subagent")).toBe(false)
    expect(isAgentLikeToolName("")).toBe(false)
    // No separator before the suffix — must not match.
    expect(isAgentLikeToolName("xdelegate_to_agent")).toBe(false)
  })
})

describe("TOOL_KIND_ORDER", () => {
  it("includes every label exactly once", () => {
    const expected: ToolKindLabel[] = [
      "search",
      "command",
      "read",
      "memory",
      "edit",
      "fetch",
      "think",
      "todo",
      "task",
      "other",
    ]
    expect([...TOOL_KIND_ORDER].sort()).toEqual([...expected].sort())
    expect(new Set(TOOL_KIND_ORDER).size).toBe(TOOL_KIND_ORDER.length)
  })
})
