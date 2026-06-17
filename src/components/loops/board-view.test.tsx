import { fireEvent, render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

import { BoardView } from "./board-view"
import type { LoopArtifactRow, LoopIterationRow } from "@/lib/types"

const stableT = (key: string) => key
vi.mock("next-intl", () => ({ useTranslations: () => stableT }))

function art(over: Partial<LoopArtifactRow>): LoopArtifactRow {
  return {
    id: 1,
    issue_id: 1,
    issue_seq: 1,
    kind: "task",
    title: "Artifact",
    status: "done",
    origin: "agent",
    produced_by_iteration_id: null,
    verdict: null,
    attempt: 0,
    sort: 0,
    updated_at: "2026-06-14T00:00:00Z",
    ...over,
  }
}

function iter(over: Partial<LoopIterationRow>): LoopIterationRow {
  return {
    id: 100,
    issue_id: 1,
    issue_seq: 1,
    stage: "design",
    target_artifact_id: null,
    target_title: null,
    conversation_id: null,
    status: "running",
    launched_by: "engine",
    attempt: 0,
    tokens_used: 0,
    created_at: "2026-06-14T00:00:00Z",
    started_at: "2026-06-14T00:00:00Z",
    ended_at: null,
    ...over,
  }
}

describe("BoardView", () => {
  it("lays out cards in per-kind columns and opens one on click", () => {
    const onSelect = vi.fn()
    render(
      <BoardView
        onSelect={onSelect}
        liveIterations={[]}
        artifacts={[
          art({ id: 1, kind: "issue", title: "Root" }), // excluded
          art({ id: 2, kind: "task", title: "Task A", status: "pending" }),
          art({ id: 3, kind: "review", title: "Review X", status: "done" }),
        ]}
      />
    )

    // Five kind columns are always present (issue is not a column).
    for (const col of ["requirement", "design", "task", "review", "result"]) {
      expect(screen.getByText(col)).toBeInTheDocument()
    }
    expect(screen.queryByText("Root")).not.toBeInTheDocument()
    expect(screen.getByText("Task A")).toBeInTheDocument()

    fireEvent.click(screen.getByText("Review X"))
    expect(onSelect).toHaveBeenCalledWith(3)
  })

  it("shows the empty state with no non-issue artifacts and no ghosts", () => {
    render(
      <BoardView
        onSelect={() => {}}
        liveIterations={[]}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
      />
    )
    expect(screen.getByText("empty")).toBeInTheDocument()
  })

  it("renders an in-flight ghost (and leaves the empty state)", () => {
    render(
      <BoardView
        onSelect={() => {}}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
        liveIterations={[iter({ id: 50, stage: "design", status: "running" })]}
      />
    )
    // "running" is unique to a ghost card (artifact statuses never read so).
    expect(screen.getByText("running")).toBeInTheDocument()
    expect(screen.queryByText("empty")).not.toBeInTheDocument()
  })

  it("suppresses a ghost once its iteration's artifact has landed (dedup by producer)", () => {
    render(
      <BoardView
        onSelect={() => {}}
        artifacts={[
          art({ id: 1, kind: "issue", title: "Root" }),
          // The design THIS iteration produced already exists (stale live snapshot).
          art({
            id: 2,
            kind: "design",
            title: "Design A",
            produced_by_iteration_id: 42,
          }),
        ]}
        liveIterations={[iter({ id: 42, stage: "design", status: "running" })]}
      />
    )
    // Only the landed artifact shows — no duplicate ghost.
    expect(screen.getByText("Design A")).toBeInTheDocument()
    expect(screen.queryByText("running")).not.toBeInTheDocument()
  })

  it("shows no ghost for an implement iteration (the task card already exists)", () => {
    render(
      <BoardView
        onSelect={() => {}}
        artifacts={[
          art({ id: 1, kind: "issue", title: "Root" }),
          art({ id: 2, kind: "task", title: "Task A", status: "in_progress" }),
        ]}
        liveIterations={[
          iter({
            id: 60,
            stage: "implement",
            status: "running",
            target_artifact_id: 2,
          }),
        ]}
      />
    )
    expect(screen.getByText("Task A")).toBeInTheDocument()
    // implement maps to no board column → the task card carries progress itself.
    expect(screen.queryByText("running")).not.toBeInTheDocument()
  })
})
