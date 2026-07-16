import { describe, expect, it } from "vitest"

import { BACKGROUND_TASK_MARKER } from "./background-agent"
import {
  collectExplicitNativeSignalsFromToolFields,
  deriveNativeActivitiesFromToolCalls,
  dedupeDelegationActivities,
  extractCodeBuddySubAgentSessionId,
  projectCodegDelegationActivity,
  projectNativeDelegationActivity,
  signalFromClaudeBackgroundLifecycle,
  signalFromCodeBuddyBackgroundBoundary,
  type CodegDelegationActivityEvent,
  type NativeDelegationSignal,
} from "./delegation-activity"
import { parseBackgroundTaskMarker } from "./background-agent"

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

  it("clears finished_at when wait timeout downgrades status to unknown", () => {
    const completed = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "spawn_agent",
      toolCallId: "m1",
      input: null,
      output: JSON.stringify({
        agent_id: "kept-id",
        status: "completed",
      }),
      at: "2026-07-16T10:00:00Z",
    })
    expect(completed?.finished_at).toBe("2026-07-16T10:00:00Z")
    const timedOut = projectNativeDelegationActivity(
      {
        platform: "codex",
        toolName: "wait_agent",
        toolCallId: "m2",
        input: JSON.stringify({ agent_id: "kept-id" }),
        output: JSON.stringify({ timed_out: true }),
        at: "2026-07-16T10:01:00Z",
      },
      completed ?? undefined
    )
    expect(timedOut?.observed_status).toBe("unknown")
    expect(timedOut?.finished_at).toBeUndefined()
  })

  it("does not project Grok short names under a non-Grok platform hint", () => {
    for (const toolName of ["get", "wait", "kill"] as const) {
      const view = projectNativeDelegationActivity({
        platform: "codex",
        toolName,
        toolCallId: "g1",
        input: null,
        output: null,
        at: "2026-07-16T10:00:00Z",
      })
      expect(view).toBeNull()
    }
  })

  it("does not re-label foreign tools via normalize aliases under wrong hint", () => {
    // wait_agent must stay Codex wait — never spawn via normalize wait_agent→task.
    const view = projectNativeDelegationActivity({
      platform: "claude_code",
      toolName: "wait_agent",
      toolCallId: "alias-1",
      input: null,
      output: null,
      at: "2026-07-16T10:00:00Z",
    })
    expect(view).toBeNull()
  })

  it("tolerates malformed JSON and array bodies without inventing status", () => {
    const malformed = projectNativeDelegationActivity({
      platform: "codex",
      toolName: "spawn_agent",
      toolCallId: "bad-1",
      input: "{not-json",
      output: "[1,2,3]",
      at: "2026-07-16T10:00:00Z",
    })
    expect(malformed?.task_id).toBeUndefined()
    expect(malformed?.observed_status).toBe("unknown")
    expect(malformed?.operation).toBe("spawn")
  })
})

describe("explicit signal producers (production envelopes)", () => {
  it("builds claude_sdk_task from parser background-task marker", () => {
    const output = `${BACKGROUND_TASK_MARKER}${JSON.stringify({
      task_id: "abc123",
      status: "completed",
      summary: "Agent finished",
      result: "Build OK",
    })}`
    const lifecycle = parseBackgroundTaskMarker(output)
    expect(lifecycle).not.toBeNull()
    const signal = signalFromClaudeBackgroundLifecycle(
      lifecycle!,
      "2026-07-16T10:05:00Z"
    )
    expect(signal).toMatchObject({
      kind: "claude_sdk_task",
      platform: "claude_code",
      taskId: "abc123",
      status: "completed",
    })
    const view = projectNativeDelegationActivity(signal)
    expect(view).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "claude_code",
      task_id: "abc123",
      observed_status: "completed",
    })
  })

  it("builds codebuddy_background from structured subAgent.sessionId output", () => {
    const output = JSON.stringify({
      providerData: {
        toolResult: {
          content: "Build succeeded",
          subAgent: { sessionId: "agent-cdd7c1ea" },
        },
      },
    })
    expect(extractCodeBuddySubAgentSessionId(output)).toBe("agent-cdd7c1ea")
    const signal = signalFromCodeBuddyBackgroundBoundary({
      taskId: extractCodeBuddySubAgentSessionId(output),
      status: "completed",
      at: "2026-07-16T10:06:00Z",
    })
    expect(signal).toMatchObject({
      kind: "codebuddy_background",
      platform: "code_buddy",
      taskId: "agent-cdd7c1ea",
    })
    const view = projectNativeDelegationActivity(signal!)
    expect(view).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "code_buddy",
      task_id: "agent-cdd7c1ea",
      observed_status: "completed",
    })
  })

  it("collects explicit signals from tool fields and merges with tool-calls", () => {
    const tools = [
      {
        toolCallId: "agent-1",
        toolName: "Agent",
        input: JSON.stringify({
          subagent_type: "Explore",
          description: "scan",
        }),
        output: `${BACKGROUND_TASK_MARKER}${JSON.stringify({
          task_id: "sdk-task-1",
          status: "completed",
          summary: "done",
          result: "ok",
        })}`,
        status: "completed",
        at: "2026-07-16T10:00:00Z",
      },
    ]
    const explicit = collectExplicitNativeSignalsFromToolFields(tools)
    expect(explicit).toHaveLength(1)
    expect(explicit[0]).toMatchObject({
      kind: "claude_sdk_task",
      taskId: "sdk-task-1",
    })

    const views = deriveNativeActivitiesFromToolCalls(tools, "claude_code")
    // Agent spawn + explicit marker for same task merge conservatively.
    expect(views.length).toBeGreaterThanOrEqual(1)
    expect(views.some((v) => v.task_id === "sdk-task-1")).toBe(true)
    expect(views.every((v) => v.authoritative === false)).toBe(true)
  })

  it("dedupes store and live activity lists deterministically", () => {
    const store = [
      {
        origin: "native" as const,
        authoritative: false,
        platform: "codex" as const,
        task_id: "a1",
        operation: "spawn" as const,
        observed_status: "running" as const,
        started_at: "2026-07-16T10:00:00Z",
        updated_at: "2026-07-16T10:00:00Z",
      },
    ]
    const live = [
      {
        origin: "native" as const,
        authoritative: false,
        platform: "codex" as const,
        task_id: "a1",
        operation: "wait" as const,
        observed_status: "completed" as const,
        started_at: "2026-07-16T10:00:00Z",
        updated_at: "2026-07-16T10:01:00Z",
        finished_at: "2026-07-16T10:01:00Z",
      },
    ]
    const merged = dedupeDelegationActivities(store, live)
    expect(merged).toHaveLength(1)
    expect(merged[0].observed_status).toBe("completed")
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
