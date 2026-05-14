import {
  getActiveRemoteConnectionId,
  isDesktop,
  getTransport,
} from "./transport"
import type { UnsubscribeFn } from "./transport"

/**
 * Platform-aware API wrappers for features that differ between
 * Tauri desktop and web browser environments.
 */

export { isDesktop }

/**
 * Subscribe to backend events.
 * Uses Tauri listen() in desktop mode, WebSocket in web mode.
 */
export async function subscribe<T>(
  event: string,
  handler: (payload: T) => void
): Promise<UnsubscribeFn> {
  return getTransport().subscribe(event, handler)
}

/**
 * Register a callback to run after a WebSocket reconnect (post `__ready__`).
 * No-op for IPC-only transports (Tauri desktop without remote workspace),
 * where the underlying channel cannot disconnect mid-session.
 */
export function onTransportReconnect(callback: () => void): UnsubscribeFn {
  // Capture the transport instance once: between two `getTransport()` calls
  // the remote transport could be swapped via `configureRemoteDesktopTransport`
  // / `clearRemoteDesktopTransport`, leaving the bound method on a different
  // object than the one we destructured from.
  const transport = getTransport()
  const reconnect = transport.onReconnect
  if (!reconnect) {
    return () => {}
  }
  return reconnect.call(transport, callback)
}

/**
 * Resolve when the transport's WebSocket is ready to relay events emitted
 * by upcoming HTTP commands. Call this immediately before any HTTP command
 * whose effects observe a WS event stream (e.g. `acp_connect`); without
 * the await, the command may race a mid-session WS reconnect and have its
 * events silently dropped. No-op for IPC-only transports.
 */
export async function waitForTransportReady(): Promise<void> {
  // See onTransportReconnect for why we capture the instance once.
  const transport = getTransport()
  const waitForReady = transport.waitForReady
  if (!waitForReady) return
  await waitForReady.call(transport)
}

/**
 * Open a URL in the default browser (desktop) or new tab (web).
 */
export async function openUrl(url: string): Promise<void> {
  if (isDesktop() && getActiveRemoteConnectionId() === null) {
    const { openUrl: tauriOpenUrl } = await import("@tauri-apps/plugin-opener")
    await tauriOpenUrl(url)
  } else {
    window.open(url, "_blank")
  }
}

/**
 * Open a path in the system file manager (desktop only).
 * No-op in web mode.
 */
export async function openPath(path: string): Promise<void> {
  if (isDesktop() && getActiveRemoteConnectionId() === null) {
    const { openPath: tauriOpenPath } =
      await import("@tauri-apps/plugin-opener")
    await tauriOpenPath(path)
  }
}

/**
 * Reveal a file/directory in the system file manager (desktop only).
 * No-op in web mode.
 */
export async function revealItemInDir(path: string): Promise<void> {
  if (isDesktop() && getActiveRemoteConnectionId() === null) {
    const { revealItemInDir: tauriReveal } =
      await import("@tauri-apps/plugin-opener")
    await tauriReveal(path)
  }
}

/**
 * Open a native file/directory dialog (desktop) or fallback (web).
 */
export async function openFileDialog(options?: {
  directory?: boolean
  multiple?: boolean
  title?: string
  defaultPath?: string
}): Promise<string | string[] | null> {
  if (isDesktop() && getActiveRemoteConnectionId() === null) {
    const { open } = await import("@tauri-apps/plugin-dialog")
    return open(options ?? {})
  }
  // Web fallback: for directory selection, prompt for server-side path.
  // For file selection, use a hidden file input.
  if (options?.directory) {
    const path = window.prompt(
      options?.title ?? "输入服务端目录路径 (Enter server directory path)"
    )
    return path || null
  }
  return new Promise((resolve) => {
    const input = document.createElement("input")
    input.type = "file"
    if (options?.multiple) input.multiple = true
    input.onchange = () => {
      if (!input.files?.length) {
        resolve(null)
        return
      }
      const paths = Array.from(input.files).map((f) => f.name)
      resolve(options?.multiple ? paths : paths[0])
    }
    input.click()
  })
}

/**
 * Get the current Tauri window (desktop only).
 * Returns null in web mode.
 */
export async function getCurrentWindow() {
  if (isDesktop()) {
    const { getCurrentWindow: tauriGetCurrentWindow } =
      await import("@tauri-apps/api/window")
    return tauriGetCurrentWindow()
  }
  return null
}

/**
 * Close the current window.
 * Desktop: closes Tauri window. Web: navigates back or closes tab.
 */
export async function closeCurrentWindow(): Promise<void> {
  if (isDesktop()) {
    const win = await getCurrentWindow()
    await win?.close()
  } else {
    window.history.back()
  }
}
