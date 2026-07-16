import { fireEvent, render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  openFilePreview: vi.fn(),
  openUrl: vi.fn(),
  toastError: vi.fn(),
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
  isDesktop: () => false,
  getActiveRemoteConnectionId: () => null,
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({ activeFolder: { path: "/repo" } }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({
    openFilePreview: mocks.openFilePreview,
  }),
}))

import { MessageResponse } from "./message"

describe("MessageResponse local-path autolinking", () => {
  beforeEach(() => {
    mocks.openFilePreview.mockReset()
    mocks.openFilePreview.mockResolvedValue(undefined)
    mocks.openUrl.mockReset()
    mocks.toastError.mockReset()
    vi.spyOn(window, "open").mockReturnValue(null)
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("leaves a bare path as text by default", () => {
    const { container } = render(
      <MessageResponse>{String.raw`changed D:\repo\src\app.ts`}</MessageResponse>
    )
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
    expect(container.textContent).toContain(String.raw`D:\repo\src\app.ts`)
  })

  it("renders supported Windows and POSIX paths only when enabled", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {String.raw`D:\repo\src\app.ts and /Users/me/repo/src/b.ts`}
      </MessageResponse>
    )
    await waitFor(() => {
      expect(
        container.querySelectorAll(
          "[data-reference-badge][data-ref-type='file']"
        )
      ).toHaveLength(2)
    })
    expect(container.textContent).not.toContain("[blocked]")
  })

  it.each([
    [":12", String.raw`see "D:\My Project\src\app.ts:12" now`],
    [":12:8", String.raw`see "D:\My Project\src\app.ts:12:8" now`],
    ["#L12", String.raw`see "D:\My Project\src\app.ts#L12" now`],
    ["#L12-L20", String.raw`see "D:\My Project\src\app.ts#L12-L20" now`],
    ["#L12-20", String.raw`see "D:\My Project\src\app.ts#L12-20" now`],
  ])(
    "opens a quoted Windows path with %s at its starting line",
    async (_suffix, source) => {
      const { container } = render(
        <MessageResponse autolinkLocalPaths>{source}</MessageResponse>
      )
      const button = await waitFor(() => {
        const found = container.querySelector<HTMLButtonElement>(
          "button[data-resource-kind='file']"
        )
        expect(found).not.toBeNull()
        return found!
      })
      fireEvent.click(button)
      await waitFor(() => {
        expect(mocks.openFilePreview).toHaveBeenCalledWith(
          "D:/My Project/src/app.ts",
          { line: 12 }
        )
      })
      expect(mocks.openUrl).not.toHaveBeenCalled()
      expect(window.open).not.toHaveBeenCalled()
    }
  )

  it("does not autolink inline code or slash commands", () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {"`D:\\repo\\src\\app.ts` and /review"}
      </MessageResponse>
    )
    expect(container.querySelector("code")).not.toBeNull()
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })

  it("fails closed after CommonMark consumes a Windows separator", () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {String.raw`D:\repo\[draft]\app.ts`}
      </MessageResponse>
    )
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })

  it("preserves an existing web autolink and ignores token-like paths", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {"https://example.com/docs and @/repo/src/app.ts"}
      </MessageResponse>
    )
    await waitFor(() => {
      expect(
        container.querySelector("button[data-resource-kind='web']")
      ).not.toBeNull()
    })
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })
})
