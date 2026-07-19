import { cleanup, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { PromptCapabilitiesInfo } from "@/lib/types"
import type { ContinuationWaitingProjection } from "@/lib/types"

import { ChatInput } from "./chat-input"

const CAPS: PromptCapabilitiesInfo = {
  image: true,
  audio: false,
  embedded_context: true,
}

const WAITING: ContinuationWaitingProjection = {
  conversation_id: 42,
  state: "waiting",
  generation: 1,
  armed_at: "2026-01-01T00:00:00.000Z",
  wake_at: "2026-01-01T00:04:00.000Z",
}

vi.mock("@/hooks/use-shortcut-settings", () => ({
  useShortcutSettings: () => ({
    shortcuts: { send_message: "enter", newline_in_message: "shift+enter" },
  }),
}))
vi.mock("@/hooks/use-agent-skills", () => ({ useAgentSkills: () => [] }))
vi.mock("@/hooks/use-built-in-experts", () => ({ useBuiltInExperts: () => [] }))
vi.mock("@/hooks/use-built-in-science", () => ({ useBuiltInScience: () => [] }))
vi.mock("@/hooks/use-enabled-skill-ids", () => ({
  useEnabledSkillIds: () => ({
    enabledIds: new Set(),
    ready: false,
    supported: true,
  }),
}))
vi.mock("@/components/chat/composer/use-reference-search", () => ({
  useReferenceSearchController: () => null,
}))
vi.mock("@/components/chat/conversation-context-bar", () => ({
  ConversationContextBar: ({
    extraContent,
  }: {
    extraContent?: React.ReactNode
  }) => <div data-testid="ctx-bar">{extraContent}</div>,
  ConversationFolderBranchPicker: () => null,
  useConversationFolderBranchPickerVisible: () => false,
}))
vi.mock("@/lib/platform", () => ({
  isDesktop: () => false,
  openFileDialog: vi.fn(),
}))
vi.mock("@/lib/transport", () => ({
  getActiveRemoteConnectionId: () => null,
}))

function renderChat(
  props: Partial<React.ComponentProps<typeof ChatInput>> = {}
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ChatInput
        status="connected"
        promptCapabilities={CAPS}
        onSend={vi.fn()}
        onCancel={vi.fn()}
        {...props}
      />
    </NextIntlClientProvider>
  )
}

describe("ChatInput waiting-for-subagents wiring", () => {
  afterEach(() => {
    cleanup()
  })

  it("while waiting renders Stop with connected status and keeps the editor editable", async () => {
    const onCancel = vi.fn()
    const { container } = renderChat({
      status: "connected",
      waitingForSubagents: WAITING,
      onCancel,
    })

    await waitFor(() =>
      expect(container.querySelector('[role="textbox"]')).not.toBeNull()
    )

    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    const stop = container.querySelector<HTMLButtonElement>(
      `button[title="${cancelTitle}"]`
    )
    expect(stop).not.toBeNull()
    expect(stop).not.toBeDisabled()

    const textbox = container.querySelector('[role="textbox"]') as HTMLElement
    // Waiting placeholder is on the composer aria-label; hint is visible copy.
    expect(textbox.getAttribute("aria-label")).toBe(
      enMessages.Folder.chat.chatInput.waitingForSubagents
    )
    expect(
      screen.getByText(enMessages.Folder.chat.chatInput.waitingForSubagentsHint)
    ).toBeTruthy()

    // Editor remains editable while send is locked (contenteditable="true").
    expect(textbox.getAttribute("contenteditable")).toBe("true")

    // Send is not shown while waiting — Stop replaces it.
    const sendTitle = enMessages.Folder.chat.messageInput.send
    expect(container.querySelector(`button[title="${sendTitle}"]`)).toBeNull()
  })

  it("waiting still shows Stop while a queue item is being edited (save-edit stays save)", async () => {
    const onCancel = vi.fn()
    const onSaveQueueEdit = vi.fn()
    const { container } = renderChat({
      status: "connected",
      waitingForSubagents: WAITING,
      onCancel,
      isEditingQueueItem: true,
      editingItemId: "q1",
      editingDraftText: "queued text",
      onSaveQueueEdit,
      onCancelQueueEdit: vi.fn(),
    })

    await waitFor(() =>
      expect(container.querySelector('[role="textbox"]')).not.toBeNull()
    )

    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    expect(
      container.querySelector(`button[title="${cancelTitle}"]`)
    ).not.toBeNull()

    // Save-edit control remains (Check), not converted into Send.
    const saveTitle = enMessages.Folder.chat.messageQueue.saveEdit
    expect(
      container.querySelector(`button[title="${saveTitle}"]`)
    ).not.toBeNull()
    const sendTitle = enMessages.Folder.chat.messageInput.send
    expect(container.querySelector(`button[title="${sendTitle}"]`)).toBeNull()
  })

  it("allowOfflineCompose cannot bypass the waiting lock", async () => {
    const onSend = vi.fn()
    const { container } = renderChat({
      status: "disconnected",
      allowOfflineCompose: true,
      waitingForSubagents: WAITING,
      onSend,
    })

    await waitFor(() =>
      expect(container.querySelector('[role="textbox"]')).not.toBeNull()
    )

    // Stop visible; Send still hidden despite allowOfflineCompose.
    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    expect(
      container.querySelector(`button[title="${cancelTitle}"]`)
    ).not.toBeNull()
    const sendTitle = enMessages.Folder.chat.messageInput.send
    expect(container.querySelector(`button[title="${sendTitle}"]`)).toBeNull()
  })

  it("waiting on one conversation leaves another composer send path unchanged", async () => {
    // Without waitingForSubagents the connected composer still shows Send.
    const { container } = renderChat({
      status: "connected",
      waitingForSubagents: null,
    })

    await waitFor(() =>
      expect(container.querySelector('[role="textbox"]')).not.toBeNull()
    )

    const sendTitle = enMessages.Folder.chat.messageInput.send
    expect(
      container.querySelector(`button[title="${sendTitle}"]`)
    ).not.toBeNull()
    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    expect(container.querySelector(`button[title="${cancelTitle}"]`)).toBeNull()
  })
})
