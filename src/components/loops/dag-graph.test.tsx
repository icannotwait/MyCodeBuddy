import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { DagGraph } from "./dag-graph"
import type { AttentionKey } from "@/lib/loop-attention"
import type { LoopArtifactRow, LoopInboxItemRow } from "@/lib/types"

// Echoing translator across every namespace the graph uses.
vi.mock("next-intl", () => ({ useTranslations: () => (k: string) => k }))

function art(
  over: Partial<LoopArtifactRow> & { id: number; kind: LoopArtifactRow["kind"] }
): LoopArtifactRow {
  return {
    issue_id: 1,
    issue_seq: 1,
    title: "T",
    status: "done",
    origin: "agent",
    produced_by_iteration_id: null,
    verdict: null,
    contribution_kind: "delta",
    attempt: 0,
    sort: 0,
    updated_at: "2026-06-17T00:00:00Z",
    ...over,
  }
}

const scrollIntoView = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  // jsdom has no layout engine; the focus effect calls scrollIntoView.
  Element.prototype.scrollIntoView = scrollIntoView
})

const base = {
  links: [],
  liveIterations: [],
  executingIds: new Set<string>(),
  onSelect: () => {},
}

describe("DagGraph focus replay (Codex r1)", () => {
  it("scrolls to and consumes a focus whose node is present", () => {
    const onFocusConsumed = vi.fn()
    render(
      <DagGraph
        {...base}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
        focus={1}
        onFocusConsumed={onFocusConsumed}
      />
    )
    expect(scrollIntoView).toHaveBeenCalledTimes(1)
    expect(onFocusConsumed).toHaveBeenCalledTimes(1)
  })

  it("consumes (without scrolling) a focus whose node is absent once the graph has data", () => {
    const onFocusConsumed = vi.fn()
    render(
      <DagGraph
        {...base}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
        focus={999}
        onFocusConsumed={onFocusConsumed}
      />
    )
    expect(scrollIntoView).not.toHaveBeenCalled()
    // Layout is ready but the target is gone → consume so it can't pulse later.
    expect(onFocusConsumed).toHaveBeenCalledTimes(1)
  })

  it("keeps an unresolved focus while the graph is still empty (replay on data)", () => {
    const onFocusConsumed = vi.fn()
    render(
      <DagGraph
        {...base}
        artifacts={[]}
        focus={999}
        onFocusConsumed={onFocusConsumed}
      />
    )
    expect(onFocusConsumed).not.toHaveBeenCalled()
  })
})

describe("DagGraph issue-level attention (Codex r2)", () => {
  const card = { id: 1 } as unknown as LoopInboxItemRow

  it("rings the issue root node for issue-level (issue-root) cards", () => {
    const attentionMap = new Map<AttentionKey, LoopInboxItemRow[]>([
      ["issue-root", [card]],
    ])
    render(
      <DagGraph
        {...base}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
        attentionMap={attentionMap}
      />
    )
    // The echoing translator renders the attention label as "attention", so an
    // attentioned node's accessible name carries the " — attention" suffix.
    expect(screen.getByLabelText("issue: Root — attention")).toBeInTheDocument()
  })

  it("leaves the issue root unmarked when there are no issue-level cards", () => {
    render(
      <DagGraph
        {...base}
        artifacts={[art({ id: 1, kind: "issue", title: "Root" })]}
        attentionMap={new Map()}
      />
    )
    expect(screen.getByLabelText("issue: Root")).toBeInTheDocument()
    expect(
      screen.queryByLabelText("issue: Root — attention")
    ).not.toBeInTheDocument()
  })
})
