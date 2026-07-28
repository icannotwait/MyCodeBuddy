import { fireEvent, render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

// End-to-end guard for the "Windows local file link renders as [blocked]" bug
// (issue #362). Exercises the REAL Streamdown pipeline (no streamdown mock) so
// the assertions cover actual rehype `sanitize` + `harden` behavior — the layer
// that read `E:` in `E:/…` as a URL protocol, stripped the href, and let harden
// replace the link with "<name> [blocked]". Only the leaf dependencies of the
// real link-safety hook are stubbed, so the click path (badge → link-safety →
// `openFilePreview`) is genuinely exercised too.
const mocks = vi.hoisted(() => ({
  openFilePreview: vi.fn(),
  openUrl: vi.fn(),
  toastError: vi.fn(),
  isDesktop: vi.fn(() => false),
  getActiveRemoteConnectionId: vi.fn(() => null),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("sonner", () => ({
  toast: { error: mocks.toastError },
}))

vi.mock("@/lib/platform", () => ({
  openUrl: mocks.openUrl,
}))

vi.mock("@/lib/transport", () => ({
  isDesktop: mocks.isDesktop,
  getActiveRemoteConnectionId: mocks.getActiveRemoteConnectionId,
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({ activeFolder: { path: "/repo" } }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({ openFilePreview: mocks.openFilePreview }),
}))

import { MessageResponse } from "./message"

function fileBadgeButton(container: HTMLElement): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(
    "button[data-resource-kind='file']"
  )
  if (!button) throw new Error("expected a clickable file badge")
  return button
}

describe("MessageResponse — Windows local file links (issue #362, real Streamdown)", () => {
  beforeEach(() => {
    mocks.openFilePreview.mockReset()
    mocks.openFilePreview.mockResolvedValue(undefined)
    mocks.openUrl.mockReset()
    mocks.openUrl.mockResolvedValue(undefined)
    mocks.toastError.mockReset()
    mocks.isDesktop.mockReset()
    mocks.isDesktop.mockReturnValue(false)
    mocks.getActiveRemoteConnectionId.mockReset()
    mocks.getActiveRemoteConnectionId.mockReturnValue(null)
    vi.spyOn(window, "open").mockReturnValue(null)
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("renders a bare Windows drive-path link as a file badge, not '[blocked]'", async () => {
    const { container } = render(
      <MessageResponse>{"[Gd.docx](E:/Desktop/docs/Gd.docx)"}</MessageResponse>
    )

    await waitFor(() => {
      expect(fileBadgeButton(container)).toBeTruthy()
    })
    expect(container.textContent).toContain("Gd.docx")
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(fileBadgeButton(container))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "E:/Desktop/docs/Gd.docx",
        {
          line: undefined,
        }
      )
    })
    expect(window.open).not.toHaveBeenCalled()
  })

  it("renders a file:///E:/… link as a file badge, not '[blocked]'", async () => {
    const { container } = render(
      <MessageResponse>
        {"[Gd.docx](file:///E:/Desktop/docs/Gd.docx)"}
      </MessageResponse>
    )

    await waitFor(() => {
      expect(fileBadgeButton(container)).toBeTruthy()
    })
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(fileBadgeButton(container))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "E:/Desktop/docs/Gd.docx",
        {
          line: undefined,
        }
      )
    })
  })

  it("handles a Chinese Windows path", async () => {
    const { container } = render(
      <MessageResponse>{"[手册](E:/桌面/使用手册/G手册.docx)"}</MessageResponse>
    )

    await waitFor(() => {
      expect(fileBadgeButton(container)).toBeTruthy()
    })
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(fileBadgeButton(container))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "E:/桌面/使用手册/G手册.docx",
        { line: undefined }
      )
    })
  })

  it("handles a URL-encoded Windows path with spaces", async () => {
    const { container } = render(
      <MessageResponse>
        {"[手册](E:/My%20Docs/%E6%89%8B%E5%86%8C.docx)"}
      </MessageResponse>
    )

    await waitFor(() => {
      expect(fileBadgeButton(container)).toBeTruthy()
    })
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(fileBadgeButton(container))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "E:/My Docs/手册.docx",
        {
          line: undefined,
        }
      )
    })
  })

  it("keeps blocking javascript:/data:/vbscript: links", async () => {
    for (const href of [
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox(1)",
    ]) {
      const { container, unmount } = render(
        <MessageResponse>{`[click](${href})`}</MessageResponse>
      )
      await waitFor(() => {
        expect(container.textContent).toContain("[blocked]")
      })
      // No openable file/web affordance was minted for a dangerous scheme.
      expect(container.querySelector("button[data-resource-kind]")).toBeNull()
      unmount()
    }
  })

  it("does not regress POSIX file links", async () => {
    const { container } = render(
      <MessageResponse>{"[app](file:///repo/src/app.ts)"}</MessageResponse>
    )

    await waitFor(() => {
      expect(fileBadgeButton(container)).toBeTruthy()
    })
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(fileBadgeButton(container))
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("/repo/src/app.ts", {
        line: undefined,
      })
    })
  })

  it("documents the known UNC end-to-end limitation (out of scope for #362)", async () => {
    // remark rewrites file://server/share/doc.md to the backslash UNC form
    // `\\server\share\doc.md` (the correct LOCAL signal for link-safety).
    // rehype-harden alone would block bare-backslash hrefs (only `/` `./` `../`
    // relatives are retried), but our harden wrapper preserves isLocalPathLike
    // hrefs — including raw UNC — so the link is no longer replaced with
    // `[blocked]`. The remaining gap is encoding: remark/rehype percent-encodes
    // the backslashes (`%5C%5Cserver…`), so classifyResourceKind does not see a
    // raw UNC prefix and the chip is not a file badge. Forward-slash
    // `//server/share` would still collapse to a hostless path. Full UNC
    // end-to-end open remains out of scope; remark + click routing of the raw
    // backslash form stay covered by unit tests.
    const { container } = render(
      <MessageResponse>{"[doc](file://server/share/doc.md)"}</MessageResponse>
    )

    await waitFor(() => {
      expect(container.textContent).toContain("doc")
      expect(container.textContent).not.toContain("[blocked]")
    })
    // Not a file chip: encoded backslashes lose the raw UNC openability signal.
    expect(container.querySelector("button[data-resource-kind]")).toBeNull()
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
  })

  it("does not regress https links (routed to the browser, not the file opener)", async () => {
    const { container } = render(
      <MessageResponse>{"[docs](https://example.com)"}</MessageResponse>
    )

    await waitFor(() => {
      expect(container.querySelector("[data-streamdown='link']")).not.toBeNull()
    })
    expect(container.textContent).not.toContain("[blocked]")

    fireEvent.click(container.querySelector("[data-streamdown='link']")!)
    await waitFor(() => {
      // Streamdown normalizes the href through `new URL`, adding the trailing
      // slash; the click still routes to the browser (never the file opener).
      expect(window.open).toHaveBeenCalledWith(
        "https://example.com/",
        "_blank",
        "noreferrer"
      )
    })
    expect(mocks.openFilePreview).not.toHaveBeenCalled()
  })
})
