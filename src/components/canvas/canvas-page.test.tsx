import { act, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { toast } from "sonner"
import enMessages from "@/i18n/messages/en.json"
import { canvasListBoards } from "@/lib/api"
import type { CanvasBoard, CanvasBoardSummary, CanvasNode } from "@/lib/types"
import { useCanvasBoardsStore } from "@/stores/canvas-boards-store"
import { useCanvasStore } from "@/stores/canvas-store"
import { CanvasPage, CanvasPageTitle } from "./canvas-page"

vi.mock("@/lib/api", () => ({
  canvasListBoards: vi.fn(),
  canvasCreateBoard: vi.fn(),
  canvasUpdateBoard: vi.fn(),
  canvasDeleteBoard: vi.fn(),
  canvasListNodes: vi.fn(),
}))
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn().mockResolvedValue(() => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))
vi.mock("@/contexts/workbench-route-context", () => ({
  useWorkbenchRoute: () => ({ openConversations: vi.fn() }),
}))
vi.mock("sonner", () => ({
  toast: { info: vi.fn(), error: vi.fn(), success: vi.fn() },
}))
// The board itself (ReactFlow and everything behind it) is not what these
// tests are about: a marker naming the board it was given is enough.
vi.mock("./canvas-view", () => ({
  default: ({
    boardId,
    exportName,
  }: {
    boardId: number
    exportName: string
  }) => (
    <div data-testid="board-view" data-export-name={exportName}>
      board {boardId}
    </div>
  ),
}))

const mockList = vi.mocked(canvasListBoards)

function board(id: number, over: Partial<CanvasBoard> = {}): CanvasBoard {
  return {
    id,
    name: `Board ${id}`,
    description: null,
    color: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    ...over,
  }
}

function summary(b: CanvasBoard): CanvasBoardSummary {
  return { board: b, node_count: 0, terminal_count: 0, preview: [] }
}

function node(id: number, kind: CanvasNode["kind"]): CanvasNode {
  return {
    id,
    board_id: 2,
    kind,
    folder_id: null,
    folder_group_id: null,
    agent_type: null,
    conversation_id: null,
    member_ids: [],
    title: null,
    content: null,
    path: kind === "terminal" ? "/tmp" : null,
    color: null,
    collapsed: false,
    grid_columns: 0,
    grid_rows: 0,
    x: 0,
    y: 0,
    width: 200,
    height: 140,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
  }
}

function renderPage() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <CanvasPageTitle />
      <CanvasPage />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  useCanvasBoardsStore.getState().reset()
  useCanvasStore.getState().reset()
  mockList.mockReset()
  mockList.mockResolvedValue([
    summary(board(1, { name: "Sprint: Q4/Launch" })),
    summary(board(2, { name: "Research" })),
  ])
  vi.mocked(toast.info).mockClear()
})

