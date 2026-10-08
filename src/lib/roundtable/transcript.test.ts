import { describe, expect, it } from "vitest"
import type {
  RoundtableConfig,
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"
import { buildRoundtableTranscript } from "@/lib/roundtable/transcript"

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
        agent: "antigravity",
        modelId: "gemini-3.8-flash-high",
      },
    })
    expect(model.phases[1].turns[0].evidenceAliases).toEqual([
      { kind: "evidence", alias: "file-a" },
    ])
    expect(model.phases[2].turns[0]).toMatchObject({
      visibility: "published",
      speaker: { role: "moderator", agent: "grok", modelId: "grok-4.6" },
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
