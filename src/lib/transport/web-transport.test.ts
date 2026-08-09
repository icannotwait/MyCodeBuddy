import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { WebTransport } from "./web-transport"

// Minimal controllable WebSocket stand-in: records instances and lets the test
// drive the lifecycle (open / __ready__ frame / drop) deterministically. The
// real browser WS is event-driven and opaque; this exposes the transitions the
// state machine reacts to.
class MockWebSocket {
  static OPEN = 1
  static CONNECTING = 0
  static CLOSING = 2
  static CLOSED = 3
  static instances: MockWebSocket[] = []
  readyState = MockWebSocket.CONNECTING
  onopen: (() => void) | null = null
  onmessage: ((ev: { data: string }) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null
  sent: string[] = []
  constructor(
    public url: string,
    public protocols?: string | string[]
  ) {
    MockWebSocket.instances.push(this)
  }
  send(data: string) {
    this.sent.push(data)
  }
  close() {
    this.readyState = MockWebSocket.CLOSED
  }
  // ── test drivers ──
  open() {
    this.readyState = MockWebSocket.OPEN
    this.onopen?.()
  }
  ready() {
    this.onmessage?.({
      data: JSON.stringify({ channel: "__ready__", payload: null }),
    })
  }
  drop() {
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.()
  }
}

function lastWs(): MockWebSocket {
  return MockWebSocket.instances[MockWebSocket.instances.length - 1]
}

let fetchMock: ReturnType<typeof vi.fn>

beforeEach(() => {
  vi.useFakeTimers()
  MockWebSocket.instances = []
  vi.stubGlobal("WebSocket", MockWebSocket)
  localStorage.setItem("codeg_token", "tok")
  fetchMock = vi.fn()
  vi.stubGlobal("fetch", fetchMock)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
  localStorage.clear()
})

// Bring a fresh transport to the application-ready "connected" state.
// `eventStream()` synchronously triggers the WS connect (no await, unlike
// `subscribe()`), which keeps the timer/promise interleaving simple.
function connectReady() {
  const t = new WebTransport("http://localhost")
  t.eventStream()
  const ws = lastWs()
  ws.open()
  ws.ready()
  return { t, ws }
}

const ok200 = () => ({ status: 200, ok: true, json: async () => ({}) })
const resp401 = () => ({ status: 401, ok: false, json: async () => ({}) })

describe("WebTransport connection state machine", () => {
  it("starts connected and the first __ready__ does not fire reconnect callbacks", () => {
    const t = new WebTransport("http://localhost")
    const onReconnect = vi.fn()
    t.onReconnect(onReconnect)
    expect(t.getConnectionSnapshot()).toBe("connected")

    t.eventStream()
    const ws = lastWs()
    ws.open()
    ws.ready()

    expect(t.getConnectionSnapshot()).toBe("connected")
    // First ready = initial connect, not a reconnect.
    expect(onReconnect).not.toHaveBeenCalled()
  })

  it("treats a dropped socket as reconnecting — never logs out or wipes the token", () => {
    const { t, ws } = connectReady()

    ws.drop()

    expect(t.getConnectionSnapshot()).toBe("reconnecting")
    // The token survives a transient drop (this is the whole point of the fix).
    expect(localStorage.getItem("codeg_token")).toBe("tok")
    // Probe is scheduled on backoff, not fired synchronously.
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("probes /api/health on backoff and reconnects on 200; the 2nd ready fires reconnect callbacks", async () => {
    const { t, ws } = connectReady()
    const onReconnect = vi.fn()
    t.onReconnect(onReconnect)
    fetchMock.mockResolvedValue(ok200())

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000) // first backoff tick → probe

    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost/api/health",
      expect.objectContaining({ method: "POST" })
    )
    const ws2 = lastWs()
    expect(ws2).not.toBe(ws) // a fresh socket was opened

    ws2.open()
    ws2.ready()
    expect(t.getConnectionSnapshot()).toBe("connected")
    // Reconnect (2nd ready) refreshes consumer state exactly once.
    expect(onReconnect).toHaveBeenCalledTimes(1)
  })

  it("enters unauthorized on a 401 probe and stops retrying (token left intact)", async () => {
    const { t, ws } = connectReady()
    fetchMock.mockResolvedValue(resp401())

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000)

    expect(t.getConnectionSnapshot()).toBe("unauthorized")
    // markUnauthorized must NOT clear the token — only the user's "Go to
    // login" action does, so a spurious 401 can't silently wipe a session.
    expect(localStorage.getItem("codeg_token")).toBe("tok")

    const callsSoFar = fetchMock.mock.calls.length
    await vi.advanceTimersByTimeAsync(60_000)
    expect(fetchMock.mock.calls.length).toBe(callsSoFar) // no further probing
  })

  it("stays reconnecting on an unreachable probe and keeps backing off", async () => {
    const { t, ws } = connectReady()
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"))

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000) // first probe fails
    expect(t.getConnectionSnapshot()).toBe("reconnecting")
    const calls1 = fetchMock.mock.calls.length

    await vi.advanceTimersByTimeAsync(2000) // second backoff tick (2s)
    expect(fetchMock.mock.calls.length).toBeGreaterThan(calls1)
    expect(t.getConnectionSnapshot()).toBe("reconnecting")
  })

