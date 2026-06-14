import { act, render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import { LoopRealtimeProvider } from "./loop-realtime-context"
import { useLoopResource } from "@/hooks/use-loop-resource"
import type { LoopChanged } from "@/lib/types"

// Capture the subscribe handler + reconnect callback so tests can fire events.
let loopHandler: ((e: LoopChanged) => void) | null = null
let reconnectCb: (() => void) | null = null
const unsub = vi.fn()
const offReconnect = vi.fn()
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(
    async (_event: string, handler: (e: LoopChanged) => void) => {
      loopHandler = handler
      return unsub
    }
  ),
  onTransportReconnect: vi.fn((cb: () => void) => {
    reconnectCb = cb
    return offReconnect
  }),
}))

// Manual animation-frame queue so coalescing is deterministic: events fired in
// the same tick accumulate, and `runFrames()` flushes them in one batch.
let frameQueue: Array<() => void> = []
function runFrames() {
  const cbs = frameQueue
  frameQueue = []
  cbs.forEach((cb) => cb())
}

// Flush microtasks (a pending fetch's `.then`) inside an act() boundary so the
// resulting state update is captured without React act warnings.
async function flush() {
  await act(async () => {
    await Promise.resolve()
  })
}

function evt(over: Partial<LoopChanged> = {}): LoopChanged {
  return {
    v: 1,
    space_id: 1,
    issue_id: null,
    subject_kind: "issue",
    subject_id: 0,
    kind: "changed",
    ...over,
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  loopHandler = null
  reconnectCb = null
  frameQueue = []
  vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
    frameQueue.push(cb)
    return frameQueue.length
  })
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    if (id >= 1 && id <= frameQueue.length) frameQueue[id - 1] = () => {}
  })
})

afterEach(() => {
  vi.unstubAllGlobals()
})

function Probe<T>({
  fetcher,
  match,
  deps,
  initial,
}: {
  fetcher: () => Promise<T>
  match: (e: LoopChanged) => boolean
  deps?: ReadonlyArray<unknown>
  initial: T
}) {
  const { data, loading } = useLoopResource(fetcher, { match, initial, deps })
  return (
    <div>
      <span data-testid="data">{String(data)}</span>
      <span data-testid="loading">{String(loading)}</span>
    </div>
  )
}

function renderProbe<T>(args: {
  fetcher: () => Promise<T>
  match: (e: LoopChanged) => boolean
  deps?: ReadonlyArray<unknown>
  initial: T
}) {
  return render(
    <LoopRealtimeProvider>
      <Probe {...args} />
    </LoopRealtimeProvider>
  )
}

describe("LoopRealtimeProvider + useLoopResource", () => {
  it("coalesces a burst of matching events into a single refetch", async () => {
    const fetcher = vi.fn().mockResolvedValue("v1")
    renderProbe({ fetcher, match: () => true, initial: "INIT" })
    await waitFor(() => expect(loopHandler).not.toBeNull())
    await flush()
    expect(fetcher).toHaveBeenCalledTimes(1) // initial
    fetcher.mockClear()

    await act(async () => {
      loopHandler!(evt())
      loopHandler!(evt())
      loopHandler!(evt())
      runFrames()
    })

    expect(fetcher).toHaveBeenCalledTimes(1) // three events → one refetch
  })

  it("ignores events that do not match", async () => {
    const fetcher = vi.fn().mockResolvedValue("v1")
    renderProbe({ fetcher, match: (e) => e.space_id === 99, initial: "INIT" })
    await waitFor(() => expect(loopHandler).not.toBeNull())
    await flush()
    fetcher.mockClear()

    await act(async () => {
      loopHandler!(evt({ space_id: 1 }))
      runFrames()
    })

    expect(fetcher).not.toHaveBeenCalled()
  })

  it("refetches on transport reconnect regardless of match", async () => {
    const fetcher = vi.fn().mockResolvedValue("v1")
    // match nothing — only the reconnect (null batch) should refetch.
    renderProbe({ fetcher, match: () => false, initial: "INIT" })
    await waitFor(() => expect(reconnectCb).not.toBeNull())
    await flush()
    fetcher.mockClear()

    await act(async () => {
      reconnectCb!()
      runFrames()
    })

    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it("drops a stale response that resolves after a newer fetch", async () => {
    const resolvers: Array<(v: string) => void> = []
    const fetcher = vi.fn(
      () => new Promise<string>((resolve) => resolvers.push(resolve))
    )
    const { getByTestId } = renderProbe({
      fetcher,
      match: () => true,
      initial: "INIT",
    })
    await waitFor(() => expect(loopHandler).not.toBeNull())
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1)) // resolvers[0]

    await act(async () => {
      loopHandler!(evt())
      runFrames()
    })
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2)) // resolvers[1]

    // Resolve the NEWER fetch first, then the older (stale) one.
    await act(async () => {
      resolvers[1]("new")
    })
    await waitFor(() => expect(getByTestId("data").textContent).toBe("new"))
    await act(async () => {
      resolvers[0]("old")
    })
    expect(getByTestId("data").textContent).toBe("new") // stale dropped
  })

  it("keeps the last good data when a refetch fails", async () => {
    let calls = 0
    const fetcher = vi.fn(() => {
      calls += 1
      return calls === 1
        ? Promise.resolve("good")
        : Promise.reject(new Error("boom"))
    })
    const { getByTestId } = renderProbe({
      fetcher,
      match: () => true,
      initial: "INIT",
    })
    await waitFor(() => expect(loopHandler).not.toBeNull())
    await waitFor(() => expect(getByTestId("data").textContent).toBe("good"))
    expect(getByTestId("loading").textContent).toBe("false")

    await act(async () => {
      loopHandler!(evt())
      runFrames()
    })
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2))

    // Old data preserved; loading stays false (no blank screen on error).
    await waitFor(() =>
      expect(getByTestId("loading").textContent).toBe("false")
    )
    expect(getByTestId("data").textContent).toBe("good")
  })

  it("throws when a consumer is used outside the provider", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {})
    expect(() =>
      render(
        <Probe
          fetcher={vi.fn().mockResolvedValue("x")}
          match={() => true}
          initial="INIT"
        />
      )
    ).toThrow(/LoopRealtimeProvider/)
    spy.mockRestore()
  })
})
