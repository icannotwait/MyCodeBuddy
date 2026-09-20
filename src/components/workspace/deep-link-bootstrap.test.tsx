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

const h = vi.hoisted(() => ({ toastError: vi.fn() }))

vi.mock("sonner", () => ({ toast: { error: h.toastError, success: vi.fn() } }))
vi.mock("@/contexts/tab-context", () => ({
  useTabStore: (selector: (s: typeof tabs) => unknown) => selector(tabs),
  useTabActions: () => tabs,
}))
// PetFocusBridge shares this module; keep its backend call off the wire.
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ subscribe: async () => () => {} }),
}))
vi.mock("@/lib/deep-link", () => ({ takePendingDeepLink: async () => null }))

import { DeepLinkBootstrap } from "./deep-link-bootstrap"

function setSearch(search: string) {
  window.history.replaceState({}, "", `/workspace${search}`)
}

const CONVERSATION = {
  id: 42,
  folder_id: 7,
  agent_type: "grok",
} as never

describe("DeepLinkBootstrap", () => {
  beforeEach(() => {
    h.toastError.mockReset()
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
      conversationsLoading: false,
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
      conversationsLoading: false,
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

  // A cold-start `codeg://session/<id>` lands here as this query string on
  // Windows/Linux. `conversations` is fetched in parallel with the folders, so
  // it routinely settles after `foldersHydrated` flips — and the URL is cleared
  // on the way out, so a premature check would reject the link for good.
  it("waits for the conversation list instead of rejecting the link", async () => {
    window.history.replaceState(
      {},
      "",
      "/workspace?folderId=7&conversationId=42&agent=grok"
    )
    useAppWorkspaceStore.setState({
      foldersHydrated: false,
      conversationsLoading: true,
      conversations: [],
      folders: [{ id: 7 }] as never,
      addFolderToWorkspaceById: vi.fn(),
    })
    tabs = { tabsHydrated: true, openTab: vi.fn() }

    render(<DeepLinkBootstrap />)
    act(() => {
      useAppWorkspaceStore.setState({ foldersHydrated: true })
    })
    await act(async () => {})
    expect(tabs.openTab).not.toHaveBeenCalled()
    expect(h.toastError).not.toHaveBeenCalled()

    act(() => {
      useAppWorkspaceStore.setState({
        conversations: [CONVERSATION],
        conversationsLoading: false,
      })
    })

    await waitFor(() =>
      expect(tabs.openTab).toHaveBeenCalledWith(7, 42, "grok", true)
    )
    expect(h.toastError).not.toHaveBeenCalled()
    await waitFor(() => expect(window.location.search).toBe(""))
  })

  it("reports a link whose conversation really is gone", async () => {
    window.history.replaceState(
      {},
      "",
      "/workspace?folderId=7&conversationId=42&agent=grok"
    )
    act(() => {
      useAppWorkspaceStore.setState({
        foldersHydrated: true,
        conversationsLoading: false,
        conversations: [],
        folders: [{ id: 7 }] as never,
      })
    })
    tabs = { tabsHydrated: true, openTab: vi.fn() }
    render(<DeepLinkBootstrap />)

    await waitFor(() => expect(h.toastError).toHaveBeenCalled())
    expect(tabs.openTab).not.toHaveBeenCalled()
  })

  // The conversation list is global, but `addFolderToWorkspaceById` awaits a
  // backend round trip that refreshes the workspace — the pre-await snapshot
  // is stale by the time the check runs.
  it("re-reads the conversation list after opening the folder", async () => {
    window.history.replaceState(
      {},
      "",
      "/workspace?folderId=7&conversationId=42&agent=grok"
    )
    const addFolderToWorkspaceById = vi.fn(async () => {
      useAppWorkspaceStore.setState({ conversations: [CONVERSATION] })
      return { id: 7 } as never
    })
    act(() => {
      useAppWorkspaceStore.setState({
        foldersHydrated: true,
        conversationsLoading: false,
        conversations: [],
        folders: [],
        addFolderToWorkspaceById,
      })
    })
    tabs = { tabsHydrated: true, openTab: vi.fn() }
    render(<DeepLinkBootstrap />)

    await waitFor(() =>
      expect(tabs.openTab).toHaveBeenCalledWith(7, 42, "grok", true)
    )
    expect(addFolderToWorkspaceById).toHaveBeenCalledWith(7)
    expect(h.toastError).not.toHaveBeenCalled()
  })

  it("does nothing without deep-link params", async () => {
    window.history.replaceState({}, "", "/workspace")
    act(() => {
      useAppWorkspaceStore.setState({
        foldersHydrated: true,
        conversationsLoading: false,
      })
    })
    tabs = { tabsHydrated: true, openTab: vi.fn() }
    render(<DeepLinkBootstrap />)
    await act(async () => {})
    expect(tabs.openTab).not.toHaveBeenCalled()
    expect(h.toastError).not.toHaveBeenCalled()
  })
})
