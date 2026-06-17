import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { MemoryPanel } from "./memory-panel"
import type { LoopMemoryRow } from "@/lib/types"

const stableT = (key: string) => key
vi.mock("next-intl", () => ({ useTranslations: () => stableT }))

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }))
vi.mock("@/components/loops/loop-realtime-context", () => ({
  useLoopRealtime: () => ({ register: () => () => {} }),
}))

// MessageResponse (Streamdown) pulls in the link-safety hook + heavy markdown
// deps; stub it to a passthrough that renders the raw content.
vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({ children }: { children: string }) => (
    <div data-testid="markdown">{children}</div>
  ),
}))

const listLoopMemory = vi.fn()
const createLoopMemory = vi.fn().mockResolvedValue(undefined)
const updateLoopMemory = vi.fn().mockResolvedValue(undefined)
const deleteLoopMemory = vi.fn().mockResolvedValue(undefined)
vi.mock("@/lib/loops-api", () => ({
  listLoopMemory: (...a: unknown[]) => listLoopMemory(...a),
  createLoopMemory: (...a: unknown[]) => createLoopMemory(...a),
  updateLoopMemory: (...a: unknown[]) => updateLoopMemory(...a),
  deleteLoopMemory: (...a: unknown[]) => deleteLoopMemory(...a),
}))

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ open, children }: { open: boolean; children: React.ReactNode }) =>
    open ? <div>{children}</div> : null,
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogFooter: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
}))
vi.mock("@/components/ui/select", () => ({
  Select: ({
    value,
    onValueChange,
    children,
  }: {
    value: string
    onValueChange: (v: string) => void
    children: React.ReactNode
  }) => (
    <select value={value} onChange={(e) => onValueChange(e.target.value)}>
      {children}
    </select>
  ),
  SelectTrigger: () => null,
  SelectValue: () => null,
  SelectContent: ({ children }: { children: React.ReactNode }) => (
    <>{children}</>
  ),
  SelectItem: ({
    value,
    children,
  }: {
    value: string
    children: React.ReactNode
  }) => <option value={value}>{children}</option>,
}))

function mem(over: Partial<LoopMemoryRow>): LoopMemoryRow {
  return {
    id: 1,
    kind: "decision",
    source: "human",
    title: "Memory",
    summary: null,
    content: "",
    trust_tier: "proposed",
    status: "active",
    superseded_by: null,
    source_issue_id: null,
    source_artifact_id: null,
    produced_by_iteration_id: null,
    created_at: "2026-06-14T00:00:00Z",
    updated_at: "2026-06-14T00:00:00Z",
    ...over,
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  listLoopMemory.mockResolvedValue([
    mem({ id: 1, source: "agent", title: "Use X", content: "because" }),
    mem({ id: 2, source: "human", title: "No Y", status: "archived" }),
  ])
})

describe("MemoryPanel", () => {
  it("labels each entry's source", async () => {
    render(<MemoryPanel spaceId={1} />)
    expect(await screen.findByText("Use X")).toBeInTheDocument()
    expect(screen.getByText("agent")).toBeInTheDocument() // source badge
    expect(screen.getByText("human")).toBeInTheDocument()
  })

  it("archives an active entry via updateLoopMemory", async () => {
    render(<MemoryPanel spaceId={1} />)
    await screen.findByText("Use X")
    fireEvent.click(screen.getByLabelText("archive"))
    await waitFor(() =>
      expect(updateLoopMemory).toHaveBeenCalledWith({
        spaceId: 1,
        id: 1,
        title: "Use X",
        content: "because",
        status: "archived",
      })
    )
  })

  it("deletes an entry via deleteLoopMemory", async () => {
    render(<MemoryPanel spaceId={1} />)
    await screen.findByText("Use X")
    fireEvent.click(screen.getAllByLabelText("delete")[0])
    await waitFor(() => expect(deleteLoopMemory).toHaveBeenCalledWith(1, 1))
  })

  it("creates a new entry from the add dialog", async () => {
    render(<MemoryPanel spaceId={1} />)
    await screen.findByText("Use X")
    fireEvent.click(screen.getByText("add"))
    fireEvent.change(screen.getByLabelText("titleLabel"), {
      target: { value: "Prefer pnpm" },
    })
    fireEvent.click(screen.getByText("create"))
    await waitFor(() =>
      expect(createLoopMemory).toHaveBeenCalledWith({
        spaceId: 1,
        kind: "decision",
        title: "Prefer pnpm",
        content: "",
      })
    )
  })

  it("groups live memories by CoALA layer", async () => {
    listLoopMemory.mockResolvedValue([
      mem({ id: 1, kind: "decision", title: "Sem note" }),
      mem({ id: 2, kind: "episodic", title: "Epi note" }),
      mem({ id: 3, kind: "procedural", title: "Proc note" }),
    ])
    render(<MemoryPanel spaceId={1} />)
    await screen.findByText("Sem note")
    // One subheading per non-empty layer (the mock t echoes its key).
    expect(screen.getByText("layerSemantic")).toBeInTheDocument()
    expect(screen.getByText("layerEpisodic")).toBeInTheDocument()
    expect(screen.getByText("layerProcedural")).toBeInTheDocument()
    // The reflect-authored kinds render in their layers.
    expect(screen.getByText("Epi note")).toBeInTheDocument()
    expect(screen.getByText("Proc note")).toBeInTheDocument()
  })

  it("folds superseded entries read-only behind a toggle", async () => {
    listLoopMemory.mockResolvedValue([
      mem({ id: 1, title: "Live one" }),
      mem({ id: 3, title: "Old one", status: "superseded" }),
    ])
    render(<MemoryPanel spaceId={1} />)
    await screen.findByText("Live one")
    // Trust tier badge renders on the live row (only one visible pre-expand).
    expect(screen.getByText("proposed")).toBeInTheDocument()
    // The superseded row is hidden until the section is expanded.
    expect(screen.queryByText("Old one")).not.toBeInTheDocument()
    fireEvent.click(screen.getByText("supersededSection"))
    expect(await screen.findByText("Old one")).toBeInTheDocument()
    // Read-only: a superseded row offers no restore (or archive) action; the live
    // row keeps its archive control.
    expect(screen.queryByLabelText("restore")).not.toBeInTheDocument()
    expect(screen.getByLabelText("archive")).toBeInTheDocument()
  })
})
