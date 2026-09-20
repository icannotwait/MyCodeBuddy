import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"
import type { DetectedService } from "@/lib/browser/types"
import enMessages from "@/i18n/messages/en.json"
import { DEFAULT_SHORTCUTS } from "@/lib/keyboard-shortcuts"

type WorkspaceActions = ReturnType<
  typeof import("@/contexts/workspace-context").useWorkspaceActions
>
type TranslateDocument = typeof import("@/lib/api").translateDocument
type SaveTranslationAs = typeof import("@/lib/api").saveTranslationAs

const closeFileTab = vi.fn()
const closeAllFileTabs = vi.fn()
const switchFileTab = vi.fn()
const closeOtherFileTabs = vi.fn()
const reorderFileTabs = vi.fn()
const toggleFileTabPreview = vi.fn()
const toggleFilesMaximized = vi.fn()
const beginTranslateRequest = vi.fn<WorkspaceActions["beginTranslateRequest"]>(
  () => 1
)
const openTranslationResultTab = vi.fn<
  WorkspaceActions["openTranslationResultTab"]
>(() => "translate:file-1:zh_cn:1")
const openFilePreview = vi.fn<WorkspaceActions["openFilePreview"]>(
  async () => ({
    ok: true,
    tabId: "file:/ws/README.zh_cn.md",
  })
)
const openBrowserTab = vi.fn(() => "browser:new")

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

const activeFolderState = vi.hoisted(() => ({
  id: 7 as number | null,
}))

const experienceState = vi.hoisted(() => ({
  documentTranslateAgent: "codex" as string | null,
}))

const translateDocument = vi.hoisted(() =>
  vi.fn<TranslateDocument>(async () => ({
    translatedContent: "你好",
    locale: "zh_cn",
    format: "markdown" as const,
  }))
)

const saveTranslationAs = vi.hoisted(() =>
  vi.fn<SaveTranslationAs>(async () => ({
    absolutePath: "/ws/README.zh_cn.md",
  }))
)

const toastMock = vi.hoisted(() => ({
  error: vi.fn(),
}))

vi.mock("next-intl", async () => {
  const { default: messages } = await import("@/i18n/messages/en.json")
  const fileWorkspace = messages.Folder.fileWorkspace as Record<string, string>
  return {
    useTranslations: (namespace?: string) => {
      return (key: string, values?: Record<string, string | number>) => {
        if (namespace === "Folder.fileWorkspace") {
          const template = fileWorkspace[key]
          if (typeof template === "string") {
            return template.replace(/\{(\w+)\}/g, (_, name: string) =>
              values?.[name] != null ? String(values[name]) : `{${name}}`
            )
          }
        }
        return key
      }
    },
    useLocale: () => "zh-CN",
  }
})

vi.mock("sonner", () => ({
  toast: {
    error: toastMock.error,
  },
}))

const browserMocks = vi.hoisted(() => ({
  openFileDialog: vi.fn(() => Promise.resolve<string | string[] | null>(null)),
  browserListServices: vi.fn(() => Promise.resolve<DetectedService[]>([])),
  browserState: null as { url: string; title: string } | null,
}))

let desktop = true
let remoteDesktop = false
let browserAvailable = true

vi.mock("@/lib/platform", () => ({
  openPath: vi.fn(),
  openFileDialog: browserMocks.openFileDialog,
}))

vi.mock("@/lib/transport", () => ({
  isDesktop: () => desktop,
  isRemoteDesktopMode: () => remoteDesktop,
}))

vi.mock("@/lib/browser/browser-api", () => ({
  browserListServices: browserMocks.browserListServices,
}))

vi.mock("@/lib/browser/use-browser-capabilities", () => ({
  useBrowserCapabilities: () => ({ available: browserAvailable }),
}))

vi.mock("@/lib/browser/browser-tab-store", () => ({
  useBrowserTabState: () => browserMocks.browserState,
}))

vi.mock("@/components/browser/browser-agent-access", () => ({
  AGENT_MARK: "agent-mark",
}))

vi.mock("@/lib/api", () => ({
  translateDocument,
  saveTranslationAs,
}))

vi.mock("@/stores/conversation-experience-store", () => ({
  useConversationExperienceStore: (
    selector: (s: {
      settings: { document_translate_agent: string | null } | null
    }) => unknown
  ) =>
    selector({
      settings: {
        document_translate_agent: experienceState.documentTranslateAgent,
      },
    }),
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolderId: activeFolderState.id,
    activeFolder:
      activeFolderState.id != null
        ? { id: activeFolderState.id, path: "/ws", name: "ws" }
        : null,
  }),
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
    beginTranslateRequest,
    openTranslationResultTab,
    openFilePreview,
    openBrowserTab,
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
  useDragControls: () => ({ start: vi.fn() }),
}))