  it("reconnectNow() probes immediately without waiting for backoff", async () => {
    const { t, ws } = connectReady()
    fetchMock.mockResolvedValue(ok200())

    ws.drop() // schedules a probe at 1s
    t.reconnectNow() // should fire one now and cancel the scheduled one
    await vi.advanceTimersByTimeAsync(0)

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const ws2 = lastWs()
    ws2.open()
    ws2.ready()
    expect(t.getConnectionSnapshot()).toBe("connected")
  })

  it("de-dupes concurrent probes (a button mash fires a single fetch)", () => {
    const { t, ws } = connectReady()
    // A probe that never settles, so the in-flight guard stays set.
    fetchMock.mockReturnValue(new Promise(() => {}))

    ws.drop()
    t.reconnectNow()
    t.reconnectNow()

    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it("aborts a hung probe at the timeout and resumes reconnecting", async () => {
    const { t, ws } = connectReady()
    fetchMock.mockImplementation(
      (_url: string, opts: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          opts.signal.addEventListener("abort", () =>
            reject(new DOMException("Aborted", "AbortError"))
          )
        })
    )

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000) // probe starts, fetch hangs
    expect(t.getConnectionSnapshot()).toBe("reconnecting")

    await vi.advanceTimersByTimeAsync(8000) // HEALTH_PROBE_TIMEOUT_MS → abort
    expect(t.getConnectionSnapshot()).toBe("reconnecting") // recovered, not hung
  })

  it("ignores a late onclose after destroy() and schedules nothing", async () => {
    const { t, ws } = connectReady()
    const lateClose = ws.onclose // capture before destroy detaches it

    t.destroy()
    lateClose?.() // simulate the browser's async close landing post-destroy

    expect(t.getConnectionSnapshot()).toBe("connected") // guard short-circuits
    await vi.advanceTimersByTimeAsync(60_000)
    expect(fetchMock).not.toHaveBeenCalled() // no backoff was scheduled
  })

