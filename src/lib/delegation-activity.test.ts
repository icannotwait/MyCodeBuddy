import { describe, expect, it } from "vitest"

import {
  projectCodegDelegationActivity,
  projectNativeDelegationActivity,
  type CodegDelegationActivityEvent,
  type NativeDelegationSignal,
} from "./delegation-activity"

function codegStartedEvent(): CodegDelegationActivityEvent {
  return {
    type: "delegation_started",
    agent_type: "codex",
    task_id: "broker-task-1",
    parent_tool_use_id: "pt-1",
    at: "2026-07-16T10:00:00Z",
  }
}

function nativeSpawnSignal(): NativeDelegationSignal {
  return {
    platform: "codex",
    toolName: "spawn_agent",
    toolCallId: "call-1",
    input: JSON.stringify({ agent_type: "worker", message: "do work" }),
    output: JSON.stringify({ agent_id: "agent-abc" }),
    at: "2026-07-16T10:00:00Z",
  }
}

describe("projectNativeDelegationActivity", () => {
  it.each([
    ["codex", "spawn_agent", "spawn"],
    ["codex", "wait_agent", "wait"],
    ["codex", "list_agents", "status"],
    ["codex", "interrupt_agent", "cancel"],
    ["grok", "spawn_subagent", "spawn"],
    ["grok", "get", "status"],
    ["grok", "wait", "wait"],
    ["grok", "kill", "cancel"],
    ["code_buddy", "Agent", "spawn"],
    ["code_buddy", "Task", "spawn"],
    ["claude_code", "Agent", "spawn"],
    ["claude_code", "TaskOutput", "wait"],
    ["claude_code", "TaskStop", "cancel"],
  ] as const)("maps %s %s to %s", (platform, toolName, operation) => {
    const view = projectNativeDelegationActivity({
      platform,
      toolName,
      toolCallId: "call-1",
      input: null,
      output: null,
      at: "2026-07-16T10:00:00Z",
    })
    expect(view).toMatchObject({
      origin: "native",
      authoritative: false,
      platform,
      operation,
    })
  })

  it("keeps missing ids and wait timeouts unknown instead of inventing failure", () => {
    const view = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "wait_agent",
      toolCallId: "call-2",
      input: JSON.stringify({ timeout_ms: 30_000 }),
      output: JSON.stringify({ timed_out: true }),
      at: "2026-07-16T10:00:30Z",
    })
    expect(view?.task_id).toBeUndefined()
    expect(view?.observed_status).toBe("unknown")
  })

  it("marks Codeg views authoritative but native views expose no broker action", () => {
    const codeg = projectCodegDelegationActivity(codegStartedEvent())
    const native = projectNativeDelegationActivity(nativeSpawnSignal())
    expect(codeg.authoritative).toBe(true)
    expect(native?.authoritative).toBe(false)
    expect(native).not.toHaveProperty("cancel")
    expect(native).not.toHaveProperty("brokerTaskId")
  })

  it("extracts documented agent_id / task_id fields only", () => {
    const withAgentId = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "spawn_agent",
      toolCallId: "c1",
      input: null,
      output: JSON.stringify({ agent_id: "ag-1", other_id: "ignore-me" }),
      at: "2026-07-16T10:00:00Z",
    })
    expect(withAgentId?.task_id).toBe("ag-1")

    const withTaskId = projectNativeDelegationActivity({
      platform: "claude_code",
      toolName: "TaskOutput",
      toolCallId: "c2",
      input: JSON.stringify({ task_id: "task-xyz", block: true }),
      output: null,
      at: "2026-07-16T10:00:00Z",
    })
    expect(withTaskId?.task_id).toBe("task-xyz")

    const withCamel = projectNativeDelegationActivity({
      platform: "grok",
      toolName: "spawn_subagent",
      toolCallId: "c3",
      input: null,
      output: JSON.stringify({ agentId: "camel-1" }),
      at: "2026-07-16T10:00:00Z",
    })
    expect(withCamel?.task_id).toBe("camel-1")
  })

  it("does not treat a tool-call error as child-task failure", () => {
    const view = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "spawn_agent",
      toolCallId: "err-1",
      input: JSON.stringify({ message: "x" }),
      output: "tool failed",
      toolCallStatus: "failed",
      at: "2026-07-16T10:00:00Z",
    })
    expect(view?.observed_status).not.toBe("failed")
    expect(view?.observed_status).toBe("unknown")
  })

  it("returns null for unmapped tools while callers keep the original tool", () => {
    const view = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "bash",
      toolCallId: "b1",
      input: "{}",
      output: null,
      at: "2026-07-16T10:00:00Z",
    })
    expect(view).toBeNull()
  })

  it("projects CodeBuddy background notifications via explicit signal variant", () => {
    const view = projectNativeDelegationActivity({
      kind: "codebuddy_background",
      platform: "code_buddy",
      taskId: "cb-bg-1",
      status: "completed",
      at: "2026-07-16T10:05:00Z",
    })
    expect(view).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "code_buddy",
      task_id: "cb-bg-1",
      observed_status: "completed",
    })
  })

  it("projects Claude raw SDK task messages via explicit signal variant", () => {
    const view = projectNativeDelegationActivity({
      kind: "claude_sdk_task",
      platform: "claude_code",
      taskId: "sdk-task-9",
      status: "running",
      operation: "wait",
      at: "2026-07-16T10:06:00Z",
    })
    expect(view).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "claude_code",
      task_id: "sdk-task-9",
      operation: "wait",
      observed_status: "running",
    })
  })

  it("merges previous view without inventing ids", () => {
    const first = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "spawn_agent",
      toolCallId: "m1",
      input: null,
      output: JSON.stringify({ agent_id: "kept-id" }),
      at: "2026-07-16T10:00:00Z",
    })
    const next = projectNativeDelegationActivity(
      {
        platform: "codex",
        toolName: "wait_agent",
        toolCallId: "m2",
        input: JSON.stringify({ agent_id: "kept-id" }),
        output: JSON.stringify({ timed_out: true }),
        at: "2026-07-16T10:01:00Z",
      },
      first ?? undefined
    )
    expect(next?.task_id).toBe("kept-id")
    expect(next?.observed_status).toBe("unknown")
    expect(next?.started_at).toBe("2026-07-16T10:00:00Z")
    expect(next?.updated_at).toBe("2026-07-16T10:01:00Z")
  })
})

describe("projectCodegDelegationActivity", () => {
  it("projects started as authoritative running spawn", () => {
    const view = projectCodegDelegationActivity(codegStartedEvent())
    expect(view).toMatchObject({
      origin: "codeg",
      authoritative: true,
      platform: "codex",
      task_id: "broker-task-1",
      operation: "spawn",
      observed_status: "running",
    })
  })

  it("projects completed terminal statuses from result", () => {
    const completed = projectCodegDelegationActivity({
      type: "delegation_completed",
      agent_type: "grok",
      task_id: "t-ok",
      status: "completed",
      at: "2026-07-16T10:10:00Z",
    })
    expect(completed.observed_status).toBe("completed")
    expect(completed.finished_at).toBe("2026-07-16T10:10:00Z")

    const failed = projectCodegDelegationActivity({
      type: "delegation_completed",
      agent_type: "grok",
      task_id: "t-fail",
      status: "failed",
      at: "2026-07-16T10:11:00Z",
    })
    expect(failed.observed_status).toBe("failed")
  })
})
