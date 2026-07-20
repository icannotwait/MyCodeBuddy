import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/platform", () => ({
  isLocalDesktop: vi.fn(() => true),
  subscribe: vi.fn(async () => () => {}),
}))

vi.mock("@/lib/api", () => ({
  focusConversationWindow: vi.fn(async () => false),
  openConversationWindow: vi.fn(async () => "opened"),
  closeConversationWindow: vi.fn(async () => true),
  completeConversationPopoutOperation: vi.fn(async () => ({
    phase: "handoff_complete",
  })),
  abortConversationPopoutOperation: vi.fn(async () => ({})),
}))

import { isLocalDesktop } from "@/lib/platform"
import { canPopOutConversation } from "@/lib/conversation-popout"

describe("canPopOutConversation", () => {
  beforeEach(() => {
    vi.mocked(isLocalDesktop).mockReturnValue(true)
  })

  it("disables for draft", () => {
    expect(
      canPopOutConversation({
        conversationId: null,
        isOpenMainTab: true,
        mainTabCount: 3,
      })
    ).toEqual({ enabled: false, reason: "draft" })
  })

  it("disables for last main tab", () => {
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 1,
      })
    ).toEqual({ enabled: false, reason: "last_tab" })
  })

  it("enables when multiple tabs", () => {
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 2,
      })
    ).toEqual({ enabled: true })
  })

  it("hides for non-local desktop", () => {
    vi.mocked(isLocalDesktop).mockReturnValue(false)
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 2,
      })
    ).toEqual({ enabled: false, reason: "not_local_desktop" })
  })
})