  it("notifies subscribers on state change and stops after unsubscribe", () => {
    const { t, ws } = connectReady()
    const listener = vi.fn()
    const unsub = t.subscribeConnection(listener)

    ws.drop()
    expect(listener).toHaveBeenCalledTimes(1) // connected → reconnecting

    unsub()
    t.reconnectNow() // would transition, but we're unsubscribed
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it("ignores a probe that resolves 200 after a definitive 401 (no resurrection)", async () => {
    const { t, ws } = connectReady()
    let resolveProbe: (v: unknown) => void = () => {}
    fetchMock.mockReturnValue(
      new Promise((resolve) => {
        resolveProbe = resolve
      })
    )

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000) // probe starts, stays pending
    expect(t.getConnectionSnapshot()).toBe("reconnecting")

    // A definitive 401 arrives via another path while the probe is in flight.
    t.markUnauthorized()
    expect(t.getConnectionSnapshot()).toBe("unauthorized")

    // The stale probe now resolves 200 — it must NOT reopen the socket.
    resolveProbe(ok200())
    await vi.advanceTimersByTimeAsync(0)
    expect(t.getConnectionSnapshot()).toBe("unauthorized")
    expect(MockWebSocket.instances).toHaveLength(1) // no second socket opened

    const calls = fetchMock.mock.calls.length
    await vi.advanceTimersByTimeAsync(60_000)
    expect(fetchMock.mock.calls.length).toBe(calls) // no further probing
  })

  it("treats a token cleared between probe and reconnect as unauthorized", async () => {
    const { t, ws } = connectReady()
    // The probe succeeds, but the token is gone by the time connectWs runs
    // (e.g. a logout in another tab landed mid-probe).
    fetchMock.mockImplementation(async () => {
      localStorage.removeItem("codeg_token")
      return ok200()
    })

    ws.drop()
    await vi.advanceTimersByTimeAsync(1000)

    // Must not dead-end in "reconnecting" with no socket and no timer.
    expect(t.getConnectionSnapshot()).toBe("unauthorized")
  })

  it("enters reconnecting when the very first connect fails (server unreachable at load)", () => {
    const t = new WebTransport("http://localhost")
    t.eventStream() // opens the socket
    const ws = lastWs()

    ws.drop() // closes before it ever opened or readied
    expect(t.getConnectionSnapshot()).toBe("reconnecting")
    expect(localStorage.getItem("codeg_token")).toBe("tok") // token preserved
  })
})

describe("WebTransport call abort + timeout", () => {
  it("throws AbortError for a pre-aborted caller signal without calling fetch", async () => {
    const t = new WebTransport("http://localhost")
    const controller = new AbortController()
    controller.abort()
    await expect(
      t.call("noop", {}, { signal: controller.signal })
    ).rejects.toMatchObject({ name: "AbortError" })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("throws AbortError when the caller aborts mid-fetch (not timeout)", async () => {
    const t = new WebTransport("http://localhost")
    const controller = new AbortController()
    fetchMock.mockImplementation(
      (_url: string, opts: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          opts.signal.addEventListener("abort", () => {
            reject(new DOMException("The operation was aborted.", "AbortError"))
          })
        })
    )
    const pending = t.call("noop", {}, { signal: controller.signal })
    // Let the listener register, then abort the caller.
    await Promise.resolve()
    controller.abort()
    await expect(pending).rejects.toMatchObject({ name: "AbortError" })
  })

  it("throws Error('Request timed out') when the internal timer fires", async () => {
    const t = new WebTransport("http://localhost")
    fetchMock.mockImplementation(
      (_url: string, opts: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          opts.signal.addEventListener("abort", () => {
            reject(new DOMException("The operation was aborted.", "AbortError"))
          })
        })
    )
    const pending = t.call("noop", {}, { timeoutMs: 100 })
    // Attach the rejection handler before advancing timers so the timeout
    // abort is not an unhandled rejection.
    const expectation = expect(pending).rejects.toThrow("Request timed out")
    await vi.advanceTimersByTimeAsync(100)
    await expectation
  })

  it("removes the caller abort listener after the call settles", async () => {
    const t = new WebTransport("http://localhost")
    const controller = new AbortController()
    const addSpy = vi.spyOn(controller.signal, "addEventListener")
    const removeSpy = vi.spyOn(controller.signal, "removeEventListener")
    fetchMock.mockResolvedValue({
      status: 200,
      ok: true,
      json: async () => ({ ok: true }),
    })
    await t.call("noop", {}, { signal: controller.signal })
    expect(addSpy).toHaveBeenCalledWith(
      "abort",
      expect.any(Function),
      expect.objectContaining({ once: true })
    )
    expect(removeSpy).toHaveBeenCalled()
  })
})

