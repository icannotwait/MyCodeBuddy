import { useState } from "react"
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { LinkSafetyModalProps } from "streamdown"
import {
  FilePathLink,
  openLinkWithSafety,
  useOpenLinkOrFile,
  useStreamdownLinkSafety,
} from "@/components/ai-elements/link-safety"
import type { GrokSessionImageScopeValue } from "./grok-session-image-context"

const mocks = vi.hoisted(() => ({
  openUrl: vi.fn(),
  openFilePreview: vi.fn(),
  openResolvedImagePreview: vi.fn(),
  resolveGrokSessionImage: vi.fn(),
  toastError: vi.fn(),
  isDesktop: vi.fn(() => false),
  getActiveRemoteConnectionId: vi.fn(() => null),
  activeFolderPath: "/repo" as string | null,
  grokScope: null as GrokSessionImageScopeValue | null,
  linkT: vi.fn((key: string) => key),
  workspaceT: vi.fn((key: string, values?: { name?: string }) =>
    key === "unableOpenFile" ? `${key}:${values?.name ?? ""}` : key
  ),
}))

vi.mock("next-intl", () => ({
  useTranslations: (namespace: string) =>
    namespace === "Folder.workspaceContext" ? mocks.workspaceT : mocks.linkT,
}))

vi.mock("sonner", () => ({
  toast: {
    error: mocks.toastError,
  },
}))

vi.mock("@/lib/platform", () => ({
  openUrl: mocks.openUrl,
}))

vi.mock("@/lib/transport", () => ({
  isDesktop: mocks.isDesktop,
  getActiveRemoteConnectionId: mocks.getActiveRemoteConnectionId,
}))

vi.mock("@/lib/api", () => ({
  resolveGrokSessionImage: mocks.resolveGrokSessionImage,
}))

vi.mock("./grok-session-image-context", () => ({
  useGrokSessionImageScope: () => mocks.grokScope,
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolder:
      mocks.activeFolderPath === null ? null : { path: mocks.activeFolderPath },
  }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({
    openFilePreview: mocks.openFilePreview,
    openResolvedImagePreview: mocks.openResolvedImagePreview,
  }),
}))

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function LinkSafetyHarness({ url }: { url: string }) {
  const linkSafety = useStreamdownLinkSafety()
  const [open, setOpen] = useState(false)
  const renderModal = linkSafety.renderModal

  const props: LinkSafetyModalProps = {
    url,
    isOpen: open,
    onClose: () => setOpen(false),
    onConfirm: () => {},
  }

  return (
    <div>
      {/* Dispatch through the very helper MarkdownLink uses, so these config
          tests exercise the real click path instead of a copy that can drift
          from it (this harness used to hand-roll an awaiting handler — the
          exact shape #410 was about). */}
      <button
        type="button"
        onClick={() => openLinkWithSafety(url, linkSafety, () => setOpen(true))}
      >
        Trigger link
      </button>
      {renderModal?.(props)}
    </div>
  )
}

function DirectOpenHarness({ url }: { url: string }) {
  const open = useOpenLinkOrFile()
  return (
    <button type="button" onClick={() => void open(url)}>
      Open target
    </button>
  )
}

