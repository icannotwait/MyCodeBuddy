import { render, screen, waitFor } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { RoundtableTranscript } from "@/components/roundtable/roundtable-transcript"
import type { RoundtableProjection } from "@/lib/roundtable/types"

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
  useLocale: () => "en",
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({ openResolvedImagePreview: () => false }),
  useOptionalWorkspaceActions: () => null,
}))

const summary = [
  "## Keep the wall",
  "",
  "- elapsed slice",
  "",
  "Use `crun delete --force`.",
  "",
  "See the [note](https://example.com/note).",
].join("\n")

function projection(): RoundtableProjection {
  return {
    projection_ref: { id: "projection", hash: "hash" },
    body: {
      schema_version: 1,
      room_id: "room",
      revision: "1",
      run_epoch: "1",
      last_seq: "1",
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
      ],
      replay: {
        config: null,
        speakers: [
          {
            speaker_id: "speaker-1",
            ordinal: 0,
            role: "member",
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
    },
  }
}

describe("roundtable transcript markdown", () => {
  it("renders headings, lists, inline code, and links", async () => {
    render(
      <RoundtableTranscript
        projection={projection()}
        messages={[
          {
            message_id: "proposal",
            body_hash: "hash",
            visibility: "published",
            speaker_id: "speaker-1",
            phase_id: "phase-proposal",
            body: { kind: "proposal", summary },
          },
        ]}
      />
    )

    expect(
      await screen.findByRole("heading", { name: "Keep the wall" })
    ).toBeVisible()
    expect(screen.getByText("elapsed slice")).toBeVisible()
    expect(screen.getByText("crun delete --force")).toBeVisible()
    const note = await screen.findByRole("button", { name: /note/ })
    await waitFor(() =>
      expect(note).toHaveAttribute("title", "https://example.com/note")
    )
  })
})
