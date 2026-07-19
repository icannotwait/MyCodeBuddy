import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { AgentType, DelegationRoutePolicy } from "@/lib/types"
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"

const h = vi.hoisted(() => ({
  setConversationDelegationRoute: vi.fn(),
  setDraftDelegationRoutePreference: vi.fn(),
  applyConversationUpsert: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  setConversationDelegationRoute: h.setConversationDelegationRoute,
  setDraftDelegationRoutePreference: h.setDraftDelegationRoutePreference,
}))

vi.mock("@/stores/app-workspace-store", () => ({
  useAppWorkspaceStore: (
    sel: (s: {
      applyConversationUpsert: typeof h.applyConversationUpsert
    }) => unknown
  ) => sel({ applyConversationUpsert: h.applyConversationUpsert }),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

import { DelegationRouteMenu } from "./delegation-route-menu"

function renderMenu(props: {
  agentType: AgentType
  conversationId: number | null
  parentId?: number | null
  connectionId?: string | null
  value: DelegationRoutePolicy | null
  onDraftChange?: (value: DelegationRoutePolicy | null) => void
}) {
  const view = render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <button type="button">open-menu</button>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <DelegationRouteMenu {...props} />
        </ContextMenuContent>
      </ContextMenu>
    </NextIntlClientProvider>
  )
  fireEvent.contextMenu(screen.getByText("open-menu"))
  return view
}

beforeEach(() => {
  h.setConversationDelegationRoute.mockReset().mockResolvedValue({
    id: 12,
    folder_id: 1,
    title: null,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "codex",
    status: "idle",
    kind: "chat",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "",
    updated_at: "",
    pinned_at: null,
  })
  h.setDraftDelegationRoutePreference.mockReset().mockResolvedValue(undefined)
  h.applyConversationUpsert.mockReset()
})

describe("DelegationRouteMenu", () => {
  it.each(["codex", "grok", "code_buddy", "claude_code"] as const)(
    "offers inherit/codeg/native for managed root %s",
    async (agentType) => {
      renderMenu({
        agentType,
        conversationId: 12,
        parentId: null,
        value: null,
      })
      await userEvent.click(await screen.findByText("Delegation route"))
      expect(await screen.findByText("Inherit global")).toBeInTheDocument()
      expect(screen.getByText("Codeg")).toBeInTheDocument()
      expect(screen.getByText("Native")).toBeInTheDocument()
    }
  )

  it("shows forced Codeg read-only for a child", async () => {
    renderMenu({
      agentType: "codex",
      conversationId: 13,
      parentId: 12,
      value: null,
    })
    const item = await screen.findByText("Codeg (inherited)")
    expect(item.closest("[data-disabled]")).toBeTruthy()
  })

  it("renders nothing for unmanaged agents", () => {
    renderMenu({
      agentType: "gemini",
      conversationId: 1,
      parentId: null,
      value: null,
    })
    expect(screen.queryByText("Delegation route")).not.toBeInTheDocument()
    expect(screen.queryByText("Codeg (inherited)")).not.toBeInTheDocument()
  })

  it("marks a connected draft stale when its in-memory route changes", async () => {
    const onDraftChange = vi.fn()
    renderMenu({
      agentType: "codex",
      conversationId: null,
      parentId: null,
      connectionId: "conn-draft",
      value: null,
      onDraftChange,
    })
    await userEvent.click(await screen.findByText("Delegation route"))
    await userEvent.click(await screen.findByText("Native"))
    await waitFor(() => {
      expect(h.setDraftDelegationRoutePreference).toHaveBeenCalledWith(
        "conn-draft",
        "native"
      )
    })
    expect(onDraftChange).toHaveBeenCalledWith("native")
  })

  it("persisted radio calls setConversationDelegationRoute and applies returned upsert", async () => {
    const onPersistedChange = vi.fn()
    const summary = {
      id: 12,
      folder_id: 1,
      title: "Root",
      title_locked: false,
      auto_title_finalized: false,
      agent_type: "codex" as const,
      status: "idle",
      kind: "chat" as const,
      model: null,
      git_branch: null,
      external_id: null,
      message_count: 0,
      child_count: 0,
      created_at: "",
      updated_at: "",
      pinned_at: null,
      delegation_route_override: "native" as const,
    }
    h.setConversationDelegationRoute.mockResolvedValueOnce(summary)
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ContextMenu>
          <ContextMenuTrigger asChild>
            <button type="button">open-menu</button>
          </ContextMenuTrigger>
          <ContextMenuContent>
            <DelegationRouteMenu
              agentType="codex"
              conversationId={12}
              parentId={null}
              value={null}
              onPersistedChange={onPersistedChange}
            />
          </ContextMenuContent>
        </ContextMenu>
      </NextIntlClientProvider>
    )
    fireEvent.contextMenu(screen.getByText("open-menu"))
    await userEvent.click(await screen.findByText("Delegation route"))
    await userEvent.click(await screen.findByText("Native"))
    await waitFor(() => {
      expect(h.setConversationDelegationRoute).toHaveBeenCalledWith(
        12,
        "native"
      )
    })
    expect(h.applyConversationUpsert).toHaveBeenCalledWith(summary)
    expect(onPersistedChange).toHaveBeenCalledWith("native")
  })

  it("does not upsert until the persisted route write resolves (busy in-flight)", async () => {
    let resolveRoute!: (v: unknown) => void
    h.setConversationDelegationRoute.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveRoute = resolve
        })
    )
    renderMenu({
      agentType: "codex",
      conversationId: 12,
      parentId: null,
      value: null,
    })
    await userEvent.click(await screen.findByText("Delegation route"))
    await userEvent.click(await screen.findByText("Native"))
    await waitFor(() => {
      expect(h.setConversationDelegationRoute).toHaveBeenCalledWith(
        12,
        "native"
      )
    })
    // In-flight: API called, store not yet updated.
    expect(h.applyConversationUpsert).not.toHaveBeenCalled()
    resolveRoute({
      id: 12,
      folder_id: 1,
      title: null,
      title_locked: false,
      auto_title_finalized: false,
      agent_type: "codex",
      status: "idle",
      kind: "chat",
      model: null,
      git_branch: null,
      external_id: null,
      message_count: 0,
      child_count: 0,
      created_at: "",
      updated_at: "",
      pinned_at: null,
      delegation_route_override: "native",
    })
    await waitFor(() => {
      expect(h.applyConversationUpsert).toHaveBeenCalledTimes(1)
    })
  })

  it("toasts and does not upsert when setConversationDelegationRoute fails", async () => {
    const { toast } = await import("sonner")
    h.setConversationDelegationRoute.mockRejectedValueOnce(
      new Error("route write failed")
    )
    renderMenu({
      agentType: "codex",
      conversationId: 12,
      parentId: null,
      value: null,
    })
    await userEvent.click(await screen.findByText("Delegation route"))
    await userEvent.click(await screen.findByText("Native"))
    await waitFor(() => {
      expect(h.setConversationDelegationRoute).toHaveBeenCalledWith(
        12,
        "native"
      )
    })
    await waitFor(() => {
      expect(toast.error).toHaveBeenCalled()
    })
    expect(h.applyConversationUpsert).not.toHaveBeenCalled()
  })

  it("ContextMenuRadioItem indicator uses logical end positioning (RTL-safe)", async () => {
    // Open a real radio item and assert the indicator class is logical end-*.
    renderMenu({
      agentType: "codex",
      conversationId: 12,
      parentId: null,
      value: "native",
    })
    await userEvent.click(await screen.findByText("Delegation route"))
    const native = await screen.findByText("Native")
    const item =
      native.closest("[data-slot='context-menu-radio-item']") ??
      native.closest("[role='menuitemradio']")
    const indicator = item?.querySelector(
      "[data-slot='context-menu-radio-item-indicator']"
    )
    expect(indicator).toBeTruthy()
    expect(indicator?.className).toMatch(/\bend-2\b/)
    expect(indicator?.className).not.toMatch(/\bright-2\b/)
  })

  it("persisted radio items wire disabled={busy} for in-flight writes", async () => {
    const { readFileSync } = await import("node:fs")
    const { resolve } = await import("node:path")
    const src = readFileSync(
      resolve(
        process.cwd(),
        "src/components/conversations/delegation-route-menu.tsx"
      ),
      "utf8"
    )
    // Three radios share the busy disable; guard against accidental removal.
    expect(src.match(/disabled=\{busy\}/g)?.length).toBeGreaterThanOrEqual(3)
  })
})
