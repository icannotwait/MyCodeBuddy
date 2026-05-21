import { useState } from "react"
import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { LinkSafetyModalProps } from "streamdown"
import {
  FilePathLink,
  useStreamdownLinkSafety,
} from "@/components/ai-elements/link-safety"

const mocks = vi.hoisted(() => ({
  openUrl: vi.fn(),
  openFilePreview: vi.fn(),
  toastError: vi.fn(),
  isDesktop: vi.fn(() => false),
  getActiveRemoteConnectionId: vi.fn(() => null),
  activeFolderPath: "/repo",
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
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

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolder: {
      path: mocks.activeFolderPath,
    },
  }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceContext: () => ({
    openFilePreview: mocks.openFilePreview,
  }),
}))

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
      <button
        type="button"
        onClick={async () => {
          if (linkSafety.onLinkCheck && (await linkSafety.onLinkCheck(url))) {
            window.open(url, "_blank", "noreferrer")
            return
          }
          setOpen(true)
        }}
      >
        Trigger link
      </button>
      {renderModal?.(props)}
    </div>
  )
}

describe("link safety direct opening", () => {
  beforeEach(() => {
    mocks.openUrl.mockReset()
    mocks.openFilePreview.mockReset()
    mocks.toastError.mockReset()
    mocks.isDesktop.mockReset()
    mocks.isDesktop.mockReturnValue(false)
    mocks.getActiveRemoteConnectionId.mockReset()
    mocks.getActiveRemoteConnectionId.mockReturnValue(null)
    mocks.openFilePreview.mockResolvedValue(undefined)
    mocks.activeFolderPath = "/repo"
    vi.spyOn(window, "open").mockReturnValue(null)
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("opens markdown hyperlinks directly from Streamdown without rendering a confirmation dialog", async () => {
    render(<LinkSafetyHarness url="https://example.com/docs" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(window.open).toHaveBeenCalledWith(
        "https://example.com/docs",
        "_blank",
        "noreferrer"
      )
    })
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("opens markdown file links directly in the workspace", async () => {
    render(<LinkSafetyHarness url="file:///repo/src/app.ts#L12" />)

    fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("src/app.ts", {
        line: 12,
      })
    })
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
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

  it("opens file path labels directly in the workspace", async () => {
    render(
      <FilePathLink filePath="/repo/src/lib.ts" line={5}>
        src/lib.ts
      </FilePathLink>
    )

    fireEvent.click(screen.getByRole("button", { name: "src/lib.ts" }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("src/lib.ts", {
        line: 5,
      })
    })
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })
})
