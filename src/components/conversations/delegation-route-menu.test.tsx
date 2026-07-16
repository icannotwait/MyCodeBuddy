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
})
