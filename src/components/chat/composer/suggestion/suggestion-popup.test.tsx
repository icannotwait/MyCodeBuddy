import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { createRef, useState } from "react"
import { afterEach, describe, expect, it, vi } from "vitest"

import type { ReferenceAttrs } from "../types"
import type {
  ReferenceGroupKind,
  ReferenceGroupSnapshot,
  ReferenceSearchController,
  ReferenceSearchSnapshot,
} from "../reference-search-controller"
import { FakeReferenceSearchController } from "./fake-reference-search-controller"
import { SuggestionPopup } from "./suggestion-popup"
import type { SuggestionItem, SuggestionPopupHandle } from "./types"

export { FakeReferenceSearchController }

const BASELINE_REFERENCE_TEXT =
  "2026-07-06-simple-packaging-storage-ballistic-throw-design.md"
const LONG_REFERENCE_TEXT = `${BASELINE_REFERENCE_TEXT}-with-additional-differentiating-suffix.md`

function expectedMiddleTruncate(text: string): string {
  const edgeChars = Math.floor(
    (BASELINE_REFERENCE_TEXT.length - "...".length) / 2
  )
  return text.length <= BASELINE_REFERENCE_TEXT.length
    ? text
    : `${text.slice(0, edgeChars)}...${text.slice(-edgeChars)}`
}

function emptyGroup(
  kind: ReferenceGroupKind,
  label: string,
  over: Partial<ReferenceGroupSnapshot> = {}
): ReferenceGroupSnapshot {
  return {
    kind,
    label,
    items: [],
    loading: false,
    truncated: false,
    error: null,
    ...over,
  }
}

function makeSnapshot(
  over: Partial<ReferenceSearchSnapshot> & {
    groups?: Partial<Record<ReferenceGroupKind, ReferenceGroupSnapshot>>
  } = {}
): ReferenceSearchSnapshot {
  const baseGroups = {
    agent: emptyGroup("agent", "Agents"),
    file: emptyGroup("file", "Files"),
    session: emptyGroup("session", "Sessions"),
    commit: emptyGroup("commit", "Commits"),
  }
  return {
    query: over.query ?? "",
    generation: over.generation ?? 1,
    patternError: over.patternError ?? false,
    groups: { ...baseGroups, ...over.groups },
  }
}

function item(
  reference: ReferenceAttrs & { uri: string },
  detail?: string | null,
  sourceOrdinal = 0,
  over: Partial<SuggestionItem> = {}
): SuggestionItem {
  return {
    reference,
    detail,
    selectable: true,
    freshness: "fresh",
    sourceOrdinal,
    regexRank: null,
    ...over,
  }
}

function file(
  name: string,
  over: Partial<SuggestionItem> = {}
): SuggestionItem {
  return item(
    {
      refType: "file",
      id: name,
      label: name,
      uri: `file:///repo/${name}`,
      meta: { fileKind: "file" },
    },
    // Detail differs from the label so getByText/name queries stay unambiguous.
    `path/${name}`,
    0,
    over
  )
}

function session(
  id: string,
  over: Partial<SuggestionItem> = {}
): SuggestionItem {
  return item(
    {
      refType: "session",
      id,
      label: `Session ${id}`,
      uri: `codeg://session/${id}`,
      meta: { agentType: "codex", status: "idle", branch: null },
    },
    null,
    Number(id) || 0,
    over
  )
}

function agent(
  id: string,
  label: string,
  over: Partial<SuggestionItem> = {}
): SuggestionItem {
  return item(
    {
      refType: "agent",
      id,
      label,
      uri: `codeg://agent/${id}`,
      meta: { agentType: id as "codex" },
    },
    null,
    0,
    over
  )
}

function snapshotWithFiles(
  files: SuggestionItem[],
  query = "a"
): ReferenceSearchSnapshot {
  return makeSnapshot({
    query,
    groups: {
      file: emptyGroup("file", "Files", { items: files }),
    },
  })
}

function snapshotWithSessions(
  sessions: SuggestionItem[],
  query = "s"
): ReferenceSearchSnapshot {
  return makeSnapshot({
    query,
    groups: {
      session: emptyGroup("session", "Sessions", { items: sessions }),
    },
  })
}

function snapshotWithAgents(
  agents: SuggestionItem[],
  query = ""
): ReferenceSearchSnapshot {
  return makeSnapshot({
    query,
    groups: {
      agent: emptyGroup("agent", "Agents", { items: agents }),
    },
  })
}

