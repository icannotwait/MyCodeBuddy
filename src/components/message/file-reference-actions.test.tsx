import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { useLayoutEffect } from "react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { GrokSessionImageResolution } from "@/lib/types"
import type { GrokSessionImageScopeValue } from "@/components/ai-elements/grok-session-image-context"

const mocks = vi.hoisted(() => ({
  isLocalDesktop: vi.fn(() => true),
  revealItemInDir: vi.fn(async () => {}),
  copyTextFromMenu: vi.fn(async () => true),
  toastSuccess: vi.fn(),
  toastError: vi.fn(),
  ancestorContextMenu: vi.fn(),
  ancestorPointerDown: vi.fn(),
  folderPath: "/repo" as string | undefined,
  resolveGrokSessionImage: vi.fn(),
}))

vi.mock("@/lib/platform", () => ({
  isLocalDesktop: mocks.isLocalDesktop,
  revealItemInDir: mocks.revealItemInDir,
}))

vi.mock("sonner", () => ({
  toast: { success: mocks.toastSuccess, error: mocks.toastError },
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolderId: 1,
    activeFolder: mocks.folderPath ? { path: mocks.folderPath } : null,
  }),
}))

vi.mock("@/lib/utils", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/utils")>()
  return { ...actual, copyTextFromMenu: mocks.copyTextFromMenu }
})

vi.mock("@/lib/api", () => ({
  resolveGrokSessionImage: mocks.resolveGrokSessionImage,
}))

import {
  FileReferenceActions,
  resolveFileReferenceTarget,
  systemFileManagerLabelKey,
} from "./file-reference-actions"
import {
  GrokConversationProvider,
  GrokSessionImageScope,
} from "@/components/ai-elements/grok-session-image-context"

type MenuLayoutSnapshot = {
  target: string
  conversationId: number | null
  revealDisabled: boolean | null
  relativeDisabled: boolean | null
  absoluteDisabled: boolean | null
}

function MenuLayoutObserver({
  target,
  conversationId,
  onSnapshot,
}: {
  target: string
  conversationId: number | null
  onSnapshot: (snapshot: MenuLayoutSnapshot) => void
}) {
  useLayoutEffect(() => {
    const menu = document.querySelector<HTMLElement>("[role='menu']")
    if (!menu) return
    const items = Array.from(
      menu.querySelectorAll<HTMLElement>("[role='menuitem']")
    )
    const disabled = (label: string | RegExp) => {
      const menuItem = items.find((candidate) =>
        typeof label === "string"
          ? candidate.textContent === label
          : label.test(candidate.textContent ?? "")
      )
      return menuItem ? menuItem.hasAttribute("data-disabled") : null
    }
    onSnapshot({
      target,
      conversationId,
      revealDisabled: disabled(/^Open in/),
      relativeDisabled: disabled("Copy relative path"),
      absoluteDisabled: disabled("Copy absolute path"),
    })
  }, [conversationId, onSnapshot, target])

  return null
}

function actionsTree(
  target: string,
  scope: GrokSessionImageScopeValue | null = null,
  onMenuLayout?: (snapshot: MenuLayoutSnapshot) => void
) {
  return (
    // The outer handler stands in for the conversation panel's own context menu
    // (copy text / export / …), which wraps the whole transcript.
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <GrokConversationProvider conversationId={scope?.conversationId ?? null}>
        <GrokSessionImageScope phase={scope?.phase ?? null}>
          <>
            <div
              onContextMenu={mocks.ancestorContextMenu}
              onPointerDown={mocks.ancestorPointerDown}
            >
              <FileReferenceActions target={target}>
                <button type="button" data-testid="badge">
                  app.ts
                </button>
              </FileReferenceActions>
            </div>
            {onMenuLayout ? (
              <MenuLayoutObserver
                target={target}
                conversationId={scope?.conversationId ?? null}
                onSnapshot={onMenuLayout}
              />
            ) : null}
          </>
        </GrokSessionImageScope>
      </GrokConversationProvider>
    </NextIntlClientProvider>
  )
}

