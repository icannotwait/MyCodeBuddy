import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { CompletionProjectionV2 } from "@/lib/types"
import { CompletionDecisionCard } from "./completion-decision-card"
import {
  resolveCompletionDecision,
  resolveDesignSelfReview,
  retryCompletionArtifact,
} from "@/lib/api"

vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api")
  return {
    ...actual,
    resolveCompletionDecision: vi.fn(),
    resolveDesignSelfReview: vi.fn(),
    retryCompletionArtifact: vi.fn(),
  }
})

const cas = {
  attention_id: "attention-1",
  task_id: "task-1",
  kind: "completion_decision" as const,
  captured_scope_digest: `sha256:${"a".repeat(64)}`,
  latest_run_id: "task-1",
  node_id: "plan-reviewer",
}

function projection(
  overrides: Partial<CompletionProjectionV2> = {}
): CompletionProjectionV2 {
  return {
    protocol_version: 2,
    graph_revision: 7,
    card: {
      state: "needs_decision",
      role: "reviewer",
      outcome: null,
      summary: "Choose the durable reviewer outcome.",
      report_file: null,
      source: null,
      evidence_validated: false,
      attention: cas,
    },
    ...overrides,
  }
}

function renderCard(request = projection()) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <CompletionDecisionCard request={request} />
    </NextIntlClientProvider>
  )
}

describe("CompletionDecisionCard", () => {
  beforeEach(() => {
    vi.mocked(resolveCompletionDecision).mockReset()
    vi.mocked(resolveDesignSelfReview).mockReset()
    vi.mocked(retryCompletionArtifact).mockReset()
  })

  it("submits a legal typed outcome with the exact six-field CAS", async () => {
    vi.mocked(resolveCompletionDecision).mockResolvedValue({
      workflow_id: "wf-1",
      task_id: "task-1",
      node_id: "plan-reviewer",
      kind: "completion_decision",
      outcome: "approve_with_minors",
      evidence_scope_digest: `sha256:${"b".repeat(64)}`,
      completion: projection({
        graph_revision: 8,
        card: {
          ...projection().card,
          state: "resolved",
          outcome: "approve_with_minors",
          source: "user_adjudication",
          evidence_validated: true,
          attention: null,
        },
      }),
      graph_revision: 8,
      idempotent_replay: false,
    })
    renderCard()

    await userEvent.click(
      screen.getByRole("button", { name: "Approve with minors" })
    )

    expect(resolveCompletionDecision).toHaveBeenCalledWith({
      cas,
      outcome: "approve_with_minors",
    })
    expect(await screen.findByText("Resolved")).toBeInTheDocument()
  })

  it("keeps the selected choice after a retryable failure", async () => {
    vi.mocked(resolveCompletionDecision).mockRejectedValue(
      new Error("transport unavailable")
    )
    renderCard()

    const choice = screen.getByRole("button", { name: "Request changes" })
    await userEvent.click(choice)

    await waitFor(() => expect(choice).toHaveAttribute("aria-pressed", "true"))
    expect(screen.getByText("transport unavailable")).toBeInTheDocument()
  })

  it("routes Design-root reviewer outcomes through self-review adjudication", async () => {
    const designCas = { ...cas, kind: "design_self_review_decision" as const }
    vi.mocked(resolveDesignSelfReview).mockResolvedValue({
      workflow_id: "wf-1",
      task_id: "task-1",
      node_id: "design-root",
      kind: "design_self_review_decision",
      outcome: "approve",
      evidence_scope_digest: `sha256:${"b".repeat(64)}`,
      graph_revision: 8,
      idempotent_replay: false,
      completion: projection({
        graph_revision: 8,
        card: {
          ...projection().card,
          state: "resolved",
          outcome: "approve",
          source: "user_adjudication",
          evidence_validated: true,
          attention: null,
        },
      }),
    })
    renderCard(
      projection({
        card: { ...projection().card, attention: designCas },
      })
    )

    await userEvent.click(screen.getByRole("button", { name: "Approve" }))

    expect(resolveDesignSelfReview).toHaveBeenCalledWith({
      cas: designCas,
      outcome: "approve",
    })
    expect(resolveCompletionDecision).not.toHaveBeenCalled()
  })

  it("offers producer outcomes only for durable producer roles", () => {
    renderCard(
      projection({
        card: { ...projection().card, role: "implementer" },
      })
    )

    expect(screen.getByRole("button", { name: "Done" })).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Done with concerns" })
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Approve" })
    ).not.toBeInTheDocument()
  })

  it("fails closed when the durable role is missing", () => {
    renderCard(
      projection({
        card: { ...projection().card, role: null },
      })
    )

    expect(screen.queryAllByRole("button")).toHaveLength(0)
  })

  it("does not manufacture validated completion from a mutation response", async () => {
    const onResolved = vi.fn()
    vi.mocked(resolveCompletionDecision).mockResolvedValue({
      workflow_id: "wf-1",
      task_id: "task-1",
      node_id: "plan-reviewer",
      kind: "completion_decision",
      outcome: "approve",
      evidence_scope_digest: `sha256:${"c".repeat(64)}`,
      graph_revision: 8,
      idempotent_replay: false,
    })
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <CompletionDecisionCard
          request={projection()}
          onResolved={onResolved}
        />
      </NextIntlClientProvider>
    )

    await userEvent.click(screen.getByRole("button", { name: "Approve" }))

    await waitFor(() => expect(resolveCompletionDecision).toHaveBeenCalled())
    expect(screen.getByText("Decision required")).toBeInTheDocument()
    expect(screen.queryByText("Evidence validated")).not.toBeInTheDocument()
    expect(onResolved).not.toHaveBeenCalled()
  })

  it("shows artifact retry only for artifact recovery", async () => {
    vi.mocked(retryCompletionArtifact).mockResolvedValue({
      workflow_id: "wf-1",
      task_id: "task-1",
      node_id: "plan-reviewer",
      kind: "completion_artifact_recovery",
      outcome: "approve",
      evidence_scope_digest: `sha256:${"d".repeat(64)}`,
      graph_revision: 8,
      idempotent_replay: false,
      completion: projection(),
    })
    renderCard(
      projection({
        card: {
          ...projection().card,
          state: "blocked",
          attention: { ...cas, kind: "completion_artifact_recovery" },
        },
      })
    )

    await userEvent.click(
      screen.getByRole("button", { name: "Retry artifact" })
    )
    expect(retryCompletionArtifact).toHaveBeenCalledWith({
      cas: {
        ...cas,
        kind: "completion_artifact_recovery",
      },
    })
  })

  it("wraps bounded summaries and report paths inside compact overlays", () => {
    renderCard(
      projection({
        card: {
          ...projection().card,
          summary: "x".repeat(1024),
          report_file: `reports/${"y".repeat(512)}.md`,
        },
      })
    )

    expect(screen.getByText("x".repeat(1024))).toHaveClass("break-words")
    expect(screen.getByText(/reports\/y+/)).toHaveClass("break-all")
  })
})
