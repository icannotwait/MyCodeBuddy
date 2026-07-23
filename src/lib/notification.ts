import { getTransport } from "./transport"
import { isDesktop } from "./transport"
import { randomUUID } from "./utils"

/**
 * Safe navigation target for desktop system notifications.
 * Watchdog (and any caller) must never put tool input / prompts here —
 * only an opaque conversation id.
 */
export type NotificationTarget = {
  kind: "conversation"
  conversationId: number
}

/** Optional host-side once-per-key gate (multi-window dedupe). */
export type SystemNotificationOptions = {
  /**
   * Opaque host dedupe key (e.g. `leaseId:version`). When set, the desktop
   * host shows the notification at most once per key across all windows.
   */
  dedupeKey?: string
}

/**
 * Show a host system notification when the document is hidden.
 *
 * - **Desktop**: always uses the Tauri `send_notification` command. Wire keys
 *   are camelCase (`actionId`, `conversationId`, `dedupeKey`) to match Tauri's
 *   default command arg renaming. When `target` is provided, registers an
 *   opaque action id so a click can emit `notification-navigate` **on platforms
 *   that support click callbacks**; the host omits registration cleanly when
 *   the platform cannot fire navigation.
 * - **Server/Web**: browser `Notification` only when **no** `target` is passed
 *   (permission / turn-complete style alerts). Watchdog callers **must** gate
 *   with `isDesktop()` and pass a target so web never invents a host channel.
 */
export async function sendSystemNotification(
  title: string,
  body: string,
  target?: NotificationTarget,
  options?: SystemNotificationOptions
): Promise<void> {
  if (typeof document !== "undefined" && !document.hidden) return

  if (isDesktop()) {
    const actionId = target?.kind === "conversation" ? randomUUID() : undefined
    await getTransport().call("send_notification", {
      title,
      body,
      actionId: actionId ?? null,
      conversationId:
        target?.kind === "conversation" ? target.conversationId : null,
      dedupeKey: options?.dedupeKey ?? null,
    })
    return
  }

  // Web fallback: only for non-targeted notifications. Watchdog warnings
  // pass a target and are desktop-only (caller-gated); never use browser
  // Notification for those.
  if (target) return

  if (typeof Notification === "undefined") return
  if (Notification.permission === "granted") {
    new Notification(title, { body })
  } else if (Notification.permission !== "denied") {
    const permission = await Notification.requestPermission()
    if (permission === "granted") {
      new Notification(title, { body })
    }
  }
}