function renderActions(
  target: string,
  scope: GrokSessionImageScopeValue | null = null,
  onMenuLayout?: (snapshot: MenuLayoutSnapshot) => void
) {
  return render(actionsTree(target, scope, onMenuLayout))
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** The context-menu trigger wrapped around the badge. */
function trigger(): HTMLElement {
  const element = document.querySelector<HTMLElement>("[data-file-actions]")
  if (!element) throw new Error("expected a context-menu trigger on the badge")
  return element
}

/** Right-click the file name. */
function openMenu(): void {
  fireEvent.contextMenu(trigger())
}

function item(name: string): HTMLElement {
  return screen.getByRole("menuitem", { name })
}

function revealItem(): HTMLElement {
  return screen.getByRole("menuitem", { name: /^Open in/ })
}

describe("resolveFileReferenceTarget", () => {
  it("resolves a file:// uri, dropping the line-range fragment", () => {
    expect(
      resolveFileReferenceTarget("file:///repo/src/app.ts#L10-25", "/repo")
    ).toEqual({ absolute: "/repo/src/app.ts", relative: "src/app.ts" })
  })

  it("resolves the sanitize-safe Windows form back to a drive path", () => {
    expect(
      resolveFileReferenceTarget("/C:/repo/src/app.ts", "C:/repo")
    ).toEqual({ absolute: "C:/repo/src/app.ts", relative: "src/app.ts" })
  })

  it("joins an explicitly-relative path onto the active folder", () => {
    expect(resolveFileReferenceTarget("./src/app.ts", "/repo")).toEqual({
      absolute: "/repo/src/app.ts",
      relative: "src/app.ts",
    })
  })

  it("reports no relative form for a file outside the folder", () => {
    expect(resolveFileReferenceTarget("/elsewhere/a.ts", "/repo")).toEqual({
      absolute: "/elsewhere/a.ts",
      relative: null,
    })
  })

  it("keeps a ~ path in tilde form (home only resolves through the backend)", () => {
    expect(resolveFileReferenceTarget("~/notes/todo.md", "/repo")).toEqual({
      absolute: "~/notes/todo.md",
      relative: null,
    })
  })

  it("returns null for anything that isn't a local file", () => {
    expect(
      resolveFileReferenceTarget("https://example.com", "/repo")
    ).toBeNull()
    expect(resolveFileReferenceTarget("codeg://embedded/x", "/repo")).toBeNull()
    // Relative with no active folder: nothing could be revealed or copied.
    expect(resolveFileReferenceTarget("./src/app.ts", null)).toBeNull()
  })
})

describe("systemFileManagerLabelKey", () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it.each([
    ["MacIntel", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", "Finder"],
    ["Win32", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", "Explorer"],
    ["Linux x86_64", "Mozilla/5.0 (X11; Linux x86_64)", "FileManager"],
  ])("maps %s to openIn%s", (platform, userAgent, expected) => {
    vi.stubGlobal("navigator", { platform, userAgent })
    expect(systemFileManagerLabelKey()).toBe(`openIn${expected}`)
  })
})

describe("FileReferenceActions", () => {
  beforeEach(() => {
    mocks.isLocalDesktop.mockReturnValue(true)
    mocks.revealItemInDir.mockResolvedValue(undefined)
    mocks.copyTextFromMenu.mockResolvedValue(true)
    mocks.copyTextFromMenu.mockClear()
    mocks.revealItemInDir.mockClear()
    mocks.toastSuccess.mockClear()
    mocks.toastError.mockClear()
    mocks.ancestorContextMenu.mockClear()
    mocks.ancestorPointerDown.mockClear()
    mocks.resolveGrokSessionImage.mockReset()
    mocks.folderPath = "/repo"
  })

  it("passes the badge through untouched when the target has no local path", () => {
    const { container } = renderActions("codeg://embedded/abc-123")
    expect(screen.getByTestId("badge")).toBeInTheDocument()
    expect(document.querySelector("[data-file-actions]")).toBeNull()

    // …and its right-click still reaches the conversation panel's own menu.
    fireEvent.contextMenu(screen.getByTestId("badge"))
    expect(mocks.ancestorContextMenu).toHaveBeenCalled()
    expect(container.querySelector("[role='menu']")).toBeNull()
  })

  it("opens on right-click, and only on right-click", () => {
    renderActions("file:///repo/src/app.ts")
    // A left click belongs to the badge (it opens the file in the workspace).
    fireEvent.click(trigger())
    expect(screen.queryByRole("menu")).toBeNull()

    openMenu()
    expect(screen.getByRole("menu")).toBeInTheDocument()
  })

  it("keeps a touch long-press from arming the conversation menu too", () => {
    renderActions("file:///repo/src/app.ts")
    // jsdom's fireEvent.pointerDown drops `pointerType`, so pin it by hand.
    const event = new MouseEvent("pointerdown", {
      bubbles: true,
      cancelable: true,
    })
    Object.defineProperty(event, "pointerType", { value: "touch" })
    fireEvent(trigger(), event)

    expect(mocks.ancestorPointerDown).not.toHaveBeenCalled()
  })

  it("keeps the right-click from also opening the conversation menu", () => {
    renderActions("file:///repo/src/app.ts")
    openMenu()

    expect(screen.getByRole("menu")).toBeInTheDocument()
    // The transcript is wrapped in the conversation panel's own context menu;
    // both opening at once was the bug this stopPropagation fixes.
    expect(mocks.ancestorContextMenu).not.toHaveBeenCalled()
  })

  it("reveals the absolute path in the file manager", async () => {
    renderActions("file:///repo/src/app.ts#L10-25")
    openMenu()

    fireEvent.click(screen.getByRole("menuitem", { name: /^Open in/ }))
    await waitFor(() => {
      expect(mocks.revealItemInDir).toHaveBeenCalledWith("/repo/src/app.ts")
    })
  })

  it("copies the relative and the absolute path", async () => {
    renderActions("file:///repo/src/app.ts")
    openMenu()

    fireEvent.click(item("Copy relative path"))
    await waitFor(() => {
      expect(mocks.copyTextFromMenu).toHaveBeenCalledWith("src/app.ts")
    })
    expect(mocks.toastSuccess).toHaveBeenCalled()

    openMenu()
    fireEvent.click(item("Copy absolute path"))
    await waitFor(() => {
      expect(mocks.copyTextFromMenu).toHaveBeenCalledWith("/repo/src/app.ts")
    })
  })

  it("toasts when the copy fails", async () => {
    mocks.copyTextFromMenu.mockResolvedValue(false)
    renderActions("file:///repo/src/app.ts")
    openMenu()

    fireEvent.click(item("Copy absolute path"))
    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenCalled()
    })
    expect(mocks.toastSuccess).not.toHaveBeenCalled()
  })

  it("disables the relative copy for a file outside the active folder", () => {
    renderActions("file:///elsewhere/a.ts")
    openMenu()

    expect(item("Copy relative path")).toHaveAttribute("data-disabled")
    expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
  })

  it("hides the reveal row when the file manager is unreachable (web / remote)", () => {
    mocks.isLocalDesktop.mockReturnValue(false)
    renderActions("file:///repo/src/app.ts")
    openMenu()

    expect(screen.queryByRole("menuitem", { name: /^Open in/ })).toBeNull()
    expect(item("Copy absolute path")).toBeInTheDocument()
  })

  it("gated menu is disabled while loading then uses session absolute only", async () => {
    const pending = deferred<GrokSessionImageResolution>()
    mocks.resolveGrokSessionImage.mockReturnValue(pending.promise)
    renderActions("images/a.png", { conversationId: 42, phase: "complete" })
    openMenu()

    expect(item("Copy relative path")).toHaveAttribute("data-disabled")
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
    expect(revealItem()).toHaveAttribute("data-disabled")
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledWith({
      conversationId: 42,
      href: "images/a.png",
      includeData: false,
    })

    await act(async () => {
      pending.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
      })
    })

    expect(item("Copy relative path")).toHaveAttribute("data-disabled")
    expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
    expect(revealItem()).not.toHaveAttribute("data-disabled")
    fireEvent.click(revealItem())
    await waitFor(() => {
      expect(mocks.revealItemInDir).toHaveBeenCalledWith(
        "/session/images/a.png"
      )
    })
  })

  it("workspace menu exposes canonical resolver-relative path", async () => {
    mocks.resolveGrokSessionImage.mockResolvedValue({
      path: "/origin/images/a.png",
      origin: "workspace",
      mimeType: "image/png",
    })
    renderActions("./images/a.png", {
      conversationId: 42,
      phase: "complete",
    })
    openMenu()

    await waitFor(() =>
      expect(item("Copy relative path")).not.toHaveAttribute("data-disabled")
    )
    fireEvent.click(item("Copy relative path"))
    await waitFor(() => {
      expect(mocks.copyTextFromMenu).toHaveBeenCalledWith("images/a.png")
    })
    expect(mocks.resolveGrokSessionImage).toHaveBeenCalledWith({
      conversationId: 42,
      href: "./images/a.png",
      includeData: false,
    })
  })

  it("menu_failure_disables_every_gated_action_without_toast_or_active_folder_fallback", async () => {
    mocks.resolveGrokSessionImage.mockRejectedValue(new Error("unavailable"))
    renderActions("images/a.png", { conversationId: 42, phase: "complete" })
    openMenu()

    await waitFor(() => {
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledWith({
        conversationId: 42,
        href: "images/a.png",
        includeData: false,
      })
    })
    expect(revealItem()).toHaveAttribute("data-disabled")
    expect(item("Copy relative path")).toHaveAttribute("data-disabled")
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
    fireEvent.click(item("Copy absolute path"))
    fireEvent.click(revealItem())
    expect(mocks.copyTextFromMenu).not.toHaveBeenCalled()
    expect(mocks.revealItemInDir).not.toHaveBeenCalled()
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("menu_unmount_or_target_change_ignores_late_resolution", async () => {
    const first = deferred<GrokSessionImageResolution>()
    const second = deferred<GrokSessionImageResolution>()
    mocks.resolveGrokSessionImage
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    const view = renderActions("images/a.png", {
      conversationId: 42,
      phase: "complete",
    })
    openMenu()

    view.rerender(
      actionsTree("images/b.png", {
        conversationId: 42,
        phase: "complete",
      })
    )
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(2)
    )
    await act(async () => {
      first.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
      })
    })
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")

    view.unmount()
    await act(async () => {
      second.resolve({
        path: "/session/images/b.png",
        origin: "session",
        mimeType: "image/png",
      })
    })
    expect(mocks.copyTextFromMenu).not.toHaveBeenCalled()
    expect(mocks.revealItemInDir).not.toHaveBeenCalled()
    expect(mocks.toastError).not.toHaveBeenCalled()
  })

  it("target_a_to_b_to_a_never_reuses_a_prior_ready_menu_result", async () => {
    const b = deferred<GrokSessionImageResolution>()
    const nextA = deferred<GrokSessionImageResolution>()
    mocks.resolveGrokSessionImage
      .mockResolvedValueOnce({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
      })
      .mockReturnValueOnce(b.promise)
      .mockReturnValueOnce(nextA.promise)
    const view = renderActions("images/a.png", {
      conversationId: 42,
      phase: "complete",
    })
    openMenu()
    await waitFor(() =>
      expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
    )

    view.rerender(
      actionsTree("images/b.png", {
        conversationId: 42,
        phase: "complete",
      })
    )
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(2)
    )
    view.rerender(
      actionsTree("images/a.png", {
        conversationId: 42,
        phase: "complete",
      })
    )
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(3)
    )

    await act(async () => {
      b.resolve({
        path: "/session/images/b.png",
        origin: "session",
        mimeType: "image/png",
      })
    })
    expect(item("Copy absolute path")).toHaveAttribute("data-disabled")

    await act(async () => {
      nextA.resolve({
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/png",
      })
    })
    expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
  })

  it("committed_target_and_scope_changes_fail_closed_before_passive_effects", async () => {
    const targetB = deferred<GrokSessionImageResolution>()
    const scopeB = deferred<GrokSessionImageResolution>()
    const scopeAAgain = deferred<GrokSessionImageResolution>()
    mocks.resolveGrokSessionImage
      .mockResolvedValueOnce({
        path: "/workspace/images/a.png",
        origin: "workspace",
        mimeType: "image/png",
      })
      .mockReturnValueOnce(targetB.promise)
      .mockReturnValueOnce(scopeB.promise)
      .mockReturnValueOnce(scopeAAgain.promise)
    const snapshots: MenuLayoutSnapshot[] = []
    const captureLayout = (snapshot: MenuLayoutSnapshot) => {
      snapshots.push(snapshot)
    }
    const latestSnapshot = () => snapshots[snapshots.length - 1]
    const view = renderActions(
      "images/a.png",
      { conversationId: 42, phase: "complete" },
      captureLayout
    )
    openMenu()
    await waitFor(() =>
      expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
    )

    view.rerender(
      actionsTree(
        "images/b.png",
        { conversationId: 42, phase: "complete" },
        captureLayout
      )
    )
    expect(latestSnapshot()).toEqual({
      target: "images/b.png",
      conversationId: 42,
      revealDisabled: true,
      relativeDisabled: true,
      absoluteDisabled: true,
    })
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(2)
    )

    await act(async () => {
      targetB.resolve({
        path: "/workspace/images/b.png",
        origin: "workspace",
        mimeType: "image/png",
      })
    })
    expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")

    view.rerender(
      actionsTree(
        "images/b.png",
        { conversationId: 43, phase: "complete" },
        captureLayout
      )
    )
    expect(latestSnapshot()).toEqual({
      target: "images/b.png",
      conversationId: 43,
      revealDisabled: true,
      relativeDisabled: true,
      absoluteDisabled: true,
    })
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(3)
    )

    view.rerender(
      actionsTree(
        "images/b.png",
        { conversationId: 42, phase: "complete" },
        captureLayout
      )
    )
    expect(latestSnapshot()).toEqual({
      target: "images/b.png",
      conversationId: 42,
      revealDisabled: true,
      relativeDisabled: true,
      absoluteDisabled: true,
    })
    await waitFor(() =>
      expect(mocks.resolveGrokSessionImage).toHaveBeenCalledTimes(4)
    )

    view.unmount()
    await act(async () => {
      scopeB.resolve({
        path: "/workspace/images/b.png",
        origin: "workspace",
        mimeType: "image/png",
      })
      scopeAAgain.resolve({
        path: "/workspace/images/b.png",
        origin: "workspace",
        mimeType: "image/png",
      })
    })
  })

  it("malformed_menu_resolution_fails_closed", async () => {
    const malformed = [
      null,
      {
        path: "images/a.png",
        origin: "session",
        mimeType: "image/png",
      },
      {
        path: "/session/images/a.png",
        origin: "invalid",
        mimeType: "image/png",
      },
      {
        path: "/session/images/a.png",
        origin: "session",
        mimeType: "image/jpeg",
      },
    ]

    for (const resolution of malformed) {
      const pending = deferred<unknown>()
      mocks.resolveGrokSessionImage.mockReturnValueOnce(pending.promise)
      const view = renderActions("images/a.png", {
        conversationId: 42,
        phase: "complete",
      })
      openMenu()
      await act(async () => {
        pending.resolve(resolution)
      })
      expect(revealItem()).toHaveAttribute("data-disabled")
      expect(item("Copy relative path")).toHaveAttribute("data-disabled")
      expect(item("Copy absolute path")).toHaveAttribute("data-disabled")
      view.unmount()
    }

    expect(mocks.toastError).not.toHaveBeenCalled()
    expect(mocks.copyTextFromMenu).not.toHaveBeenCalled()
    expect(mocks.revealItemInDir).not.toHaveBeenCalled()
  })

  it("ungated_menu_keeps_synchronous_existing_paths", () => {
    renderActions("file:///repo/src/app.ts")
    openMenu()

    expect(revealItem()).not.toHaveAttribute("data-disabled")
    expect(item("Copy relative path")).not.toHaveAttribute("data-disabled")
    expect(item("Copy absolute path")).not.toHaveAttribute("data-disabled")
    expect(mocks.resolveGrokSessionImage).not.toHaveBeenCalled()
  })
})
