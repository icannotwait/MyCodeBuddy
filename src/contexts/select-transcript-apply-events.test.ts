import { describe, expect, it } from "vitest"
import { selectTranscriptApplyEvents } from "@/contexts/acp-connections-context"
import type { EventEnvelope } from "@/lib/types"

function delta(seq: number, text: string): EventEnvelope {
  return {
    connection_id: "c1",
    seq,
    type: "content_delta",
    text,
  }
}

function turnComplete(seq: number): EventEnvelope {
  return {
    connection_id: "c1",
    seq,
    type: "turn_complete",
  }
}

function statusChanged(
  seq: number,
  status: "prompting" | "connected"
): EventEnvelope {
  return {
    connection_id: "c1",
    seq,
    type: "status_changed",
    status,
  }
}

describe("selectTranscriptApplyEvents", () => {
  it("drops content after turn_complete in the same frame", () => {
    const events = [delta(1, "hello"), turnComplete(2), delta(3, " leaked")]
    const projected = selectTranscriptApplyEvents(events, "prompting")
    expect(projected.map((e) => e.type)).toEqual([
      "content_delta",
      "turn_complete",
    ])
    expect(projected[0]).toMatchObject({ text: "hello" })
  })

  it("projects status_changed then content only while prompting", () => {
    const events = [
      statusChanged(1, "prompting"),
      delta(2, "a"),
      statusChanged(3, "connected"),
      delta(4, "b"),
    ]
    const projected = selectTranscriptApplyEvents(events, "connected")
    expect(projected.map((e) => e.type)).toEqual([
      "status_changed",
      "content_delta",
      "status_changed",
    ])
  })

  it("keeps turn_complete when starting out-of-turn", () => {
    const projected = selectTranscriptApplyEvents(
      [delta(1, "x"), turnComplete(2)],
      "connected"
    )
    expect(projected.map((e) => e.type)).toEqual(["turn_complete"])
  })
})
