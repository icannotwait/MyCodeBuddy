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

  it("leaves a bare path as text by default", async () => {
    const path = String.raw`D:\repo\src\app.ts`
    const { container } = render(
      <MessageResponse>{String.raw`changed ${path}`}</MessageResponse>
    )
    await waitFor(() => {
      expect(container.textContent).toContain(path)
      expect(container.querySelector("p")).not.toBeNull()
    })
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
    expect(container.textContent).toContain(path)
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

  it("keeps autolink when caller provides remarkPlugins", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths remarkPlugins={[]}>
        {String.raw`D:\repo\src\app.ts`}
      </MessageResponse>
    )
    await waitFor(() => {
      expect(
        container.querySelector("[data-reference-badge][data-ref-type='file']")
      ).not.toBeNull()
    })
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

  it("does not autolink inline code or slash commands", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {"`D:\\repo\\src\\app.ts` and /review"}
      </MessageResponse>
    )
    await waitFor(() => {
      expect(container.querySelector("code")).not.toBeNull()
      expect(container.querySelector("code")?.textContent).toContain(
        String.raw`D:\repo\src\app.ts`
      )
    })
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })

  it("fails closed after CommonMark consumes a Windows separator", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {String.raw`D:\repo\[draft]\app.ts`}
      </MessageResponse>
    )
    await waitFor(() => {
      // After CommonMark link/ref parsing, some visible path text remains.
      expect(container.textContent).toMatch(/D:\\repo|app\.ts/)
    })
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

  it("autolinks bare relative prose when enabled and opens via openFilePreview", async () => {
    const rel =
      "docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md"
    const { container } = render(
      <MessageResponse autolinkLocalPaths>{`see ${rel} now`}</MessageResponse>
    )
    const button = await waitFor(() => {
      const el = container.querySelector<HTMLButtonElement>(
        "button[data-resource-kind='file']"
      )
      expect(el).not.toBeNull()
      return el!
    })
    fireEvent.click(button)
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(rel, {
        line: undefined,
      })
    })
  })

  it.each([
    ["./src/a.ts", "src/a.ts"],
    ["../plans/x.md", "../plans/x.md"],
  ])("opens relative prose %s as %s", async (prose, expectedPath) => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>{`see ${prose} now`}</MessageResponse>
    )
    const button = await waitFor(() => {
      const el = container.querySelector<HTMLButtonElement>(
        "button[data-resource-kind='file']"
      )
      expect(el).not.toBeNull()
      return el!
    })
    fireEvent.click(button)
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith(expectedPath, {
        line: undefined,
      })
    })
  })

  it("does not autolink relative prose when flag off", async () => {
    const { container } = render(
      <MessageResponse>{"see docs/a.md now"}</MessageResponse>
    )
    await waitFor(() => expect(container.textContent).toContain("docs/a.md"))
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })

  it("does not autolink relative path in inline code", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>{"`docs/a.md`"}</MessageResponse>
    )
    await waitFor(() => expect(container.querySelector("code")).not.toBeNull())
    expect(
      container.querySelector("[data-reference-badge][data-ref-type='file']")
    ).toBeNull()
  })

  it("opens explicit bare relative markdown link [x](docs/a.md)", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>{"[x](docs/a.md)"}</MessageResponse>
    )
    const button = await waitFor(() => {
      const el = container.querySelector<HTMLButtonElement>(
        "button[data-resource-kind='file']"
      )
      expect(el).not.toBeNull()
      return el!
    })
    fireEvent.click(button)
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/a.md", {
        line: undefined,
      })
    })
  })

  it("keeps extensionless explicit relative markdown [app](./src/app) as file", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>{"[app](./src/app)"}</MessageResponse>
    )
    await waitFor(() => {
      expect(
        container.querySelector("button[data-resource-kind='file']")
      ).not.toBeNull()
    })
  })

  it("quoted relative with spaces and line suffix round-trips", async () => {
    const { container } = render(
      <MessageResponse autolinkLocalPaths>
        {'see "docs/My File.md:12" now'}
      </MessageResponse>
    )
    const button = await waitFor(() => {
      const el = container.querySelector<HTMLButtonElement>(
        "button[data-resource-kind='file']"
      )
      expect(el).not.toBeNull()
      return el!
    })
    fireEvent.click(button)
    await waitFor(() => {
      expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/My File.md", {
        line: 12,
      })
    })
  })
})
