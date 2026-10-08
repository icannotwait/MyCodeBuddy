import { fireEvent, render, screen } from "@testing-library/react"
import type { ReactNode } from "react"
import { describe, expect, it, vi } from "vitest"
import { RoundtableTranscript } from "@/components/roundtable/roundtable-transcript"
import type {
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string, values?: Record<string, unknown>) =>
    values ? `${key}:${JSON.stringify(values)}` : key,
  useLocale: () => "en",
}))

vi.mock("streamdown", () => ({
  Streamdown: ({
    children,
    className,
  }: {
    children: ReactNode
    className?: string
  }) => (
    <div data-testid="markdown" className={className}>
      {children}
    </div>
  ),
  defaultRemarkPlugins: {},
  defaultRehypePlugins: {},
}))

function projection(): RoundtableProjection {
  return {
    projection_ref: { id: "projection", hash: "hash" },
    body: {
      schema_version: 1,
      room_id: "room",
      revision: "1",
      run_epoch: "1",
      last_seq: "4",
      status: "running",
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
        config: {
          schema_version: 1,
          topic: "Question",
          workspace_id: "workspace",
          source_refs: [],
          participants: [
            {
              ordinal: 0,
              role: "reviewer",
              provider_ref: "provider:grok",
              agent: "grok",
            },
            {
              ordinal: 1,
              role: "critic",
              provider_ref: `provider:${"gemini".repeat(20)}`,
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
        },
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
            provider_ref: `provider:${"gemini".repeat(20)}`,
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
        attempts: [
          {
            attempt_id: "attempt-1",
            state: "accepted",
            turn_id: "turn-1",
            attempt_no: 1,
          },
        ],
        turns: [
          {
            turn_id: "turn-1",
            phase_id: "phase-proposal",
            speaker_id: "speaker-1",
            accepted_attempt_id: "attempt-1",
          },
        ],
        message_memberships: [
          {
            message_id: "proposal",
            membership_version: "1",
            visibility: "published",
            published_seq: "1",
          },
        ],
        evidence: [],
        result_quality: null,
      },
    },
  }
}

function proposal(): RoundtableMessage {
  return {
    message_id: "proposal",
    body_hash: "a".repeat(64),
    visibility: "published",
    speaker_id: "speaker-1",
    attempt_id: "attempt-1",
    phase_id: "phase-proposal",
    attempt_no: 1,
    attempt_state: "accepted",
    finished_at: "2026-10-08T03:41:00.000Z",
    body: {
      kind: "proposal",
      summary: "Fail fast\n\n- elapsed slice\n\n`crun delete --force`",
      claims: [
        {
          text: "sk-secret-token must stay hidden",
          confidence: "high",
          evidence_aliases: ["alias-1"],
        },
      ],
    },
  }
}

describe("roundtable transcript view", () => {
  it("attributes speakers, groups phases, and renders markdown text", () => {
    const { container } = render(
      <RoundtableTranscript
        projection={projection()}
        messages={[
          {
            message_id: "synthesis",
            body_hash: "c".repeat(64),
            visibility: "published",
            speaker_id: "speaker-mod",
            phase_id: "phase-synthesis",
            body: {
              kind: "synthesis",
              recommendation: { text: "Keep the prepaid wall", aliases: [] },
            },
          },
          {
            message_id: "critique",
            body_hash: "b".repeat(64),
            visibility: "staged",
            speaker_id: "speaker-2",
            phase_id: "phase-critique",
            body: { kind: "critique", summary: "Checkpoint lag" },
          },
          proposal(),
        ]}
      />
    )

    expect(
      [...container.querySelectorAll("[data-phase]")].map((node) =>
        node.getAttribute("data-phase")
      )
    ).toEqual(["proposal", "critique", "synthesis"])
    expect(screen.getByRole("heading", { name: "conclusion" })).toBeVisible()
    const titles = [...container.querySelectorAll("title")].map(
      (node) => node.textContent
    )
    expect(titles).toEqual(
      expect.arrayContaining(["Grok", "Google Antigravity"])
    )

    const proposalTurn = container.querySelector('[data-message-id="proposal"]')
    expect(proposalTurn).toHaveAttribute("data-visibility", "published")
    expect(proposalTurn).toHaveAttribute("data-speaker-role", "member")
    expect(proposalTurn).toHaveTextContent("member 1")
    expect(proposalTurn).toHaveTextContent("reviewer")
    expect(proposalTurn).toHaveTextContent("grok-4.6")
    expect(proposalTurn).toHaveTextContent("published")
    expect(proposalTurn).toHaveTextContent(
      'attemptBadge:{"count":1,"state":"accepted"}'
    )
    expect(proposalTurn?.querySelector("time")).toHaveAttribute(
      "dateTime",
      "2026-10-08T03:41:00.000Z"
    )
    expect(
      proposalTurn?.querySelector("[data-testid='markdown']")
    ).toHaveTextContent("elapsed slice")
    expect(proposalTurn).toHaveTextContent("[redacted]")
    expect(proposalTurn).not.toHaveTextContent("sk-secret-token")

    const critique = container.querySelector('[data-message-id="critique"]')
    expect(critique).toHaveAttribute("data-visibility", "staged")
    expect(critique).toHaveTextContent("member 2")
    expect(critique).toHaveTextContent("staged")
    expect(critique).not.toHaveTextContent("published")

    const synthesis = container.querySelector('[data-message-id="synthesis"]')
    expect(synthesis).toHaveAttribute("data-speaker-role", "moderator")
    expect(synthesis).toHaveTextContent("moderator")
    expect(synthesis).toHaveTextContent("Keep the prepaid wall")
    expect(container.querySelector('[data-phase="synthesis"]')).toHaveClass(
      "border-primary/30"
    )
  })

  it("keeps hashes and evidence aliases behind a closed, focusable details row", () => {
    const hash = "a".repeat(64)
    const { container } = render(
      <RoundtableTranscript projection={projection()} messages={[proposal()]} />
    )
    const details = container.querySelector("details")
    expect(details).not.toHaveAttribute("open")
    expect(details).toHaveTextContent("alias-1")
    expect(details).toHaveTextContent(hash)
    const summary = details?.querySelector("summary")
    expect(summary).toHaveTextContent("details")
    summary?.focus()
    expect(document.activeElement).toBe(summary)
    expect(summary?.className).toContain("focus-visible:")
    fireEvent.click(summary!)
    expect(details).toHaveAttribute("open")
  })

  it("appends a newly published message without reordering earlier phases", () => {
    const { container, rerender } = render(
      <RoundtableTranscript projection={projection()} messages={[proposal()]} />
    )
    expect(container.querySelectorAll("[data-message-id]")).toHaveLength(1)
    rerender(
      <RoundtableTranscript
        projection={projection()}
        messages={[
          proposal(),
          {
            message_id: "synthesis",
            body_hash: "c".repeat(64),
            visibility: "published",
            speaker_id: "speaker-mod",
            phase_id: "phase-synthesis",
            body: {
              kind: "synthesis",
              recommendation: { text: "Second published turn" },
            },
          },
        ]}
      />
    )
    expect(
      [...container.querySelectorAll("[data-message-id]")].map((node) =>
        node.getAttribute("data-message-id")
      )
    ).toEqual(["proposal", "synthesis"])
    expect(screen.getByText("Second published turn")).toBeInTheDocument()
  })

  it("wraps long attribution on a narrow transcript row", () => {
    const provider = `provider:${"gemini".repeat(20)}`
    const { container } = render(
      <RoundtableTranscript
        projection={projection()}
        messages={[
          {
            ...proposal(),
            message_id: "critique",
            speaker_id: "speaker-2",
            phase_id: "phase-critique",
            visibility: "staged",
            body: { kind: "critique", summary: "narrow" },
          },
        ]}
      />
    )
    const article = container.querySelector("article")
    expect(article).toHaveClass("min-w-0")
    expect(article?.querySelector("header")).toHaveClass("flex-wrap")
    expect(screen.getByText(provider)).toHaveClass("[overflow-wrap:anywhere]")
    expect(container.querySelector("[data-testid='markdown']")).toHaveClass(
      "min-w-0",
      "[overflow-wrap:anywhere]"
    )
  })
})
