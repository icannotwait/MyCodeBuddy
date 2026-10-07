import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { FOCUS_COMPOSER_EVENT } from "@/lib/conversation-popout-detached-bootstrap"
import {
  bindWebPopoutOwnerPresence,
  focusOrRefuseWebPopout,
  publishWebPopoutOpened,
  rememberWebPopoutWindow,
  webPopoutConversationIdsBlockingMain,
  __resetWebPopoutPresenceForTests,
  __setWebPopoutTimingsForTests,
} from "@/lib/conversation-popout-web-presence"

class MemoryBroadcastChannel {
  static readonly peers = new Map<string, Set<MemoryBroadcastChannel>>()
  onmessage: ((event: MessageEvent) => void) | null = null
  private readonly listeners = new Set<(event: MessageEvent) => void>()

  constructor(private readonly name: string) {
    let peers = MemoryBroadcastChannel.peers.get(name)
    if (!peers) {
      peers = new Set()
      MemoryBroadcastChannel.peers.set(name, peers)
    }
    peers.add(this)
  }

  postMessage(data: unknown) {
    const event = { data } as MessageEvent
    for (const peer of MemoryBroadcastChannel.peers.get(this.name) ?? []) {
      if (peer === this) continue
      queueMicrotask(() => {
        peer.onmessage?.(event)
        for (const listener of peer.listeners) listener(event)
      })
    }
  }

  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    if (type === "message") this.listeners.add(listener)
  }

  removeEventListener(type: string, listener: (event: MessageEvent) => void) {
    if (type === "message") this.listeners.delete(listener)
  }

  close() {
    MemoryBroadcastChannel.peers.get(this.name)?.delete(this)
  }
}

beforeEach(() => {
  MemoryBroadcastChannel.peers.clear()
  vi.stubGlobal("BroadcastChannel", MemoryBroadcastChannel)
})

afterEach(() => {
  __resetWebPopoutPresenceForTests()
  vi.unstubAllGlobals()
})

describe("web pop-out presence", () => {
  it("focuses a remembered window without asking the channel", async () => {
    const focus = vi.fn()
    const popup = { closed: false, focus } as unknown as Window
    publishWebPopoutOpened({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })
    rememberWebPopoutWindow(7, popup)

    await expect(focusOrRefuseWebPopout(7)).resolves.toBe("focused")
    expect(focus).toHaveBeenCalledTimes(1)
    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(true)
  })

  it("lets the pop-out document ack focus and stay the owner", async () => {
    const stop = bindWebPopoutOwnerPresence({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })
    const focused = vi.fn()
    vi.spyOn(window, "focus").mockImplementation(() => {})
    window.addEventListener(FOCUS_COMPOSER_EVENT, focused)

    try {
      await expect(focusOrRefuseWebPopout(7)).resolves.toBe("focused")
      expect(focused).toHaveBeenCalledTimes(1)
      expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(true)
    } finally {
      window.removeEventListener(FOCUS_COMPOSER_EVENT, focused)
      stop()
    }
  })

  it("refuses when presence is fresh but nothing acks focus", async () => {
    __setWebPopoutTimingsForTests({ ackTimeoutMs: 20 })
    publishWebPopoutOpened({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })

    await expect(focusOrRefuseWebPopout(7)).resolves.toBe("refused")
    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(true)
  })

  it("clears a stale record when the pop-out does not answer", async () => {
    __setWebPopoutTimingsForTests({ ackTimeoutMs: 20, staleMs: 5 })
    publishWebPopoutOpened({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })
    await new Promise((resolve) => setTimeout(resolve, 15))

    await expect(focusOrRefuseWebPopout(7)).resolves.toBe("absent")
    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(false)
  })

  it("stops blocking after the pop-out document leaves", async () => {
    __setWebPopoutTimingsForTests({ reloadGraceMs: 30 })
    const stop = bindWebPopoutOwnerPresence({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })
    stop()

    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(true)
    await new Promise((resolve) => setTimeout(resolve, 40))
    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(false)
    await expect(focusOrRefuseWebPopout(7)).resolves.toBe("absent")
  })

  it("drops a closed window handle so main can own again", async () => {
    const popup = { closed: false, focus: vi.fn() }
    publishWebPopoutOpened({
      conversationId: 7,
      folderId: 3,
      agentType: "codex",
    })
    rememberWebPopoutWindow(7, popup as unknown as Window)
    popup.closed = true

    await expect(focusOrRefuseWebPopout(7)).resolves.toBe("absent")
    expect(webPopoutConversationIdsBlockingMain().has(7)).toBe(false)
  })
})
