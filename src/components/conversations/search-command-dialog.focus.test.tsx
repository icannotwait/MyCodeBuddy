import { describe, expect, it, vi, beforeEach } from "vitest"
import { act, render, waitFor, fireEvent } from "@testing-library/react"
import type { DbConversationSummary } from "@/lib/types"

const openTab = vi.fn(async () => true)
const openConversations = vi.fn()

const conv: DbConversationSummary = {
  id: 42,
  folder_id: 1,
  title: "Detached chat",
  title_locked: false,
  auto_title_finalized: false,
  agent_type: "claude_code",
  status: "pending",
  awaiting_reply_token: null,
  kind: "regular",
  model: null,
  git_branch: null,
  external_id: null,
  message_count: 0,
  child_count: 0,
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
  pinned_at: null,
}

vi.mock("@/contexts/tab-context", () => ({
  useTabActions: () => ({ openTab }),
}))

vi.mock("@/contexts/workbench-route-context", () => ({
  useWorkbenchRoute: () => ({ openConversations }),
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolder: { id: 1, name: "proj", path: "/x" },
    activeFolderId: 1,
  }),
}))

vi.mock("@/stores/app-workspace-store", () => ({
  useAppWorkspaceStore: (
    selector: (s: { conversations: DbConversationSummary[] }) => unknown
  ) => selector({ conversations: [conv] }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({ openFilePreview: vi.fn() }),
}))

vi.mock("@/contexts/aux-panel-context", () => ({
  useAuxPanelContext: () => ({ revealInFileTree: vi.fn() }),
}))

vi.mock("@/hooks/use-workspace-file-search", () => ({
  useWorkspaceFileSearch: () => ({
    files: [],
    loading: false,
  }),
}))

vi.mock("@/lib/api", () => ({
  listAllConversations: vi.fn(async () => [conv]),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string, values?: { name?: string }) =>
    values?.name ? `${key}:${values.name}` : key,
  useLocale: () => "en",
}))

import { SearchCommandDialog } from "./search-command-dialog"

describe("SearchCommandDialog focus-before-open via openTab", () => {
  beforeEach(() => {
    openTab.mockReset()
    openConversations.mockReset()
    openTab.mockResolvedValue(true)
  })

  async function selectFirstResult() {
    const onOpenChange = vi.fn()
    const view = render(
      <SearchCommandDialog open onOpenChange={onOpenChange} />
    )
    const input = view.getByPlaceholderText("placeholder")
    fireEvent.change(input, { target: { value: "Detached" } })
    await waitFor(() => {
      expect(view.getByText("Detached chat")).toBeTruthy()
    })
    await act(async () => {
      view.getByText("Detached chat").click()
    })
    return { onOpenChange }
  }

  it("routes selection through openTab and skips openConversations when focus short-circuits", async () => {
    openTab.mockResolvedValue(false)
    const { onOpenChange } = await selectFirstResult()

    await waitFor(() => {
      expect(openTab).toHaveBeenCalledWith(1, 42, "claude_code", true)
    })
    expect(openConversations).not.toHaveBeenCalled()
    expect(onOpenChange).toHaveBeenCalledWith(false)
  })

  it("opens conversation pane when openTab opens/activates a main tab", async () => {
    openTab.mockResolvedValue(true)
    await selectFirstResult()

    await waitFor(() => {
      expect(openTab).toHaveBeenCalledWith(1, 42, "claude_code", true)
      expect(openConversations).toHaveBeenCalled()
    })
  })
})