function fakeController(
  snapshot: ReferenceSearchSnapshot
): FakeReferenceSearchController {
  return new FakeReferenceSearchController(snapshot)
}

function validatingController(
  seed: SuggestionItem
): FakeReferenceSearchController {
  return new FakeReferenceSearchController(snapshotWithFiles([seed]), {
    autoResolveConfirm: false,
  })
}

const fileRef = {
  refType: "file" as const,
  id: "alpha.md",
  label: "alpha.md",
  uri: "file:///docs/alpha.md",
  meta: { fileKind: "file" as const },
}
const agentRef = {
  refType: "agent" as const,
  id: "codex",
  label: "Codex Helper",
  uri: "codeg://agent/codex",
  meta: { agentType: "codex" as const },
}
const agentRef2 = {
  refType: "agent" as const,
  id: "claude_code",
  label: "Claude Helper",
  uri: "codeg://agent/claude_code",
  meta: { agentType: "claude_code" as const },
}

const defaultSnapshot = makeSnapshot({
  query: "a",
  groups: {
    file: emptyGroup("file", "Files", {
      items: [item(fileRef, "docs/alpha.md", 0)],
    }),
    agent: emptyGroup("agent", "Agents", {
      items: [item(agentRef, null, 0), item(agentRef2, null, 1)],
    }),
  },
})

const emptySnapshot = makeSnapshot({ query: "a" })

const defaultState = {
  query: "a",
  range: { from: 1, to: 3 },
  getClientRect: () => null,
}

function mountPopup(
  overrides: {
    controller?: ReferenceSearchController | FakeReferenceSearchController
    state?: typeof defaultState
    onSelect?: ReturnType<typeof vi.fn>
    onClose?: ReturnType<typeof vi.fn>
    emptyLabel?: string
    loadingLabel?: string
    listboxLabel?: string
    moreLabel?: string
    countLabel?: (count: number) => string
    tabLabels?: Partial<Record<string, string>>
    onActiveOptionChange?: (optionId: string | null) => void
  } = {}
) {
  const ref = createRef<SuggestionPopupHandle>()
  const onSelect = overrides.onSelect ?? vi.fn()
  const onClose = overrides.onClose ?? vi.fn()
  const controller = overrides.controller ?? fakeController(defaultSnapshot)
  controller.markActive?.()
  render(
    <SuggestionPopup
      ref={ref}
      state={overrides.state ?? defaultState}
      controller={controller as ReferenceSearchController}
      onSelect={onSelect}
      onClose={onClose}
      emptyLabel={overrides.emptyLabel}
      loadingLabel={overrides.loadingLabel}
      listboxLabel={overrides.listboxLabel}
      moreLabel={overrides.moreLabel}
      countLabel={overrides.countLabel}
      tabLabels={overrides.tabLabels}
      onActiveOptionChange={overrides.onActiveOptionChange}
    />
  )
  return { ref, onSelect, onClose, controller }
}

function key(name: string, shiftKey = false): KeyboardEvent {
  return { key: name, shiftKey } as KeyboardEvent
}

function activeUri(): string | null {
  const active = document.querySelector(
    '[role="option"][aria-selected="true"]'
  ) as HTMLElement | null
  return active?.dataset.uri ?? null
}

