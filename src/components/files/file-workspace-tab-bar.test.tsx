import { act, cleanup, render } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"
import { DEFAULT_SHORTCUTS } from "@/lib/keyboard-shortcuts"

const closeFileTab = vi.fn()
const closeAllFileTabs = vi.fn()
const switchFileTab = vi.fn()
const closeOtherFileTabs = vi.fn()
const reorderFileTabs = vi.fn()
const toggleFileTabPreview = vi.fn()
const toggleFilesMaximized = vi.fn()

const viewState = {
  mode: "fusion" as string,
  activePane: "files" as string,
  filesMaximized: false,
}

const tabsState = {
  fileTabs: [] as FileWorkspaceTab[],
  activeFileTabId: null as string | null,
  previewFileTabIds: new Set<string>(),
}

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/lib/platform", () => ({
  openPath: vi.fn(),
}))

vi.mock("@/hooks/use-is-coarse-pointer", () => ({
  useIsCoarsePointer: () => false,
}))

vi.mock("@/hooks/use-mobile", () => ({
  useIsMobile: () => false,
}))

vi.mock("@/hooks/use-long-press-drag", () => ({
  useLongPressDrag: () => ({
    dragControls: undefined,
    gestureHandlers: {},
  }),
}))

vi.mock("@/hooks/use-shortcut-settings", () => ({
  useShortcutSettings: () => ({
    shortcuts: DEFAULT_SHORTCUTS,
    updateShortcut: vi.fn(),
    resetShortcuts: vi.fn(),
  }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceView: () => viewState,
  useWorkspaceFileTabs: () => tabsState,
  useWorkspaceActions: () => ({
    switchFileTab,
    closeFileTab,
    closeOtherFileTabs,
    closeAllFileTabs,
    reorderFileTabs,
    toggleFileTabPreview,
    toggleFilesMaximized,
  }),
}))

vi.mock("@/components/ui/context-menu", () => ({
  ContextMenu: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  ContextMenuTrigger: ({ children }: { children: React.ReactNode }) => (
    <>{children}</>
  ),
  ContextMenuContent: () => null,
  ContextMenuItem: () => null,
  ContextMenuSeparator: () => null,
}))

vi.mock("motion/react", () => ({
  Reorder: {
    Group: ({
      children,
    }: {
      children: React.ReactNode
      [key: string]: unknown
    }) => <div role="tablist">{children}</div>,
    Item: ({
      children,
      "data-file-tab-id": dataFileTabId,
    }: {
      children: React.ReactNode
      "data-file-tab-id"?: string
      [key: string]: unknown
    }) => <div data-file-tab-id={dataFileTabId}>{children}</div>,
  },
}))

import {
  FileWorkspaceTabBar,
  shouldHandleFilesEscape,
} from "./file-workspace-tab-bar"

function makeFileTab(
  overrides: Partial<FileWorkspaceTab> & { id: string } = { id: "file-1" }
): FileWorkspaceTab {
  return {
    id: overrides.id,
    kind: "file",
    folderId: null,
    title: overrides.title ?? "readme.md",
    path: overrides.path ?? "/proj/readme.md",
    language: overrides.language ?? "markdown",
    content: overrides.content ?? "",
    loading: false,
    isDirty: overrides.isDirty ?? false,
    description: overrides.description ?? null,
    hasLoadedSuccessfully: true,
    ...overrides,
  }
}

function baseCtx(
  overrides: Partial<{
    mode: string
    activePane: string
    filesMaximized: boolean
    activeFileTabId: string | null
  }> = {}
) {
  return {
    mode: "fusion",
    activePane: "files",
    filesMaximized: false,
    activeFileTabId: "file-1",
    ...overrides,
  }
}

function escapeEvent(overrides: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return {
    key: "Escape",
    defaultPrevented: false,
    preventDefault: vi.fn(),
    ...overrides,
  } as unknown as KeyboardEvent
}

describe("shouldHandleFilesEscape", () => {
  afterEach(() => {
    document.body.innerHTML = ""
  })

  it("returns true for Escape in fusion files pane with an active tab", () => {
    expect(shouldHandleFilesEscape(escapeEvent(), baseCtx())).toBe(true)
  })

  it("returns true when files are maximized even if activePane is not files", () => {
    expect(
      shouldHandleFilesEscape(
        escapeEvent(),
        baseCtx({ activePane: "conversation", filesMaximized: true })
      )
    ).toBe(true)
  })

  it("returns false when defaultPrevented is true", () => {
    expect(
      shouldHandleFilesEscape(
        escapeEvent({ defaultPrevented: true }),
        baseCtx()
      )
    ).toBe(false)
  })

  it("returns false for non-Escape keys", () => {
    expect(
      shouldHandleFilesEscape(escapeEvent({ key: "Enter" }), baseCtx())
    ).toBe(false)
  })

  it("returns false when mode is not fusion", () => {
    expect(
      shouldHandleFilesEscape(escapeEvent(), baseCtx({ mode: "conversation" }))
    ).toBe(false)
  })

  it("returns false when wrong pane and not maximized", () => {
    expect(
      shouldHandleFilesEscape(
        escapeEvent(),
        baseCtx({ activePane: "conversation", filesMaximized: false })
      )
    ).toBe(false)
  })

  it("returns false when there is no active file tab", () => {
    expect(
      shouldHandleFilesEscape(
        escapeEvent(),
        baseCtx({ activeFileTabId: null })
      )
    ).toBe(false)
  })

  it("returns false when an open dialog is present", () => {
    const dialog = document.createElement("div")
    dialog.setAttribute("role", "dialog")
    dialog.setAttribute("data-state", "open")
    document.body.appendChild(dialog)

    expect(shouldHandleFilesEscape(escapeEvent(), baseCtx())).toBe(false)
  })

  it("returns false when an alertdialog is present", () => {
    const alert = document.createElement("div")
    alert.setAttribute("role", "alertdialog")
    document.body.appendChild(alert)

    expect(shouldHandleFilesEscape(escapeEvent(), baseCtx())).toBe(false)
  })

  it("returns false when focus is inside a radix popper", () => {
    const wrapper = document.createElement("div")
    wrapper.setAttribute("data-radix-popper-content-wrapper", "")
    const button = document.createElement("button")
    wrapper.appendChild(button)
    document.body.appendChild(wrapper)
    button.focus()

    expect(shouldHandleFilesEscape(escapeEvent(), baseCtx())).toBe(false)
  })

  it("returns false when focus is inside menu content", () => {
    const menu = document.createElement("div")
    menu.setAttribute("data-radix-menu-content", "")
    const item = document.createElement("button")
    menu.appendChild(item)
    document.body.appendChild(menu)
    item.focus()

    expect(shouldHandleFilesEscape(escapeEvent(), baseCtx())).toBe(false)
  })
})

describe("FileWorkspaceTabBar Escape", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    viewState.mode = "fusion"
    viewState.activePane = "files"
    viewState.filesMaximized = false
    tabsState.fileTabs = [makeFileTab({ id: "file-1" })]
    tabsState.activeFileTabId = "file-1"
    tabsState.previewFileTabIds = new Set()
    document.body.innerHTML = ""
  })

  afterEach(() => {
    cleanup()
    document.body.innerHTML = ""
  })

  function pressEscape(init: KeyboardEventInit = {}) {
    const event = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
      ...init,
    })
    act(() => {
      window.dispatchEvent(event)
    })
    return event
  }

  it("calls closeFileTab for the active tab on Escape", () => {
    render(<FileWorkspaceTabBar />)
    const event = pressEscape()
    expect(closeFileTab).toHaveBeenCalledTimes(1)
    expect(closeFileTab).toHaveBeenCalledWith("file-1")
    expect(event.defaultPrevented).toBe(true)
  })

  it("does not call closeFileTab when defaultPrevented", () => {
    render(<FileWorkspaceTabBar />)
    const event = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    })
    // Simulate a prior handler consuming Escape (e.g. Monaco suggest).
    Object.defineProperty(event, "defaultPrevented", {
      get: () => true,
    })
    act(() => {
      window.dispatchEvent(event)
    })
    expect(closeFileTab).not.toHaveBeenCalled()
  })

  it("does not call closeFileTab when an open dialog is in the document", () => {
    const dialog = document.createElement("div")
    dialog.setAttribute("role", "dialog")
    dialog.setAttribute("data-state", "open")
    document.body.appendChild(dialog)

    render(<FileWorkspaceTabBar />)
    pressEscape()
    expect(closeFileTab).not.toHaveBeenCalled()
  })

  it("does not call closeFileTab on the wrong pane", () => {
    viewState.activePane = "conversation"
    viewState.filesMaximized = false
    render(<FileWorkspaceTabBar />)
    pressEscape()
    expect(closeFileTab).not.toHaveBeenCalled()
  })

  it("still calls closeFileTab for a dirty tab (confirm lives in closeFileTab)", () => {
    tabsState.fileTabs = [makeFileTab({ id: "file-dirty", isDirty: true })]
    tabsState.activeFileTabId = "file-dirty"
    render(<FileWorkspaceTabBar />)
    pressEscape()
    expect(closeFileTab).toHaveBeenCalledTimes(1)
    expect(closeFileTab).toHaveBeenCalledWith("file-dirty")
  })
})