import { FileWorkspaceTabBar } from "./file-workspace-tab-bar"

function makeFileTab(
  overrides: Partial<FileWorkspaceTab> & { id: string } = { id: "file-1" }
): FileWorkspaceTab {
  return {
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

describe("FileWorkspaceTabBar Translate", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    beginTranslateRequest.mockReturnValue(1)
    openTranslationResultTab.mockReturnValue("translate:file-1:zh_cn:1")
    translateDocument.mockReset()
    translateDocument.mockResolvedValue({
      translatedContent: "你好",
      locale: "zh_cn",
      format: "markdown",
    })
    experienceState.documentTranslateAgent = "codex"
    viewState.mode = "fusion"
    viewState.activePane = "files"
    viewState.filesMaximized = false
    tabsState.fileTabs = [
      makeFileTab({
        id: "file-1",
        title: "readme.md",
        path: "/proj/readme.md",
        language: "markdown",
        content: "# Hello world",
      }),
    ]
    tabsState.activeFileTabId = "file-1"
    tabsState.previewFileTabIds = new Set()
  })

  afterEach(() => {
    cleanup()
  })

  it("shows Translate for eligible markdown tabs", () => {
    render(<FileWorkspaceTabBar />)
    expect(screen.getByTestId("translate-document")).toBeTruthy()
  })

  it("hides Translate for non-eligible tabs", () => {
    tabsState.fileTabs = [
      makeFileTab({
        id: "file-ts",
        title: "a.ts",
        path: "/proj/a.ts",
        language: "typescript",
        content: "const x = 1",
      }),
    ]
    tabsState.activeFileTabId = "file-ts"
    render(<FileWorkspaceTabBar />)
    expect(screen.queryByTestId("translate-document")).toBeNull()
  })

  it("toasts agent-not-configured without calling the API", async () => {
    experienceState.documentTranslateAgent = null
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("translate-document").click()
    })
    const notConfigured =
      enMessages.Folder.fileWorkspace.translateAgentNotConfigured
    expect(toastMock.error).toHaveBeenCalledWith(notConfigured)
    expect(notConfigured.toLowerCase()).toContain("document translation agent")
    expect(notConfigured.toLowerCase()).not.toContain("automatic title")
    expect(translateDocument).not.toHaveBeenCalled()
    expect(beginTranslateRequest).not.toHaveBeenCalled()
  })

  it("snapshots content at click so later edits do not change the payload", async () => {
    const tab = tabsState.fileTabs[0]
    render(<FileWorkspaceTabBar />)

    await act(async () => {
      screen.getByTestId("translate-document").click()
      // Mutate the tab after click, as if the user typed while in-flight.
      tab.content = "# EDITED after click"
    })

    expect(translateDocument).toHaveBeenCalledTimes(1)
    expect(translateDocument.mock.calls[0]?.[0]).toMatchObject({
      content: "# Hello world",
      format: "markdown",
      locale: "zh_cn",
      displayName: "readme.md",
    })
    expect(beginTranslateRequest).toHaveBeenCalledWith("file-1")
    expect(openTranslationResultTab).toHaveBeenCalledWith(
      expect.objectContaining({
        sourceTabId: "file-1",
        requestGen: 1,
        content: "你好",
        locale: "zh_cn",
        format: "markdown",
        sourcePath: "/proj/readme.md",
        sourceTitle: "readme.md",
      })
    )
  })

  it("disables the button while busy and ignores a second click", async () => {
    let resolveTranslate!: (value: {
      translatedContent: string
      locale: string
      format: "markdown"
    }) => void
    translateDocument.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveTranslate = resolve
        })
    )

    render(<FileWorkspaceTabBar />)
    const button = screen.getByTestId("translate-document")

    await act(async () => {
      button.click()
    })
    expect(button).toHaveProperty("disabled", true)

    await act(async () => {
      button.click()
    })
    expect(translateDocument).toHaveBeenCalledTimes(1)

    await act(async () => {
      resolveTranslate({
        translatedContent: "done",
        locale: "zh_cn",
        format: "markdown",
      })
    })
  })

  it("toasts when the result tab is not opened (stale gen / unmounted)", async () => {
    openTranslationResultTab.mockReturnValueOnce(null)
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("translate-document").click()
    })
    expect(openTranslationResultTab).toHaveBeenCalled()
    expect(toastMock.error).toHaveBeenCalledWith(
      enMessages.Folder.fileWorkspace.translateResultNotShown
    )
  })

  it("toasts when the API returns empty translated content", async () => {
    translateDocument.mockResolvedValueOnce({
      translatedContent: "   ",
      locale: "zh_cn",
      format: "markdown",
    })
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("translate-document").click()
    })
    expect(openTranslationResultTab).not.toHaveBeenCalled()
    expect(toastMock.error).toHaveBeenCalledWith(
      enMessages.Folder.fileWorkspace.translateFailed
    )
  })
})