describe("WebTransport completion context capture/replay", () => {
  const COMPLETION_CONTEXT_HEADER = "x-codeg-completion-context"

  function mockJsonResponse(body: unknown, headers?: Record<string, string>) {
    const headerMap = new Map(
      Object.entries(headers ?? {}).map(([k, v]) => [k.toLowerCase(), v])
    )
    return {
      status: 200,
      ok: true,
      headers: {
        get: (name: string) => headerMap.get(name.toLowerCase()) ?? null,
      },
      json: async () => body,
    }
  }

  function callHeaders(callIndex: number): HeadersInit | undefined {
    const init = fetchMock.mock.calls[callIndex]?.[1] as
      | { headers?: HeadersInit }
      | undefined
    return init?.headers
  }

  const unknownCommandResponse = () => ({
    status: 501,
    ok: false,
    json: async () => ({
      code: "not_implemented",
      message: "Unknown command",
    }),
  })

  it("captures snapshot capability and replays it on completion mutations", async () => {
    const t = new WebTransport("http://localhost")
    const attentionId = "attention-root-42"
    const token = "completion-token-root-42"

    fetchMock
      .mockResolvedValueOnce(
        mockJsonResponse(
          {
            schema_version: 1,
            workflow_kind: "brainstorm_to_delivery",
            nodes: [
              {
                node_id: "plan-author",
                kind: "work_unit",
                completion: {
                  protocol_version: 2,
                  graph_revision: 1,
                  card: {
                    state: "needs_decision",
                    evidence_validated: false,
                    attention: {
                      attention_id: attentionId,
                      task_id: "task-1",
                      kind: "completion_decision",
                      captured_scope_digest: "sha256:abc",
                      latest_run_id: "task-1",
                      node_id: "plan-author",
                    },
                  },
                },
              },
            ],
            edges: [],
            gates: [],
            phases: [],
            current_node_ids: [],
            overall_state: "in_progress",
            compatibility: "manifest",
          },
          { [COMPLETION_CONTEXT_HEADER]: token }
        )
      )
      .mockResolvedValueOnce(mockJsonResponse({ ok: true }))

    await t.call("get_workflow_graph_snapshot", { conversationId: 42 })

    await t.call("resolve_completion_decision", {
      cas: {
        attention_id: attentionId,
        task_id: "task-1",
        kind: "completion_decision",
        captured_scope_digest: "sha256:abc",
        latest_run_id: "task-1",
        node_id: "plan-author",
      },
      outcome: "done",
    })

    const decisionHeaders = callHeaders(1) as Record<string, string>
    expect(decisionHeaders[COMPLETION_CONTEXT_HEADER]).toBe(token)
  })

  it.each([
    [["restart_legacy_", "workflow"].join(""), { sourceConversationId: 42 }],
    [["get_completion_protocol_", "settings"].join(""), {}],
  ])(
    "treats removed %s as an unknown command without replaying a capability",
    async (command, args) => {
      const t = new WebTransport("http://localhost")
      fetchMock
        .mockResolvedValueOnce(
          mockJsonResponse(
            { nodes: [] },
            { [COMPLETION_CONTEXT_HEADER]: "token-root-42" }
          )
        )
        .mockResolvedValueOnce(unknownCommandResponse())

      await t.call("get_workflow_graph_snapshot", { conversationId: 42 })
      await expect(t.call(command, args)).rejects.toEqual({
        code: "not_implemented",
        message: "Unknown command",
      })

      const headers = callHeaders(1) as Record<string, string>
      expect(headers[COMPLETION_CONTEXT_HEADER]).toBeUndefined()
    }
  )

  it("scopes replayed capabilities to the snapshot root", async () => {
    const t = new WebTransport("http://localhost")

    fetchMock
      .mockResolvedValueOnce(
        mockJsonResponse(
          {
            schema_version: 1,
            workflow_kind: "brainstorm_to_delivery",
            nodes: [
              {
                node_id: "n1",
                kind: "work_unit",
                completion: {
                  protocol_version: 2,
                  graph_revision: 1,
                  card: {
                    state: "needs_decision",
                    evidence_validated: false,
                    attention: {
                      attention_id: "att-1",
                      task_id: "t1",
                      kind: "completion_decision",
                      captured_scope_digest: "sha256:1",
                      latest_run_id: "t1",
                      node_id: "n1",
                    },
                  },
                },
              },
            ],
            edges: [],
            gates: [],
            phases: [],
            current_node_ids: [],
            overall_state: "in_progress",
            compatibility: "manifest",
          },
          { [COMPLETION_CONTEXT_HEADER]: "token-root-1" }
        )
      )
      .mockResolvedValueOnce(
        mockJsonResponse(
          {
            schema_version: 1,
            workflow_kind: "brainstorm_to_delivery",
            nodes: [
              {
                node_id: "n2",
                kind: "work_unit",
                completion: {
                  protocol_version: 2,
                  graph_revision: 1,
                  card: {
                    state: "needs_decision",
                    evidence_validated: false,
                    attention: {
                      attention_id: "att-2",
                      task_id: "t2",
                      kind: "completion_decision",
                      captured_scope_digest: "sha256:2",
                      latest_run_id: "t2",
                      node_id: "n2",
                    },
                  },
                },
              },
            ],
            edges: [],
            gates: [],
            phases: [],
            current_node_ids: [],
            overall_state: "in_progress",
            compatibility: "manifest",
          },
          { [COMPLETION_CONTEXT_HEADER]: "token-root-2" }
        )
      )
      .mockResolvedValueOnce(mockJsonResponse({ ok: true }))
      .mockResolvedValueOnce(mockJsonResponse({ ok: true }))

    await t.call("get_workflow_graph_snapshot", { conversationId: 1 })
    await t.call("get_workflow_graph_snapshot", { conversationId: 2 })

    await t.call("retry_completion_artifact", {
      cas: {
        attention_id: "att-1",
        task_id: "t1",
        kind: "completion_artifact_recovery",
        captured_scope_digest: "sha256:1",
        latest_run_id: "t1",
        node_id: "n1",
      },
    })
    await t.call("resolve_design_self_review", {
      cas: {
        attention_id: "att-2",
        task_id: "t2",
        kind: "design_self_review_decision",
        captured_scope_digest: "sha256:2",
        latest_run_id: "t2",
        node_id: "n2",
      },
      outcome: "approve",
    })

    const retryHeaders = callHeaders(2) as Record<string, string>
    const selfReviewHeaders = callHeaders(3) as Record<string, string>
    expect(retryHeaders[COMPLETION_CONTEXT_HEADER]).toBe("token-root-1")
    expect(selfReviewHeaders[COMPLETION_CONTEXT_HEADER]).toBe("token-root-2")
  })

  it("does not send bearer-only completion mutations without a captured capability", async () => {
    const t = new WebTransport("http://localhost")
    fetchMock.mockResolvedValueOnce(mockJsonResponse({ ok: true }))

    await t.call("resolve_completion_decision", {
      cas: {
        attention_id: "unknown-attention",
        task_id: "t",
        kind: "completion_decision",
        captured_scope_digest: "sha256:x",
        latest_run_id: "t",
        node_id: "n",
      },
      outcome: "done",
    })

    const headers = callHeaders(0) as Record<string, string>
    expect(headers[COMPLETION_CONTEXT_HEADER]).toBeUndefined()
    expect(headers.Authorization).toBe("Bearer tok")
  })
})
