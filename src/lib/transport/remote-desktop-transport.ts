import { WS_READY_CHANNEL } from "./constants"
import type { RemoteTransportConfig, Transport, UnsubscribeFn } from "./types"
import { buildCodegWebSocketProtocols } from "./ws-auth"

const REMOTE_CALL_TIMEOUT_MS = 30_000
// See WebTransport for rationale. Bounded so an older remote codeg-server
// (no `__ready__` support) can't permanently hang the desktop UI.
const READY_TIMEOUT_MS = 5_000

interface WebEvent {
  channel: string
  payload: unknown
}

export class RemoteDesktopTransport implements Transport {
  private ws: WebSocket | null = null
  private handlers = new Map<string, Set<(payload: unknown) => void>>()
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private wsFailCount = 0
  private config: RemoteTransportConfig
  private readyPromise!: Promise<void>
  private readyResolve!: () => void
  // See WebTransport for rationale. Reconnect callbacks fire only on
  // subsequent `__ready__` arrivals (not the initial connect).
  private hasReadiedOnce = false
  private reconnectCallbacks = new Set<() => void>()
  // Latched in `destroy()`. Without it, the async `onclose` fired by
  // `ws.close()` inside `destroy()` would schedule a new reconnect timer
  // (resurrected zombie WS, ignored by every consumer but still consuming
  // sockets and CPU). `onclose` short-circuits when this is true.
  private destroyed = false

  constructor(config: RemoteTransportConfig) {
    this.config = {
      ...config,
      baseUrl: config.baseUrl.replace(/\/+$/, ""),
    }
    this.resetReady()
  }

  private resetReady() {
    this.readyPromise = new Promise<void>((resolve) => {
      this.readyResolve = resolve
    })
  }

  // Bounded wait on `readyPromise`; logs and falls through on timeout
  // rather than hanging the UI. Public for the same reason as
  // WebTransport.waitForReady — callers gate HTTP commands on WS readiness
  // to avoid mid-reconnect drops.
  async waitForReady(): Promise<void> {
    let timeoutId: ReturnType<typeof setTimeout> | undefined
    const timeoutPromise = new Promise<"timeout">((resolve) => {
      timeoutId = setTimeout(() => resolve("timeout"), READY_TIMEOUT_MS)
    })
    const result = await Promise.race([
      this.readyPromise.then(() => "ready" as const),
      timeoutPromise,
    ])
    if (timeoutId !== undefined) clearTimeout(timeoutId)
    if (result === "timeout") {
      console.warn(
        `[RemoteDesktopTransport] WS __ready__ frame did not arrive within ${READY_TIMEOUT_MS}ms; ` +
          "proceeding without server-side subscribe confirmation (initial-connect race may reopen)."
      )
    }
  }

  async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    const controller = new AbortController()
    const timeout = window.setTimeout(
      () => controller.abort(),
      REMOTE_CALL_TIMEOUT_MS
    )
    let res: Response
    try {
      res = await fetch(`${this.config.baseUrl}/api/${command}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.config.token}`,
        },
        body: JSON.stringify(args ?? {}),
        signal: controller.signal,
      })
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        throw new Error("Remote Workspace request timed out")
      }
      throw err
    } finally {
      window.clearTimeout(timeout)
    }
    if (res.status === 401) {
      this.config.onUnauthorized?.()
      throw new Error("Remote Workspace connection expired")
    }
    if (!res.ok) {
      const error = await res.json().catch(() => ({
        code: "network_error",
        message: `HTTP ${res.status}`,
      }))
      throw error
    }
    return res.json()
  }

  async subscribe<T>(
    event: string,
    handler: (payload: T) => void
  ): Promise<UnsubscribeFn> {
    if (!this.handlers.has(event)) {
      this.handlers.set(event, new Set())
    }
    const wrappedHandler = handler as (payload: unknown) => void
    this.handlers.get(event)!.add(wrappedHandler)

    if (!this.ws) {
      this.connectWs()
    }

    // Gate on the server-side broadcaster receiver being subscribed (see
    // WebTransport for the full rationale). Without this await, events fired
    // before the server-side `subscribe()` runs are dropped by the
    // `receiver_count == 0` guard, leaving the UI stuck on "正在连接".
    await this.waitForReady()

    return () => {
      this.handlers.get(event)?.delete(wrappedHandler)
    }
  }

  isDesktop(): boolean {
    return true
  }

  onReconnect(callback: () => void): UnsubscribeFn {
    this.reconnectCallbacks.add(callback)
    return () => {
      this.reconnectCallbacks.delete(callback)
    }
  }

  private connectWs() {
    const wsUrl = this.config.baseUrl.replace(/^http/, "ws") + "/ws/events"
    this.ws = new WebSocket(
      wsUrl,
      buildCodegWebSocketProtocols(this.config.token)
    )

    this.ws.onopen = () => {
      this.wsFailCount = 0
    }

    this.ws.onmessage = (msg) => {
      try {
        const event = JSON.parse(msg.data) as WebEvent
        if (event.channel === WS_READY_CHANNEL) {
          this.readyResolve()
          if (this.hasReadiedOnce) {
            for (const cb of this.reconnectCallbacks) {
              try {
                cb()
              } catch (err) {
                console.error(
                  "[RemoteDesktopTransport] reconnect callback threw:",
                  err
                )
              }
            }
          } else {
            this.hasReadiedOnce = true
          }
          return
        }
        const handlers = this.handlers.get(event.channel)
        if (handlers) {
          for (const h of handlers) h(event.payload)
        }
      } catch {
        return
      }
    }

    this.ws.onclose = () => {
      this.ws = null
      this.resetReady()
      if (this.destroyed) return
      this.wsFailCount++
      if (this.wsFailCount >= 3) {
        this.config.onUnauthorized?.()
        return
      }
      this.reconnectTimer = setTimeout(() => this.connectWs(), 3000)
    }

    this.ws.onerror = () => {
      this.ws?.close()
    }
  }

  destroy() {
    this.destroyed = true
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    if (this.ws) {
      // Detach handlers BEFORE close() so the async `onclose` fired by the
      // browser doesn't see this instance at all. The `destroyed` guard above
      // is defensive belt-and-suspenders for any pending events already queued.
      this.ws.onopen = null
      this.ws.onmessage = null
      this.ws.onclose = null
      this.ws.onerror = null
      this.ws.close()
      this.ws = null
    }
    this.handlers.clear()
    this.reconnectCallbacks.clear()
    // Settle any in-flight `subscribe()` awaiters so their promises don't
    // leak alongside the destroyed transport.
    this.readyResolve?.()
  }
}
