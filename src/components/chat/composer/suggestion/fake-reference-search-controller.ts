import type { ReferenceAttrs } from "../types"
import type {
  ReferenceSearchController,
  ReferenceSearchSnapshot,
} from "../reference-search-controller"

type ConfirmDeferred = {
  promise: Promise<ReferenceAttrs | null>
  resolve: (value: ReferenceAttrs | null) => void
}

/** Fake Task-8 controller surface for popup / composer unit tests. */
export class FakeReferenceSearchController {
  readonly searchSessionId = "fake-session"
  private listeners = new Set<() => void>()
  private snapshot: ReferenceSearchSnapshot
  private selectedUri: string | null = null
  private confirmCount = 0
  private closeCount = 0
  private active = false
  private confirmDeferred: ConfirmDeferred | null = null
  private autoResolveConfirm: boolean
  queries: string[] = []
  selectedUris: Array<string | null> = []

  constructor(
    snapshot: ReferenceSearchSnapshot,
    options: { autoResolveConfirm?: boolean } = {}
  ) {
    this.snapshot = snapshot
    this.autoResolveConfirm = options.autoResolveConfirm ?? true
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  getSnapshot = (): ReferenceSearchSnapshot => this.snapshot

  setQuery = (query: string): void => {
    this.active = true
    this.queries.push(query)
    this.snapshot = { ...this.snapshot, query }
    this.notify()
  }

  updateInputs = (): void => {}

  setSelectedUri = (uri: string | null): void => {
    this.selectedUri = uri
    this.selectedUris.push(uri)
  }

  confirmCandidate = async (uri: string): Promise<ReferenceAttrs | null> => {
    this.confirmCount += 1
    if (!this.autoResolveConfirm) {
      if (!this.confirmDeferred) {
        let resolve!: (value: ReferenceAttrs | null) => void
        const promise = new Promise<ReferenceAttrs | null>((res) => {
          resolve = res
        })
        this.confirmDeferred = { promise, resolve }
      }
      return this.confirmDeferred.promise
    }
    for (const group of Object.values(this.snapshot.groups)) {
      const found = group.items.find((i) => i.reference.uri === uri)
      if (found?.selectable) return { ...found.reference }
    }
    return null
  }

  close = (): void => {
    if (!this.active) return
    this.active = false
    this.closeCount += 1
  }

  publish(next: ReferenceSearchSnapshot): void {
    this.snapshot = next
    this.notify()
  }

  resolveConfirmation(value: ReferenceAttrs | null): void {
    const deferred = this.confirmDeferred
    if (!deferred) throw new Error("no pending confirmation")
    this.confirmDeferred = null
    deferred.resolve(value)
  }

  confirmCallCount(): number {
    return this.confirmCount
  }

  closeCallCount(): number {
    return this.closeCount
  }

  getSelectedUri(): string | null {
    return this.selectedUri
  }

  /** Test helper: mark active so close() is not a no-op. */
  markActive(): void {
    this.active = true
  }

  enableAutoResolveConfirm(): void {
    this.autoResolveConfirm = true
  }

  asController(): ReferenceSearchController {
    return this as unknown as ReferenceSearchController
  }

  private notify(): void {
    for (const listener of this.listeners) listener()
  }
}
