import { act, cleanup, render, screen } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"
import enMessages from "@/i18n/messages/en.json"
import { DEFAULT_SHORTCUTS } from "@/lib/keyboard-shortcuts"

const closeFileTab = vi.fn()
const closeAllFileTabs = vi.fn()
const switchFileTab = vi.fn()
const closeOtherFileTabs = vi.fn()
const reorderFileTabs = vi.fn()
const toggleFileTabPreview = vi.fn()
const toggleFilesMaximized = vi.fn()
const beginTranslateRequest = vi.fn(() => 1)
const openTranslationResultTab = vi.fn(() => "translate:file-1:zh_cn:1")
const openFilePreview = vi.fn(
  async () =>
    ({ ok: true as const, tabId: "file:/ws/README.zh_cn.md" }) as const
)

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
  vi.fn(async () => ({
    translatedContent: "你好",
    locale: "zh_cn",
    format: "markdown" as const,
  }))
)

const saveTranslationAs = vi.hoisted(() =>
  vi.fn(async () => ({
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

vi.mock("@/lib/platform", () => ({
  openPath: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  translateDocument: (...args: unknown[]) => translateDocument(...args),
  saveTranslationAs: (...args: unknown[]) => saveTranslationAs(...args),
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

import { FileWorkspaceTabBar } from "./file-workspace-tab-bar"

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
