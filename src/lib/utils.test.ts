import type { MouseEvent } from "react"
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest"

import {
  cn,
  copyTextFromMenu,
  copyTextToClipboard,
  handleMiddleClickClose,
  installClipboardFallback,
  randomUUID,
} from "./utils"

// jsdom does not implement document.execCommand; provide a stub so the legacy
// clipboard-copy path can be spied on.
beforeAll(() => {
  if (typeof document.execCommand !== "function") {
    document.execCommand = () => false
  }
})

function mouseEventWithButton(button: number) {
  const preventDefault = vi.fn()
  const event = { button, preventDefault } as unknown as MouseEvent
  return { event, preventDefault }
}

describe("handleMiddleClickClose", () => {
  it("closes and prevents default on middle-click (button 1)", () => {
    const onClose = vi.fn()
    const { event, preventDefault } = mouseEventWithButton(1)

    handleMiddleClickClose(event, onClose)

    expect(onClose).toHaveBeenCalledTimes(1)
    expect(preventDefault).toHaveBeenCalledTimes(1)
  })

  it("ignores left-click (button 0)", () => {
    const onClose = vi.fn()
    const { event, preventDefault } = mouseEventWithButton(0)

    handleMiddleClickClose(event, onClose)

    expect(onClose).not.toHaveBeenCalled()
    expect(preventDefault).not.toHaveBeenCalled()
  })

  it("ignores right-click (button 2) so the context menu still opens", () => {
    const onClose = vi.fn()
    const { event, preventDefault } = mouseEventWithButton(2)

    handleMiddleClickClose(event, onClose)

    expect(onClose).not.toHaveBeenCalled()
    expect(preventDefault).not.toHaveBeenCalled()
  })
})

describe("cn", () => {
  it("merges class names", () => {
    expect(cn("a", "b")).toBe("a b")
  })

  it("drops falsy values", () => {
    expect(cn("a", false, null, undefined, "b")).toBe("a b")
  })

  it("dedupes conflicting tailwind classes, keeping the last", () => {
    expect(cn("px-2", "px-4")).toBe("px-4")
  })

  it("resolves conditional object syntax", () => {
    expect(cn("base", { active: true, hidden: false })).toBe("base active")
  })
})

describe("copyTextToClipboard", () => {
  const originalClipboard = navigator.clipboard

  afterEach(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: originalClipboard,
    })
    vi.restoreAllMocks()
  })

  function setClipboard(value: unknown) {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value,
    })
  }

  it("uses the async Clipboard API when available", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    setClipboard({ writeText })

    await expect(copyTextToClipboard("hello")).resolves.toBe(true)
    expect(writeText).toHaveBeenCalledWith("hello")
  })

  it("falls back to execCommand when the async API rejects", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"))
    setClipboard({ writeText })
    const execCommand = vi.spyOn(document, "execCommand").mockReturnValue(true)

    await expect(copyTextToClipboard("hi")).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith("copy")
  })

  it("falls back to execCommand when the async API is absent", async () => {
    setClipboard(undefined)
    const execCommand = vi.spyOn(document, "execCommand").mockReturnValue(true)

    await expect(copyTextToClipboard("hi")).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith("copy")
  })

  it("returns false when the legacy copy command reports failure", async () => {
    setClipboard(undefined)
    vi.spyOn(document, "execCommand").mockReturnValue(false)

    await expect(copyTextToClipboard("hi")).resolves.toBe(false)
  })

  it("returns false when execCommand throws", async () => {
    setClipboard(undefined)
    vi.spyOn(document, "execCommand").mockImplementation(() => {
      throw new Error("boom")
    })

    await expect(copyTextToClipboard("hi")).resolves.toBe(false)
  })
})

describe("copyTextFromMenu", () => {
  const originalClipboard = navigator.clipboard

  afterEach(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: originalClipboard,
    })
    vi.restoreAllMocks()
  })

  it("defers the write and resolves with the result", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    })

    await expect(copyTextFromMenu("deferred")).resolves.toBe(true)
    expect(writeText).toHaveBeenCalledWith("deferred")
  })
})

describe("installClipboardFallback", () => {
  const originalClipboard = navigator.clipboard

  afterEach(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: originalClipboard,
    })
    vi.restoreAllMocks()
  })

  it("is a no-op when a native writeText already exists", () => {
    const writeText = vi.fn()
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    })

    installClipboardFallback()

    expect(navigator.clipboard.writeText).toBe(writeText)
  })

  it("augments an existing clipboard object lacking writeText", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {},
    })
    vi.spyOn(document, "execCommand").mockReturnValue(true)

    installClipboardFallback()

    expect(typeof navigator.clipboard.writeText).toBe("function")
    await expect(navigator.clipboard.writeText("x")).resolves.toBeUndefined()
  })

  it("installs a clipboard object when none exists", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: undefined,
    })
    vi.spyOn(document, "execCommand").mockReturnValue(true)

    installClipboardFallback()

    expect(typeof navigator.clipboard.writeText).toBe("function")
    await expect(navigator.clipboard.writeText("x")).resolves.toBeUndefined()
  })

  it("rejects when the legacy copy fails", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {},
    })
    vi.spyOn(document, "execCommand").mockReturnValue(false)

    installClipboardFallback()

    await expect(navigator.clipboard.writeText("x")).rejects.toThrow()
  })
})

describe("randomUUID", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("delegates to crypto.randomUUID when available", () => {
    const uuid = "12345678-1234-4234-8234-123456789abc"
    vi.spyOn(crypto, "randomUUID").mockReturnValue(uuid)

    expect(randomUUID()).toBe(uuid)
  })

  it("produces a valid v4 UUID via the getRandomValues fallback", () => {
    vi.spyOn(crypto, "randomUUID").mockImplementation(() => {
      throw new TypeError("unavailable")
    })
    // Force the availability check to fail so the fallback path runs.
    const original = crypto.randomUUID
    Object.defineProperty(crypto, "randomUUID", {
      configurable: true,
      value: undefined,
    })

    const id = randomUUID()
    Object.defineProperty(crypto, "randomUUID", {
      configurable: true,
      value: original,
    })

    expect(id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    )
  })

  it("returns distinct values across calls in the fallback path", () => {
    const original = crypto.randomUUID
    Object.defineProperty(crypto, "randomUUID", {
      configurable: true,
      value: undefined,
    })

    const a = randomUUID()
    const b = randomUUID()

    Object.defineProperty(crypto, "randomUUID", {
      configurable: true,
      value: original,
    })

    expect(a).not.toBe(b)
  })
})
