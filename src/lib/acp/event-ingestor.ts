import type {
  AcceptedConnectionFrame,
  AcceptedEventFrame,
  DesktopAcpEventBatch,
  EventEnvelope,
  SequenceGap,
} from "@/lib/types"

export type { AcceptedConnectionFrame, AcceptedEventFrame, SequenceGap }

export interface EventIngestorDeps {
  resolveContextKey(connectionId: string): string | null
  readCursor(contextKey: string): number
  commit(frame: AcceptedEventFrame): void
  onGap(gap: SequenceGap): void
  /** Duplicate / already-applied seq (seq ≤ provisional cursor). */
  onDuplicate?(info: {
    connectionId: string
    contextKey: string
    seq: number
  }): void
  onUnmapped(event: EventEnvelope): void
  scheduleFrame(callback: FrameRequestCallback): number
  cancelFrame(handle: number): void
}

interface PendingItem {
  deliveryId: number
  event: EventEnvelope
  /** Pre-resolved context key from `pushMapped` (attach path). */
  mappedKey?: string
}

interface ConnectionDrainState {
  contextKey: string
  provisional: number
  accepted: EventEnvelope[]
  acceptedItems: PendingItem[]
  deliveryIds: number[]
  deliveryIdSet: Set<number>
}

/**
 * Merge only adjacent same-type text/thinking deltas for one connection.
 * The merged envelope keeps the last event's `seq` and concatenates `text`.
 * Never merges `tool_call_update` or any other variant.
 */
export function compactAdjacentDeltas(
  events: readonly EventEnvelope[]
): EventEnvelope[] {
  const out: EventEnvelope[] = []
  for (const event of events) {
    const prev = out[out.length - 1]
    if (
      prev &&
      prev.type === event.type &&
      (event.type === "content_delta" || event.type === "thinking") &&
      (prev.type === "content_delta" || prev.type === "thinking") &&
      prev.type === event.type
    ) {
      out[out.length - 1] = {
        ...event,
        text: prev.text + event.text,
      }
      continue
    }
    out.push(event)
  }
  return out
}

/**
 * Non-React desktop/attach event coalescer.
 *
 * Queues envelopes in delivery order, validates per-connection contiguity,
 * compacts adjacent text/thinking deltas, and commits one immutable frame
 * per browser animation frame (or via `flushNow`).
 *
 * Does not touch React or Zustand — the provider owns the commit callback.
 */
export class EventIngestor {
  private readonly deps: EventIngestorDeps
  private pending: PendingItem[] = []
  private frameHandle: number | null = null
  private disposed = false
  private readonly paused = new Set<string>()
  /** Resume floor used until a successful commit advances the provider cursor. */
  private readonly resumeCursors = new Map<string, number>()
  private syntheticDeliveryId = 0

  constructor(deps: EventIngestorDeps) {
    this.deps = deps
  }

  pushBatch(batch: DesktopAcpEventBatch): void {
    if (this.disposed) return
    for (const event of batch.events) {
      this.pending.push({ deliveryId: batch.batch_id, event })
    }
    this.ensureScheduled()
  }

  /**
   * Attach/replay path: events already mapped to a context key, with no
   * desktop batch id — allocate a process-local synthetic delivery id.
   */
  pushMapped(contextKey: string, events: readonly EventEnvelope[]): void {
    if (this.disposed) return
    if (events.length === 0) return
    this.syntheticDeliveryId += 1
    const deliveryId = this.syntheticDeliveryId
    for (const event of events) {
      this.pending.push({ deliveryId, event, mappedKey: contextKey })
    }
    this.ensureScheduled()
  }

  pauseConnection(connectionId: string): void {
    this.paused.add(connectionId)
  }

  /**
   * Unpause a connection after snapshot recovery.
   * Drops buffered duplicates (`seq <= cursor`), then requires contiguity
   * from `cursor + 1` on the next drain.
   */
  resumeConnection(connectionId: string, cursor: number): void {
    if (this.disposed) return
    this.paused.delete(connectionId)
    this.resumeCursors.set(connectionId, cursor)
    this.pending = this.pending.filter((item) => {
      if (item.event.connection_id !== connectionId) return true
      return item.event.seq > cursor
    })
    this.ensureScheduled()
  }

