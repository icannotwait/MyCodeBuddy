import { fireEvent, render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { FileWorkspaceTab } from "@/contexts/workspace-context"

const mocks = vi.hoisted(() => ({
  activeFileTab: null as FileWorkspaceTab | null,
  previewFileTabIds: new Set<string>(),
  toggleFileTabPreview: vi.fn(),
  openPath: vi.fn().mockResolvedValue(undefined),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceFileTabs: () => ({
    activeFileTab: mocks.activeFileTab,
    activeFileTabId: mocks.activeFileTab?.id ?? null,
    previewFileTabIds: mocks.previewFileTabIds,
  }),
  useWorkspaceActions: () => ({
    toggleFileTabPreview: mocks.toggleFileTabPreview,
  }),
}))

vi.mock("@/lib/platform", () => ({
  openPath: mocks.openPath,
}))

vi.mock("@/components/files/file-path-breadcrumb", () => ({
  FilePathBreadcrumb: ({ path }: { path: string }) => <span>{path}</span>,
}))

import { FileWorkspaceHeader } from "./file-workspace-header"

function htmlTab(snapshot: boolean): FileWorkspaceTab {
  return {
    id: "file:%2Frepo%2Fimages%2Falias.html",
    kind: "file",
    folderId: null,
    title: "alias.html",
    description: "/repo/images/alias.html",
    path: "/repo/images/alias.html",
    language: snapshot ? "image" : "html",
    content: snapshot ? "data:image/png;base64,AA==" : "<p>ready</p>",
    loading: false,
    savedContent: snapshot ? "data:image/png;base64,AA==" : "<p>ready</p>",
    isDirty: false,
    etag: snapshot ? null : "etag-1",
    mtimeMs: snapshot ? null : 1,
    readonly: snapshot,
    lineEnding: snapshot ? "none" : "lf",
    saveState: "idle",
    saveError: null,
    stale: false,
    hasLoadedSuccessfully: true,
    snapshotSource: snapshot
      ? {
          type: "grok-session-image",
          conversationId: 42,
          href: "images/a.png",
        }
      : undefined,
  }
}

describe("FileWorkspaceHeader snapshot actions", () => {
  beforeEach(() => {
    mocks.activeFileTab = null
    mocks.previewFileTabIds = new Set()
    mocks.toggleFileTabPreview.mockReset()
    mocks.openPath.mockReset()
    mocks.openPath.mockResolvedValue(undefined)
  })

  it("exposes no path-derived preview or browser action for an HTML-path snapshot", () => {
    mocks.activeFileTab = htmlTab(true)

    render(<FileWorkspaceHeader />)

    expect(screen.queryAllByRole("button")).toHaveLength(0)
    expect(mocks.toggleFileTabPreview).not.toHaveBeenCalled()
    expect(mocks.openPath).not.toHaveBeenCalled()
  })

  it("keeps both HTML actions for ordinary tabs and restores them after conversion", () => {
    mocks.activeFileTab = htmlTab(false)
    const { rerender } = render(<FileWorkspaceHeader />)

    let actions = screen.getAllByRole("button", { name: "preview" })
    expect(actions).toHaveLength(2)
    fireEvent.click(actions[0])
    fireEvent.click(actions[1])
    expect(mocks.toggleFileTabPreview).toHaveBeenCalledWith(
      mocks.activeFileTab.id
    )
    expect(mocks.openPath).toHaveBeenCalledWith("/repo/images/alias.html")

    mocks.activeFileTab = htmlTab(true)
    rerender(<FileWorkspaceHeader />)
    expect(screen.queryAllByRole("button")).toHaveLength(0)

    mocks.activeFileTab = htmlTab(false)
    rerender(<FileWorkspaceHeader />)
    actions = screen.getAllByRole("button", { name: "preview" })
    expect(actions).toHaveLength(2)
  })
})
