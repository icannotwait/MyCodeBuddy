import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import type { ReactElement } from "react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { GrokSessionImageResolution } from "@/lib/types"

const mocks = vi.hoisted(() => ({
  openResolved: vi.fn(),
  resolve: vi.fn(),
  toast: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  resolveGrokSessionImage: mocks.resolve,
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({
    openResolvedImagePreview: mocks.openResolved,
  }),
}))

vi.mock("sonner", () => ({
  toast: mocks.toast,
}))

import {
  GrokConversationProvider,
  GrokSessionImageScope,
} from "./grok-session-image-context"
import { GrokSessionImage } from "./grok-session-image"

type Phase = "live" | "complete"

function resolution(
  overrides: Partial<GrokSessionImageResolution> = {}
): GrokSessionImageResolution {
  return {
    path: "/session/images/a.png",
    origin: "session",
    mimeType: "image/png",
    dataBase64: "YWJj",
    ...overrides,
  }
}

function imageTree({
  conversationId = 42,
  phase = "complete",
  src = "images/a.png",
  alt,
}: {
  conversationId?: number | null
  phase?: Phase
  src?: string
  alt?: string
} = {}): ReactElement {
  return (
    <GrokConversationProvider conversationId={conversationId}>
      <GrokSessionImageScope phase={phase}>
        <GrokSessionImage src={src} alt={alt} />
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
}

function renderImage(
  props: { src?: string; alt?: string },
  phase: Phase = "complete"
) {
  return render(
    <GrokConversationProvider conversationId={42}>
      <GrokSessionImageScope phase={phase}>
        <GrokSessionImage {...props} />
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

async function flushEffects() {
  await act(async () => {})
}

async function expectMuted(label: string) {
  await waitFor(() => {
    expect(screen.getByText(label)).toHaveClass("text-muted-foreground")
  })
}

describe("GrokSessionImage complete history and validation", () => {
  beforeEach(() => {
    mocks.resolve.mockReset()
    mocks.openResolved.mockReset()
    mocks.openResolved.mockReturnValue({ ok: true, tabId: "file:a" })
    mocks.toast.mockReset()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it("complete history performs one request and opens cached bytes accessibly", async () => {
    mocks.resolve.mockResolvedValue(resolution())
    renderImage({ src: "images/a.png", alt: "目标图" })

    expect(screen.getByText("目标图")).toHaveAttribute("aria-busy", "true")
    const image = await screen.findByRole("img", { name: "目标图" })
    expect(image).toHaveAttribute("src", "data:image/png;base64,YWJj")
    fireEvent.load(image)
    fireEvent.click(screen.getByRole("button", { name: "目标图" }))

    expect(mocks.openResolved).toHaveBeenCalledWith({
      path: "/session/images/a.png",
      mimeType: "image/png",
      dataBase64: "YWJj",
      source: {
        type: "grok-session-image",
        conversationId: 42,
        href: "images/a.png",
      },
    })
    expect(mocks.resolve).toHaveBeenCalledWith({
      conversationId: 42,
      href: "images/a.png",
      includeData: true,
    })
    expect(mocks.resolve).toHaveBeenCalledTimes(1)
  })

  it("invalid src or inactive durable context renders muted fallback without I/O", () => {
    const { rerender } = renderImage({ src: "images/a/b.png", alt: "bad" })
    expect(screen.getByText("bad")).toHaveClass("text-muted-foreground")
    expect(mocks.resolve).not.toHaveBeenCalled()

    rerender(
      <GrokConversationProvider conversationId={null}>
        <GrokSessionImageScope phase="complete">
          <GrokSessionImage src="images/a.png" />
        </GrokSessionImageScope>
      </GrokConversationProvider>
    )
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(screen.getByText("a.png")).toHaveClass("text-muted-foreground")
  })

  it("starts resolution when an unpersisted draft receives its durable id", async () => {
    mocks.resolve.mockResolvedValue(resolution())
    const { rerender } = render(imageTree({ conversationId: null, alt: "a" }))
    expect(mocks.resolve).not.toHaveBeenCalled()

    rerender(imageTree({ conversationId: 42, alt: "a" }))

    expect(await screen.findByRole("img", { name: "a" })).toBeInTheDocument()
    expect(mocks.resolve).toHaveBeenCalledWith({
      conversationId: 42,
      href: "images/a.png",
      includeData: true,
    })
  })

  it.each([
    ["null", null],
    ["non-object", "bad"],
    ["empty data", resolution({ dataBase64: "" })],
    ["blank data", resolution({ dataBase64: "   " })],
    ["unsupported MIME", resolution({ mimeType: "image/svg+xml" as never })],
    ["href-mismatched MIME", resolution({ mimeType: "image/jpeg" })],
    ["invalid origin", resolution({ origin: "cache" as never })],
    ["relative path", resolution({ path: "session/images/a.png" })],
  ])("malformed %s success becomes a muted fallback", async (_, payload) => {
    mocks.resolve.mockResolvedValueOnce(payload)
    renderImage({ src: "images/a.png", alt: "invalid result" })

    await expectMuted("invalid result")
    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(mocks.openResolved).not.toHaveBeenCalled()
    expect(mocks.toast).not.toHaveBeenCalled()
  })

  it("a synchronous transport throw is terminal instead of staying busy", async () => {
    mocks.resolve.mockImplementationOnce(() => {
      throw new Error("transport unavailable")
    })
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()

    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
    expect(screen.getByText("a")).not.toHaveAttribute("aria-busy")
  })
})

describe("GrokSessionImage bounded live lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers()
    mocks.resolve.mockReset()
    mocks.openResolved.mockReset()
    mocks.openResolved.mockReturnValue({ ok: true, tabId: "file:a" })
    mocks.toast.mockReset()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it.each([
    ["invalid", "images/a/b.png"],
    ["absent", undefined],
  ])("keeps %s src inactive across complete to live", async (_, src) => {
    const unexpected = deferred<GrokSessionImageResolution>()
    mocks.resolve.mockReturnValue(unexpected.promise)
    const view = render(
      <GrokConversationProvider conversationId={42}>
        <GrokSessionImageScope phase="complete">
          <GrokSessionImage src={src} alt="inactive" />
        </GrokSessionImageScope>
      </GrokConversationProvider>
    )
    expect(screen.getByText("inactive")).toHaveClass("text-muted-foreground")
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)

    view.rerender(
      <GrokConversationProvider conversationId={42}>
        <GrokSessionImageScope phase="live">
          <GrokSessionImage src={src} alt="inactive" />
        </GrokSessionImageScope>
      </GrokConversationProvider>
    )
    await flushEffects()

    expect(screen.getByText("inactive")).toHaveClass("text-muted-foreground")
    expect(screen.getByText("inactive")).not.toHaveAttribute("aria-busy")
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
  })

  it("keeps an invalid src inactive when a live scope gains its durable id", async () => {
    const unexpected = deferred<GrokSessionImageResolution>()
    mocks.resolve.mockReturnValue(unexpected.promise)
    const view = render(
      <GrokConversationProvider conversationId={null}>
        <GrokSessionImageScope phase="live">
          <GrokSessionImage src="images/a/b.png" alt="inactive" />
        </GrokSessionImageScope>
      </GrokConversationProvider>
    )
    expect(screen.getByText("inactive")).toHaveClass("text-muted-foreground")
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)

    view.rerender(
      <GrokConversationProvider conversationId={42}>
        <GrokSessionImageScope phase="live">
          <GrokSessionImage src="images/a/b.png" alt="inactive" />
        </GrokSessionImageScope>
      </GrokConversationProvider>
    )
    await flushEffects()

    expect(screen.getByText("inactive")).toHaveClass("text-muted-foreground")
    expect(screen.getByText("inactive")).not.toHaveAttribute("aria-busy")
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))
    expect(mocks.resolve).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
  })

  it("live not_found attempts at 0 400 1200 and 2500 ms only", async () => {
    mocks.resolve.mockRejectedValue({ code: "not_found", message: "missing" })
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()
    expect(mocks.resolve).toHaveBeenCalledTimes(1)

    await act(async () => vi.advanceTimersByTimeAsync(399))
    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    await act(async () => vi.advanceTimersByTimeAsync(1))
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
    await act(async () => vi.advanceTimersByTimeAsync(800))
    expect(mocks.resolve).toHaveBeenCalledTimes(3)
    await act(async () => vi.advanceTimersByTimeAsync(1_300))
    expect(mocks.resolve).toHaveBeenCalledTimes(4)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))
    expect(mocks.resolve).toHaveBeenCalledTimes(4)
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
  })

  it("coalesces elapsed deadlines behind one in-flight request", async () => {
    const first = deferred<GrokSessionImageResolution>()
    mocks.resolve.mockReturnValueOnce(first.promise)
    mocks.resolve.mockRejectedValue({ code: "not_found", message: "missing" })
    renderImage({ src: "images/a.png" }, "live")

    await act(async () => vi.advanceTimersByTimeAsync(1_300))
    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    await act(async () =>
      first.reject({ code: "not_found", message: "missing" })
    )
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
    await act(async () => vi.advanceTimersByTimeAsync(1_200))
    expect(mocks.resolve).toHaveBeenCalledTimes(3)
  })

  it("does not consume a due deadline twice when settlement wins the timer race", async () => {
    const first = deferred<GrokSessionImageResolution>()
    const second = deferred<GrokSessionImageResolution>()
    mocks.resolve
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
      .mockRejectedValue({ code: "not_found", message: "missing" })
    const startedAt = Date.now()
    renderImage({ src: "images/a.png" }, "live")
    await flushEffects()

    vi.setSystemTime(startedAt + 400)
    await act(async () =>
      first.reject({ code: "not_found", message: "missing" })
    )
    expect(mocks.resolve).toHaveBeenCalledTimes(2)

    await act(async () => vi.advanceTimersByTimeAsync(0))
    await act(async () =>
      second.reject({ code: "not_found", message: "missing" })
    )
    expect(mocks.resolve).toHaveBeenCalledTimes(2)

    await act(async () => vi.advanceTimersByTimeAsync(799))
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
    await act(async () => vi.advanceTimersByTimeAsync(1))
    expect(mocks.resolve).toHaveBeenCalledTimes(3)
  })

  it("provisional workspace is shown then replaced by session", async () => {
    mocks.resolve
      .mockResolvedValueOnce(
        resolution({
          path: "/workspace/images/a.png",
          origin: "workspace",
          dataBase64: "d29ya3NwYWNl",
        })
      )
      .mockResolvedValueOnce(resolution({ dataBase64: "c2Vzc2lvbg==" }))
    renderImage({ src: "images/a.png", alt: "a" }, "live")

    await flushEffects()
    let image = screen.getByRole("img", { name: "a" })
    expect(image).toHaveAttribute("src", "data:image/png;base64,d29ya3NwYWNl")
    fireEvent.load(image)
    await act(async () => vi.advanceTimersByTimeAsync(400))
    image = screen.getByRole("img", { name: "a" })
    expect(image).toHaveAttribute("src", "data:image/png;base64,c2Vzc2lvbg==")
    fireEvent.load(image)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(screen.getByRole("img", { name: "a" })).toHaveAttribute(
      "src",
      "data:image/png;base64,c2Vzc2lvbg=="
    )
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
  })

  it("provisional workspace survives retry wait but final not_found ends muted", async () => {
    mocks.resolve
      .mockResolvedValueOnce(
        resolution({ origin: "workspace", dataBase64: "d29ya3NwYWNl" })
      )
      .mockRejectedValue({ code: "not_found", message: "missing" })
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()
    fireEvent.load(screen.getByRole("img", { name: "a" }))

    await act(async () => vi.advanceTimersByTimeAsync(400))
    expect(screen.getByRole("img", { name: "a" })).toHaveAttribute(
      "src",
      "data:image/png;base64,d29ya3NwYWNl"
    )
    await act(async () => vi.advanceTimersByTimeAsync(800))
    expect(screen.getByRole("img", { name: "a" })).toHaveAttribute(
      "src",
      "data:image/png;base64,d29ya3NwYWNl"
    )
    await act(async () => vi.advanceTimersByTimeAsync(1_300))

    expect(mocks.resolve).toHaveBeenCalledTimes(4)
    expect(screen.queryByRole("img", { name: "a" })).not.toBeInTheDocument()
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
  })

  it("fourth attempt workspace success remains visible", async () => {
    mocks.resolve
      .mockRejectedValueOnce({ code: "not_found", message: "missing" })
      .mockRejectedValueOnce({ code: "not_found", message: "missing" })
      .mockRejectedValueOnce({ code: "not_found", message: "missing" })
      .mockResolvedValueOnce(
        resolution({ origin: "workspace", dataBase64: "ZmluYWw=" })
      )
    renderImage({ src: "images/a.png", alt: "a" }, "live")

    await flushEffects()
    await act(async () => vi.advanceTimersByTimeAsync(2_500))
    const image = screen.getByRole("img", { name: "a" })
    expect(image).toHaveAttribute("src", "data:image/png;base64,ZmluYWw=")
    fireEvent.load(image)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(4)
    expect(screen.getByRole("img", { name: "a" })).toHaveAttribute(
      "src",
      "data:image/png;base64,ZmluYWw="
    )
  })

  it("browser decode error consumes remaining live budget", async () => {
    mocks.resolve.mockResolvedValue(resolution())
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()
    fireEvent.error(screen.getByRole("img", { name: "a" }))

    await act(async () => vi.advanceTimersByTimeAsync(400))
    fireEvent.error(screen.getByRole("img", { name: "a" }))
    await act(async () => vi.advanceTimersByTimeAsync(800))
    fireEvent.error(screen.getByRole("img", { name: "a" }))
    await act(async () => vi.advanceTimersByTimeAsync(1_300))
    fireEvent.error(screen.getByRole("img", { name: "a" }))
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(4)
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
  })

  it.each(["not_found", "decode"])(
    "complete %s error stops after one attempt",
    async (failure) => {
      if (failure === "not_found") {
        mocks.resolve.mockRejectedValue({
          code: "not_found",
          message: "missing",
        })
      } else {
        mocks.resolve.mockResolvedValue(resolution())
      }
      renderImage({ src: "images/a.png", alt: "a" }, "complete")
      await flushEffects()
      if (failure === "decode") {
        fireEvent.error(screen.getByRole("img", { name: "a" }))
      }
      await act(async () => vi.advanceTimersByTimeAsync(10_000))

      expect(mocks.resolve).toHaveBeenCalledTimes(1)
      expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
    }
  )

  it("workspace timer and decode error still start only one next request", async () => {
    const second = deferred<GrokSessionImageResolution>()
    mocks.resolve
      .mockResolvedValueOnce(resolution({ origin: "workspace" }))
      .mockReturnValueOnce(second.promise)
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()
    const firstImage = screen.getByRole("img", { name: "a" })

    await act(async () => vi.advanceTimersByTimeAsync(400))
    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    fireEvent.error(firstImage)
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
    fireEvent.error(firstImage)
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
  })

  it.each([
    ["invalid input", { code: "invalid_input", message: "bad href" }],
    ["permission", { code: "permission_denied", message: "no access" }],
    ["database", { code: "database", message: "database failed" }],
    ["I/O", { code: "io", message: "read failed" }],
    ["transport", new Error("offline")],
  ])("%s errors never retry or toast", async (_, error) => {
    mocks.resolve.mockRejectedValue(error)
    renderImage({ src: "images/a.png", alt: "a" }, "live")
    await flushEffects()
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
    expect(mocks.openResolved).not.toHaveBeenCalled()
    expect(mocks.toast).not.toHaveBeenCalled()
  })

  it.each([
    ["invalid", { code: "invalid_input", message: "bad href" }],
    ["permission", { code: "permission_denied", message: "no access" }],
    ["transport", new Error("offline")],
  ])(
    "terminal %s error after provisional workspace clears old preview",
    async (_, error) => {
      mocks.resolve
        .mockResolvedValueOnce(
          resolution({ origin: "workspace", dataBase64: "b2xk" })
        )
        .mockRejectedValueOnce(error)
      renderImage({ src: "images/a.png", alt: "a" }, "live")
      await flushEffects()
      fireEvent.load(screen.getByRole("img", { name: "a" }))
      await act(async () => vi.advanceTimersByTimeAsync(400))

      expect(screen.queryByRole("img", { name: "a" })).not.toBeInTheDocument()
      expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
      expect(
        screen.queryByRole("button", { name: "a" })
      ).not.toBeInTheDocument()
    }
  )

  it("href conversation change and unmount ignore late results", async () => {
    const oldConversation = deferred<GrokSessionImageResolution>()
    const oldHref = deferred<GrokSessionImageResolution>()
    const unmounted = deferred<GrokSessionImageResolution>()
    mocks.resolve
      .mockReturnValueOnce(oldConversation.promise)
      .mockReturnValueOnce(oldHref.promise)
      .mockReturnValueOnce(unmounted.promise)
    const view = render(imageTree({ conversationId: 42, alt: "image" }))
    await flushEffects()

    view.rerender(imageTree({ conversationId: 43, alt: "image" }))
    await flushEffects()
    await act(async () => oldConversation.resolve(resolution()))
    expect(screen.queryByRole("img")).not.toBeInTheDocument()

    view.rerender(
      imageTree({ conversationId: 43, src: "images/b.png", alt: "image" })
    )
    await flushEffects()
    await act(async () => oldHref.resolve(resolution()))
    expect(screen.queryByRole("img")).not.toBeInTheDocument()

    view.unmount()
    await act(async () =>
      unmounted.resolve(
        resolution({ path: "/session/images/b.png", dataBase64: "bmV3" })
      )
    )
    expect(mocks.openResolved).not.toHaveBeenCalled()
  })

  it("identity change hides old ready preview before effect flush", async () => {
    const next = deferred<GrokSessionImageResolution>()
    mocks.resolve
      .mockResolvedValueOnce(resolution({ dataBase64: "b2xk" }))
      .mockReturnValueOnce(next.promise)
    const view = render(imageTree({ src: "images/a.png", alt: "old" }))
    await flushEffects()
    fireEvent.load(screen.getByRole("img", { name: "old" }))

    view.rerender(imageTree({ src: "images/b.png", alt: "new" }))

    expect(screen.queryByRole("img", { name: "old" })).not.toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "old" })
    ).not.toBeInTheDocument()
    expect(screen.getByText("new")).toHaveAttribute("aria-busy", "true")
    expect(mocks.openResolved).not.toHaveBeenCalled()
  })

  it("live to complete clears future timer without duplicate or losing success", async () => {
    mocks.resolve.mockResolvedValue(
      resolution({ origin: "workspace", dataBase64: "d29ya3NwYWNl" })
    )
    const view = render(imageTree({ phase: "live", alt: "a" }))
    await flushEffects()
    const image = screen.getByRole("img", { name: "a" })
    fireEvent.load(image)

    view.rerender(imageTree({ phase: "complete", alt: "a" }))
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(screen.getByRole("img", { name: "a" })).toHaveAttribute(
      "src",
      "data:image/png;base64,d29ya3NwYWNl"
    )
  })

  it("live to complete while waiting for retry finishes muted", async () => {
    mocks.resolve.mockRejectedValue({ code: "not_found", message: "missing" })
    const view = render(imageTree({ phase: "live", alt: "a" }))
    await flushEffects()
    expect(screen.getByText("a")).toHaveAttribute("aria-busy", "true")

    view.rerender(imageTree({ phase: "complete", alt: "a" }))
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(screen.getByText("a")).toHaveClass("text-muted-foreground")
    expect(screen.getByText("a")).not.toHaveAttribute("aria-busy")
  })

  it("live to complete accepts current inflight result but schedules no retry", async () => {
    const inflight = deferred<GrokSessionImageResolution>()
    mocks.resolve.mockReturnValue(inflight.promise)
    const view = render(imageTree({ phase: "live", alt: "a" }))
    await flushEffects()

    view.rerender(imageTree({ phase: "complete", alt: "a" }))
    await act(async () => inflight.resolve(resolution()))
    const image = screen.getByRole("img", { name: "a" })
    fireEvent.load(image)
    await act(async () => vi.advanceTimersByTimeAsync(10_000))

    expect(mocks.resolve).toHaveBeenCalledTimes(1)
    expect(image).toHaveAttribute("src", "data:image/png;base64,YWJj")
  })

  it("completed remount performs one normal history attempt", async () => {
    mocks.resolve.mockResolvedValue(resolution())
    const first = render(imageTree({ phase: "complete", alt: "a" }))
    await flushEffects()
    expect(screen.getByRole("img", { name: "a" })).toBeInTheDocument()
    first.unmount()

    render(imageTree({ phase: "complete", alt: "a" }))
    await flushEffects()

    expect(screen.getByRole("img", { name: "a" })).toBeInTheDocument()
    expect(mocks.resolve).toHaveBeenCalledTimes(2)
  })
})
