import { detectEnvironment } from "./detect"
import type { RemoteTransportConfig, Transport } from "./types"

export type { RemoteTransportConfig, Transport, UnsubscribeFn } from "./types"

let _shellTransport: Transport | null = null
let _remoteTransport: Transport | null = null
let _remoteConfig: RemoteTransportConfig | null = null

function createTauriTransport(): Transport {
  // Use dynamic require to avoid bundling tauri deps in web mode.
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { TauriTransport } = require("./tauri-transport") as {
    TauriTransport: new () => Transport
  }
  return new TauriTransport()
}

function createWebTransport(baseUrl: string): Transport {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { WebTransport } = require("./web-transport") as {
    WebTransport: new (baseUrl: string) => Transport
  }
  return new WebTransport(baseUrl)
}

export function getShellTransport(): Transport {
  if (!_shellTransport) {
    const env = detectEnvironment()
    _shellTransport =
      env === "tauri"
        ? createTauriTransport()
        : createWebTransport(window.location.origin)
  }
  return _shellTransport
}

export function configureRemoteDesktopTransport(
  config: RemoteTransportConfig
): void {
  _remoteTransport?.destroy?.()
  _remoteConfig = config
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { RemoteDesktopTransport } = require("./remote-desktop-transport") as {
    RemoteDesktopTransport: new (config: RemoteTransportConfig) => Transport
  }
  _remoteTransport = new RemoteDesktopTransport(config)
}

export function clearRemoteDesktopTransport(): void {
  _remoteTransport?.destroy?.()
  _remoteTransport = null
  _remoteConfig = null
}

export function getActiveRemoteConnectionId(): number | null {
  return _remoteConfig?.id ?? null
}

/**
 * Stable key identifying the active backend for client-side caches.
 * Changes when the shell switches between local Tauri, a web origin, or a
 * remote-desktop connection so generation-scoped stores can reset cleanly.
 */
export function getActiveBackendCacheKey(): string {
  const remote = getActiveRemoteConnectionId()
  if (remote != null) return `remote:${remote}`
  if (typeof window !== "undefined" && detectEnvironment() === "web") {
    return `web:${window.location.origin}`
  }
  return "local:tauri"
}

export function getTransport(): Transport {
  return _remoteTransport ?? getShellTransport()
}

export function isDesktop(): boolean {
  return detectEnvironment() === "tauri"
}

/// True when the current window is a Tauri client bound to a remote
/// codeg-server (a remote-desktop window). Distinct from `isDesktop()`,
/// which is purely a runtime check — a remote-desktop window IS a Tauri
/// runtime but its API calls and file ops must target the remote host,
/// not the local filesystem.
export function isRemoteDesktopMode(): boolean {
  return _remoteTransport !== null
}

/// Base URL of the codeg server backing the current transport, for building
/// raw resource URLs that bypass the JSON transport (iframes, downloads).
/// Remote-desktop → the remote host; web → this page's origin. In pure-desktop
/// mode there's no server (callers use loopback / `invoke`), so this returns
/// the local origin only as a harmless fallback.
export function getServerBaseUrl(): string {
  if (_remoteConfig) return _remoteConfig.baseUrl.replace(/\/+$/, "")
  return typeof window !== "undefined" ? window.location.origin : ""
}

/// Surface a remote-server 401 to the same UI the transport uses for its
/// own auth failures. Direct `invoke()` calls (workspace file
/// upload/download) bypass `RemoteDesktopTransport.call`, so without
/// this they'd toast "token invalid" but never raise the
/// `connection-expired` dialog the rest of the app uses. Calling this on
/// a non-remote-desktop window is a no-op so the helper is safe to use
/// unconditionally from a 401 catch block.
export function notifyRemoteDesktopUnauthorized(): void {
  _remoteConfig?.onUnauthorized?.()
}

/**
 * Test-only: clear the cached shell + remote transports so a subsequent
 * `getTransport()` / `getShellTransport()` call re-runs environment detection
 * against the current `window` mock. The module-level singletons would
 * otherwise stick across test cases. Not intended for production use.
 * @internal
 */
export function __resetTransportForTests(): void {
  // Hard guard: collapses to a no-op outside vitest. Turbopack/webpack DCE
  // the dead branch in `next build` so the function ships as a single
  // `return` in the prod bundle instead of reaching into module state.
  if (process.env.NODE_ENV !== "test") return
  _shellTransport?.destroy?.()
  _remoteTransport?.destroy?.()
  _shellTransport = null
  _remoteTransport = null
  _remoteConfig = null
}

/**
 * Test-only: install a remote-desktop binding without dynamically requiring
 * the real transport class (Node `require` cannot resolve the TS module under
 * Vitest). Production code must use `configureRemoteDesktopTransport`.
 * @internal
 */
export function __setRemoteDesktopForTests(
  config: RemoteTransportConfig,
  transport: Transport
): void {
  if (process.env.NODE_ENV !== "test") return
  _remoteTransport?.destroy?.()
  _remoteConfig = config
  _remoteTransport = transport
}
