import { describe, expect, it } from "vitest"

import type { EventEnvelope } from "@/lib/types"

import {
  shouldClearTerminalDisconnectLatch,
  shouldLatchTerminalDisconnect,
  type TerminalDisconnectLatch,
} from "./terminal-reconnect"

const CONN = "conn-1"
const OTHER = "conn-2"
const BASELINE = "2026-07-22T01:00:00.000Z"

function errorEvent(connectionId: string, terminal: boolean): EventEnvelope {
  return {
    seq: 1,
    connection_id: connectionId,
    type: "error",
    message: "agent died",
    agent_type: "claude",
    code: "process_exited",
    terminal,
  }
}

function statusEvent(
  connectionId: string,
  status: "disconnected" | "connected" | "connecting"
): EventEnvelope {
  return {
    seq: 2,
    connection_id: connectionId,
    type: "status_changed",
    status,
  }
}

function summary(
  status: string,
  updatedAt: string
): { status: string; updated_at: string } {
  return { status, updated_at: updatedAt }
}

describe("shouldLatchTerminalDisconnect", () => {
  it("latches terminal error for same connection while root is in_progress", () => {
    expect(
      shouldLatchTerminalDisconnect(
        errorEvent(CONN, true),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(true)
  })

  it("latches bare disconnected status for same connection while in_progress", () => {
    expect(
      shouldLatchTerminalDisconnect(
        statusEvent(CONN, "disconnected"),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(true)
  })

  it("does not latch recoverable (non-terminal) error", () => {
    expect(
      shouldLatchTerminalDisconnect(
        errorEvent(CONN, false),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
  })

  it("does not latch when connection id does not match", () => {
    expect(
      shouldLatchTerminalDisconnect(
        errorEvent(OTHER, true),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
    expect(
      shouldLatchTerminalDisconnect(
        statusEvent(OTHER, "disconnected"),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
  })

  it("does not latch when connectionId is null", () => {
    expect(
      shouldLatchTerminalDisconnect(
        errorEvent(CONN, true),
        null,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
  })

  it("does not latch when summary is not in_progress", () => {
    for (const status of [
      "cancelled",
      "pending",
      "completed",
      "pending_review",
    ]) {
      expect(
        shouldLatchTerminalDisconnect(
          errorEvent(CONN, true),
          CONN,
          summary(status, BASELINE)
        )
      ).toBe(false)
      expect(
        shouldLatchTerminalDisconnect(
          statusEvent(CONN, "disconnected"),
          CONN,
          summary(status, BASELINE)
        )
      ).toBe(false)
    }
  })

  it("does not latch when summary is null", () => {
    expect(
      shouldLatchTerminalDisconnect(errorEvent(CONN, true), CONN, null)
    ).toBe(false)
  })

  it("does not latch non-disconnect status_changed events", () => {
    expect(
      shouldLatchTerminalDisconnect(
        statusEvent(CONN, "connected"),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
  })
})

describe("shouldClearTerminalDisconnectLatch", () => {
  const latch: TerminalDisconnectLatch = { baselineUpdatedAt: BASELINE }

  it("follows the full ordering sequence for clear vs hold", () => {
    // Unchanged stale in_progress (same updated_at) must not clear.
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)

    // Newer cancelled must not clear.
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("cancelled", "2026-07-22T02:00:00.000Z")
      )
    ).toBe(false)

    // Newer non-cancelled statuses clear.
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("in_progress", "2026-07-22T02:00:00.000Z")
      )
    ).toBe(true)
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("pending_review", "2026-07-22T02:00:00.000Z")
      )
    ).toBe(true)
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("completed", "2026-07-22T02:00:00.000Z")
      )
    ).toBe(true)
  })

  it("does not clear when latch or summary is null", () => {
    expect(
      shouldClearTerminalDisconnectLatch(
        null,
        summary("in_progress", "2026-07-22T02:00:00.000Z")
      )
    ).toBe(false)
    expect(shouldClearTerminalDisconnectLatch(latch, null)).toBe(false)
  })

  it("does not clear when updated_at is older than baseline", () => {
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("completed", "2026-07-22T00:00:00.000Z")
      )
    ).toBe(false)
  })
})
