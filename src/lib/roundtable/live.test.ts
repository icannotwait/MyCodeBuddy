import { describe, expect, it } from "vitest"
import {
  applyRoundtableLive,
  liveTranscriptInputs,
  LIVE_THOUGHT_CAP_BYTES,
  type RoundtableLiveState,
} from "@/lib/roundtable/live"
import type { RoundtableProjection } from "@/lib/roundtable/types"

function frame(
  attempts: Record<string, unknown>[],
  removed: string[] = [],
  room = "room"
) {
  return {
    type: "roundtable_live",
    room_id: room,
    verified: false,
    attempts,
    removed,
  }
}

function item(
  id: string,
  message: [number, string],
  thought: [number, string] = [0, ""],
  extra: Record<string, unknown> = {}
) {
  return {
    attempt_id: id,
    speaker_id: `speaker-${id}`,
    phase_id: "phase",
    phase_kind: "proposal",
    incarnation: "inc-1",
    ended: false,
    activity: null,
    message: { offset: message[0], text: message[1], truncated: false },
    thought: { offset: thought[0], text: thought[1], start: thought[0] },
    ...extra,
  }
}

function projection(
  status: string,
  attempts: { attempt_id: string; state: string }[]
): RoundtableProjection {
  return {
    projection_ref: { id: "p", hash: "h" },
    body: {
      status,
      replay: {
        attempts: attempts.map((attempt) => ({ ...attempt, turn_id: "t" })),
      },
    },
  } as unknown as RoundtableProjection
}

describe("roundtable live output", () => {
  it("applies whole buffers then byte-offset deltas for concurrent members", () => {
    let state: RoundtableLiveState = {}
    let result = applyRoundtableLive(
      state,
      frame([item("a", [0, "Héllo"], [0, "think"]), item("b", [0, "Hi"])]),
      "room"
    )
    expect(result.resync).toBe(false)
    state = result.state
    // "Héllo" is 6 UTF-8 bytes.
    result = applyRoundtableLive(
      state,
      frame([item("a", [6, " world"], [5, "ing"]), item("b", [2, " there"])]),
      "room"
    )
    expect(result.resync).toBe(false)
    expect(result.state.a.message).toBe("Héllo world")
    expect(result.state.a.thought).toBe("thinking")
    expect(result.state.b.message).toBe("Hi there")
  })

  it("asks for a resync when a delta does not line up and ignores other rooms", () => {
    const state = applyRoundtableLive(
      {},
      frame([item("a", [0, "abc"])]),
      "room"
    ).state
    const gap = applyRoundtableLive(state, frame([item("a", [9, "x"])]), "room")
    expect(gap.resync).toBe(true)
    expect(gap.state.a.message).toBe("abc")
    const other = applyRoundtableLive(
      state,
      frame([item("a", [0, "nope"])], [], "other"),
      "room"
    )
    expect(other.state).toBe(state)
    // A delta for an unknown attempt (e.g. after a reconnect reset) resyncs.
    expect(
      applyRoundtableLive({}, frame([item("z", [4, "late"])]), "room").resync
    ).toBe(true)
  })

  it("drops removed attempts and restarts on a new incarnation", () => {
    let state = applyRoundtableLive(
      {},
      frame([item("a", [0, "old"]), item("b", [0, "b"])]),
      "room"
    ).state
    state = applyRoundtableLive(
      state,
      frame([item("a", [0, "new"], [0, ""], { incarnation: "inc-2" })], ["b"]),
      "room"
    ).state
    expect(Object.keys(state)).toEqual(["a"])
    expect(state.a.message).toBe("new")
  })

  it("caps thinking to its tail and keeps the absolute offset", () => {
    const big = "x".repeat(LIVE_THOUGHT_CAP_BYTES)
    let state = applyRoundtableLive(
      {},
      frame([item("a", [0, ""], [0, big])]),
      "room"
    ).state
    state = applyRoundtableLive(
      state,
      frame([item("a", [0, ""], [LIVE_THOUGHT_CAP_BYTES, "yz"])]),
      "room"
    ).state
    expect(new TextEncoder().encode(state.a.thought).byteLength).toBe(
      LIVE_THOUGHT_CAP_BYTES
    )
    expect(state.a.thought.endsWith("yz")).toBe(true)
    expect(state.a.thoughtStart).toBe(2)
    expect(state.a.thoughtEnd).toBe(LIVE_THOUGHT_CAP_BYTES + 2)
  })

  it("only offers live text for projection attempts that are still live", () => {
    const state = applyRoundtableLive(
      {},
      frame([
        item("run", [0, "streaming"], [0, "hmm"], {
          activity: "Read file",
        }),
        item("dead", [0, "gone"]),
        item("ghost", [0, "unknown attempt"]),
      ]),
      "room"
    ).state
    const inputs = liveTranscriptInputs(
      state,
      projection("running", [
        { attempt_id: "run", state: "streaming" },
        { attempt_id: "dead", state: "failed" },
      ])
    )
    expect(inputs).toEqual([
      {
        attemptId: "run",
        text: "streaming",
        live: {
          thought: "hmm",
          thoughtOmitted: false,
          activity: "Read file",
          ended: false,
          truncated: false,
        },
      },
    ])
    expect(
      liveTranscriptInputs(
        state,
        projection("completed", [{ attempt_id: "run", state: "streaming" }])
      )
    ).toEqual([])
  })
})
