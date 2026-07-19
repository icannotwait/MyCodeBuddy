import { act, cleanup, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import type { ComponentProps } from "react"
import { afterEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { PromptCapabilitiesInfo } from "@/lib/types"
import type { ContinuationWaitingProjection } from "@/lib/types"

import type { RichComposerHandle } from "./composer/rich-composer"
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

// Capture MessageInput's internal RichComposer handle so tests can set text and
// drive a real Enter submit (not a mock-existence assertion).
const composerHandle = vi.hoisted(() => ({
  current: null as RichComposerHandle | null,
}))
vi.mock("./composer/rich-composer", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./composer/rich-composer")>()
  const React = await import("react")
  const Captured = React.forwardRef<
    RichComposerHandle,
    ComponentProps<typeof actual.RichComposer>
  >((props, ref) => {
    const assign = (handle: RichComposerHandle | null) => {
      composerHandle.current = handle
      if (typeof ref === "function") ref(handle)
      else if (ref) ref.current = handle
    }
    return React.createElement(actual.RichComposer, { ...props, ref: assign })
  })
  Captured.displayName = "CapturedRichComposer"
  return { ...actual, RichComposer: Captured }
})

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

function submitComposerEnter() {
  const dom = composerHandle.current?.getEditor()?.view.dom as
    | HTMLElement
    | undefined
  expect(dom).toBeTruthy()
  act(() => {
    dom!.dispatchEvent(
      new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        key: "Enter",
      })
    )
  })
}

describe("ChatInput waiting-for-subagents wiring", () => {
  afterEach(() => {
    cleanup()
    composerHandle.current = null
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

  it("waiting + prompting does not enqueue on real Enter submit (Stop stays visible)", async () => {
    // Resume can leave status=prompting while waitingForSubagents is still set.
    // MessageInput intentionally enqueues when isPrompting bypasses disabled —
    // ChatInput must not pass that combo while waiting.
    const onEnqueue = vi.fn()
    const onSend = vi.fn()
    const onCancel = vi.fn()
    const { container } = renderChat({
      status: "prompting",
      waitingForSubagents: WAITING,
      onEnqueue,
      onSend,
      onCancel,
    })

    await waitFor(() =>
      expect(composerHandle.current?.getEditor()).toBeTruthy()
    )

    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    const stop = container.querySelector<HTMLButtonElement>(
      `button[title="${cancelTitle}"]`
    )
    expect(stop).not.toBeNull()
    expect(stop).not.toBeDisabled()

    act(() => {
      composerHandle.current?.setText("must not enqueue while waiting")
    })
    expect(composerHandle.current?.getText()).toContain(
      "must not enqueue while waiting"
    )

    submitComposerEnter()

    expect(onEnqueue).not.toHaveBeenCalled()
    expect(onSend).not.toHaveBeenCalled()
    // Draft remains (composer not reset by enqueue/send path).
    expect(composerHandle.current?.getText()).toContain(
      "must not enqueue while waiting"
    )
  })

  it("ordinary prompting without waiting still enqueues on Enter", async () => {
    const onEnqueue = vi.fn()
    const onSend = vi.fn()
    renderChat({
      status: "prompting",
      waitingForSubagents: null,
      onEnqueue,
      onSend,
    })

    await waitFor(() =>
      expect(composerHandle.current?.getEditor()).toBeTruthy()
    )

    act(() => {
      composerHandle.current?.setText("queue me please")
    })
    submitComposerEnter()

    expect(onEnqueue).toHaveBeenCalledTimes(1)
    const draft = onEnqueue.mock.calls[0]?.[0] as {
      displayText?: string
      blocks?: Array<{ text?: string }>
    }
    const draftText =
      draft?.displayText ?? draft?.blocks?.map((b) => b.text).join("") ?? ""
    expect(draftText).toContain("queue me please")
    expect(onSend).not.toHaveBeenCalled()
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
    // Dual mount: waiting composer is locked; non-waiting sibling still sends.
    const onEnqueueWaiting = vi.fn()
    const onEnqueueIdle = vi.fn()
    const cancelTitle = enMessages.Folder.chat.messageInput.cancel
    const sendTitle = enMessages.Folder.chat.messageInput.send

    const { container } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <div data-testid="waiting-composer">
          <ChatInput
            status="prompting"
            promptCapabilities={CAPS}
            onSend={vi.fn()}
            onCancel={vi.fn()}
            onEnqueue={onEnqueueWaiting}
            waitingForSubagents={WAITING}
          />
        </div>
        <div data-testid="idle-composer">
          <ChatInput
            status="connected"
            promptCapabilities={CAPS}
            onSend={vi.fn()}
            onCancel={vi.fn()}
            onEnqueue={onEnqueueIdle}
            waitingForSubagents={null}
          />
        </div>
      </NextIntlClientProvider>
    )

    await waitFor(() =>
      expect(container.querySelectorAll('[role="textbox"]').length).toBe(2)
    )

    const waitingRoot = screen.getByTestId("waiting-composer")
    const idleRoot = screen.getByTestId("idle-composer")

    expect(
      waitingRoot.querySelector(`button[title="${cancelTitle}"]`)
    ).not.toBeNull()
    expect(waitingRoot.querySelector(`button[title="${sendTitle}"]`)).toBeNull()

    expect(
      idleRoot.querySelector(`button[title="${sendTitle}"]`)
    ).not.toBeNull()
    expect(idleRoot.querySelector(`button[title="${cancelTitle}"]`)).toBeNull()
  })
})