describe("FileWorkspaceTabBar Save as translation", () => {
  const translationTab = (): FileWorkspaceTab =>
    ({
      id: "translate:file-1:zh_cn:1",
      kind: "file",
      title: "README.zh_cn.md",
      path: null,
      folderId: null,
      content: "你好世界",
      language: "markdown",
      loading: false,
      isDirty: false,
      saveState: "idle",
      saveError: null,
      readonly: true,
      stale: false,
      hasLoadedSuccessfully: true,
      transient: {
        type: "translation",
        sourceTabId: "file-1",
        sourcePath: "/proj/README.md",
        sourceContentHash: "abc",
        locale: "zh_cn",
        format: "markdown",
        suggestedName: "README.zh_cn.md",
      },
    }) as FileWorkspaceTab

  beforeEach(() => {
    vi.clearAllMocks()
    openFilePreview.mockResolvedValue({
      ok: true,
      tabId: "file:/ws/README.zh_cn.md",
    })
    saveTranslationAs.mockResolvedValue({
      absolutePath: "/ws/README.zh_cn.md",
    })
    activeFolderState.id = 7
    viewState.mode = "fusion"
    viewState.activePane = "files"
    viewState.filesMaximized = false
    tabsState.fileTabs = [translationTab()]
    tabsState.activeFileTabId = "translate:file-1:zh_cn:1"
    tabsState.previewFileTabIds = new Set()
    vi.spyOn(window, "prompt").mockReturnValue("README.zh_cn.md")
  })

  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
  })

  it("shows Save as for transient translation tabs", () => {
    render(<FileWorkspaceTabBar />)
    expect(screen.getByTestId("save-translation-as")).toBeTruthy()
    expect(screen.queryByTestId("translate-document")).toBeNull()
  })

  it("saves with folderId + suggested path, opens preview, closes transient on ok", async () => {
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("save-translation-as").click()
    })
    expect(saveTranslationAs).toHaveBeenCalledWith({
      folderId: 7,
      relativePath: "README.zh_cn.md",
      content: "你好世界",
    })
    expect(openFilePreview).toHaveBeenCalledWith("/ws/README.zh_cn.md", {
      reload: true,
      maximizeOnSuccess: false,
    })
    expect(closeFileTab).toHaveBeenCalledWith("translate:file-1:zh_cn:1")
  })

  it("does not close the transient tab when open settle is not ok", async () => {
    openFilePreview.mockResolvedValueOnce({
      ok: false,
      reason: "load",
    })
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("save-translation-as").click()
    })
    expect(saveTranslationAs).toHaveBeenCalled()
    expect(openFilePreview).toHaveBeenCalled()
    expect(closeFileTab).not.toHaveBeenCalled()
  })

  it("cancels when the user dismisses the prompt", async () => {
    vi.spyOn(window, "prompt").mockReturnValueOnce(null)
    render(<FileWorkspaceTabBar />)
    await act(async () => {
      screen.getByTestId("save-translation-as").click()
    })
    expect(saveTranslationAs).not.toHaveBeenCalled()
    expect(openFilePreview).not.toHaveBeenCalled()
  })
})

function fileTab(path: string): FileWorkspaceTab {
  return {
    id: `file:${path}`,
    kind: "file",
    folderId: null,
    title: path.split("/").pop() ?? path,
    description: path,
    path,
    language: "typescript",
    content: "",
    loading: false,
  } as FileWorkspaceTab
}

function htmlTab(path: string, content: string): FileWorkspaceTab {
  return {
    ...fileTab(path),
    language: "html",
    content,
  } as FileWorkspaceTab
}

function browserTab(url: string, title = url): FileWorkspaceTab {
  return {
    id: "browser:abc",
    kind: "browser",
    folderId: null,
    title,
    description: null,
    path: null,
    language: "browser",
    content: "",
    loading: false,
    readonly: true,
    browser: { initialUrl: url, openerTabId: null, profile: "default" },
  } as FileWorkspaceTab
}

