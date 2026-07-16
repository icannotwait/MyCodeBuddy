import { getTransport } from "@/lib/transport"
import type { Transport, UnsubscribeFn } from "@/lib/transport/types"
import type {
  DesktopAcpEventBatch,
  DesktopAcpEventHandlers,
  DesktopDeliveryCapabilities,
  DesktopDeliveryFailure,
  EventEnvelope,
} from "@/lib/types"

/**
 * Subscribe to exactly one desktop ACP data event for the process lifetime.
 *
 * - `batched` → `acp://event-batch` + `failure_event`
 * - `legacy`  → `acp://event` only (wrapped as one-event batches)
 *
 * Never hot-switches event names. Call once at startup after reading
 * capabilities; do not re-subscribe based on runtime failures.
 */
export async function subscribeDesktopAcpEvents(
  capabilities: DesktopDeliveryCapabilities,
  handlers: DesktopAcpEventHandlers,
  transport: Pick<Transport, "subscribe"> = getTransport()
): Promise<UnsubscribeFn> {
  const unsubs: UnsubscribeFn[] = []
  if (capabilities.mode === "batched") {
    unsubs.push(
      await transport.subscribe<DesktopAcpEventBatch>(
        "acp://event-batch",
        handlers.onBatch
      )
    )
    unsubs.push(
      await transport.subscribe<DesktopDeliveryFailure>(
        capabilities.failure_event,
        handlers.onFailure
      )
    )
  } else {
    let nextLegacyDeliveryId = 0
    unsubs.push(
      await transport.subscribe<EventEnvelope>("acp://event", (event) => {
        nextLegacyDeliveryId += 1
        handlers.onBatch({ batch_id: nextLegacyDeliveryId, events: [event] })
      })
    )
  }
  return () => {
    for (const unsubscribe of unsubs.splice(0)) unsubscribe()
  }
}
