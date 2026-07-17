import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { UserResourceLinks } from "./user-resource-links"

const mocks = vi.hoisted(() => ({
  openFilePreview: vi.fn(),
  toastError: vi.fn(),
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
  openUrl: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  isDesktop: () => false,
  getActiveRemoteConnectionId: () => null,
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolder: {
      path: mocks.activeFolderPath,
    },
  }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({
    openFilePreview: mocks.openFilePreview,
  }),
}))

describe("UserResourceLinks", () => {
  beforeEach(() => {
    mocks.openFilePreview.mockReset()
    mocks.openFilePreview.mockResolvedValue(undefined)
    mocks.toastError.mockReset()
    mocks.activeFolderPath = "/repo"
  })

  it("renders nothing when there are no resources", () => {
    const { container } = render(<UserResourceLinks resources={[]} />)
    expect(container).toBeEmptyDOMElement()
  })

  it("opens a file:// attachment in the workspace file panel on left-click", async () => {
    render(
      <UserResourceLinks
        resources={[
          {
            name: "app.ts",
            uri: "file:///repo/src/app.ts",
            mime_type: null,
          },
        ]}
      />
    )

    fireEvent.click(screen.getByRole("button", { name: /app\.ts/i }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "/repo/src/app.ts",
        expect.objectContaining({ line: undefined })
      )
    })
  })

  it("opens a Windows file:// attachment path", async () => {
    render(
      <UserResourceLinks
        resources={[
          {
            name: "foo.ts",
            uri: "file:///D:/MyCodeBuddy/src/foo.ts",
            mime_type: null,
          },
        ]}
      />
    )

    fireEvent.click(screen.getByRole("button", { name: /foo\.ts/i }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "D:/MyCodeBuddy/src/foo.ts",
        expect.objectContaining({ line: undefined })
      )
    })
  })

  it("renders each resource as its own clickable chip", async () => {
    render(
      <UserResourceLinks
        resources={[
          { name: "a.ts", uri: "file:///repo/a.ts", mime_type: null },
          { name: "b.ts", uri: "file:///repo/b.ts", mime_type: null },
        ]}
      />
    )

    fireEvent.click(screen.getByRole("button", { name: /b\.ts/i }))

    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(
        "/repo/b.ts",
        expect.objectContaining({ line: undefined })
      )
    })
    expect(mocks.openFilePreview).toHaveBeenCalledTimes(1)
  })
})
