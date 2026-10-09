import { describe, expect, it } from "vitest"
import type {
  RoundtableConfig,
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"
import {
  buildRoundtableTranscript,
  replaceSeatAliases,
} from "@/lib/roundtable/transcript"

function config(): RoundtableConfig {
  return {
    schema_version: 1,
    topic: "Question",
    workspace_id: "workspace",
    source_refs: [],
    participants: [
      {
        ordinal: 0,
        role: "reviewer",
        provider_ref: "provider:grok",
        model: "grok-4.6",
        agent: "grok",
      },
      {
        ordinal: 1,
        role: "critic",
        provider_ref: "provider:gemini",
        model: "gemini-3.8-flash-high",
        agent: "antigravity",
      },
    ],
    moderator_ordinal: 0,
    strategy: { type: "phased_rounds", version: 1, critique_rounds: 1 },
    concurrency: 2,
    strict_snapshot_v1: true,
    budgets: { room_budget: "1", phase_budget: "1" },
    timeouts: { attempt_timeout: "1" },
    quotas: {
      output_byte_limit: 1,
      input_byte_limit: 1,
      interjection_byte_limit: 1,
    },
  }
}

function projection(
  patch?: (body: RoundtableProjection["body"]) => void
): RoundtableProjection {
  const body: RoundtableProjection["body"] = {
    schema_version: 1,
    room_id: "room",
    revision: "1",
    run_epoch: "1",
    last_seq: "3",
    status: "completed",
    blocked_reason: null,
    messages: [],
    phase_refs: [
      {
        phase_id: "phase-proposal",
        revision: "1",
        state: "published",
        index: 0,
        kind: "proposal",
      },
      {
        phase_id: "phase-critique",
        revision: "1",
        state: "published",
        index: 1,
        kind: "critique",
      },
      {
        phase_id: "phase-synthesis",
        revision: "1",
        state: "published",
        index: 2,
        kind: "synthesis",
      },
    ],
    replay: {
      config: config(),
      speakers: [
        {
          speaker_id: "speaker-1",
          ordinal: 0,
          role: "member",
          provider_ref: "provider:grok",
          model_id: "grok-4.6",
        },
        {
          speaker_id: "speaker-2",
          ordinal: 1,
          role: "member",
          provider_ref: "provider:gemini",
          model_id: "gemini-3.8-flash-high",
        },
        {
          speaker_id: "speaker-mod",
          ordinal: 2,
          role: "moderator",
          provider_ref: "provider:grok",
          model_id: "grok-4.6",
        },
      ],
      attempts: [],
      turns: [],
      message_memberships: [],
      evidence: [],
      result_quality: null,
    },
  }
  patch?.(body)
  return { projection_ref: { id: "projection", hash: "hash" }, body }
}

function message(
  partial: Partial<RoundtableMessage> & Pick<RoundtableMessage, "message_id">
): RoundtableMessage {
  return {
    body_hash: "hash",
    visibility: "published",
    body: { summary: partial.message_id },
    ...partial,
  }
}

describe("roundtable transcript", () => {
  it("groups messages by phase and attributes each speaker", () => {
    const model = buildRoundtableTranscript(projection(), [
      message({
        message_id: "synthesis",
        speaker_id: "speaker-mod",
        phase_id: "phase-synthesis",
        attempt_state: "accepted",
        attempt_no: 1,
        finished_at: "2026-10-08T03:41:00.000Z",
        body: {
          kind: "synthesis",
          speaker_id: "speaker-mod",
          recommendation: {
            text: "Keep the prepaid wall",
            aliases: [{ kind: "claim", alias: "c0" }],
            inference: true,
          },
        },
      }),
      message({
        message_id: "critique",
        speaker_id: "speaker-2",
        phase_id: "phase-critique",
        visibility: "staged",
        body: {
          kind: "critique",
          summary: "The checkpoint can lag",
          claims: [
            {
              local_key: "c0",
              text: "SQLite commit latency",
              confidence: "high",
              evidence_aliases: ["file-a"],
            },
          ],
        },
      }),
      message({
        message_id: "proposal",
        speaker_id: "speaker-1",
        phase_id: "phase-proposal",
        body: { kind: "proposal", summary: "Fail fast on the elapsed slice" },
      }),
      message({
        message_id: "voided",
        visibility: "void",
        phase_id: "phase-proposal",
        body: { summary: "hidden" },
      }),
    ])

    expect(model.phases.map((phase) => phase.kind)).toEqual([
      "proposal",
      "critique",
      "synthesis",
    ])
    expect(model.phases[0].turns.map((turn) => turn.messageId)).toEqual([
      "proposal",
    ])
    expect(model.phases[0].turns[0]).toMatchObject({
      visibility: "published",
      speaker: {
        role: "member",
        ordinal: 0,
        seatOrdinal: 0,
        modelId: "grok-4.6",
        providerRef: "provider:grok",
        agent: "grok",
        participantRole: "reviewer",
      },
    })
    expect(model.phases[1].turns[0]).toMatchObject({
      visibility: "staged",
      speaker: {
        role: "member",
        ordinal: 1,
        seatOrdinal: 1,
        agent: "antigravity",
        modelId: "gemini-3.8-flash-high",
      },
    })
    expect(model.phases[1].turns[0].evidenceAliases).toEqual([
      { kind: "evidence", alias: "file-a" },
    ])
    expect(model.phases[2].turns[0]).toMatchObject({
      visibility: "published",
      speaker: {
        role: "moderator",
        seatOrdinal: 0,
        agent: "grok",
        modelId: "grok-4.6",
        participantRole: null,
      },
    })
    expect(model.seats).toMatchObject({
      s0: { role: "member", seatOrdinal: 0, modelId: "grok-4.6" },
      s1: { role: "member", seatOrdinal: 1, agent: "antigravity" },
      s2: { role: "moderator", seatOrdinal: 0, agent: "grok" },
    })
    expect(model.phases[2].turns[0].recommendation?.text).toBe(
      "Keep the prepaid wall"
    )
    expect(model.phases[2].turns[0].evidenceAliases).toEqual([
      { kind: "claim", alias: "c0" },
    ])
    expect(JSON.stringify(model)).not.toContain("hidden")
  })

  it("orders a phase by the highest membership published_seq", () => {
    const source = projection((body) => {
      body.replay.message_memberships = [
        {
          message_id: "later",
          membership_version: "1",
          visibility: "staged",
          published_seq: null,
        },
        {
          message_id: "later",
          membership_version: "2",
          visibility: "published",
          published_seq: "9",
        },
        {
          message_id: "earlier",
          membership_version: "1",
          visibility: "published",
          published_seq: "4",
        },
        {
          message_id: "earlier",
          membership_version: "2",
          visibility: "published",
          published_seq: "2",
        },
      ]
    })
    const model = buildRoundtableTranscript(source, [
      message({
        message_id: "later",
        speaker_id: "speaker-2",
        phase_id: "phase-proposal",
      }),
      message({
        message_id: "earlier",
        speaker_id: "speaker-1",
        phase_id: "phase-proposal",
      }),
    ])
    expect(model.phases[0].turns.map((turn) => turn.publishedSeq)).toEqual([
      "2",
      "9",
    ])
  })

  it("keeps a staged revision from inheriting an older published sequence", () => {
    const source = projection((body) => {
      body.replay.message_memberships = [
        {
          message_id: "message",
          membership_version: "1",
          visibility: "published",
          published_seq: "4",
        },
        {
          message_id: "message",
          membership_version: "2",
          visibility: "staged",
          published_seq: null,
        },
      ]
    })
    const model = buildRoundtableTranscript(source, [
      message({
        message_id: "message",
        visibility: "staged",
        phase_id: "phase-proposal",
        speaker_id: "speaker-1",
      }),
    ])
    expect(model.phases[0].turns[0]).toMatchObject({
      visibility: "staged",
      publishedSeq: null,
    })
  })

  it("falls back to result kind when the envelope has no phase id", () => {
    const model = buildRoundtableTranscript(projection(), [
      message({
        message_id: "synthesis",
        body: {
          kind: "synthesis",
          speaker_id: "speaker-mod",
          recommendation: { text: "Ship the wall" },
        },
      }),
      message({
        message_id: "proposal",
        body: { kind: "proposal", summary: "Fail fast" },
      }),
    ])
    expect(model.phases.map((phase) => phase.kind)).toEqual([
      "proposal",
      "synthesis",
    ])
    expect(model.phases[0].turns[0].speaker.role).toBe("unknown")
    expect(model.phases[1].turns[0].speaker.role).toBe("moderator")
  })

  it("numbers critique rounds and keeps a live preview out of published", () => {
    const source = projection((body) => {
      body.phase_refs = [
        {
          phase_id: "phase-critique-1",
          revision: "1",
          state: "published",
          index: 1,
          kind: "critique",
        },
        {
          phase_id: "phase-critique-2",
          revision: "1",
          state: "running",
          index: 2,
          kind: "critique",
        },
      ]
      body.replay.attempts = [
        {
          attempt_id: "attempt-live",
          state: "streaming",
          turn_id: "turn-live",
          attempt_no: 2,
        },
        {
          attempt_id: "attempt-done",
          state: "accepted",
          turn_id: "turn-done",
          attempt_no: 1,
        },
      ]
      body.replay.turns = [
        {
          turn_id: "turn-done",
          phase_id: "phase-critique-1",
          speaker_id: "speaker-1",
          accepted_attempt_id: "attempt-done",
        },
        {
          turn_id: "turn-live",
          phase_id: "phase-critique-2",
          speaker_id: "speaker-2",
          accepted_attempt_id: null,
        },
      ]
    })
    const model = buildRoundtableTranscript(
      source,
      [
        message({
          message_id: "done",
          attempt_id: "attempt-done",
          body: { kind: "critique", summary: "First critique" },
        }),
      ],
      [
        { attemptId: "attempt-done", text: "stale preview" },
        { attemptId: "attempt-live", text: "still writing" },
      ]
    )
    expect(model.critiqueRoundCount).toBe(2)
    expect(model.phases.map((phase) => phase.critiqueRound)).toEqual([1, 2])
    expect(model.phases[0].turns.map((turn) => turn.visibility)).toEqual([
      "published",
    ])
    expect(model.phases[1].turns[0]).toMatchObject({
      visibility: "preview",
      summary: "still writing",
      attemptNo: 2,
      speaker: { ordinal: 1, agent: "antigravity" },
    })
    expect(JSON.stringify(model)).not.toContain("stale preview")
  })
})

describe("live preview turns", () => {
  it("carries live output until a verified message claims the attempt", () => {
    const source = projection((body) => {
      body.status = "running"
      body.phase_refs = [
        {
          phase_id: "phase-proposal",
          revision: "1",
          state: "running",
          index: 0,
          kind: "proposal",
        },
      ]
      body.replay.attempts = [
        { attempt_id: "attempt-a", state: "streaming", turn_id: "turn-a" },
        { attempt_id: "attempt-b", state: "accepted", turn_id: "turn-b" },
      ]
      body.replay.turns = [
        {
          turn_id: "turn-a",
          phase_id: "phase-proposal",
          speaker_id: "speaker-1",
          accepted_attempt_id: null,
        },
        {
          turn_id: "turn-b",
          phase_id: "phase-proposal",
          speaker_id: "speaker-2",
          accepted_attempt_id: "attempt-b",
        },
      ]
    })
    const live = {
      thought: "considering",
      thoughtOmitted: false,
      activity: null,
      ended: false,
      truncated: false,
    }
    const model = buildRoundtableTranscript(
      source,
      [
        message({
          message_id: "verified-b",
          attempt_id: "attempt-b",
          speaker_id: "speaker-2",
          phase_id: "phase-proposal",
          visibility: "staged",
          body: { kind: "proposal", summary: "Verified B" },
        }),
      ],
      [
        { attemptId: "attempt-a", text: "A so far", live },
        { attemptId: "attempt-b", text: "B unverified", live },
      ]
    )
    const turns = model.phases.flatMap((phase) => phase.turns)
    expect(turns.find((turn) => turn.attemptId === "attempt-a")).toMatchObject({
      visibility: "preview",
      summary: "A so far",
      live,
    })
    const b = turns.filter((turn) => turn.attemptId === "attempt-b")
    expect(b).toHaveLength(1)
    expect(b[0].visibility).toBe("staged")
    expect(b[0].live).toBeUndefined()
    expect(JSON.stringify(model)).not.toContain("B unverified")
  })
})

describe("replaceSeatAliases", () => {
  it("maps seat aliases outside code, leaves code and unknown aliases alone", () => {
    const label = (alias: string) =>
      alias === "s0"
        ? "Member 1 · grok-4.6"
        : alias === "s1"
          ? "Member 2 · gemini"
          : null
    expect(
      replaceSeatAliases(
        "s1 prefers Strict; s0 prefers Lax. Keep `s0` literal and ```\ns1\n```.",
        label
      )
    ).toBe(
      "Member 2 · gemini prefers Strict; Member 1 · grok-4.6 prefers Lax. Keep `s0` literal and ```\ns1\n```."
    )
    expect(replaceSeatAliases("s99 is unknown", label)).toBe("s99 is unknown")
    expect(replaceSeatAliases("session_id s0x", label)).toBe("session_id s0x")
  })
})