  flushNow(): void {
    if (this.disposed) return
    this.cancelScheduled()
    this.drain()
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.cancelScheduled()
    this.pending = []
    this.paused.clear()
    this.resumeCursors.clear()
  }

  private ensureScheduled(): void {
    if (this.disposed || this.frameHandle !== null) return
    if (this.pending.length === 0) return
    this.frameHandle = this.deps.scheduleFrame(() => {
      this.frameHandle = null
      this.drain()
    })
  }

  private cancelScheduled(): void {
    if (this.frameHandle === null) return
    this.deps.cancelFrame(this.frameHandle)
    this.frameHandle = null
  }

  private initialCursor(connectionId: string, contextKey: string): number {
    const resumed = this.resumeCursors.get(connectionId)
    if (resumed !== undefined) return resumed
    return this.deps.readCursor(contextKey)
  }

  private drain(): void {
    if (this.disposed) return

    const retained: PendingItem[] = []
    const byConnection = new Map<string, ConnectionDrainState>()
    const globalDeliveryIds: number[] = []
    const globalDeliveryIdSet = new Set<number>()
    const rawInOrder: EventEnvelope[] = []
    const acceptedItems: PendingItem[] = []

    for (const item of this.pending) {
      const { event } = item
      const connectionId = event.connection_id

      const contextKey =
        item.mappedKey ?? this.deps.resolveContextKey(connectionId)
      if (contextKey === null) {
        this.deps.onUnmapped(event)
        continue
      }

      if (this.paused.has(connectionId)) {
        retained.push(item)
        continue
      }

      let state = byConnection.get(connectionId)
      if (!state) {
        state = {
          contextKey,
          provisional: this.initialCursor(connectionId, contextKey),
          accepted: [],
          acceptedItems: [],
          deliveryIds: [],
          deliveryIdSet: new Set(),
        }
        byConnection.set(connectionId, state)
      }

      if (event.seq <= state.provisional) {
        // Duplicate / already applied — drop (report for integrity).
        this.deps.onDuplicate?.({
          connectionId,
          contextKey: state.contextKey,
          seq: event.seq,
        })
        continue
      }

      if (event.seq !== state.provisional + 1) {
        this.deps.onGap({
          contextKey: state.contextKey,
          connectionId,
          expectedSeq: state.provisional + 1,
          receivedSeq: event.seq,
        })
        this.paused.add(connectionId)
        retained.push(item)
        continue
      }

      state.provisional = event.seq
      state.accepted.push(event)
      state.acceptedItems.push(item)
      acceptedItems.push(item)
      if (!state.deliveryIdSet.has(item.deliveryId)) {
        state.deliveryIdSet.add(item.deliveryId)
        state.deliveryIds.push(item.deliveryId)
      }
      if (!globalDeliveryIdSet.has(item.deliveryId)) {
        globalDeliveryIdSet.add(item.deliveryId)
        globalDeliveryIds.push(item.deliveryId)
      }
      rawInOrder.push(event)
    }

    this.pending = retained

    const connections: AcceptedConnectionFrame[] = []
    for (const [connectionId, state] of byConnection) {
      if (state.accepted.length === 0) continue
      connections.push({
        contextKey: state.contextKey,
        connectionId,
        deliveryIds: state.deliveryIds,
        applyEvents: compactAdjacentDeltas(state.accepted),
        rawEvents: state.accepted,
        highestSeq: state.provisional,
      })
    }

    if (connections.length === 0) return

    const frame: AcceptedEventFrame = {
      deliveryIds: globalDeliveryIds,
      connections,
      rawEventsInDeliveryOrder: rawInOrder,
    }

    try {
      this.deps.commit(frame)
      for (const connection of connections) {
        this.resumeCursors.delete(connection.connectionId)
      }
    } catch (error) {
      // Leave cursors unadvanced: re-buffer accepted work and pause for recovery.
      this.pending = [...acceptedItems, ...this.pending]
      for (const connection of connections) {
        this.paused.add(connection.connectionId)
      }
      throw error
    }
  }
}
