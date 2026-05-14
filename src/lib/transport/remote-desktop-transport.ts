import { invoke } from "@tauri-apps/api/core"
import { listen, type UnlistenFn } from "@tauri-apps/api/event"
import { WS_READY_CHANNEL } from "./constants"
import type { RemoteTransportConfig, Transport, UnsubscribeFn } from "./types"

// See WebTransport for rationale. Bounded so an older remote codeg-server
// (no `__ready__` support) can't permanently hang the desktop UI.
const READY_TIMEOUT_MS = 5_000

/// Internal lifecycle channels emitted by the Rust-side proxy
/// (`src-tauri/src/commands/remote_proxy.rs`). Keep these in sync with
/// the `WS_*_CHANNEL` consts on the Rust side. The string literals match
/// the wire format exactly; we re-export `__ready__` via `WS_READY_CHANNEL`
/// to keep the shared-constants invariant with `WebTransport`.
const WS_DISCONNECTED_CHANNEL = "__disconnected__"
const WS_UNAUTHORIZED_CHANNEL = "__unauthorized__"

interface WsEnvelope {
  channel: string
  payload: unknown
}

interface RustError {
  code?: string
  message?: string
  detail?: string | null
  i18n_key?: string | null
  i18n_params?: Record<string, string> | null
}

function isAuthenticationFailed(err: unknown): boolean {
  if (err && typeof err === "object" && "code" in err) {
    return (err as RustError).code === "authentication_failed"
  }
  return false
}

/**
 * Transport that the desktop client uses when a window is bound to a
 * remote codeg-server. Every HTTP call and WebSocket event is routed
 * through Rust commands (`remote_http_call`, `remote_ws_subscribe`,
 * `remote_ws_unsubscribe`) defined in `src-tauri/src/commands/remote_proxy.rs`.
 *
 * We never open a fetch or WebSocket from the webview directly: the Tauri
 * webview is a secure context, so plain `http://` / `ws://` connections
 * to the remote host get blocked by mixed-content rules. Routing through
 * Rust (reqwest + tokio-tungstenite) bypasses those restrictions.
 *
 * Window isolation: the Rust side dispatches each WS frame only to the
 * webview labels that explicitly subscribed via this transport, never
 * `app.emit` broadcasting. Two remote workspaces opened side-by-side
 * see entirely separate event streams.
 */
export class RemoteDesktopTransport implements Transport {
  private handlers = new Map<string, Set<(payload: unknown) => void>>()
  private config: RemoteTransportConfig
  private readyPromise!: Promise<void>
  private readyResolve!: () => void
  /// Tracks whether `__ready__` has arrived since this transport was
  /// constructed. The first arrival is the initial connect; subsequent
  /// arrivals are reconnects (after `__disconnected__` reset the promise).
  /// Reconnect callbacks fire only on subsequent arrivals.
  private hasReadiedOnce = false
  private reconnectCallbacks = new Set<() => void>()
  /// Listener handle for `remote-ws-event-{id}`. Null when not subscribed.
  private unlistenWsEvent: UnlistenFn | null = null
  /// Tracks whether we have requested the Rust task to start. Used to
  /// keep `subscribe()` idempotent on multiple event registrations.
  private wsRequested = false
  /// Latched in `destroy()` so any in-flight `subscribe()` awaiters
  /// settle promptly instead of hanging on `readyPromise`.
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
    try {
      const result = await invoke<T>("remote_http_call", {
        connectionId: this.config.id,
        command,
        args: args ?? {},
      })
      return result
    } catch (err) {
      // The Rust proxy returns 401 from the remote server as
      // `AppErrorCode::AuthenticationFailed`. Surface the connection-expired
      // UI in just the calling window (the rest stay live until they
      // themselves hit a 401 — per design we don't broadcast).
      if (isAuthenticationFailed(err)) {
        this.config.onUnauthorized?.()
      }
      throw err
    }
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

    if (!this.wsRequested && !this.destroyed) {
      await this.startWs()
    }

    // Gate on the server-side broadcaster receiver being subscribed (see
    // WebTransport for the full rationale). Without this await, events
    // fired before the server-side `subscribe()` runs are dropped by the
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

  private async startWs() {
    // Register the listener BEFORE asking the Rust task to start, so the
    // first `__ready__` emit is never missed. Tauri's `listen()` is async,
    // and `remote_ws_subscribe` can emit synchronously the moment a
    // pre-existing socket already exists (e.g. another window opened
    // first), so this ordering matters.
    this.wsRequested = true

    try {
      this.unlistenWsEvent = await listen<WsEnvelope>(
        `remote-ws-event-${this.config.id}`,
        (event) => this.handleWsEvent(event.payload)
      )
    } catch (err) {
      this.wsRequested = false
      throw err
    }

    try {
      await invoke("remote_ws_subscribe", {
        connectionId: this.config.id,
      })
    } catch (err) {
      // If the Rust side refused the subscription (e.g. connection row
      // missing), tear down the listener so we don't leak it.
      this.unlistenWsEvent?.()
      this.unlistenWsEvent = null
      this.wsRequested = false
      throw err
    }
  }

  private handleWsEvent(envelope: WsEnvelope) {
    if (this.destroyed) return

    const { channel, payload } = envelope
    if (channel === WS_READY_CHANNEL) {
      this.readyResolve()
      if (this.hasReadiedOnce) {
        // Reconnect path: server-side receiver_count was 0 during the
        // disconnect window, so any event fired in that gap was dropped.
        // Notify consumers to recover state (e.g. refetch snapshots).
        // Errors in user callbacks must not break sibling callbacks.
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
    if (channel === WS_DISCONNECTED_CHANNEL) {
      // New subscribers (and any concurrent subscribe() calls in flight)
      // must wait for the next `__ready__` before resolving.
      this.resetReady()
      return
    }
    if (channel === WS_UNAUTHORIZED_CHANNEL) {
      // Rust gave up after WS_RECONNECT_FAIL_THRESHOLD failures, OR the
      // remote rejected the handshake. Either way, surface as expired.
      this.config.onUnauthorized?.()
      return
    }
    const handlers = this.handlers.get(channel)
    if (handlers) {
      for (const h of handlers) {
        h(payload)
      }
    }
  }

  destroy() {
    this.destroyed = true
    if (this.unlistenWsEvent) {
      this.unlistenWsEvent()
      this.unlistenWsEvent = null
    }
    if (this.wsRequested) {
      // Fire and forget — destroy() runs during React unmount and we
      // must not block. Rust's WindowEvent::Destroyed hook provides a
      // belt-and-suspenders backup for forced-kill paths where this
      // invoke never lands.
      invoke("remote_ws_unsubscribe", {
        connectionId: this.config.id,
      }).catch((err) => {
        console.warn(
          "[RemoteDesktopTransport] remote_ws_unsubscribe failed:",
          err
        )
      })
      this.wsRequested = false
    }
    this.handlers.clear()
    this.reconnectCallbacks.clear()
    // Settle any in-flight `subscribe()` awaiters so their promises don't
    // leak alongside the destroyed transport. Safe to call multiple times.
    this.readyResolve?.()
  }
}
