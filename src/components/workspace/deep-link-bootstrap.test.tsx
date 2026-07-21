import { act, render, waitFor, cleanup } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"

// Tab half is a mutable hook mock (same pattern as pet-focus-bridge tests).
let tabs: {
  tabsHydrated: boolean
  openTab: ReturnType<typeof vi.fn>
}

vi.mock("@/contexts/tab-context", () => ({
  useTabStore: (selector: (s: typeof tabs) => unknown) => selector(tabs),
  useTabActions: () => tabs,
}))

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}))

import { DeepLinkBootstrap } from "./deep-link-bootstrap"

function setSearch(search: string) {
  window.history.replaceState({}, "", `/workspace${search}`)
}

describe("DeepLinkBootstrap", () => {
  beforeEach(() => {
    resetAppWorkspaceStore()
    setSearch("")
    tabs = {
      tabsHydrated: false,
      openTab: vi.fn(async () => true),
    }
  })
  afterEach(() => {
    cleanup()
    setSearch("")
  })

  it("awaits openTab (deferred) and clears the deep-link URL", async () => {
    let resolveOpen!: (openedMain: boolean) => void
    const openStarted = new Promise<void>((resolveStarted) => {
      tabs.openTab = vi.fn(
        () =>
          new Promise<boolean>((resolve) => {
            resolveOpen = resolve
            resolveStarted()
          })
      )
    })

    useAppWorkspaceStore.setState({
      foldersHydrated: true,
      folders: [{ id: 7 }] as never,
      conversations: [
        {
          id: 42,
          folder_id: 7,
          agent_type: "claude_code",
        },
      ] as never,
    })
    tabs = { ...tabs, tabsHydrated: true }

    setSearch("?folderId=7&conversationId=42&agent=claude_code")
    render(<DeepLinkBootstrap />)

    await openStarted
    expect(tabs.openTab).toHaveBeenCalledWith(7, 42, "claude_code", true)
    // URL still present until the awaited openTab settles (finally block).
    expect(window.location.search).toContain("conversationId=42")

    resolveOpen(false)
    await waitFor(() => {
      expect(window.location.pathname).toBe("/workspace")
      expect(window.location.search).toBe("")
    })
  })

  it("waits for hydration before calling openTab", async () => {
    setSearch("?folderId=7&conversationId=42&agent=claude_code")
    useAppWorkspaceStore.setState({
      foldersHydrated: false,
      folders: [{ id: 7 }] as never,
      conversations: [
        {
          id: 42,
          folder_id: 7,
          agent_type: "claude_code",
        },
      ] as never,
    })
    tabs = { tabsHydrated: false, openTab: vi.fn(async () => true) }

    const { rerender } = render(<DeepLinkBootstrap />)
    expect(tabs.openTab).not.toHaveBeenCalled()

    tabs = { ...tabs, tabsHydrated: true }
    rerender(<DeepLinkBootstrap />)
    act(() => {
      useAppWorkspaceStore.setState({ foldersHydrated: true })
    })

    await waitFor(() =>
      expect(tabs.openTab).toHaveBeenCalledWith(7, 42, "claude_code", true)
    )
  })
})