function renderStrip() {
  return render(<FileWorkspaceTabBar />)
}

// jsdom has no `PointerEvent`; Radix reads `button` off the event, so a real
// `MouseEvent` under the pointer-event name is what opens the menu.
async function openAddMenu() {
  const trigger = screen.getByRole("button", { name: "New tab" })
  await act(async () => {
    for (const type of ["pointerdown", "pointerup", "click"]) {
      fireEvent(
        trigger,
        new MouseEvent(type, { bubbles: true, cancelable: true, button: 0 })
      )
    }
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}

beforeEach(() => {
  desktop = true
  remoteDesktop = false
  browserAvailable = true
  tabsState.fileTabs = [fileTab("/repo/a.ts")]
  tabsState.activeFileTabId = tabsState.fileTabs[0]?.id ?? null
  tabsState.previewFileTabIds = new Set<string>()
  browserMocks.browserState = null
  vi.clearAllMocks()
  browserMocks.openFileDialog.mockResolvedValue(null)
  browserMocks.browserListServices.mockResolvedValue([])
})

function detectedService(
  url: string,
  source: DetectedService["source"] = "terminal"
): DetectedService {
  return {
    url,
    origin: new URL(url).origin,
    authority: new URL(url).host,
    ownerWindow: "main",
    source,
    terminalId: "t1",
  }
}

describe("FileWorkspaceTabBar — the add-tab '+'", () => {
  it("opens an empty browser tab", async () => {
    renderStrip()
    await openAddMenu()
    await act(async () => {
      screen.getByRole("menuitem", { name: "Browser tab" }).click()
    })
    // The blank page, not a home page: an empty tab with a focused address bar.
    expect(openBrowserTab).toHaveBeenCalledWith("about:blank")
  })

  it("opens the file the native picker returned, as an absolute path", async () => {
    browserMocks.openFileDialog.mockResolvedValue("/repo/src/../src/notes.md")
    renderStrip()
    await openAddMenu()
    await act(async () => {
      screen.getByRole("menuitem", { name: "Open file…" }).click()
    })
    expect(browserMocks.openFileDialog).toHaveBeenCalledWith({ title: "Open file" })
    expect(openFilePreview).toHaveBeenCalledWith("/repo/src/notes.md")
  })

  it("does nothing when the picker is dismissed", async () => {
    renderStrip()
    await openAddMenu()
    await act(async () => {
      screen.getByRole("menuitem", { name: "Open file…" }).click()
    })
    expect(openFilePreview).not.toHaveBeenCalled()
  })

  it("drops the browser row when there is no built-in browser", async () => {
    browserAvailable = false
    renderStrip()
    await openAddMenu()
    expect(screen.queryByRole("menuitem", { name: "Browser tab" })).toBeNull()
    expect(
      screen.getByRole("menuitem", { name: "Open file…" })
    ).toBeInTheDocument()
  })

  it("drops the picker row where a native dialog would pick the wrong machine", async () => {
    remoteDesktop = true
    renderStrip()
    await openAddMenu()
    expect(screen.queryByRole("menuitem", { name: "Open file…" })).toBeNull()
    expect(
      screen.getByRole("menuitem", { name: "Browser tab" })
    ).toBeInTheDocument()
  })

  it("lists the local servers running now, and opens one", async () => {
    browserMocks.browserListServices.mockResolvedValue([
      detectedService("http://localhost:5173/"),
      detectedService("http://127.0.0.1:8000/", "agent"),
    ])
    renderStrip()
    await openAddMenu()
    // Asked when the menu opened, not held from an earlier answer: a server
    // that has stopped is already out of what the backend returns.
    expect(browserMocks.browserListServices).toHaveBeenCalledTimes(1)
    const entry = screen.getByRole("menuitem", { name: /localhost:5173/ })
    // Where it came from is on the row, so a server an agent started is not
    // mistaken for one the person started.
    expect(
      screen.getByRole("menuitem", { name: /127\.0\.0\.1:8000/ })
    ).toHaveTextContent("Agent")
    await act(async () => {
      entry.click()
    })
    expect(openBrowserTab).toHaveBeenCalledWith("http://localhost:5173/")
  })

  it("shows no local-server section when nothing is running", async () => {
    renderStrip()
    await openAddMenu()
    expect(screen.queryByText("Local servers")).toBeNull()
  })

  it("does not ask for local servers where there is no browser to open them in", async () => {
    browserAvailable = false
    renderStrip()
    await openAddMenu()
    expect(browserMocks.browserListServices).not.toHaveBeenCalled()
  })

  it("hides itself entirely rather than opening an empty menu", () => {
    desktop = false
    browserAvailable = false
    renderStrip()
    expect(screen.queryByRole("button", { name: "New tab" })).toBeNull()
    // The strip itself still renders — this is the button's own gate.
    expect(screen.getByRole("tablist")).toBeInTheDocument()
  })
})

describe("FileWorkspaceTabBar — an empty browser tab", () => {
  it("names itself instead of showing 'about:blank'", () => {
    tabsState.fileTabs = [browserTab("about:blank")]
    renderStrip()
    expect(screen.getByRole("tab")).toHaveTextContent("New tab")
    expect(screen.getByRole("tab")).not.toHaveTextContent("about:blank")
  })

  it("carries no address in its tooltip — there is none to show", () => {
    tabsState.fileTabs = [browserTab("about:blank")]
    renderStrip()
    expect(screen.getByRole("tab").title).not.toContain("about:blank")
  })

  // A tab opened empty keeps `about:blank` as its record title for life — the
  // record is stamped once, and there was no host to name it after. So once
  // it has gone somewhere, the record title is the one thing that must NOT be
  // shown: the page it names is not the page it is on.
  it("names the host, not 'about:blank', once it has navigated", () => {
    tabsState.fileTabs = [browserTab("about:blank")]
    // A page that never says its title — a JSON endpoint, a directory index —
    // and every page for as long as a navigation is in flight.
    browserMocks.browserState = {
      url: "https://example.com/api/items.json",
      title: "",
    }
    renderStrip()
    const tab = screen.getByRole("tab")
    expect(tab).not.toHaveTextContent("about:blank")
    expect(tab).not.toHaveTextContent("New tab")
    expect(tab).toHaveTextContent("example.com")
    expect(tab.title).toBe("example.com\nhttps://example.com/api/items.json")
  })
})

describe("FileWorkspaceTabBar — a previewed HTML file", () => {
  const PAGE = "<!doctype html><title>NETRUNNER // ACCESS TERMINAL</title><p>x"

  it("is named by the document, with the file behind it on hover", () => {
    tabsState.fileTabs = [htmlTab("/repo/site/index.html", PAGE)]
    tabsState.previewFileTabIds = new Set(["file:/repo/site/index.html"])
    renderStrip()
    const tab = screen.getByRole("tab")
    expect(tab).toHaveTextContent("NETRUNNER // ACCESS TERMINAL")
    expect(tab).not.toHaveTextContent("index.html")
    // Browser-tab shape: what it is, then where it lives, one per line.
    expect(tab.title).toBe(
      "NETRUNNER // ACCESS TERMINAL\n/repo/site/index.html"
    )
  })

  it("is named by the file while its source is showing", () => {
    // The tab is an editor then, and `index.html` is what is being edited.
    tabsState.fileTabs = [htmlTab("/repo/site/index.html", PAGE)]
    renderStrip()
    const tab = screen.getByRole("tab")
    expect(tab).toHaveTextContent("index.html")
    expect(tab).not.toHaveTextContent("NETRUNNER")
    expect(tab.title).toBe("/repo/site/index.html")
  })

  it("keeps the file name when the document has no title of its own", () => {
    tabsState.fileTabs = [htmlTab("/repo/site/index.html", "<p>no title here")]
    tabsState.previewFileTabIds = new Set(["file:/repo/site/index.html"])
    renderStrip()
    expect(screen.getByRole("tab")).toHaveTextContent("index.html")
  })

  it("leaves a previewed markdown file alone", () => {
    // Only HTML has a document title; a `# heading` is not one, and reading
    // one out of the source would rename the tab on every keystroke.
    const md = { ...fileTab("/repo/notes.md"), language: "markdown" }
    tabsState.fileTabs = [md as FileWorkspaceTab]
    tabsState.previewFileTabIds = new Set(["file:/repo/notes.md"])
    renderStrip()
    expect(screen.getByRole("tab")).toHaveTextContent("notes.md")
  })
})

describe("FileWorkspaceTabBar — browser tab hover", () => {
  it("gives the full title and the address, one per line", () => {
    tabsState.fileTabs = [
      browserTab(
        "https://example.com/docs",
        "A page title far too long for the tab"
      ),
    ]
    renderStrip()
    // The label in the strip is always cut short (a page title is a sentence)
    // and the address appears nowhere in the strip, so hovering has to supply
    // both — that is the whole point of the tooltip on this tab kind.
    expect(screen.getByRole("tab").title).toBe(
      "A page title far too long for the tab\nhttps://example.com/docs"
    )
  })
})