describe("CanvasPage", () => {
  it("starts at the list", async () => {
    renderPage()
    expect(await screen.findByRole("button", { name: "Research" })).toBeTruthy()
    expect(screen.queryByTestId("board-view")).toBeNull()
    // The breadcrumb is just the route title here — nothing to go back to.
    expect(
      screen.getByRole("heading", { name: enMessages.Canvas.title })
    ).toBeTruthy()
  })

  it("opens a canvas from its card, and the breadcrumb leads back", async () => {
    renderPage()
    await userEvent.click(
      await screen.findByRole("button", { name: "Sprint: Q4/Launch" })
    )
    const view = await screen.findByTestId("board-view")
    expect(view.textContent).toBe("board 1")
    // Named after the board, minus what a file name can't hold.
    expect(view.dataset.exportName).toBe("Sprint- Q4-Launch")
    // Where the user is now…
    expect(
      screen.getByRole("button", { name: "Canvas options" }).textContent
    ).toContain("Sprint: Q4/Launch")

    // …and the way back up.
    await userEvent.click(
      screen.getByRole("button", { name: "Back to all canvases" })
    )
    expect(screen.queryByTestId("board-view")).toBeNull()
    expect(await screen.findByRole("button", { name: "Research" })).toBeTruthy()
  })

  it("leaves a canvas deleted elsewhere, and says so", async () => {
    useCanvasBoardsStore.getState().openBoard(2)
    renderPage()
    expect(await screen.findByTestId("board-view")).toBeTruthy()

    // What the backend answers from now on.
    mockList.mockResolvedValue([
      summary(board(1, { name: "Sprint: Q4/Launch" })),
    ])
    act(() => {
      useCanvasBoardsStore
        .getState()
        .handleBoardChanged({ kind: "deleted", id: 2 })
    })

    expect(screen.queryByTestId("board-view")).toBeNull()
    expect(toast.info).toHaveBeenCalledWith("This canvas was deleted")
    expect(useCanvasBoardsStore.getState().closedNotice).toBeNull()
    // Back at the list, and the deleted board is not on it.
    expect(
      await screen.findByRole("button", { name: "Sprint: Q4/Launch" })
    ).toBeTruthy()
    expect(screen.queryByRole("button", { name: "Research" })).toBeNull()
  })

  it("warns about nodes added after opening the board when deleting from its title", async () => {
    useCanvasBoardsStore.getState().openBoard(2)
    renderPage()
    await screen.findByRole("button", { name: "Canvas options" })

    act(() => {
      const store = useCanvasStore.getState()
      store.openBoard(2)
      store.acceptSnapshot({ board_id: 2, revision: 0, nodes: [] })
      store.handleCanvasChanged({
        kind: "upsert",
        node: node(10, "note"),
        revision: 1,
      })
      store.handleCanvasChanged({
        kind: "upsert",
        node: node(11, "terminal"),
        revision: 2,
      })
    })
    // The board-list cache is still the empty-board snapshot.
    expect(
      useCanvasBoardsStore.getState().boards.find((s) => s.board.id === 2)
        ?.node_count
    ).toBe(0)
    await userEvent.click(
      screen.getByRole("button", { name: "Canvas options" })
    )
    await userEvent.click(
      await screen.findByRole("menuitem", { name: "Delete canvas…" })
    )
    expect(screen.getByText("It holds 2 items.")).toBeTruthy()
    expect(
      screen.getByText(
        "1 terminal on it will be closed and its process stopped."
      )
    ).toBeTruthy()

    // A live deletion while the confirmation is open updates it too.
    act(() => {
      useCanvasStore
        .getState()
        .handleCanvasChanged({ kind: "deleted", id: 11, revision: 3 })
    })
    expect(screen.getByText("It holds 1 item.")).toBeTruthy()
    expect(
      screen.queryByText(
        "1 terminal on it will be closed and its process stopped."
      )
    ).toBeNull()
  })

  it.each([
    { boardId: 1, hydrated: true },
    { boardId: 2, hydrated: false },
  ])(
    "keeps the list counts until this board is hydrated ($boardId, $hydrated)",
    async ({ boardId, hydrated }) => {
      mockList.mockResolvedValue([
        { ...summary(board(2)), node_count: 3, terminal_count: 1 },
      ])
      useCanvasBoardsStore.getState().openBoard(2)
      useCanvasStore.setState({ boardId, hydrated, nodes: new Map() })
      renderPage()
      await userEvent.click(
        await screen.findByRole("button", { name: "Canvas options" })
      )
      await userEvent.click(
        await screen.findByRole("menuitem", { name: "Delete canvas…" })
      )
      expect(screen.getByText("It holds 3 items.")).toBeTruthy()
      expect(
        screen.getByText(
          "1 terminal on it will be closed and its process stopped."
        )
      ).toBeTruthy()
    }
  )

  it("leaves a canvas whose nodes can no longer be read", async () => {
    // Deleted while this client wasn't listening: no board event ever comes,
    // only a `not_found` snapshot.
    useCanvasBoardsStore.getState().openBoard(2)
    renderPage()
    expect(await screen.findByTestId("board-view")).toBeTruthy()

    act(() => {
      useCanvasStore.setState({ boardId: 2, boardMissing: true })
    })

    await waitFor(() => expect(screen.queryByTestId("board-view")).toBeNull())
    expect(toast.info).toHaveBeenCalledWith("This canvas was deleted")
    expect(await screen.findByRole("button", { name: "Research" })).toBeTruthy()
  })

  it("ignores a missing flag for a board that is not the open one", async () => {
    useCanvasBoardsStore.getState().openBoard(2)
    renderPage()
    expect(await screen.findByTestId("board-view")).toBeTruthy()
    act(() => {
      useCanvasStore.setState({ boardId: 1, boardMissing: true })
    })
    expect(screen.getByTestId("board-view")).toBeTruthy()
    expect(toast.info).not.toHaveBeenCalled()
  })
})
