import { describe, expect, it } from "vitest"

import {
  coalescePreview,
  resyncAfterReconnect,
  applyRoundtablePreview,
} from "@/lib/roundtable/stream"
import { initialRoundtableView } from "@/lib/roundtable/reducer"
import type { RoundtableProjection } from "@/lib/roundtable/types"

describe("roundtable stream", () => {
  it("binds previews to the live attempt and never revives accepted text", () => {
    const projection = {
      body: {
        room_id: "room",
        run_epoch: "3",
        status: "running",
        phase_refs: [{ phase_id: "phase", revision: "2", state: "running" }],
        replay: {
          turns: [
            { turn_id: "turn", phase_id: "phase", speaker_id: "speaker" },
          ],
          attempts: [
            { attempt_id: "attempt", turn_id: "turn", state: "streaming" },
          ],
        },
      },
    } as RoundtableProjection
    const frame = {
      subscription_id: "sub",
      room_id: "room",
      run_epoch: "3",
      phase_revision: "2",
      speaker_id: "speaker",
      attempt_id: "attempt",
      incarnation: "current",
      chunk_seq: "1",
      text: "a",
    }
    const first = applyRoundtablePreview(
      initialRoundtableView,
      frame,
      projection,
      "sub"
    )
    expect(first.preview).toBe("a")
    expect(
      applyRoundtablePreview(
        first,
        { ...frame, chunk_seq: "2", text: "b" },
        projection,
        "sub"
      ).preview
    ).toBe("ab")
    expect(
      applyRoundtablePreview(
        first,
        { ...frame, reset_baseline_seq: "0", incarnation: "old" },
        projection,
        "sub"
      )
    ).toBe(first)
    expect(
      applyRoundtablePreview(
        first,
        { ...frame, room_id: "foreign" },
        projection,
        "sub"
      )
    ).toBe(first)
    projection.body.replay.attempts[0].state = "accepted"
    expect(
      applyRoundtablePreview(initialRoundtableView, frame, projection, "sub")
        .preview
    ).toBeNull()
  })
  it("coalesces the open window and drops an old incarnation", () => {
    const window = coalescePreview(
      [
        { incarnation: 2, seq: 1 },
        { incarnation: 1, seq: 9 },
        { incarnation: 2, seq: 4 },
      ],
      2
    )
    expect(window).toEqual({ first: 1, last: 4 })
    const restored = resyncAfterReconnect(["durable"], 9, "paused")
    expect(restored.preview).toBeNull()
    expect(restored.lastSeq).toBe(9)
    expect(restored.status).toBe("paused")
  })
})