describe("SuggestionPopup", () => {
  afterEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
  })

  it("renders the active (agent-first) tab's options plus a four-tab strip", async () => {
    mountPopup()
    expect(await screen.findByText("Codex Helper")).toBeInTheDocument()
    expect(screen.getByText("Claude Helper")).toBeInTheDocument()
    expect(screen.queryByText("alpha.md")).toBeNull()
    expect(screen.getAllByRole("tab")).toHaveLength(4)
    expect(screen.getByRole("tab", { selected: true })).toHaveAccessibleName(
      /Agents/
    )
  })

  it("widens file suggestions and middle-shortens over-limit labels and paths", async () => {
    const longPath = `Client/Docs/AllWorkDocs/${LONG_REFERENCE_TEXT}`
    const controller = fakeController(
      snapshotWithFiles([
        item(
          {
            ...fileRef,
            id: LONG_REFERENCE_TEXT,
            label: LONG_REFERENCE_TEXT,
            uri: `file:///repo/${LONG_REFERENCE_TEXT}`,
          },
          longPath
        ),
      ])
    )
    mountPopup({ controller })
    const panel = screen.getByTestId("mention-popup")
    expect(panel).toHaveClass("w-[52rem]")
    expect(
      await screen.findByText(expectedMiddleTruncate(LONG_REFERENCE_TEXT))
    ).toBeInTheDocument()
    expect(
      screen.getByText(expectedMiddleTruncate(longPath))
    ).toBeInTheDocument()
    expect(screen.queryByText(LONG_REFERENCE_TEXT)).toBeNull()
    expect(screen.queryByText(longPath)).toBeNull()
  })

  it("shows an empty state (but keeps the tabs) when there are no matches", async () => {
    mountPopup({
      controller: fakeController(emptySnapshot),
      emptyLabel: "Nothing",
    })
    const panel = screen.getByTestId("mention-popup")
    expect(await within(panel).findByText("Nothing")).toBeInTheDocument()
    expect(screen.getAllByRole("tab")).toHaveLength(4)
  })

  it("selects the active tab's highlighted row on Enter (default = first agent)", async () => {
    const { ref, onSelect } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => {
      expect(ref.current?.onKeyDown(key("Enter"))).toBe(true)
    })
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith(agentRef, defaultState.range)
    )
  })

  it("moves the selection with ArrowDown within the active tab", async () => {
    const { ref, onSelect } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => ref.current?.onKeyDown(key("ArrowDown")))
    act(() => ref.current?.onKeyDown(key("Enter")))
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith(agentRef2, defaultState.range)
    )
  })

  it("wraps the selection with ArrowUp from the first row", async () => {
    const { ref, onSelect } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => ref.current?.onKeyDown(key("ArrowUp")))
    act(() => ref.current?.onKeyDown(key("Enter")))
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith(agentRef2, defaultState.range)
    )
  })

  it("switches to the next tab with Tab and reveals its options", async () => {
    const { ref, onSelect } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => {
      expect(ref.current?.onKeyDown(key("Tab"))).toBe(true)
    })
    expect(await screen.findByText("alpha.md")).toBeInTheDocument()
    expect(screen.queryByText("Codex Helper")).toBeNull()
    expect(screen.getByRole("tab", { selected: true })).toHaveAccessibleName(
      /Files/
    )
    expect(onSelect).not.toHaveBeenCalled()
  })

  it("wraps to the last tab with Shift+Tab", async () => {
    const { ref } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => ref.current?.onKeyDown(key("Tab", true)))
    expect(screen.getByRole("tab", { selected: true })).toHaveAccessibleName(
      /Commits/
    )
  })

  it("switches tabs on click, preventing default on mousedown to keep editor focus", async () => {
    mountPopup()
    await screen.findByText("Codex Helper")
    const filesTab = screen.getByRole("tab", { name: /Files/ })
    const down = new MouseEvent("mousedown", {
      bubbles: true,
      cancelable: true,
    })
    act(() => {
      filesTab.dispatchEvent(down)
    })
    expect(down.defaultPrevented).toBe(true)
    act(() => {
      fireEvent.click(filesTab)
    })
    expect(await screen.findByText("alpha.md")).toBeInTheDocument()
    expect(screen.queryByText("Codex Helper")).toBeNull()
  })

  it("closes on Escape and reports the key as consumed", async () => {
    const { ref, onClose } = mountPopup()
    await screen.findByText("Codex Helper")
    let consumed = false
    act(() => {
      consumed = ref.current?.onKeyDown(key("Escape")) ?? false
    })
    expect(consumed).toBe(true)
    expect(onClose).toHaveBeenCalled()
  })

  it("does not consume unrelated keys", async () => {
    const { ref } = mountPopup()
    await screen.findByText("Codex Helper")
    expect(ref.current?.onKeyDown(key("x"))).toBe(false)
  })

  it("does not insert a non-selectable continuity row on Enter", async () => {
    const controller = fakeController(
      snapshotWithAgents([
        agent("codex", "Codex Helper", {
          selectable: false,
          freshness: "cache",
        }),
      ])
    )
    const { ref, onSelect } = mountPopup({ controller })
    await screen.findByText("Codex Helper")
    act(() => ref.current?.onKeyDown(key("Enter")))
    expect(onSelect).not.toHaveBeenCalled()
  })

  it("selects on click (mousedown) and prevents default to keep editor focus", async () => {
    const { onSelect } = mountPopup()
    const option = await screen.findByRole("option", { name: "Codex Helper" })
    const event = new MouseEvent("mousedown", {
      bubbles: true,
      cancelable: true,
    })
    act(() => {
      option.dispatchEvent(event)
    })
    expect(event.defaultPrevented).toBe(true)
    await waitFor(() =>
      expect(onSelect).toHaveBeenCalledWith(agentRef, defaultState.range)
    )
  })

  it("positions and reveals the caret-anchored panel once measured", async () => {
    render(
      <SuggestionPopup
        ref={createRef<SuggestionPopupHandle>()}
        state={{
          query: "a",
          range: { from: 1, to: 3 },
          getClientRect: () =>
            ({ left: 100, top: 600, bottom: 620 }) as DOMRect,
        }}
        controller={
          fakeController(defaultSnapshot) as ReferenceSearchController
        }
        onSelect={vi.fn()}
        onClose={vi.fn()}
      />
    )
    await screen.findByText("Codex Helper")
    const container = screen.getByTestId("mention-popup")
      .parentElement as HTMLElement
    expect(container.style.visibility).toBe("visible")
    expect(container.style.position).toBe("fixed")
    expect(container.dataset.placement).toBeTruthy()
  })

  it("clamps the rendered panel coordinates into the viewport", async () => {
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
      width: 320,
      height: 288,
    } as DOMRect)
    render(
      <SuggestionPopup
        ref={createRef<SuggestionPopupHandle>()}
        state={{
          query: "a",
          range: { from: 1, to: 3 },
          getClientRect: () =>
            ({ left: 1000, top: 600, bottom: 620 }) as DOMRect,
        }}
        controller={
          fakeController(defaultSnapshot) as ReferenceSearchController
        }
        onSelect={vi.fn()}
        onClose={vi.fn()}
      />
    )
    await screen.findByText("Codex Helper")
    const container = screen.getByTestId("mention-popup")
      .parentElement as HTMLElement
    expect(container.style.left).toBe("696px")
    expect(container.style.top).toBe("308px")
    expect(container.dataset.placement).toBe("above")
  })

  it("re-anchors to the live caret rect on resize (not a stale snapshot)", async () => {
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
      width: 320,
      height: 288,
    } as DOMRect)
    let caretLeft = 100
    const getClientRect = vi.fn(
      () => ({ left: caretLeft, top: 600, bottom: 620 }) as DOMRect
    )
    render(
      <SuggestionPopup
        ref={createRef<SuggestionPopupHandle>()}
        state={{ query: "a", range: { from: 1, to: 3 }, getClientRect }}
        controller={
          fakeController(defaultSnapshot) as ReferenceSearchController
        }
        onSelect={vi.fn()}
        onClose={vi.fn()}
      />
    )
    await screen.findByText("Codex Helper")
    const container = screen.getByTestId("mention-popup")
      .parentElement as HTMLElement
    expect(container.style.left).toBe("100px")
    const before = getClientRect.mock.calls.length
    caretLeft = 300
    act(() => {
      window.dispatchEvent(new Event("resize"))
    })
    expect(getClientRect.mock.calls.length).toBeGreaterThan(before)
    expect(container.style.left).toBe("300px")
  })

  it("exposes listbox + option roles with the active option selected", async () => {
    mountPopup({ listboxLabel: "Mentions" })
    await screen.findByText("Codex Helper")
    const listbox = screen.getByRole("listbox", { name: "Mentions: Agents" })
    expect(listbox).toHaveAttribute("id", "mention-listbox")
    const options = within(listbox).getAllByRole("option")
    expect(options).toHaveLength(2)
    expect(options[0]).toHaveAttribute("aria-selected", "true")
    expect(options[0]).toHaveAttribute("id", "mention-option-agent-0")
    expect(options[0]).toHaveAttribute("data-uri", agentRef.uri)
    expect(options[1]).toHaveAttribute("aria-selected", "false")
    expect(options[1]).toHaveAttribute("id", "mention-option-agent-1")
  })

  it("keeps the decorative icon out of the option's accessible name", async () => {
    mountPopup()
    await screen.findByText("Codex Helper")
    expect(
      screen.getByRole("option", { name: "Codex Helper" })
    ).toBeInTheDocument()
  })

  it("moves aria-selected with the keyboard", async () => {
    const { ref } = mountPopup()
    await screen.findByText("Codex Helper")
    act(() => ref.current?.onKeyDown(key("ArrowDown")))
    const options = screen
      .getByTestId("mention-popup")
      .querySelectorAll('[role="option"]')
    expect(options[0]).toHaveAttribute("aria-selected", "false")
    expect(options[1]).toHaveAttribute("aria-selected", "true")
  })

  it("announces the active tab + result count via a polite live region", async () => {
    mountPopup()
    await screen.findByText("Codex Helper")
    const status = screen.getByRole("status")
    expect(status).toHaveAttribute("aria-live", "polite")
    expect(status).toHaveTextContent("Agents: 2 results")
  })

  it("reports the active option id to the host for aria-activedescendant", async () => {
    const onActiveOptionChange = vi.fn()
    mountPopup({ onActiveOptionChange })
    await screen.findByText("Codex Helper")
    expect(onActiveOptionChange).toHaveBeenLastCalledWith(
      "mention-option-agent-0"
    )
  })

  it("reports a null active option while loading or empty", async () => {
    const onActiveOptionChange = vi.fn()
    mountPopup({
      controller: fakeController(emptySnapshot),
      onActiveOptionChange,
      emptyLabel: "None",
    })
    const panel = screen.getByTestId("mention-popup")
    await within(panel).findByText("None")
    expect(onActiveOptionChange).toHaveBeenLastCalledWith(null)
  })

  it("shows a non-selectable, aria-hidden hint for a truncated active tab", async () => {
    const controller = fakeController(
      makeSnapshot({
        query: "a",
        groups: {
          agent: emptyGroup("agent", "Agents", {
            items: [item(agentRef)],
            truncated: true,
          }),
        },
      })
    )
    mountPopup({ controller, moreLabel: "More — keep typing" })
    await screen.findByText("Codex Helper")
    const panel = screen.getByTestId("mention-popup")
    const hint = within(panel).getByText("More — keep typing")
    expect(hint).toHaveAttribute("aria-hidden", "true")
    expect(panel.querySelectorAll('[role="option"]')).toHaveLength(1)
    expect(screen.getByRole("status")).toHaveTextContent("More — keep typing")
  })

  it("keeps the selected URI when pages insert and rerank around it", async () => {
    const controller = fakeController(
      snapshotWithFiles([file("b.ts"), file("c.ts")])
    )
    const { ref } = mountPopup({ controller })
    await screen.findByRole("option", { name: /^b\.ts/ })
    // File tab is first non-empty when agents are empty.
    act(() => ref.current?.onKeyDown(key("ArrowDown")))
    expect(activeUri()).toBe("file:///repo/c.ts")
    act(() => {
      controller.publish(
        snapshotWithFiles([file("a.ts"), file("b.ts"), file("c.ts")])
      )
    })
    expect(activeUri()).toBe("file:///repo/c.ts")
  })

  it("moves explicit invalidation to same index then previous then none", () => {
    const controller = fakeController(
      snapshotWithSessions([session("1"), session("2"), session("3")])
    )
    const { ref } = mountPopup({
      controller,
      state: {
        query: "s",
        range: { from: 1, to: 3 },
        getClientRect: () => null,
      },
    })
    act(() => ref.current?.onKeyDown(key("ArrowDown")))
    expect(activeUri()).toBe("codeg://session/2")
    act(() => {
      controller.publish(snapshotWithSessions([session("1"), session("3")]))
    })
    expect(activeUri()).toBe("codeg://session/3")
    act(() => {
      controller.publish(snapshotWithSessions([session("1")]))
    })
    expect(activeUri()).toBe("codeg://session/1")
    act(() => {
      controller.publish(snapshotWithSessions([]))
    })
    expect(activeUri()).toBeNull()
  })

  it("consumes Enter while validation is pending and inserts only a permitted result", async () => {
    const controller = validatingController(file("cached.ts"))
    const { ref, onSelect } = mountPopup({ controller })
    await screen.findByRole("option", { name: /^cached\.ts/ })
    act(() => expect(ref.current?.onKeyDown(key("Enter"))).toBe(true))
    act(() => expect(ref.current?.onKeyDown(key("Enter"))).toBe(true))
    expect(controller.confirmCallCount()).toBe(1)
    expect(onSelect).not.toHaveBeenCalled()
    act(() => {
      controller.resolveConfirmation(file("cached.ts").reference)
    })
    await waitFor(() => expect(onSelect).toHaveBeenCalledOnce())
    expect(onSelect).toHaveBeenCalledWith(
      file("cached.ts").reference,
      defaultState.range
    )
  })

  it("known_negative_confirmation_keeps_picker_open_on_nearest_survivor", async () => {
    const seed = [file("a.ts"), file("b.ts"), file("c.ts")]
    const controller = new FakeReferenceSearchController(
      snapshotWithFiles(seed),
      { autoResolveConfirm: false }
    )
    const { ref, onSelect, onClose } = mountPopup({ controller })
    await screen.findByRole("option", { name: /^b\.ts/ })
    act(() => ref.current?.onKeyDown(key("ArrowDown")))
    expect(activeUri()).toBe("file:///repo/b.ts")
    act(() => {
      expect(ref.current?.onKeyDown(key("Enter"))).toBe(true)
    })
    act(() => {
      controller.publish(snapshotWithFiles([file("a.ts"), file("c.ts")]))
    })
    act(() => {
      controller.resolveConfirmation(null)
    })
    await waitFor(() => expect(controller.confirmCallCount()).toBe(1))
    expect(onSelect).not.toHaveBeenCalled()
    expect(onClose).not.toHaveBeenCalled()
    expect(activeUri()).toBe("file:///repo/c.ts")
    // Can confirm the survivor next.
    controller.enableAutoResolveConfirm()
    act(() => {
      expect(ref.current?.onKeyDown(key("Enter"))).toBe(true)
    })
    await waitFor(() => expect(onSelect).toHaveBeenCalledOnce())
    expect(onSelect.mock.calls[0][0].uri).toBe("file:///repo/c.ts")
  })

  it("settled_confirmation_cannot_insert_after_the_same_query_moves_range", async () => {
    const controller = validatingController(file("cached.ts"))
    const onSelect = vi.fn()
    const onClose = vi.fn()
    const ref = createRef<SuggestionPopupHandle>()

    function Harness() {
      const [state, setState] = useState({
        query: "a",
        range: { from: 1, to: 3 },
        getClientRect: () => null,
      })
      return (
        <>
          <button
            type="button"
            data-testid="remap-range"
            onClick={() =>
              setState((s) => ({ ...s, range: { from: 5, to: 7 } }))
            }
          >
            remap
          </button>
          <SuggestionPopup
            ref={ref}
            state={state}
            controller={controller as ReferenceSearchController}
            onSelect={onSelect}
            onClose={onClose}
          />
        </>
      )
    }

    render(<Harness />)
    await screen.findByRole("option", { name: /^cached\.ts/ })
    act(() => {
      expect(ref.current?.onKeyDown(key("Enter"))).toBe(true)
    })
    act(() => {
      fireEvent.click(screen.getByTestId("remap-range"))
    })
    act(() => {
      controller.resolveConfirmation(file("cached.ts").reference)
    })
    await waitFor(() => expect(controller.confirmCallCount()).toBe(1))
    // Allow the settled confirmation microtask to run.
    await act(async () => {
      await Promise.resolve()
    })
    expect(onSelect).not.toHaveBeenCalled()
    expect(onClose).not.toHaveBeenCalled()
  })

  it("bare_query_publishes_catalog_without_the_legacy_fetch_debounce", async () => {
    vi.useFakeTimers()
    const setTimeoutSpy = vi.spyOn(globalThis, "setTimeout")
    const controller = fakeController(
      snapshotWithAgents([agent("codex", "Codex Helper")], "")
    )
    // Start empty; setQuery("") will re-publish (fake keeps items).
    controller.publish(snapshotWithAgents([agent("codex", "Codex Helper")], ""))
    mountPopup({
      controller,
      state: {
        query: "",
        range: { from: 1, to: 2 },
        getClientRect: () => null,
      },
    })
    expect(screen.getByText("Codex Helper")).toBeInTheDocument()
    expect(controller.queries).toContain("")
    // Legacy 150ms search debounce must not be scheduled.
    const delays = setTimeoutSpy.mock.calls.map((call) => call[1])
    expect(delays).not.toContain(150)
    act(() => {
      vi.advanceTimersByTime(200)
    })
    expect(screen.getByText("Codex Helper")).toBeInTheDocument()
  })
})