describe("link safety direct opening", () => {
  beforeEach(() => {
    mocks.openUrl.mockReset()
    mocks.openFilePreview.mockReset()
    mocks.openResolvedImagePreview.mockReset()
    mocks.resolveGrokSessionImage.mockReset()
    mocks.toastError.mockReset()
    mocks.isDesktop.mockReset()
    mocks.isDesktop.mockReturnValue(false)
    mocks.getActiveRemoteConnectionId.mockReset()
    mocks.getActiveRemoteConnectionId.mockReturnValue(null)
    mocks.openFilePreview.mockResolvedValue(undefined)
    mocks.openResolvedImagePreview.mockReturnValue({
      ok: true,
      tabId: "file:resolved",
    })
    mocks.activeFolderPath = "/repo"
    mocks.grokScope = null
    mocks.linkT.mockClear()
    mocks.workspaceT.mockClear()
    vi.spyOn(window, "open").mockReturnValue(null)
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("opens markdown hyperlinks in the click's own task, without rendering a confirmation dialog", () => {
    render(<LinkSafetyHarness url="https://example.com/docs" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    // No waitFor: the web verdict is synchronous, so the open must land in the
    // click's own task or WebKit's popup blocker eats it. See #410.
    expect(window.open).toHaveBeenCalledWith(
      "https://example.com/docs",
      "_blank",
      "noreferrer"
    )
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("opens markdown file links directly in the workspace", async () => {
    render(<LinkSafetyHarness url="file:///repo/src/app.ts#L12" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    // Absolute paths pass through untouched — openFilePreview owns routing
    // (owning-folder match or outside-workspace open) by absolute path.
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/repo/src/app.ts", {
        line: 12,
      })
    })
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("jumps to the start line of a ranged file link (#L<start>-<end>)", async () => {
    render(<LinkSafetyHarness url="file:///repo/src/app.ts#L10-25" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/repo/src/app.ts", {
        line: 10,
      })
    })
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("preserves the UNC authority of a file://server/share URI", async () => {
    // new URL("file://server/share/doc.md") parses host=server,
    // pathname=/share/doc.md — the opener must receive the full UNC path
    // //server/share/doc.md, not the local /share/doc.md.
    render(<LinkSafetyHarness url="file://server/share/doc.md" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "//server/share/doc.md",
        { line: undefined }
      )
    })
  })

  it("opens a UNC link through the real chat path (remark-rewritten backslash form)", async () => {
    // In chat, remark-file-uri-links rewrites file://server/share/doc.md to
    // the backslash UNC form BEFORE MarkdownLink sees it. That rewritten
    // href must route to the file opener (as //server/share/doc.md), not
    // the browser.
    // JS string "\\\\server\\share\\doc.md" is the 2-backslash UNC form.
    render(<LinkSafetyHarness url={"\\\\server\\share\\doc.md"} />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "//server/share/doc.md",
        { line: undefined }
      )
    })
    expect(window.open).not.toHaveBeenCalled()
  })

  it("opens a literal Windows root at the line before its column", async () => {
    render(<LinkSafetyHarness url="/C:/repo/src/app.ts:12:8" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("C:/repo/src/app.ts", {
        line: 12,
      })
    })
  })

  it("keeps an encoded POSIX drive-like prefix rooted", async () => {
    render(<LinkSafetyHarness url="/C%3A/repo/src/app.ts" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "/C:/repo/src/app.ts",
        { line: undefined }
      )
    })
  })

  it("keeps an encoded terminal colon and digits in the POSIX filename", async () => {
    render(<LinkSafetyHarness url="/tmp/report%3A12" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/tmp/report:12", {
        line: undefined,
      })
    })
  })

  it("preserves encoded POSIX data in a direct file URI", async () => {
    render(<LinkSafetyHarness url="file:///C%3A/repo/report%3A12" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/C:/repo/report:12", {
        line: undefined,
      })
    })
  })

  it("decodes encoded hash and query characters after location syntax", async () => {
    render(<LinkSafetyHarness url="/tmp/a%23b%3Fc.ts#L4" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/tmp/a#b?c.ts", {
        line: 4,
      })
    })
  })

  it("passes ~ paths through for home expansion by the opener", async () => {
    render(<LinkSafetyHarness url="~/.claude/plans/notes.md" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "~/.claude/plans/notes.md",
        { line: undefined }
      )
    })
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("blocks unsupported markdown link protocols without rendering a confirmation dialog", async () => {
    render(<LinkSafetyHarness url="vscode://file/repo/src/app.ts" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenCalledWith("errorFailedLink", {
        description: "errorUnsupportedLinkProtocol",
      })
    })
    expect(window.open).not.toHaveBeenCalled()
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("treats protocol-relative // URLs as web links, never local file IO", () => {
    // `//cdn.example.com/app.js` is protocol-relative — the browser resolves
    // it against the page protocol. parseLocalFileTarget must NOT claim it
    // (that would route "//Users/…"-style urls into local file reads);
    // classifyResourceKind tags `//…` with the web icon to match.
    render(<LinkSafetyHarness url="//cdn.example.com/app.js" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    expect(window.open).toHaveBeenCalledWith(
      "//cdn.example.com/app.js",
      "_blank",
      "noreferrer"
    )
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
  })

  it("opens bare relative docs/a.md via openFilePreview", async () => {
    render(<LinkSafetyHarness url="docs/a.md" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/a.md", {
        line: undefined,
      })
    })
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(window.open).not.toHaveBeenCalled()
  })

  it("opens ./src/a.ts:12 with line and strips ./", async () => {
    render(<LinkSafetyHarness url="./src/a.ts:12" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("src/a.ts", {
        line: 12,
      })
    })
  })

  it("opens extensionless ./src/app for compatibility", async () => {
    render(<LinkSafetyHarness url="./src/app" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("src/app", {
        line: undefined,
      })
    })
  })

  it("toasts errorNoWorkspace for bare relative when no active folder", async () => {
    mocks.activeFolderPath = null
    render(<LinkSafetyHarness url="docs/a.md" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenCalled()
    })
    // useTranslations mock returns the key; toast receives description: "errorNoWorkspace"
    expect(
      mocks.toastError.mock.calls.some((c) =>
        JSON.stringify(c).includes("errorNoWorkspace")
      )
    ).toBe(true)
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(window.open).not.toHaveBeenCalled()
  })

  it("parent traversal ../outside.md still attempts open (no containment block)", async () => {
    render(<LinkSafetyHarness url="../outside.md" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("../outside.md", {
        line: undefined,
      })
    })
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("opens file path labels directly in the workspace", async () => {
    render(
      <FilePathLink filePath="/repo/src/lib.ts" line={5}>
        src/lib.ts
      </FilePathLink>
    )

    fireEvent.click(screen.getByRole("button", { name: "src/lib.ts" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/repo/src/lib.ts", {
        line: 5,
      })
    })
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("opens protocol-relative links on desktop as concrete https URLs", async () => {
    // The Tauri opener capability only allows http(s) URLs; a raw "//host"
    // would resolve against the webview's own scheme. The dispatch must
    // canonicalize.
    mocks.isDesktop.mockReturnValue(true)
    mocks.openUrl.mockResolvedValue(undefined)

    render(<LinkSafetyHarness url="//cdn.example.com/app.js" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openUrl).toHaveBeenCalledWith(
        "https://cdn.example.com/app.js"
      )
    })
    expect(window.open).not.toHaveBeenCalled()
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("routes desktop external links through the platform opener instead of streamdown", async () => {
    mocks.isDesktop.mockReturnValue(true)
    mocks.openUrl.mockResolvedValue(undefined)

    render(<LinkSafetyHarness url="https://example.com/docs" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openUrl).toHaveBeenCalledWith("https://example.com/docs")
    })
    expect(window.open).not.toHaveBeenCalled()
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("opens mailto: links via a synthetic anchor click in the browser to avoid an about:blank tab", async () => {
    mocks.isDesktop.mockReturnValue(false)
    const clickedHrefs: string[] = []
    const clickSpy = vi
      .spyOn(HTMLElement.prototype, "click")
      .mockImplementation(function (this: HTMLElement) {
        if (this instanceof HTMLAnchorElement) clickedHrefs.push(this.href)
      })

    render(<LinkSafetyHarness url="mailto:hi@example.com" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(clickedHrefs).toContain("mailto:hi@example.com")
    })
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(window.open).not.toHaveBeenCalled()
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
    clickSpy.mockRestore()
  })

  it("opens mailto: links via the platform opener on desktop", async () => {
    mocks.isDesktop.mockReturnValue(true)
    mocks.openUrl.mockResolvedValue(undefined)

    render(<LinkSafetyHarness url="mailto:hi@example.com" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openUrl).toHaveBeenCalledWith("mailto:hi@example.com")
    })
    expect(window.open).not.toHaveBeenCalled()
  })

  it("reflects the in-flight state on the file path button while a preview is opening", async () => {
    let resolveOpen: (() => void) | undefined
    mocks.openFilePreview.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveOpen = resolve
        })
    )

    render(<FilePathLink filePath="/repo/src/lib.ts">src/lib.ts</FilePathLink>)

    const button = screen.getByRole("button", { name: "src/lib.ts" })
    fireEvent.click(button)

    await waitFor(() => {
      expect(button).toBeDisabled()
      expect(button).toHaveAttribute("aria-busy", "true")
    })

    // Clicking while busy must not enqueue another open call.
    fireEvent.click(button)
    expect(mocks.openFilePreview).toHaveBeenCalledTimes(1)

    await act(async () => {
      resolveOpen?.()
    })

    await waitFor(() => {
      expect(button).not.toBeDisabled()
      expect(button).toHaveAttribute("aria-busy", "false")
    })
  })

  it("survives a parent re-render that swaps handler identities mid-flight", async () => {
    let resolvePendingOpen: (() => void) | undefined
    const initialOpenPreview = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolvePendingOpen = resolve
        })
    )
    mocks.openFilePreview = initialOpenPreview

    function ChurningHarness({ url, churn }: { url: string; churn: number }) {
      const linkSafety = useStreamdownLinkSafety()
      const [open, setOpen] = useState(false)
      return (
        <div data-churn={churn}>
          <button type="button" onClick={() => setOpen(true)}>
            Trigger link
          </button>
          {linkSafety.renderModal?.({
            url,
            isOpen: open,
            onClose: () => setOpen(false),
            onConfirm: () => {},
          })}
        </div>
      )
    }

    const { rerender } = render(
      <ChurningHarness url="file:///repo/src/app.ts" churn={0} />
    )

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(initialOpenPreview).toHaveBeenCalledTimes(1)
    })

    // Swap `useWorkspaceActions().openFilePreview` to a fresh vi.fn so the
    // next render forces `useOpenLinkOrFile`'s `useCallback` to rebuild —
    // i.e. the `onAction` prop of `<DirectLinkOpen>` changes identity while
    // the original open is still pending. The previous `cancelled`-flag
    // implementation tore down the in-flight callback chain here and never
    // fired `onClose()`, stranding streamdown's `isOpen` at true.
    const replacementOpenPreview = vi.fn().mockResolvedValue(undefined)
    mocks.openFilePreview = replacementOpenPreview
    rerender(<ChurningHarness url="file:///repo/src/app.ts" churn={1} />)

    await act(async () => {
      resolvePendingOpen?.()
    })

    // A second click must reach the replacement handler — which proves the
    // modal state was reset by `onClose()` despite the mid-flight identity
    // change, and that subsequent opens route through the latest handler.
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(replacementOpenPreview).toHaveBeenCalledTimes(1)
    })
    expect(initialOpenPreview).toHaveBeenCalledTimes(1)
  })

  it("gated Grok badge resolves bytes and opens the authoritative snapshot", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    mocks.openResolvedImagePreview.mockReturnValue({
      ok: true,
      tabId: "file:%2Fsession%2Fimages%2Fa.png",
    })
    mocks.resolveGrokSessionImage.mockResolvedValue({
      path: "/session/images/a.png",
      origin: "session",
      mimeType: "image/png",
      dataBase64: "YWJj",
    })

    render(<LinkSafetyHarness url="images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledWith({
        conversationId: 42,
        href: "images/a.png",
        includeData: true,
      })
    })
    expect(mocks.openResolvedImagePreview).toHaveBeenCalledWith({
      path: "/session/images/a.png",
      mimeType: "image/png",
      dataBase64: "YWJj",
      source: {
        type: "grok-session-image",
        conversationId: 42,
        href: "images/a.png",
      },
    })
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
  })

  it("gated not_found uses existing wording and never falls through", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    mocks.resolveGrokSessionImage.mockRejectedValue({
      code: "not_found",
      message: "missing",
    })

    render(<LinkSafetyHarness url="./images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => expect(mocks.toastError).toHaveBeenCalled())
    expect(mocks.toastError).toHaveBeenCalledWith("unableOpenFile:a.png")
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.openUrl).not.toHaveBeenCalled()
  })

  it("invalid_input_permission_and_transport_errors_use_local_open_error_and_stop", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const errors = [
      { code: "invalid_input", message: "bad href" },
      { code: "permission_denied", message: "denied" },
      new Error("transport down"),
    ]

    for (const error of errors) {
      mocks.resolveGrokSessionImage.mockRejectedValueOnce(error)
      const view = render(<DirectOpenHarness url="images/a.png" />)
      fireEvent.click(screen.getByRole("button", { name: "Open target" }))
      await waitFor(() => {
        expect(mocks.toastError).toHaveBeenLastCalledWith("errorFailedOpen", {
          description: error instanceof Error ? error.message : error.message,
        })
      })
      view.unmount()
    }

    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.openUrl).not.toHaveBeenCalled()
  })

  it("non_grok_and_nonmatching_grok_links_keep_generic_behavior", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const first = render(<DirectOpenHarness url="docs/a.md" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/a.md", {
        line: undefined,
      })
    })
    first.unmount()

    render(<DirectOpenHarness url="images/a.bmp" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("images/a.bmp", {
        line: undefined,
      })
    })

    expect(mocks.resolveGrokSessionImage).not.toHaveBeenCalled()
  })

  it("rapid_repeated_same_href_clicks_share_one_inflight_promise_but_different_hrefs_do_not", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const a = deferred<{
      path: string
      origin: "session"
      mimeType: "image/png"
      dataBase64: string
    }>()
    const b = deferred<{
      path: string
      origin: "session"
      mimeType: "image/png"
      dataBase64: string
    }>()
    mocks.resolveGrokSessionImage
      .mockReturnValueOnce(a.promise)
      .mockReturnValueOnce(b.promise)
    const { rerender } = render(<DirectOpenHarness url="images/a.png" />)

    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(1)

    rerender(<DirectOpenHarness url="images/b.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(2)

    await act(async () => {
      a.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "YQ==",
      })
      b.resolve({
        path: "/session/images/b.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "Yg==",
      })
    })
  })

  it("malformed_or_action_rejected_success_fails_closed", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const malformed = [
      null,
      {
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
      },
      {
        path: "/session/images/a.png",
        origin: "other",
        mimeType: "image/png",
        dataBase64: "YWJj",
      },
      {
        path: "images/a.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "YWJj",
      },
      {
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/jpeg",
        dataBase64: "YWJj",
      },
    ]

    for (const resolution of malformed) {
      mocks.resolveGrokSessionImage.mockResolvedValueOnce(resolution)
      const view = render(<DirectOpenHarness url="images/a.png" />)
      fireEvent.click(screen.getByRole("button", { name: "Open target" }))
      await waitFor(() => {
        expect(mocks.toastError).toHaveBeenLastCalledWith("errorFailedOpen", {
          description: "Resolved image response was malformed",
        })
      })
      view.unmount()
    }

    mocks.openResolvedImagePreview.mockReturnValueOnce({
      ok: false,
      reason: "resolve",
    })
    mocks.resolveGrokSessionImage.mockResolvedValueOnce({
      path: "/session/images/a.png",
      origin: "session",
      mimeType: "image/png",
      dataBase64: "YWJj",
    })
    const view = render(<DirectOpenHarness url="images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenLastCalledWith("errorFailedOpen", {
        description: "Resolved image preview rejected the response",
      })
    })
    view.unmount()

    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.openUrl).not.toHaveBeenCalled()
  })

  it("conversation_change_ignores_a_late_gated_click_result", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const pending = deferred<{
      path: string
      origin: "session"
      mimeType: "image/png"
      dataBase64: string
    }>()
    mocks.resolveGrokSessionImage.mockReturnValue(pending.promise)
    const { rerender } = render(<DirectOpenHarness url="images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(1)

    mocks.grokScope = null
    rerender(<DirectOpenHarness url="images/a.png" />)
    await act(async () => {
      pending.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "YWJj",
      })
    })

    expect(mocks.openResolvedImagePreview).not.toHaveBeenCalled()
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("leaving_and_returning_to_the_same_conversation_invalidates_the_old_click", async () => {
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    const first = deferred<{
      path: string
      origin: "session"
      mimeType: "image/png"
      dataBase64: string
    }>()
    const second = deferred<{
      path: string
      origin: "session"
      mimeType: "image/png"
      dataBase64: string
    }>()
    mocks.resolveGrokSessionImage
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    const { rerender } = render(<DirectOpenHarness url="images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))

    mocks.grokScope = null
    rerender(<DirectOpenHarness url="images/a.png" />)
    mocks.grokScope = { conversationId: 42, phase: "complete" }
    rerender(<DirectOpenHarness url="images/a.png" />)
    fireEvent.click(screen.getByRole("button", { name: "Open target" }))
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(2)

    await act(async () => {
      first.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "b2xk",
      })
    })
    expect(mocks.openResolvedImagePreview).not.toHaveBeenCalled()
    expect(mocks.toastError).not.toHaveBeenCalled()

    await act(async () => {
      second.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
        dataBase64: "bmV3",
      })
    })
    expect(mocks.openResolvedImagePreview).toHaveBeenCalledTimes(1)
    expect(mocks.openResolvedImagePreview).toHaveBeenCalledWith(
      expect.objectContaining({ dataBase64: "bmV3" })
    )
  })
})
