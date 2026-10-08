import { fireEvent, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"
import { RoundtableRoomList } from "./roundtable-room-list"

vi.mock("next-intl", () => ({ useTranslations: () => (key: string) => key }))

const rooms = [
  {
    room_id: "current&room",
    status: "running",
    config: { topic: "Current question" },
  },
  {
    room_id: "other",
    status: "completed",
    config: { topic: "Other question" },
  },
]

function props() {
  return {
    workspaceId: "team/a&b",
    roomId: rooms[0].room_id,
    rooms,
    loading: false,
    error: null,
    cursor: "next-page",
    onRefresh: vi.fn(),
    onMore: vi.fn(),
  }
}

describe("roundtable room navigation", () => {
  it("starts collapsed on narrow screens while keeping the desktop sidebar visible", () => {
    render(<RoundtableRoomList {...props()} />)
    const disclosure = screen.getByRole("button", { name: "discussions" })
    expect(disclosure).toHaveAttribute("aria-expanded", "false")
    expect(disclosure).toHaveClass("md:hidden")
    const panel = document.getElementById(
      disclosure.getAttribute("aria-controls")!
    )
    expect(panel).toHaveClass("hidden", "md:flex")
    expect(
      screen.getByRole("complementary", { name: "discussions" })
    ).toHaveClass("min-w-0")
  })

  it("opens and closes the narrow-screen disclosure from the keyboard", async () => {
    const user = userEvent.setup()
    render(<RoundtableRoomList {...props()} />)
    const disclosure = screen.getByRole("button", { name: "discussions" })
    disclosure.focus()
    await user.keyboard("{Enter}")
    expect(disclosure).toHaveAttribute("aria-expanded", "true")
    const panel = document.getElementById(
      disclosure.getAttribute("aria-controls")!
    )
    expect(panel).not.toHaveClass("hidden")
    await user.keyboard(" ")
    expect(disclosure).toHaveAttribute("aria-expanded", "false")
    expect(panel).toHaveClass("hidden")
  })

  it("keeps query-parameter navigation and identifies the current discussion", () => {
    render(<RoundtableRoomList {...props()} />)
    expect(screen.getByRole("link", { name: "new" })).toHaveAttribute(
      "href",
      "/roundtable?workspace_id=team%2Fa%26b"
    )
    const current = screen.getByRole("link", { name: /Current question/ })
    expect(current).toHaveAttribute(
      "href",
      "/roundtable?workspace_id=team%2Fa%26b&room_id=current%26room"
    )
    expect(current).toHaveAttribute("aria-current", "page")
    expect(
      screen.getByRole("link", { name: /Other question/ })
    ).not.toHaveAttribute("aria-current")
    expect(current.className).toContain("focus-visible:")
  })

  it("bounds a large room list and wraps uninterrupted long topics", () => {
    const topic = "unbroken".repeat(80)
    render(
      <RoundtableRoomList
        {...props()}
        rooms={Array.from({ length: 100 }, (_, index) => ({
          room_id: String(index),
          status: "completed",
          config: { topic: index === 0 ? topic : `Room ${index}` },
        }))}
      />
    )
    expect(screen.getAllByRole("listitem")).toHaveLength(100)
    expect(screen.getByRole("list")).toHaveClass("max-h-64", "overflow-y-auto")
    expect(screen.getByRole("link", { name: new RegExp(topic) })).toHaveClass(
      "[overflow-wrap:anywhere]"
    )
  })

  it("disables list requests and announces loading without an empty-state flash", () => {
    const handlers = props()
    render(<RoundtableRoomList {...handlers} loading rooms={[]} />)
    expect(screen.getByRole("status")).toHaveTextContent("loadingRooms")
    expect(screen.queryByText("noRooms")).not.toBeInTheDocument()
    const refresh = screen.getByRole("button", { name: "refresh" })
    const more = screen.getByRole("button", { name: "more" })
    expect(refresh).toBeDisabled()
    expect(more).toBeDisabled()
    fireEvent.click(refresh)
    fireEvent.click(more)
    expect(handlers.onRefresh).not.toHaveBeenCalled()
    expect(handlers.onMore).not.toHaveBeenCalled()
  })

  it("shows an empty state only after a successful empty load", () => {
    const { rerender } = render(
      <RoundtableRoomList {...props()} rooms={[]} cursor={null} />
    )
    expect(screen.getByText("noRooms")).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "more" })
    ).not.toBeInTheDocument()
    rerender(
      <RoundtableRoomList
        {...props()}
        rooms={[]}
        cursor={null}
        error="list_failed"
      />
    )
    expect(screen.getByRole("alert")).toHaveTextContent("list_failed")
    expect(screen.queryByText("noRooms")).not.toBeInTheDocument()
  })

  it("keeps verified rooms visible after an error and supports refreshing or paging", () => {
    const handlers = props()
    render(<RoundtableRoomList {...handlers} error="list_failed" />)
    expect(screen.getByRole("alert")).toHaveTextContent("list_failed")
    expect(
      screen.getByRole("link", { name: /Current question/ })
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "refresh" }))
    fireEvent.click(screen.getByRole("button", { name: "more" }))
    expect(handlers.onRefresh).toHaveBeenCalledOnce()
    expect(handlers.onMore).toHaveBeenCalledOnce()
  })

  it("clamps long topics to three lines, keeps the full text as a tooltip, and renders code spans", () => {
    const topic = `Is \`crun delete --force\` safe? ${"long context ".repeat(60)}`
    render(
      <RoundtableRoomList
        {...props()}
        rooms={[{ room_id: "long", status: "completed", config: { topic } }]}
        roomId="long"
      />
    )
    const link = screen.getByRole("link", { name: /crun delete --force/ })
    expect(link).toHaveAttribute("aria-current", "page")
    expect(link).toHaveAttribute("title", topic.replace(/`/g, ""))
    const text = link.querySelector("[data-testid='room-topic']")
    expect(text).toHaveClass("line-clamp-3")
    expect(text?.querySelector("code")).toHaveTextContent("crun delete --force")
    expect(link).not.toHaveTextContent("`")
    link.focus()
    expect(document.activeElement).toBe(link)
  })
})
