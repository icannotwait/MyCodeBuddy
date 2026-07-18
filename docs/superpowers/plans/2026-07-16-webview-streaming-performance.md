# WebView Streaming Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate pause-then-jump rendering during high-rate agent replies by batching desktop ACP delivery, applying each browser frame as one transaction, isolating the active transcript from historical rows, and rendering only stable rich content.

**Architecture:** Rust remains authoritative for ACP state, sequence numbers, replay, and persistence. Only the local Tauri delivery boundary changes from one IPC event per envelope to bounded ordered batches; server and remote WebSocket clients retain their current per-envelope attach stream. A non-React ingestor commits accepted events once per frame, a per-conversation live projection drives a footer outside Virtua's historical item array, and an incremental Markdown document upgrades sealed blocks without reparsing the growing response.

**Tech Stack:** Rust 2021, Tokio, Tauri 2, React 19, TypeScript strict mode, Zustand 5, Streamdown 2.2, Shiki 3.22, Virtua 0.48, use-stick-to-bottom 1.1, Vitest, Next.js 16 static export.

## Global Constraints

- Do not adopt or prototype EUI-NEO, remove Axum/server mode, change ACP/MCP/SSE protocols, replace Tauri/React, or optimize Monaco/editor behavior in this scope.
- Keep `next.config.ts` on `output: "export"`; add no dynamic Next.js routes or server-only frontend APIs.
- The reference fixture is synthetic and deterministic: exactly 30,000 text characters, prose, CJK, fenced code, a GFM table, math, Mermaid, at least 50 tools, and interleaved text/thinking/tool/plan/control events.
- Exercise the fixture at exactly 100, 500, and 1,000 incoming envelopes per second.
- Windows WebView2 reference gates are batch-receipt-to-paint P95 `< 100 ms`, input-to-paint P95 `< 50 ms`, no main-thread task `> 200 ms`, and at least 30 visual updates/second while input is queued.
- Preserve every per-connection sequence number and event. Never drop, duplicate, reorder, or last-write-win an append update.
- Desktop batches flush on the first of 16 ms, 128 envelopes, 64 KiB estimated serialized bytes, a permission/question/completion/error event, or shutdown.
- Use a bounded 1,024-envelope Tokio channel. A full channel increments pressure metrics and awaits capacity only after the `SessionState` write lock is released.
- Keep raw per-envelope `ConnectionEventStream` and `InternalEventBus` delivery unchanged for server, remote, lifecycle, pet, and chat-channel consumers.
- Select `acp://event` or `acp://event-batch` once at desktop startup. Never hot-switch event names after connections become active.
- Keep three internal controls for one release: `desktop_acp_event_batching`, `incremental_live_transcript`, and `deferred_streaming_rich_content`. Invalid combinations normalize downward: incremental transcript requires batching; deferred rich content requires incremental transcript.
- Completed top-level blocks render rich Markdown. Only the unfinished tail updates at streaming frequency and remains escaped, selectable, copyable, and lightweight.
- The canonical `LiveMessage` remains the source for reconnect, snapshot recovery, completion, persistence promotion, export, and parity tests. The live projection is UI-only.
- Normal profiling must never persist or upload prompts, responses, tool inputs, or tool outputs. Reports contain fixture IDs, counts, byte sizes, timings, platform/build metadata, and pass/fail results only.
- P0 through P4 execute sequentially. Do not start a later phase until the preceding correctness tests and performance report have been reviewed.
- Every production behavior begins with a focused failing test and a confirmed RED result. Every task ends with focused GREEN verification and a scoped commit.
- Preserve unrelated worktree changes, especially the pre-existing edits to `src-tauri/Cargo.toml` and `src-tauri/resources/THIRD_PARTY_LICENSES.txt`; stage only exact paths owned by the current task.

---

## File Map

### Backend Delivery And Replay

- `src-tauri/src/acp/internal_bus.rs`: extends the existing atomic metrics and serializable snapshot with desktop-delivery fields.
- `src-tauri/src/acp/streaming_performance.rs`: owns rollout-flag normalization, desktop capability wire types, and metric-only diagnostic metadata.
- `src-tauri/src/acp/perf_fixture.rs`: test-utils-only deterministic fixture generation, fixed schedules, checksum, and replay driver.
- `src-tauri/src/acp/desktop_event_batcher.rs`: owns the bounded queue, flush policy, startup mode, failure state, shutdown drain, and Tauri batch sink.
- `src-tauri/src/acp/mod.rs`: exports the new ACP modules and wire types under the correct feature gates.
- `src-tauri/src/web/event_bridge.rs`: records P0 legacy delivery metrics, then delegates only the local Tauri leg to `DesktopAcpDelivery` in P1.
- `src-tauri/src/web/handlers/event_metrics.rs`: keeps the authenticated HTTP snapshot in parity with the desktop command.
- `src-tauri/src/commands/acp.rs`: exposes metrics/capabilities commands and the test-utils-only replay command.
- `src-tauri/src/lib.rs`: initializes managed desktop delivery, registers commands, enables test-utils replay registration, and drains the batcher at exit.
- `src-tauri/Cargo.toml`: enables Tokio time in production and Tauri devtools only through the existing `test-utils` feature.

### Frontend Measurement And Ingestion

- `src/lib/types.ts`: mirrors desktop capability, batch, failure, metrics, fixture, and replay-result wire contracts.
- `src/lib/api.ts`: adds transport-neutral metrics, capabilities, and test replay calls.
- `src/lib/transport/desktop-acp-events.ts`: subscribes to exactly one startup-selected ACP event name plus the batch-failure signal.
- `src/lib/transport/desktop-acp-events.test.ts`: verifies mode selection and unsubscribe behavior.
- `src/lib/acp/streaming-performance-config.ts`: process-local immutable capability snapshot and narrow React hooks for rollout flags.
- `src/lib/acp/event-ingestor.ts`: non-React frame queue, cursor validation, gap pause/recovery, compaction, and post-commit raw ordering.
- `src/lib/acp/event-ingestor.test.ts`: deterministic scheduler tests for deduplication, gaps, append ordering, and one frame commit.
- `src/lib/perf/streaming-perf-recorder.ts`: marks pipeline stages, observes long tasks/fallback drift, probes input, and counts renders.
- `src/lib/perf/streaming-perf-report.ts`: percentile math, cadence/integrity evaluation, JSON schema, and local download.
- `src/lib/perf/streaming-perf-recorder.test.ts`: recorder lifecycle, disabled fast path, long-task fallback, and quiet-drain tests.
- `src/lib/perf/streaming-perf-report.test.ts`: nearest-rank percentile and acceptance calculations.
- `src/contexts/acp-connections-context.tsx`: replaces timer queues with the ingestor, prepares event actions/effects, commits one connection transaction, and publishes canonical/live state once.
- `src/contexts/acp-connections-context.test.tsx`: provider transaction counts, recovery, control ordering, rekey, and subscriber regressions.
- `src/components/chat/message-input.tsx`: mounts the non-mutating input-latency marker only while a replay is active.
- `src/components/chat/message-input.test.tsx`: proves the probe never changes editor content.

### Stable History And Live Projection

- `src/stores/conversation-runtime-store.ts`: adds a stable historical selector and coordinated live-to-local promotion.
- `src/stores/conversation-runtime-store.test.ts`: focused historical cache, invalidation, collision, and handoff tests.
- `src/stores/runtime-live-message-slice-decoupling.test.ts`: extends the existing reference-stability regression coverage.
- `src/lib/acp/live-transcript-projector.ts`: pure snapshot construction, incremental event application, and canonical parity conversion.
- `src/lib/acp/live-transcript-projector.test.ts`: cross-agent snapshot/event parity fixtures.
- `src/stores/live-transcript-store.ts`: per-conversation segment/tool records and `useSyncExternalStore` selectors.
- `src/stores/live-transcript-store.test.ts`: structural identity, per-segment notification, rekey, reset, and recovery tests.
- `src/components/message/live-transcript-row.tsx`: renders narrow live segment subscriptions and completion state.
- `src/components/message/live-transcript-row.test.tsx`: render-count, accessibility, and handoff tests.
- `src/components/message/message-list-view.tsx`: consumes historical items and supplies a separate live footer.
- `src/components/message/message-list-view.test.tsx`: proves historical rows remain inert through hundreds of live publications.
- `src/components/message/virtualized-message-thread.tsx`: adds a stable footer inside `MessageThreadContent` but outside Virtua's item array.
- `src/components/message/virtualized-message-thread.test.tsx`: footer/item-key/navigation and observer-boundary tests.
- `src/components/message/message-scroll-context.tsx`: exposes live-footer commit coordination only if the existing handle is insufficient.
- `src/components/conversations/conversation-detail-panel.tsx`: registers the live frame sink and uses the atomic completion coordinator.
- `src/components/message/sub-agent-session-dialog.tsx`: uses the same live projection and handoff for child sessions.

### Incremental Rich Content And Tools

- `src/lib/cache/weighted-lru.ts`: reusable entry-and-byte-bounded LRU with explicit reset.
- `src/lib/cache/weighted-lru.test.ts`: recency, replacement, oversize, and byte-budget tests.
- `src/lib/markdown/incremental-stream-blocks.ts`: fence scanner, sealed-block/tail partition, completion cache, and invariant fallback.
- `src/lib/markdown/incremental-stream-blocks.test.ts`: arbitrary chunk boundaries, malformed input, exact-source parity, and bounded work.
- `src/components/message/streaming-markdown-document.tsx`: memoized sealed `MessageResponse` blocks and lightweight tail rendering.
- `src/components/message/streaming-markdown-document.test.tsx`: stable-block render counts, copy/selection, source fallback, and completion upgrade.
- `src/components/ai-elements/message.tsx`: accepts an explicit complete/sealed rich-content policy.
- `src/components/ai-elements/code-block.tsx`: shares in-flight highlighting, rejects stale results, and uses bounded completed-token storage.
- `src/components/ai-elements/code-block.test.tsx`: one-start, stale callback, failure fallback, and cache-eviction tests.
- `src/components/ai-elements/streamdown-plugins.ts`: idle code scheduling, sealed math policy, and completion/visibility-gated Mermaid.
- `src/components/ai-elements/streamdown-plugins.test.ts`: policy and capability-fallback tests.
- `src/components/ai-elements/heavy-plugins-warmup.tsx`: reuses the common idle scheduler without changing its current warmup contract.
- `src/lib/scheduling/idle-work.ts`: requestIdleCallback abstraction with timeout fallback and cancellation.
- `src/lib/scheduling/idle-work.test.ts`: WebView capability and cancellation tests.
- `src/components/message/content-parts-renderer.tsx`: exports focused live renderers and avoids constructing hidden tool bodies.
- Tool/terminal/diff/delegation/background component tests reached by `ContentPartsRenderer`: verify lazy bodies without changing completed output.

### Scroll, Layout, And Evidence

- `src/components/ai-elements/message-thread.tsx`: accepts explicit instant streaming resize behavior.
- `src/app/globals.css`: receives containment classes only when P4 traces meet the documented decision rule.
- `docs/superpowers/performance/webview-streaming/README.md`: exact build/run procedure and named reference-machine metadata.
- `docs/superpowers/performance/webview-streaming/baseline-{100,500,1000}eps.json`: P0 median-of-three Windows reports.
- `docs/superpowers/performance/webview-streaming/p1-{100,500,1000}eps.json`: P1 comparison reports.
- `docs/superpowers/performance/webview-streaming/p2-{100,500,1000}eps.json`: stable-history/live-footer reports.
- `docs/superpowers/performance/webview-streaming/p3-{100,500,1000}eps.json`: incremental-rich-content reports.
- `docs/superpowers/performance/webview-streaming/final-{100,500,1000}eps.json`: final Windows reports.
- `docs/superpowers/performance/webview-streaming/comparison.md`: before/after P50/P95/max, callback/commit/render counts, and phase attribution.
- `docs/superpowers/performance/webview-streaming/platform-smoke.md`: macOS/Linux/Windows acceleration and recovery matrix.
- `docs/superpowers/performance/webview-streaming/rollout.md`: flag ownership, fallback, diagnostics, and one-release removal criteria.

---

## Cross-Task Interfaces

The following names and wire shapes are fixed for the plan. Later tasks must extend them without renaming fields.

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDeliveryMode {
    Legacy,
    Batched,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamingPerformanceFlags {
    pub desktop_acp_event_batching: bool,
    pub incremental_live_transcript: bool,
    pub deferred_streaming_rich_content: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopDeliveryCapabilities {
    pub mode: DesktopDeliveryMode,
    pub flags: StreamingPerformanceFlags,
    pub perf_replay_available: bool,
    pub failure_event: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopAcpEventBatch {
    pub batch_id: u64,
    pub events: Vec<EventEnvelope>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopConnectionSeqRange {
    pub connection_id: String,
    pub first_seq: u64,
    pub last_seq: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopDeliveryFailure {
    pub generation: u64,
    pub reason: &'static str,
    pub affected: Vec<DesktopConnectionSeqRange>,
}
```

```ts
export type DesktopDeliveryMode = "legacy" | "batched"

export interface StreamingPerformanceFlags {
  desktop_acp_event_batching: boolean
  incremental_live_transcript: boolean
  deferred_streaming_rich_content: boolean
}

export interface DesktopDeliveryCapabilities {
  mode: DesktopDeliveryMode
  flags: StreamingPerformanceFlags
  perf_replay_available: boolean
  failure_event: "acp://delivery-failed"
}

export interface DesktopAcpEventBatch {
  batch_id: number
  events: EventEnvelope[]
}

export interface DesktopDeliveryFailure {
  generation: number
  reason: "batch_emit_failed" | "batch_task_stopped"
  affected: Array<{
    connection_id: string
    first_seq: number
    last_seq: number
  }>
}
```

The ingestor-to-provider boundary is one immutable frame:

```ts
export interface AcceptedConnectionFrame {
  contextKey: string
  connectionId: string
  deliveryIds: readonly number[]
  applyEvents: readonly EventEnvelope[]
  rawEvents: readonly EventEnvelope[]
  highestSeq: number
}

export interface AcceptedEventFrame {
  deliveryIds: readonly number[]
  connections: readonly AcceptedConnectionFrame[]
  rawEventsInDeliveryOrder: readonly EventEnvelope[]
}

export interface SequenceGap {
  contextKey: string
  connectionId: string
  expectedSeq: number
  receivedSeq: number
}
```

The P2 live projection boundary is independent of React and persistence:

```ts
export type LiveTranscriptSegment =
  | { id: string; type: "text"; text: string }
  | { id: string; type: "thinking"; text: string }
  | { id: string; type: "tool"; toolCallId: string }
  | { id: string; type: "plan"; entries: PlanEntryInfo[] }
  | { id: string; type: "generated-image"; toolCallId: string }

export interface LiveTranscriptSnapshot {
  conversationId: number
  connectionId: string
  messageId: string
  startedAt: number
  status: "streaming" | "completing"
  segmentIds: readonly string[]
  segments: ReadonlyMap<string, LiveTranscriptSegment>
  tools: ReadonlyMap<string, ToolCallInfo>
  lastAppliedSeq: number
}

export interface LiveTranscriptFrameSink {
  rebuild(canonical: LiveMessage, lastAppliedSeq: number): void
  publish(frame: AcceptedConnectionFrame, canonical: LiveMessage): void
  markCompleting(messageId: string): void
  clear(messageId: string): void
}
```

The P3 Markdown document is append-only until completion:

```ts
export interface SealedMarkdownBlock {
  id: string
  markdown: string
}

export interface MarkdownLineScanner {
  pendingLine: string
  fence: null | {
    marker: "`" | "~"
    length: number
    language: string
    openingOffset: number
    bodyOffset: number
  }
  safeBoundarySeen: boolean
  safeBoundaryOffset: number
  safeBoundaryKind: "blank" | "closed_fence" | null
  closedFenceBoundaryOffset: number
  scannedLength: number
}

export interface IncrementalStreamBlocks {
  segmentId: string
  sealed: readonly SealedMarkdownBlock[]
  tail: string
  sourceLength: number
  nextBlockIndex: number
  scanner: MarkdownLineScanner
  splitBlocks(markdown: string): string[]
  valid: boolean
}

export function appendStreamingMarkdown(
  document: IncrementalStreamBlocks,
  delta: string
): IncrementalStreamBlocks

export function sealStreamingMarkdownBoundary(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks

export function completeStreamingMarkdown(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks
```

---

## P0: Measurement And Baseline

### Task 1: Extend Backend Metrics And Add Desktop Snapshots

**Files:**
- Modify: `src-tauri/src/acp/internal_bus.rs:80-177`
- Create: `src-tauri/src/acp/streaming_performance.rs`
- Modify: `src-tauri/src/acp/mod.rs:1-40`
- Modify: `src-tauri/src/web/event_bridge.rs:327-402`
- Modify: `src-tauri/src/web/handlers/event_metrics.rs:14-22`
- Modify: `src-tauri/src/commands/acp.rs:6087-6135`
- Modify: `src-tauri/src/lib.rs:863-1216`
- Modify: `src/lib/types.ts:1366-1370`
- Modify: `src/lib/api.ts`

**Interfaces:**
- Produces: `EventBusMetricsSnapshot` with stable desktop fields used unchanged by P1 reports.
- Produces: `acp_get_event_metrics() -> EventBusMetricsSnapshot` for local Tauri diagnostics.
- Produces: `StreamingPerformanceFlags::normalized()` and `DesktopDeliveryCapabilities` types; Task 5 supplies the live mode owner.
- Preserves: `/api/debug/event_metrics` authentication and existing event-bus field names.

- [ ] **Step 1: Write failing metric-snapshot and flag-normalization tests**

Extend `internal_bus.rs` tests and add tests in `streaming_performance.rs`:

```rust
#[test]
fn metrics_snapshot_includes_desktop_delivery_counters() {
    let metrics = EventBusMetrics::default();
    metrics.desktop_raw_envelope_count.store(9, Ordering::Relaxed);
    metrics.desktop_raw_bytes.store(4_096, Ordering::Relaxed);
    metrics.desktop_emit_failure_count.store(2, Ordering::Relaxed);
    metrics.desktop_batch_max_events.store(17, Ordering::Relaxed);

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.desktop_raw_envelope_count, 9);
    assert_eq!(snapshot.desktop_raw_bytes, 4_096);
    assert_eq!(snapshot.desktop_emit_failure_count, 2);
    assert_eq!(snapshot.desktop_batch_max_events, 17);
}

#[test]
fn invalid_flag_combinations_normalize_downward() {
    let flags = StreamingPerformanceFlags {
        desktop_acp_event_batching: false,
        incremental_live_transcript: true,
        deferred_streaming_rich_content: true,
    }
    .normalized();

    assert_eq!(
        flags,
        StreamingPerformanceFlags {
            desktop_acp_event_batching: false,
            incremental_live_transcript: false,
            deferred_streaming_rich_content: false,
        }
    );
}

#[test]
fn serialization_failure_is_counted_without_payload_logging() {
    struct Fails;
    impl Serialize for Fails {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom("injected"))
        }
    }
    let metrics = EventBusMetrics::default();
    assert_eq!(estimate_desktop_payload_bytes(&Fails, &metrics), 0);
    assert_eq!(
        metrics
            .desktop_serialization_failure_count
            .load(Ordering::Relaxed),
        1
    );
}
```

- [ ] **Step 2: Run the focused Rust tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils metrics_snapshot_includes_desktop_delivery_counters
cargo test --features test-utils invalid_flag_combinations_normalize_downward
```

Expected: compilation fails because the desktop counters and streaming-performance module do not exist.

- [ ] **Step 3: Add payload-free atomic counters and a stable snapshot**

Add these fields to both `EventBusMetrics` and `EventBusMetricsSnapshot`, load them in `snapshot()`, and use `fetch_max` for maxima:

```rust
pub desktop_raw_envelope_count: AtomicU64,
pub desktop_raw_bytes: AtomicU64,
pub desktop_emit_attempt_count: AtomicU64,
pub desktop_serialization_failure_count: AtomicU64,
pub desktop_emit_failure_count: AtomicU64,
pub desktop_legacy_emit_count: AtomicU64,
pub desktop_batch_count: AtomicU64,
pub desktop_batch_event_count: AtomicU64,
pub desktop_batch_bytes: AtomicU64,
pub desktop_batch_max_events: AtomicU64,
pub desktop_batch_max_bytes: AtomicU64,
pub desktop_batch_latency_total_us: AtomicU64,
pub desktop_batch_latency_max_us: AtomicU64,
pub desktop_queue_full_count: AtomicU64,
pub desktop_startup_fallback_count: AtomicU64,
pub desktop_runtime_failure_count: AtomicU64,
```

Add explicit helpers so later code never manipulates several counters inconsistently:

```rust
impl EventBusMetrics {
    pub fn record_desktop_offer(&self, estimated_bytes: usize) {
        self.desktop_raw_envelope_count.fetch_add(1, Ordering::Relaxed);
        self.desktop_raw_bytes
            .fetch_add(estimated_bytes as u64, Ordering::Relaxed);
    }

    pub fn record_desktop_batch(&self, events: usize, bytes: usize, latency: Duration) {
        self.desktop_batch_count.fetch_add(1, Ordering::Relaxed);
        self.desktop_batch_event_count
            .fetch_add(events as u64, Ordering::Relaxed);
        self.desktop_batch_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.desktop_batch_max_events
            .fetch_max(events as u64, Ordering::Relaxed);
        self.desktop_batch_max_bytes
            .fetch_max(bytes as u64, Ordering::Relaxed);
        let latency_us = latency.as_micros().min(u64::MAX as u128) as u64;
        self.desktop_batch_latency_total_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.desktop_batch_latency_max_us
            .fetch_max(latency_us, Ordering::Relaxed);
    }
}
```

- [ ] **Step 4: Add flag/capability types without enabling batching**

Implement pure normalization and environment parsing in `streaming_performance.rs`; P0 defaults remain legacy:

```rust
impl StreamingPerformanceFlags {
    pub fn legacy() -> Self {
        Self {
            desktop_acp_event_batching: false,
            incremental_live_transcript: false,
            deferred_streaming_rich_content: false,
        }
    }

    pub fn normalized(mut self) -> Self {
        if !self.desktop_acp_event_batching {
            self.incremental_live_transcript = false;
        }
        if !self.incremental_live_transcript {
            self.deferred_streaming_rich_content = false;
        }
        self
    }
}
```

Do not read message payloads or expose environment values in the snapshot.

- [ ] **Step 5: Instrument the current single-event Tauri leg**

In `emit_with_state_gated`, serialize only to estimate bytes, record the offer after the state lock is released, then count the actual legacy emit result:

```rust
fn estimate_desktop_payload_bytes<T: Serialize>(
    payload: &T,
    metrics: &EventBusMetrics,
) -> usize {
  match serde_json::to_vec(payload) {
    Ok(value) => value.len(),
    Err(error) => {
        tracing::error!("[ACP] desktop envelope size serialization failed: {error}");
        metrics
            .desktop_serialization_failure_count
            .fetch_add(1, Ordering::Relaxed);
        0
    }
  }
}

let estimated_bytes = emitter
    .metrics()
    .map(|metrics| estimate_desktop_payload_bytes(envelope_arc.as_ref(), &metrics))
    .unwrap_or_default();

if let Some(metrics) = emitter.metrics() {
    metrics.record_desktop_offer(estimated_bytes);
    metrics
        .desktop_emit_attempt_count
        .fetch_add(1, Ordering::Relaxed);
}

match app.emit("acp://event", envelope_arc.as_ref()) {
    Ok(()) => {
        if let Some(metrics) = emitter.metrics() {
            metrics
                .desktop_legacy_emit_count
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    Err(error) => {
        tracing::error!("[ACP] desktop legacy emit failed: {error}");
        if let Some(metrics) = emitter.metrics() {
            metrics
                .desktop_emit_failure_count
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

Guard the offer counters inside the Tauri match arm so `WebOnly` and `Noop` do not report desktop traffic.
Add a test that emits through `WebOnly` and asserts every `desktop_*` snapshot field remains zero.

- [ ] **Step 6: Add desktop metrics API and TypeScript mirror**

Add a pure command core and a Tauri wrapper:

```rust
pub(crate) fn acp_get_event_metrics_core(
    metrics: &EventBusMetrics,
) -> EventBusMetricsSnapshot {
    metrics.snapshot()
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn acp_get_event_metrics(
    bus: tauri::State<'_, Arc<InternalEventBus>>,
) -> EventBusMetricsSnapshot {
    acp_get_event_metrics_core(bus.metrics())
}
```

Register `acp_commands::acp_get_event_metrics` in `lib.rs`, mirror every field as `number` in `src/lib/types.ts`, and add:

```ts
export function acpGetEventMetrics(): Promise<EventBusMetricsSnapshot> {
  return getTransport().call("acp_get_event_metrics")
}
```

- [ ] **Step 7: Run focused and compatibility tests**

```powershell
cd src-tauri
cargo test --features test-utils metrics_snapshot
cargo test --features test-utils internal_bus
cargo test --no-default-features --bin codeg-server --lib event_metrics
```

Expected: all selected tests pass; the server handler serializes the expanded snapshot and desktop-only counters remain zero in WebOnly tests.

- [ ] **Step 8: Commit P0 backend observability**

```powershell
git add src-tauri/src/acp/internal_bus.rs src-tauri/src/acp/streaming_performance.rs src-tauri/src/acp/mod.rs src-tauri/src/web/event_bridge.rs src-tauri/src/web/handlers/event_metrics.rs src-tauri/src/commands/acp.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts
git commit -m "feat(perf): measure desktop ACP delivery"
```

---

### Task 2: Add Deterministic Fixture Generation And Test-Utils Replay

**Files:**
- Create: `src-tauri/src/acp/perf_fixture.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`

**Interfaces:**
- Produces: `build_perf_fixture(PerfFixtureId::GrokRichV1, seed) -> PerfFixture`.
- Produces: fixed profiles `eps_100`, `eps_500`, and `eps_1000` as schedule offsets; tests never sleep.
- Produces: test-utils-only `acp_replay_streaming_perf_fixture(connection_id, request)`.
- Consumes: `emit_with_state` and `ConnectionManager::get_state_and_emitter`; this guarantees normal apply, sequence, ring-buffer, internal-bus, and desktop delivery behavior.

- [ ] **Step 1: Write failing deterministic fixture tests**

Add the module tests first:

```rust
#[test]
fn grok_rich_v1_has_fixed_contract_and_checksum() {
    let fixture = build_perf_fixture(PerfFixtureId::GrokRichV1, 0xC0DE);
    assert_eq!(fixture.version, "grok-rich-v1");
    assert_eq!(fixture.final_text.chars().count(), 30_000);
    assert_eq!(fixture.events.len(), 1_223);
    assert_eq!(fixture.tool_call_count, 51);
    assert_eq!(
        fixture.final_text_sha256,
        "65380735c9a752758c7bace17cc722d86400480a0ae1dff62759f37eafa4b039"
    );
    assert!(fixture.final_text.contains("中文流式输出"));
    assert!(fixture.final_text.contains("```mermaid"));
    assert!(fixture.code_fence_is_split_across_chunks);
}

#[test]
fn schedules_are_exact_and_do_not_sleep() {
    let count = 1_223;
    assert_eq!(PerfRateProfile::Eps100.offsets(count)[1], Duration::from_millis(10));
    assert_eq!(PerfRateProfile::Eps500.offsets(count)[1], Duration::from_millis(2));
    assert_eq!(PerfRateProfile::Eps1000.offsets(count)[1], Duration::from_millis(1));
    assert_eq!(PerfRateProfile::Eps1000.offsets(count).len(), count);
}

#[test]
fn seed_changes_synthetic_tool_payloads_but_not_the_text_contract() {
    let first = build_perf_fixture(PerfFixtureId::GrokRichV1, 1);
    let second = build_perf_fixture(PerfFixtureId::GrokRichV1, 2);
    assert_ne!(serde_json::to_value(&first.events).unwrap(), serde_json::to_value(&second.events).unwrap());
    assert_eq!(first.final_text_sha256, second.final_text_sha256);
    assert_eq!(first.events.len(), second.events.len());
}
```

- [ ] **Step 2: Run fixture tests and confirm RED**

```powershell
cd src-tauri
cargo test --features test-utils grok_rich_v1_has_fixed_contract_and_checksum
cargo test --features test-utils schedules_are_exact_and_do_not_sleep
```

Expected: compilation fails because `perf_fixture` and its types are absent.

- [ ] **Step 3: Generate the exact 30K source and 1,000 content chunks**

Use this exact prefix/filler contract so the checksum is stable across platforms; normalize only with Rust `\n` literals:

```rust
const TARGET_TEXT_CHARS: usize = 30_000;
const CONTENT_CHUNKS: usize = 1_000;
const TOOL_CALLS: usize = 51;

const PREFIX: &str = concat!(
    "# Streaming fixture\n\n",
    "English prose before CJK fast.\n",
    "中文流式输出用于验证 WebView2 在快速回复时不会停顿后跳跃。\n\n",
    "```rust\n",
    "fn main() {\n    for i in 0..2048 {\n",
    "        println!(\"frame {i}\");\n    }\n}\n",
    "```\n\n",
    "| index | value | 状态 |\n",
    "| ---: | :--- | :--- |\n",
    "| 1 | alpha | 运行中 |\n",
    "| 2 | beta | 完成 |\n\n",
    "Inline math $a^2+b^2=c^2$ and display math:\n",
    "$$\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n$$\n\n",
    "```mermaid\nsequenceDiagram\n",
    "    participant A as Agent\n",
    "    participant W as WebView\n",
    "    A->>W: event batch\n",
    "    W-->>A: paint\n```\n\n",
);

const FILLER: &str =
    "Fast Grok output keeps prose, 中文字符, `inline code`, and table-like | cells | moving.\n";

fn fixture_text() -> String {
    let mut text = PREFIX.to_owned();
    while text.chars().count() < TARGET_TEXT_CHARS {
        text.push_str(FILLER);
    }
    text.chars().take(TARGET_TEXT_CHARS).collect()
}

fn content_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(TARGET_TEXT_CHARS / CONTENT_CHUNKS)
        .map(|chunk| chunk.iter().collect())
        .collect()
}
```

The first Rust fence starts at character 88, so the 30-character chunk boundary splits the opening delimiter after two backticks. Assert that fact rather than relying on random chunking.

- [ ] **Step 4: Interleave the exact structural event contract**

Build events in this order:

```rust
let mut events = vec![AcpEvent::StatusChanged {
    status: ConnectionStatus::Prompting,
}];
let mut tool_index = 0usize;

for (index, text) in content_chunks(&final_text).into_iter().enumerate() {
    events.push(AcpEvent::ContentDelta { text });

    if index % 20 == 19 {
        events.push(AcpEvent::Thinking {
            text: format!("thinking-{index}\n"),
        });
    }
    if index % 19 == 18 && tool_index < TOOL_CALLS {
        let id = format!("perf-tool-{tool_index:02}");
        events.push(tool_call(&id, tool_index, seed));
        events.push(tool_append(&id, format!("chunk-{tool_index}\n")));
        events.push(tool_complete(&id, format!("done-{tool_index}\n")));
        tool_index += 1;
    }
    if index % 100 == 99 {
        events.push(AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: format!("phase-{}", index / 100),
                priority: "medium".into(),
                status: "in_progress".into(),
            }],
        });
    }
    if index == 249 {
        events.push(permission_request());
        events.push(AcpEvent::PermissionResolved {
            request_id: "perf-permission".into(),
        });
    }
    if index == 499 {
        events.push(AcpEvent::QuestionRequest {
            question_id: "perf-question".into(),
            questions: vec![],
        });
        events.push(AcpEvent::QuestionResolved {
            question_id: "perf-question".into(),
        });
    }
    if index % 250 == 249 {
        events.push(AcpEvent::UsageUpdate {
            used: (index + 1) as u64,
            size: 200_000,
        });
    }
}

events.push(AcpEvent::TurnComplete {
    session_id: "perf-grok-rich-v1".into(),
    stop_reason: "end_turn".into(),
    agent_type: "grok".into(),
});
assert_eq!(events.len(), 1_223);
```

Use these exact helpers so tool payloads vary by seed while count/text remain fixed, and completion preserves append semantics:

```rust
fn tool_call(id: &str, index: usize, seed: u64) -> AcpEvent {
    AcpEvent::ToolCall {
        tool_call_id: id.to_owned(),
        title: format!("perf command {index}"),
        kind: "execute".into(),
        status: "in_progress".into(),
        content: None,
        raw_input: Some(
            serde_json::json!({
                "command": format!("fixture-step-{index}"),
                "nonce": seed ^ index as u64,
            })
            .to_string(),
        ),
        raw_output: None,
        locations: None,
        meta: None,
        images: None,
    }
}

fn tool_append(id: &str, output: String) -> AcpEvent {
    AcpEvent::ToolCallUpdate {
        tool_call_id: id.to_owned(),
        title: None,
        status: None,
        content: None,
        raw_input: None,
        raw_output: Some(output),
        raw_output_append: Some(true),
        locations: None,
        meta: None,
        images: None,
    }
}

fn tool_complete(id: &str, output: String) -> AcpEvent {
    AcpEvent::ToolCallUpdate {
        tool_call_id: id.to_owned(),
        title: None,
        status: Some("completed".into()),
        content: None,
        raw_input: None,
        raw_output: Some(output),
        raw_output_append: Some(true),
        locations: None,
        meta: None,
        images: None,
    }
}

fn permission_request() -> AcpEvent {
    AcpEvent::PermissionRequest {
        request_id: "perf-permission".into(),
        tool_call: serde_json::json!({
            "toolCallId": "perf-tool-12",
            "title": "Synthetic permission",
        }),
        options: vec![],
    }
}
```

- [ ] **Step 5: Add canonical-state and sequence tests through `emit_with_state`**

Use `ConnectionManager::insert_test_connection` and `EventEmitter::test_web_only`, apply all events except completion, then assert snapshot text/tool state before applying `TurnComplete`:

```rust
#[tokio::test]
async fn replay_uses_normal_state_sequence_and_ring_path() {
    let manager = ConnectionManager::new();
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let emitter = EventEmitter::test_web_only(broadcaster);
    manager
        .insert_test_connection("perf-c1", AgentType::Grok, None, emitter.clone())
        .await;
    let state = manager.get_state("perf-c1").await.expect("state");
    let fixture = build_perf_fixture(PerfFixtureId::GrokRichV1, 0xC0DE);

    for event in fixture.events[..fixture.events.len() - 1].iter().cloned() {
        emit_with_state(&state, &emitter, event).await;
    }
    let snapshot = state.read().await.to_snapshot();
    assert_eq!(snapshot.event_seq, 1_222);
    assert_eq!(snapshot.active_tool_calls.len(), 51);
    assert_eq!(joined_snapshot_text(&snapshot).chars().count(), 30_000);
    let last_tool = snapshot
        .active_tool_calls
        .iter()
        .find(|tool| tool.id == "perf-tool-50")
        .expect("final tool");
    assert!(matches!(
        &last_tool.output,
        Some(ToolCallOutput::Text { content })
            if content == "chunk-50\ndone-50\n"
    ));

    emit_with_state(&state, &emitter, fixture.events.last().unwrap().clone()).await;
    let completed = state.read().await;
    assert_eq!(completed.event_seq, 1_223);
    assert!(completed.live_message.is_none());
    assert!(completed.active_tool_calls.is_empty());
}

fn joined_snapshot_text(snapshot: &LiveSessionSnapshot) -> String {
    snapshot
        .live_message
        .as_ref()
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    LiveContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 6: Add the test-utils-only replay command and frontend API**

Define request/result types with `serde(rename_all = "snake_case")`. The driver sleeps against absolute offsets so late ticks do not reorder events:

```rust
#[cfg(all(feature = "test-utils", feature = "tauri-runtime"))]
#[tauri::command]
pub async fn acp_replay_streaming_perf_fixture(
    connection_id: String,
    request: PerfReplayRequest,
    manager: tauri::State<'_, ConnectionManager>,
) -> Result<PerfReplayResult, AcpError> {
    let (state, emitter) = manager
        .get_state_and_emitter(&connection_id)
        .await
        .ok_or_else(|| AcpError::ConnectionNotFound(connection_id.clone()))?;
    replay_perf_fixture(&state, &emitter, request).await
}
```

Register it with an outer attribute inside `tauri::generate_handler!` so release registration is physically absent:

```rust
#[cfg(all(feature = "test-utils", feature = "tauri-runtime"))]
acp_commands::acp_replay_streaming_perf_fixture,
```

Mirror `PerfReplayRequest` and `PerfReplayResult` and add:

```ts
export function acpReplayStreamingPerfFixture(
  connectionId: string,
  request: PerfReplayRequest
): Promise<PerfReplayResult> {
  return getTransport().call("acp_replay_streaming_perf_fixture", {
    connectionId,
    request,
  })
}
```

- [ ] **Step 7: Run fixture, release-registration, and server checks**

```powershell
cd src-tauri
cargo test --features test-utils perf_fixture
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: fixture tests pass with 1,223 events and the fixed checksum; default desktop and server builds compile without the replay command or fixture module in their release command surface.

- [ ] **Step 8: Commit deterministic replay**

```powershell
git add src-tauri/src/acp/perf_fixture.rs src-tauri/src/acp/mod.rs src-tauri/src/commands/acp.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts
git commit -m "test(perf): add deterministic ACP replay"
```

---

### Task 3: Add Frontend Timing Recorder, Report Schema, And Local Harness

**Files:**
- Create: `src/lib/perf/streaming-perf-recorder.ts`
- Create: `src/lib/perf/streaming-perf-recorder.test.ts`
- Create: `src/lib/perf/streaming-perf-report.ts`
- Create: `src/lib/perf/streaming-perf-report.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/stores/conversation-runtime-store.ts`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/chat/message-input.test.tsx`

**Interfaces:**
- Produces: singleton `streamingPerfRecorder` whose inactive hot path is one boolean branch.
- Produces: `window.__codegStreamingPerf.run({ rateProfile, seed, download })` only when the test-utils replay command is available.
- Consumes: P0 legacy envelopes as one-event delivery IDs; Task 6 later supplies real batch IDs without changing the recorder API.
- Produces: schema version 1 JSON reports with no content fields.

- [ ] **Step 1: Write failing percentile and acceptance tests**

```ts
describe("summarizeSamples", () => {
  it("uses nearest-rank percentiles", () => {
    expect(summarizeSamples([1, 2, 3, 4, 100])).toEqual({
      count: 5,
      p50: 3,
      p95: 100,
      max: 100,
    })
  })
})

it("evaluates the exact Windows contract", () => {
  const result = evaluateStreamingPerf({
    batchToPaintMs: [20, 40, 80],
    inputToPaintMs: [10, 20, 45],
    longTasksMs: [120, 199],
    visualUpdatesPerSecond: 35,
    integrityOk: true,
  })
  expect(result).toEqual({
    batchToPaint: true,
    inputToPaint: true,
    longTask: true,
    visualCadence: true,
    eventIntegrity: true,
    passed: true,
  })
})
```

- [ ] **Step 2: Run report tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/perf/streaming-perf-report.test.ts
```

Expected: the module import fails because the report implementation is absent.

- [ ] **Step 3: Implement the content-free report schema and exact thresholds**

```ts
export interface StreamingPerfReport {
  schemaVersion: 1
  runId: string
  fixture: {
    id: "grok_rich_v1"
    version: "grok-rich-v1"
    seed: number
    rateProfile: "eps_100" | "eps_500" | "eps_1000"
    expectedEvents: number
    expectedTextChars: 30000
    expectedTextSha256: string
  }
  environment: {
    platform: string
    userAgent: string
    webviewVersion: string | null
    buildMode: "development" | "production"
    hardwareAcceleration: "enabled" | "disabled" | "unknown"
    deliveryMode: "legacy" | "batched"
    flags: StreamingPerformanceFlags
  }
  delivery: EventBusMetricsSnapshot
  pipelineCounts: {
    deliveryCallbacks: number
    ingestorFrames: number
    connectionMapPublications: number
    connectionTransactions: number
    livePublications: number
    reactCommits: number
    paints: number
  }
  timings: {
    receiptToTransaction: MetricSummary
    transactionToLivePublication: MetricSummary
    batchToCommit: MetricSummary
    batchToPaint: MetricSummary
    inputToPaint: MetricSummary
    longTasks: MetricSummary
    frameGaps: MetricSummary
    eventLoopDrift: MetricSummary
  }
  renders: Record<
    | "conversationPanel"
    | "historicalThread"
    | "historicalRow"
    | "liveRow"
    | "markdownBlock"
    | "toolCard",
    number
  >
  integrity: {
    expectedEvents: number
    appliedEvents: number
    firstSeq: number
    lastSeq: number
    duplicateCount: number
    gapCount: number
    finalTextSha256: string
    ok: boolean
  }
  cadence: { queuedDurationMs: number; paintCount: number; updatesPerSecond: number }
  resources: {
    markdownCacheEntries: number | null
    markdownCacheBytes: number | null
    highlightCacheEntries: number | null
    highlightCacheBytes: number | null
    liveConversations: number | null
    liveSegments: number | null
    liveTools: number | null
    usedHeapBytes: number | null
    heapMeasurement: "supported" | "unsupported"
  }
  acceptance: StreamingPerfAcceptance
}
```

Use nearest-rank `Math.ceil(percentile * count) - 1`, and encode the four numeric targets exactly as constants.

- [ ] **Step 4: Write failing recorder lifecycle and fallback tests**

Use injected clock/schedulers so tests do not depend on jsdom frame timing:

```ts
it("matches a delivery to its commit and next paint", () => {
  const clock = manualClock()
  const recorder = new StreamingPerfRecorder({ clock, raf: immediateRaf })
  recorder.start(runMetadata)
  clock.set(0)
  recorder.markBatchReceived(7, 4)
  clock.set(4)
  recorder.markTransactionComplete([7])
  clock.set(7)
  recorder.markLivePublication([7])
  clock.set(9)
  const committed = recorder.markReactCommit()
  clock.set(12)
  recorder.markNextPaint(committed)
  expect(recorder.snapshot().batchToPaintMs).toEqual([12])
})

it("uses frame gaps and timer drift when longtask is unsupported", () => {
  const recorder = new StreamingPerfRecorder({
    supportedEntryTypes: [],
    clock: monotonicClock(),
    raf: controlledRaf,
    setTimer: controlledTimer,
  })
  recorder.start(runMetadata)
  advanceRafBy(240)
  advanceTimerBy(230)
  expect(recorder.snapshot().frameGapsMs).toContain(240)
  expect(recorder.snapshot().eventLoopDriftMs).toContain(180)
})

it("does no allocation when inactive", () => {
  const recorder = new StreamingPerfRecorder()
  recorder.markBatchReceived(1, 1)
  expect(recorder.isActive()).toBe(false)
  expect(recorder.debugAllocationCount()).toBe(0)
})
```

Define the test clock/schedulers locally so the assertions are deterministic:

```ts
function manualClock() {
  let value = 0
  return {
    now: () => value,
    set: (next: number) => {
      value = next
    },
  }
}

const immediateRaf = (callback: FrameRequestCallback): number => {
  callback(0)
  return 1
}

let pendingRaf: FrameRequestCallback | null = null
const controlledRaf = (callback: FrameRequestCallback): number => {
  pendingRaf = callback
  return 1
}

function advanceRafBy(timestamp: number): void {
  const callback = pendingRaf
  pendingRaf = null
  callback?.(timestamp)
}
```

Use the same capture pattern for `controlledTimer`; its `advanceTimerBy` sets the fake clock then invokes the captured callback once.

- [ ] **Step 5: Implement stage marks, observers, render counters, and quiet drain**

Expose only explicit methods, never `performance.mark` names containing connection IDs or content:

```ts
export type PerfRenderKind =
  | "conversationPanel"
  | "historicalThread"
  | "historicalRow"
  | "liveRow"
  | "markdownBlock"
  | "toolCard"

export class StreamingPerfRecorder {
  private active: ActiveRun | null = null

  markBatchReceived(deliveryId: number, eventCount: number): void {
    const active = this.active
    if (!active) return
    active.deliveries.set(deliveryId, {
      receivedAt: this.clock.now(),
      eventCount,
    })
  }

  countRender(kind: PerfRenderKind): void {
    const active = this.active
    if (!active) return
    active.renderCounts[kind] += 1
  }

  markTransactionComplete(deliveryIds: readonly number[]): void
  markConnectionFrameCommitted(
    deliveryIds: readonly number[],
    changedConnections: number,
    mapPublished: boolean
  ): void
  markLivePublication(deliveryIds: readonly number[]): void
  markReactCommit(): readonly number[]
  markNextPaint(deliveryIds: readonly number[]): void

  async waitForQuiet(quietMs = 250, timeoutMs = 5_000): Promise<void> {
    const active = this.active
    if (!active) return
    const startedAt = this.clock.now()
    await new Promise<void>((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | null = null
      const finish = (error?: Error) => {
        if (timer !== null) this.clearTimer(timer)
        if (error) reject(error)
        else resolve()
      }
      const poll = () => {
        if (this.active !== active) {
          finish()
          return
        }
        const now = this.clock.now()
        if (now - startedAt >= timeoutMs) {
          finish(new Error(`streaming perf did not become quiet in ${timeoutMs}ms`))
          return
        }
        if (now - active.lastActivityAt >= quietMs) {
          finish()
          return
        }
        timer = this.setTimer(poll, 25)
      }
      timer = this.setTimer(poll, 25)
    })
  }
}

export const streamingPerfRecorder = new StreamingPerfRecorder()
```

Every receipt, transaction, publication, commit, and paint method updates `active.lastActivityAt`. `markLivePublication` adds IDs to one pending-paint set; `markReactCommit` timestamps and atomically drains that set, returning the exact IDs for the following RAF. This lets several callbacks coalesced into one React commit receive the same commit/paint timestamp without putting recorder state in React props. The injected `setTimer`/`clearTimer` default to `window.setTimeout`/`window.clearTimeout`. Observe `longtask` only when `PerformanceObserver.supportedEntryTypes` contains it; otherwise run the RAF-gap and 50 ms timer-drift loops. Increment `pipelineCounts` at the corresponding callback/frame/map-publication/connection/publication/commit/paint boundaries; resource fields are `null` until their owning P3/P4 stores exist.

- [ ] **Step 6: Instrument the five pipeline stages and render sites**

Add one guarded call at each point:

1. The desktop listener owns a process-local monotonic delivery counter and calls `markBatchReceived(deliveryId, 1)` in P0; do not use per-connection `envelope.seq` as a globally unique ID.
2. After all reducer effects for that envelope, call `markTransactionComplete([deliveryId])`.
3. In the runtime `setLiveMessage` action after the Zustand state update, call `markLivePublication([deliveryId])`; pass the delivery ID through an optional internal argument.
4. In `MessageListView`, call `const deliveryIds = markReactCommit()` from `useLayoutEffect`.
5. When `deliveryIds.length > 0`, schedule exactly one `requestAnimationFrame` from that layout effect and call `markNextPaint(deliveryIds)`.

Add `countRender` to the conversation panel, `MessageListView` historical-thread shell, `HistoricalMessageGroup`, current streaming row, `MessageResponse` block wrapper, and tool card. Do not put recorder state in React props.

- [ ] **Step 7: Add the non-mutating input probe and prove editor content is unchanged**

The probe uses a dedicated `MessageChannel`, not an `InputEvent` against Tiptap:

```ts
export function probeInputToPaint(recorder: StreamingPerfRecorder): void {
  const probeId = recorder.markInputQueued()
  const channel = new MessageChannel()
  channel.port1.onmessage = () => {
    requestAnimationFrame(() => recorder.markInputPaint(probeId))
    channel.port1.close()
    channel.port2.close()
  }
  channel.port2.postMessage(null)
}
```

While a replay is active, `MessageInput` schedules this probe at 100 ms intervals and cancels it on stop/unmount. Add a test that seeds editor text, runs all probe timers, and asserts the editor text and `onChange` call count are unchanged.

- [ ] **Step 8: Install the explicit local run harness**

Declare the global only in `streaming-perf-recorder.ts`:

```ts
declare global {
  interface Window {
    __codegStreamingPerf?: {
      run(options: {
        rateProfile: "eps_100" | "eps_500" | "eps_1000"
        seed?: number
        download?: boolean
      }): Promise<StreamingPerfReport>
    }
  }
}
```

The provider installs it only after probing `acp_replay_streaming_perf_fixture`; `run` reads the active connection from `storeRef.current.activeKey`, snapshots metrics before and after, starts the recorder, invokes replay, waits for quiet plus two RAFs, verifies event count/checksum, optionally downloads JSON with a Blob URL, and removes no user data. Populate environment metadata with `navigator.platform`, `navigator.userAgent`, a WebView version extracted from `Edg/<version>` or `Version/<version>`, `process.env.NODE_ENV`, normalized startup capabilities, and `getSystemRenderingSettings()` (`disable_hardware_acceleration` inverted). Delete the global on provider unmount.

- [ ] **Step 9: Run focused frontend tests and type/lint checks**

```powershell
pnpm exec vitest run src/lib/perf/streaming-perf-report.test.ts src/lib/perf/streaming-perf-recorder.test.ts src/components/chat/message-input.test.tsx src/components/message/message-list-view.test.tsx
pnpm eslint src/lib/perf src/contexts/acp-connections-context.tsx src/stores/conversation-runtime-store.ts src/components/message/message-list-view.tsx src/components/chat/message-input.tsx
```

Expected: all focused tests pass; ESLint reports no hook, unused, or formatting errors.

- [ ] **Step 10: Commit the P0 recorder**

```powershell
git add src/lib/perf/streaming-perf-recorder.ts src/lib/perf/streaming-perf-recorder.test.ts src/lib/perf/streaming-perf-report.ts src/lib/perf/streaming-perf-report.test.ts src/contexts/acp-connections-context.tsx src/stores/conversation-runtime-store.ts src/components/message/message-list-view.tsx src/components/chat/message-input.tsx src/components/chat/message-input.test.tsx
git commit -m "feat(perf): record ACP delivery to paint"
```

---

### Task 4: Capture And Review The P0 Windows Baseline

**Files:**
- Create: `docs/superpowers/performance/webview-streaming/README.md`
- Create: `docs/superpowers/performance/webview-streaming/baseline-100eps.json`
- Create: `docs/superpowers/performance/webview-streaming/baseline-500eps.json`
- Create: `docs/superpowers/performance/webview-streaming/baseline-1000eps.json`
- Create: `docs/superpowers/performance/webview-streaming/comparison.md`

**Interfaces:**
- Consumes: P0 replay and `window.__codegStreamingPerf.run`.
- Produces: reviewed median-of-three baseline reports used by Tasks 8, 14, and 16.
- Gate: P0 must attribute time to receipt, transaction, live publication, commit, and paint before P1 begins.

- [ ] **Step 1: Run repository checks before measuring**

```powershell
pnpm exec vitest run src/lib/perf/streaming-perf-report.test.ts src/lib/perf/streaming-perf-recorder.test.ts src/contexts/acp-connections-context.test.tsx src/components/message/message-list-view.test.tsx
cd src-tauri
cargo test --features test-utils perf_fixture
cargo test --features test-utils metrics_snapshot
```

Expected: all selected tests pass. Do not capture a baseline from a tree with failing fixture or recorder tests.

- [ ] **Step 2: Record the reference-machine and build procedure**

Create `README.md` with the exact outputs of:

```powershell
Get-ComputerInfo | Select-Object WindowsProductName,WindowsVersion,OsBuildNumber,CsManufacturer,CsModel,CsProcessors,CsTotalPhysicalMemory
Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion,AdapterRAM
rustc -Vv
node --version
pnpm --version
```

Also record the commit SHA from `git rev-parse HEAD`, whether hardware acceleration is disabled in Settings, the WebView user agent from the generated report, and the rule that all comparisons run on this same named machine with foreground apps closed.

- [ ] **Step 3: Build a production-frontend WebView2 binary with test replay**

```powershell
pnpm exec tauri build --debug --features test-utils
```

Expected: the static production frontend builds and a debug desktop executable is produced under `src-tauri/target/debug`; the test-utils replay command is registered, while normal release builds remain unaffected.

- [ ] **Step 4: Capture three runs for each profile**

Launch the built app, open a disposable Grok conversation, open WebView devtools, and run each expression three times, waiting for the previous download to finish:

```js
await window.__codegStreamingPerf.run({ rateProfile: "eps_100", seed: 49374, download: true })
await window.__codegStreamingPerf.run({ rateProfile: "eps_500", seed: 49374, download: true })
await window.__codegStreamingPerf.run({ rateProfile: "eps_1000", seed: 49374, download: true })
```

Expected for every run: `integrity.ok === true`, `expectedEvents === appliedEvents === 1223`, no gaps/duplicates, and the final checksum equals the fixture checksum. A baseline is allowed to fail latency targets; integrity is not.

- [ ] **Step 5: Select median runs deterministically and save them**

For each profile, order the three reports by `timings.batchToPaint.p95`; choose the middle report and rename it to `baseline-<rate>eps.json`. Record all three P95 values in `comparison.md` so selecting a convenient outlier is impossible.

Run:

```powershell
pnpm exec prettier --write docs/superpowers/performance/webview-streaming/README.md docs/superpowers/performance/webview-streaming/comparison.md
```

Expected: Markdown is formatted; JSON remains the recorder's deterministic pretty-printed output.

- [ ] **Step 6: Attribute the pause and review the P0 exit gate**

In `comparison.md`, add a P0 table with receipt-to-transaction, transaction-to-publication, batch-to-commit, batch-to-paint, long-task max, frame-gap max, and render counts for all three profiles. State the largest measured stage for each profile using the report values, not an assumption.

P0 passes only when:

- three consecutive runs per profile complete with integrity;
- the selected report is the median by the documented rule;
- every timing stage has non-zero sample coverage;
- unsupported `longtask` capability is paired with frame-gap and drift samples;
- report objects pass an exact-key whitelist test and contain no `prompt`, `response`, `raw_input`, `raw_output`, `tool_call_id`, or fixture content values. Schema keys such as `expectedTextChars` and `finalTextSha256` are allowed because they contain counts/digests, not content.

- [ ] **Step 7: Force-stage only P0 evidence and commit**

`docs/superpowers` is ignored, so use exact forced paths:

```powershell
git add -f docs/superpowers/performance/webview-streaming/README.md docs/superpowers/performance/webview-streaming/baseline-100eps.json docs/superpowers/performance/webview-streaming/baseline-500eps.json docs/superpowers/performance/webview-streaming/baseline-1000eps.json docs/superpowers/performance/webview-streaming/comparison.md
git commit -m "docs(perf): record WebView streaming baseline"
```

---

## P1: Desktop Batching And Transactional Ingestion

### Task 5: Implement The Rust Desktop Event Batcher

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/acp/desktop_event_batcher.rs`
- Modify: `src-tauri/src/acp/streaming_performance.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/web/event_bridge.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 desktop metrics and the fixed batch/capability wire types.
- Produces: managed `Arc<DesktopAcpDelivery>` with `deliver`, `capabilities`, and `shutdown` methods.
- Emits: `acp://event-batch` and, on terminal runtime failure, one `acp://delivery-failed` signal.
- Preserves: raw `ConnectionEventStream` and `InternalEventBus` sends before desktop delivery.

- [ ] **Step 1: Write failing flush-policy tests against a fake sink**

```rust
#[derive(Default)]
struct RecordingSink {
    batches: Mutex<Vec<DesktopAcpEventBatch>>,
    failures: Mutex<Vec<DesktopDeliveryFailure>>,
    fail: AtomicBool,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl DesktopBatchSink for RecordingSink {
    async fn emit_batch(&self, batch: &DesktopAcpEventBatch) -> Result<(), String> {
        if self.block.load(Ordering::Acquire) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        if self.fail.load(Ordering::Relaxed) {
            return Err("injected emit failure".into());
        }
        self.batches.lock().unwrap().push(batch.clone());
        Ok(())
    }

    async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String> {
        self.failures.lock().unwrap().push(failure.clone());
        Ok(())
    }
}

struct TestEnvelope {
    envelope: Arc<EventEnvelope>,
    estimated_bytes: usize,
}

#[derive(Clone)]
struct BatcherHarness {
    batcher: Arc<DesktopAcpEventBatcher>,
    sink: Arc<RecordingSink>,
    metrics: Arc<EventBusMetrics>,
}

impl BatcherHarness {
    fn new() -> Self {
        Self::with_sink(RecordingSink::default())
    }

    fn with_blocked_sink() -> Self {
        let sink = RecordingSink::default();
        sink.block.store(true, Ordering::Release);
        Self::with_sink(sink)
    }

    fn with_sink(sink: RecordingSink) -> Self {
        let sink = Arc::new(sink);
        let metrics = Arc::new(EventBusMetrics::default());
        let batcher = Arc::new(DesktopAcpEventBatcher::start(
            sink.clone(),
            metrics.clone(),
        ));
        Self { batcher, sink, metrics }
    }

    async fn enqueue(&self, item: TestEnvelope) -> Result<(), DesktopDeliveryError> {
        self.batcher
            .enqueue(item.envelope, item.estimated_bytes)
            .await
    }

    async fn shutdown(&self) -> Result<(), DesktopDeliveryError> {
        self.batcher.shutdown().await
    }

    async fn yield_task(&self) {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for_batches(&self, expected: usize) {
        for _ in 0..1_000 {
            if self.batches().len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("batcher did not emit {expected} batch(es)");
    }

    async fn wait_for_failure(&self) {
        for _ in 0..1_000 {
            if self.is_failed() && !self.failures().is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("batcher did not enter failed state");
    }

    async fn wait_until_sink_blocked(&self) {
        self.sink.entered.notified().await;
    }

    fn release_sink(&self) {
        self.sink.block.store(false, Ordering::Release);
        self.sink.release.notify_one();
    }

    fn fail_sink(&self) {
        self.sink.fail.store(true, Ordering::Release);
    }

    fn batches(&self) -> Vec<DesktopAcpEventBatch> {
        self.sink.batches.lock().unwrap().clone()
    }

    fn batch_seqs(&self) -> Vec<Vec<u64>> {
        self.batches()
            .into_iter()
            .map(|batch| batch.events.into_iter().map(|event| event.seq).collect())
            .collect()
    }

    fn flattened_seqs(&self, connection_id: &str) -> Vec<u64> {
        self.batches()
            .into_iter()
            .flat_map(|batch| batch.events)
            .filter(|event| event.connection_id == connection_id)
            .map(|event| event.seq)
            .collect()
    }

    fn failures(&self) -> Vec<DesktopDeliveryFailure> {
        self.sink.failures.lock().unwrap().clone()
    }

    fn metrics(&self) -> EventBusMetricsSnapshot {
        self.metrics.snapshot()
    }

    fn is_failed(&self) -> bool {
        self.batcher.is_failed()
    }
}

fn test_envelope(
    connection_id: &str,
    seq: u64,
    payload: AcpEvent,
    estimated_bytes: usize,
) -> TestEnvelope {
    TestEnvelope {
        envelope: Arc::new(EventEnvelope {
            seq,
            connection_id: connection_id.to_owned(),
            payload,
        }),
        estimated_bytes,
    }
}

fn content(seq: u64, estimated_bytes: usize) -> TestEnvelope {
    content_for("c1", seq, estimated_bytes)
}

fn content_for(connection_id: &str, seq: u64, estimated_bytes: usize) -> TestEnvelope {
    test_envelope(
        connection_id,
        seq,
        AcpEvent::ContentDelta { text: "x".into() },
        estimated_bytes,
    )
}

fn permission(seq: u64) -> TestEnvelope {
    test_envelope(
        "c1",
        seq,
        AcpEvent::PermissionRequest {
            request_id: "permission-1".into(),
            tool_call: serde_json::json!({}),
            options: vec![],
        },
        1,
    )
}

fn question(seq: u64) -> TestEnvelope {
    test_envelope(
        "c1",
        seq,
        AcpEvent::QuestionRequest {
            question_id: "question-1".into(),
            questions: vec![],
        },
        1,
    )
}

fn completion(seq: u64) -> TestEnvelope {
    test_envelope(
        "c1",
        seq,
        AcpEvent::TurnComplete {
            session_id: "session-1".into(),
            stop_reason: "end_turn".into(),
            agent_type: "grok".into(),
        },
        1,
    )
}

fn error(connection_id: &str, seq: u64) -> TestEnvelope {
    test_envelope(
        connection_id,
        seq,
        AcpEvent::Error {
            message: "synthetic".into(),
            agent_type: "grok".into(),
            code: Some("synthetic".into()),
            terminal: false,
        },
        1,
    )
}

#[tokio::test(start_paused = true)]
async fn timer_flushes_16ms_after_first_event() {
    let harness = BatcherHarness::new();
    harness.enqueue(content(1, 8)).await.unwrap();
    tokio::time::advance(Duration::from_millis(15)).await;
    assert!(harness.batches().is_empty());
    tokio::time::advance(Duration::from_millis(1)).await;
    harness.yield_task().await;
    assert_eq!(harness.batch_seqs(), vec![vec![1]]);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn count_byte_control_and_shutdown_flush_in_order() {
    assert_flushes_at_count(128).await;
    assert_flushes_at_bytes(64 * 1024).await;
    assert_control_flushes_with_preceding_events(permission(3)).await;
    assert_control_flushes_with_preceding_events(question(3)).await;
    assert_control_flushes_with_preceding_events(completion(3)).await;
    assert_control_flushes_with_preceding_events(error("c1", 3)).await;
    assert_shutdown_drains(vec![content(1, 1), content(2, 1)]).await;
}

async fn assert_flushes_at_count(count: usize) {
    let harness = BatcherHarness::new();
    for seq in 1..=count as u64 {
        harness.enqueue(content(seq, 1)).await.unwrap();
    }
    harness.wait_for_batches(1).await;
    assert_eq!(harness.batches()[0].events.len(), count);
    harness.shutdown().await.unwrap();
}

async fn assert_flushes_at_bytes(bytes: usize) {
    let harness = BatcherHarness::new();
    harness.enqueue(content(1, bytes / 2)).await.unwrap();
    harness.enqueue(content(2, bytes - bytes / 2)).await.unwrap();
    harness.wait_for_batches(1).await;
    assert_eq!(harness.batch_seqs(), vec![vec![1, 2]]);
    harness.shutdown().await.unwrap();
}

async fn assert_control_flushes_with_preceding_events(control: TestEnvelope) {
    let harness = BatcherHarness::new();
    harness.enqueue(content(1, 1)).await.unwrap();
    harness.enqueue(content(2, 1)).await.unwrap();
    harness.enqueue(control).await.unwrap();
    harness.wait_for_batches(1).await;
    assert_eq!(harness.batch_seqs(), vec![vec![1, 2, 3]]);
    harness.shutdown().await.unwrap();
}

async fn assert_shutdown_drains(events: Vec<TestEnvelope>) {
    let harness = BatcherHarness::new();
    for event in events {
        harness.enqueue(event).await.unwrap();
    }
    harness.shutdown().await.unwrap();
    assert_eq!(harness.batch_seqs(), vec![vec![1, 2]]);
}
```

- [ ] **Step 2: Run batcher tests and confirm RED**

```powershell
cd src-tauri
cargo test --features test-utils desktop_event_batcher
```

Expected: compilation fails because the module, sink trait, and batcher harness do not exist.

- [ ] **Step 3: Enable required feature-gated runtime support**

Merge, do not overwrite, the user's existing `Cargo.toml` edits. Add Tokio `time` to the normal dependency features and weakly add Tauri devtools to test-utils:

```toml
test-utils = ["tauri?/devtools"]
tokio = { version = "1", features = ["process", "io-util", "sync", "macros", "rt", "net", "rt-multi-thread", "time"] }
```

The weak `tauri?` feature must not pull Tauri into `--no-default-features` server builds.

- [ ] **Step 4: Implement queue messages and flush-sensitive classification**

```rust
const QUEUE_CAPACITY: usize = 1_024;
const MAX_BATCH_EVENTS: usize = 128;
const MAX_BATCH_BYTES: usize = 64 * 1024;
const MAX_BATCH_DELAY: Duration = Duration::from_millis(16);

enum QueueMessage {
    Event(QueuedEnvelope),
    Shutdown(oneshot::Sender<()>),
}

struct QueuedEnvelope {
    envelope: Arc<EventEnvelope>,
    estimated_bytes: usize,
    queued_at: Instant,
}

fn is_flush_sensitive(event: &AcpEvent) -> bool {
    matches!(
        event,
        AcpEvent::PermissionRequest { .. }
            | AcpEvent::QuestionRequest { .. }
            | AcpEvent::TurnComplete { .. }
            | AcpEvent::Error { .. }
            | AcpEvent::SessionLoadFailed { .. }
    )
}
```

Tool calls and tool updates must not be included in `is_flush_sensitive`.

- [ ] **Step 5: Implement bounded enqueue and the single batch task**

`DesktopAcpEventBatcher::enqueue` records `{ connection_id, seq }` in a shared `OutstandingEnvelopes` set before sending, then first uses `try_send`; on `Full`, increments `desktop_queue_full_count` and awaits `send` with the original message. On `Closed` or an awaited-send failure, it removes that not-accepted item and returns `DesktopDeliveryError::Stopped`. No normal-capacity branch may discard the message. The set is bounded by pending + queue + concurrent blocked producers, and is used only to identify every connection requiring snapshot recovery after a terminal delivery failure.

```rust
#[async_trait]
trait DesktopBatchSink: Send + Sync {
    async fn emit_batch(&self, batch: &DesktopAcpEventBatch) -> Result<(), String>;
    async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String>;
}

#[derive(Default)]
struct OutstandingEnvelopes(Mutex<BTreeMap<String, BTreeSet<u64>>>);

impl OutstandingEnvelopes {
    fn insert(&self, connection_id: &str, seq: u64) {
        self.0
            .lock()
            .unwrap()
            .entry(connection_id.to_owned())
            .or_default()
            .insert(seq);
    }

    fn remove(&self, connection_id: &str, seq: u64) {
        let mut entries = self.0.lock().unwrap();
        if let Some(sequences) = entries.get_mut(connection_id) {
            sequences.remove(&seq);
            if sequences.is_empty() {
                entries.remove(connection_id);
            }
        }
    }

    fn remove_emitted(&self, events: &[EventEnvelope]) {
        for event in events {
            self.remove(&event.connection_id, event.seq);
        }
    }

    fn connection_seq_ranges(&self) -> Vec<DesktopConnectionSeqRange> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(connection_id, sequences)| {
                Some(DesktopConnectionSeqRange {
                    connection_id: connection_id.clone(),
                    first_seq: *sequences.first()?,
                    last_seq: *sequences.last()?,
                })
            })
            .collect()
    }
}

pub async fn enqueue(
    &self,
    envelope: Arc<EventEnvelope>,
    estimated_bytes: usize,
) -> Result<(), DesktopDeliveryError> {
    if self.failed.load(Ordering::Acquire) {
        return Err(DesktopDeliveryError::Stopped);
    }
    let connection_id = envelope.connection_id.clone();
    let seq = envelope.seq;
    self.outstanding.insert(&connection_id, seq);
    let message = QueueMessage::Event(QueuedEnvelope {
        envelope,
        estimated_bytes,
        queued_at: Instant::now(),
    });
    match self.sender.try_send(message) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(message)) => {
            self.metrics
                .desktop_queue_full_count
                .fetch_add(1, Ordering::Relaxed);
            if self.sender.send(message).await.is_ok() {
                Ok(())
            } else {
                self.outstanding.remove(&connection_id, seq);
                Err(DesktopDeliveryError::Stopped)
            }
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            self.outstanding.remove(&connection_id, seq);
            Err(DesktopDeliveryError::Stopped)
        }
    }
}
```

`DesktopBatchSink` is an `#[async_trait]` with async `emit_batch`/`emit_failure` methods; the Tauri implementation performs its synchronous `app.emit` inside those methods, while tests can block without blocking a Tokio worker thread. The worker owns one `Vec<QueuedEnvelope>`, total bytes, first-event deadline, and monotonic batch ID. On flush it clones the unmodified envelopes into the fixed wire type, calls the sink once, then records success metrics and removes only the successfully emitted events from the outstanding set:

```rust
async fn run_batcher(
    mut receiver: mpsc::Receiver<QueueMessage>,
    sink: Arc<dyn DesktopBatchSink>,
    metrics: Arc<EventBusMetrics>,
    failed: Arc<AtomicBool>,
    outstanding: Arc<OutstandingEnvelopes>,
) {
    let mut pending = Vec::with_capacity(MAX_BATCH_EVENTS);
    let mut pending_bytes = 0usize;
    let mut next_batch_id = 0u64;

    loop {
        let message = if pending.is_empty() {
            receiver.recv().await
        } else {
            let deadline = pending[0].queued_at + MAX_BATCH_DELAY;
            tokio::select! {
                value = receiver.recv() => value,
                _ = tokio::time::sleep_until(deadline) => {
                    if flush(
                        &mut pending,
                        &mut pending_bytes,
                        &mut next_batch_id,
                        sink.as_ref(),
                        &metrics,
                        &outstanding,
                    ).await.is_err() {
                        failed.store(true, Ordering::Release);
                        receiver.close();
                        return;
                    }
                    continue;
                }
            }
        };

        match message {
            Some(QueueMessage::Event(queued)) => {
                let flush_sensitive = is_flush_sensitive(&queued.envelope.payload);
                pending_bytes = pending_bytes.saturating_add(queued.estimated_bytes);
                pending.push(queued);
                if pending.len() >= MAX_BATCH_EVENTS
                    || pending_bytes >= MAX_BATCH_BYTES
                    || flush_sensitive
                {
                    if flush(
                        &mut pending,
                        &mut pending_bytes,
                        &mut next_batch_id,
                        sink.as_ref(),
                        &metrics,
                        &outstanding,
                    ).await.is_err() {
                        failed.store(true, Ordering::Release);
                        receiver.close();
                        return;
                    }
                }
            }
            Some(QueueMessage::Shutdown(done)) => {
                if flush(
                    &mut pending,
                    &mut pending_bytes,
                    &mut next_batch_id,
                    sink.as_ref(),
                    &metrics,
                    &outstanding,
                ).await.is_err() {
                    failed.store(true, Ordering::Release);
                }
                let _ = done.send(());
                return;
            }
            None => {
                if flush(
                    &mut pending,
                    &mut pending_bytes,
                    &mut next_batch_id,
                    sink.as_ref(),
                    &metrics,
                    &outstanding,
                ).await.is_err() {
                    failed.store(true, Ordering::Release);
                }
                return;
            }
        }
    }
}

async fn flush(
    pending: &mut Vec<QueuedEnvelope>,
    pending_bytes: &mut usize,
    next_batch_id: &mut u64,
    sink: &dyn DesktopBatchSink,
    metrics: &EventBusMetrics,
    outstanding: &OutstandingEnvelopes,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    let latency = pending[0].queued_at.elapsed();
    let events = pending
        .drain(..)
        .map(|queued| queued.envelope.as_ref().clone())
        .collect::<Vec<_>>();
    let bytes = std::mem::take(pending_bytes);
    *next_batch_id += 1;
    let batch = DesktopAcpEventBatch {
        batch_id: *next_batch_id,
        events,
    };
    metrics
        .desktop_emit_attempt_count
        .fetch_add(1, Ordering::Relaxed);
    match sink.emit_batch(&batch).await {
        Ok(()) => {
            metrics.record_desktop_batch(batch.events.len(), bytes, latency);
            outstanding.remove_emitted(&batch.events);
            Ok(())
        }
        Err(error) => {
            metrics
                .desktop_emit_failure_count
                .fetch_add(1, Ordering::Relaxed);
            metrics
                .desktop_runtime_failure_count
                .fetch_add(1, Ordering::Relaxed);
            let failure = DesktopDeliveryFailure {
                generation: *next_batch_id,
                reason: "batch_emit_failed",
                affected: outstanding.connection_seq_ranges(),
            };
            let _ = sink.emit_failure(&failure).await;
            Err(error)
        }
    }
}
```

`OutstandingEnvelopes` uses `Mutex<BTreeMap<String, BTreeSet<u64>>>`; `connection_seq_ranges()` emits deterministic connection order and the first/last outstanding sequence for each connection. This deliberately includes the failed batch, queued messages, and producers already waiting on capacity, because all of them require snapshot recovery. When `flush` returns an error, the worker marks the shared delivery state failed before closing the receiver and never emits a later batch. A supervisor awaits the worker `JoinHandle`; an unexpected panic/early exit atomically emits the same signal once with reason `batch_task_stopped` and current outstanding ranges, while an orderly `shutdown` is marked before joining and emits no failure. Test both reasons and assert failure logs/payloads contain ranges and reason only, never content/tool fields.

- [ ] **Step 6: Add backpressure, multi-connection, and runtime-failure tests**

```rust
#[tokio::test]
async fn full_queue_applies_backpressure_without_loss() {
    let harness = BatcherHarness::with_blocked_sink();
    for seq in 1..=MAX_BATCH_EVENTS as u64 {
        harness.enqueue(content_for("c1", seq, 1)).await.unwrap();
    }
    harness.wait_until_sink_blocked().await;
    for seq in (MAX_BATCH_EVENTS as u64 + 1)
        ..=(MAX_BATCH_EVENTS + QUEUE_CAPACITY) as u64
    {
        harness.enqueue(content_for("c1", seq, 1)).await.unwrap();
    }
    let blocked = tokio::spawn({
        let harness = harness.clone();
        async move {
            harness
                .enqueue(content_for(
                    "c1",
                    (MAX_BATCH_EVENTS + QUEUE_CAPACITY + 1) as u64,
                    1,
                ))
                .await
        }
    });
    harness.yield_task().await;
    assert!(!blocked.is_finished());
    harness.release_sink();
    blocked.await.unwrap().unwrap();
    harness.shutdown().await.unwrap();
    assert_eq!(
        harness.flattened_seqs("c1"),
        (1..=(MAX_BATCH_EVENTS + QUEUE_CAPACITY + 1) as u64).collect::<Vec<_>>()
    );
    assert!(harness.metrics().desktop_queue_full_count > 0);
}

#[tokio::test]
async fn failed_emit_stops_delivery_and_reports_affected_ranges_once() {
    let harness = BatcherHarness::with_blocked_sink();
    for seq in 1..=MAX_BATCH_EVENTS as u64 {
        harness.enqueue(content_for("a", seq, 1)).await.unwrap();
    }
    harness.wait_until_sink_blocked().await;
    harness.enqueue(content_for("b", 3, 1)).await.unwrap();
    harness.enqueue(content_for("c", 9, 1)).await.unwrap();
    harness.fail_sink();
    harness.release_sink();
    harness.wait_for_failure().await;
    assert_eq!(harness.failures().len(), 1);
    assert_eq!(
        harness.failures()[0].affected,
        vec![
            DesktopConnectionSeqRange {
                connection_id: "a".into(),
                first_seq: 1,
                last_seq: MAX_BATCH_EVENTS as u64,
            },
            DesktopConnectionSeqRange {
                connection_id: "b".into(),
                first_seq: 3,
                last_seq: 3,
            },
            DesktopConnectionSeqRange {
                connection_id: "c".into(),
                first_seq: 9,
                last_seq: 9,
            },
        ]
    );
    assert!(harness.enqueue(content_for("a", 129, 1)).await.is_err());
    assert_eq!(harness.metrics().desktop_runtime_failure_count, 1);
}
```

- [ ] **Step 7: Add startup flags, capability query, and legacy fallback**

Parse `CODEG_DESKTOP_ACP_EVENT_BATCHING`, `CODEG_INCREMENTAL_LIVE_TRANSCRIPT`, and `CODEG_DEFERRED_STREAMING_RICH_CONTENT` as tri-state values: true is `1|true|yes|on`, false is `0|false|no|off`, matching is ASCII case-insensitive after trim, and absent/invalid uses the phase default while logging only the variable name. P1 defaults all three to false; Task 15 changes only those defaults, so an explicit false continues to work. Keep parsing pure by passing a lookup closure in tests.

`DesktopAcpDelivery::start` chooses batched only when the normalized flag is enabled and the worker starts. If initialization fails, increment `desktop_startup_fallback_count`, retain legacy mode, and expose that exact mode from:

```rust
#[tauri::command]
pub fn acp_get_desktop_delivery_capabilities(
    delivery: tauri::State<'_, Arc<DesktopAcpDelivery>>,
) -> DesktopDeliveryCapabilities {
    delivery.capabilities()
}
```

Add tests proving an unavailable worker returns legacy, disables the two dependent flags, and never emits both event names.

- [ ] **Step 8: Route only the Tauri leg through the delivery owner**

Keep `stream.send` and `bus.send` exactly where they are. Replace only `app.emit("acp://event", ...)` with:

```rust
if let Some(delivery) = app.try_state::<Arc<DesktopAcpDelivery>>() {
    if let Err(error) = delivery
        .deliver(Arc::clone(&envelope_arc), estimated_bytes)
        .await
    {
        tracing::error!("[ACP] desktop delivery stopped: {error}");
    }
} else {
    emit_legacy(app, &envelope_arc, emitter.metrics());
}
```

Record `record_desktop_offer(estimated_bytes)` exactly once before this branch. `DesktopAcpDelivery::deliver`'s legacy arm and the no-state `emit_legacy` fallback increment only attempt/success/failure counters; they must not record a second offer. Move the existing `InternalEventBus::send` before this awaited desktop branch so in-process lifecycle/pet/chat consumers retain immediate per-envelope delivery even when the desktop queue applies backpressure. This await occurs after the `SessionState` lock scope and therefore may backpressure without blocking state mutation.

- [ ] **Step 9: Initialize before connections and drain after disconnect**

In `setup`, create/manage `DesktopAcpDelivery` before spawning any manager/background tasks that can emit ACP events. In `RunEvent::ExitRequested`, use the existing `tauri::async_runtime::block_on` shutdown path: call `ConnectionManager::disconnect_all()` first so terminal events enter the queue, then call `DesktopAcpDelivery::shutdown()` and wait for the worker/supervisor join so the final batch is drained before runtime teardown. `shutdown()` is idempotent for duplicate exit paths.

- [ ] **Step 10: Run desktop, server, and clippy verification**

```powershell
cd src-tauri
cargo test --features test-utils desktop_event_batcher
cargo test --features test-utils event_bridge
cargo check
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib internal_bus
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all commands pass; server tests still observe per-envelope delivery, and no server target references Tauri batch types at runtime.

- [ ] **Step 11: Commit the Rust batcher**

```powershell
git add src-tauri/Cargo.toml src-tauri/src/acp/desktop_event_batcher.rs src-tauri/src/acp/streaming_performance.rs src-tauri/src/acp/mod.rs src-tauri/src/web/event_bridge.rs src-tauri/src/commands/acp.rs src-tauri/src/lib.rs
git commit -m "feat(perf): batch desktop ACP events"
```

---

### Task 6: Add Batch Types, Startup Subscription, And `EventIngestor`

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Create: `src/lib/transport/desktop-acp-events.ts`
- Create: `src/lib/transport/desktop-acp-events.test.ts`
- Create: `src/lib/acp/streaming-performance-config.ts`
- Create: `src/lib/acp/event-ingestor.ts`
- Create: `src/lib/acp/event-ingestor.test.ts`

**Interfaces:**
- Consumes: Task 5 capabilities, batch, and failure wire types.
- Produces: `subscribeDesktopAcpEvents(capabilities, handlers)` that subscribes to exactly one data event.
- Produces: `EventIngestor` and the fixed `AcceptedEventFrame`/`SequenceGap` interfaces.
- Does not mutate React or Zustand state; Task 7 owns the commit callback.

- [ ] **Step 1: Write failing startup-subscription tests**

```ts
it.each([
  ["legacy", "acp://event"],
  ["batched", "acp://event-batch"],
] as const)("subscribes %s mode to only %s", async (mode, expectedEvent) => {
  const transport = fakeTransport()
  const unsubscribe = await subscribeDesktopAcpEvents(
    capabilities(mode),
    handlers,
    transport
  )
  expect(transport.subscribedDataEvents()).toEqual([expectedEvent])
  expect(transport.subscribedEvents().includes("acp://delivery-failed")).toBe(
    mode === "batched"
  )
  unsubscribe()
  expect(transport.unsubscribeCount()).toBe(transport.subscribedEvents().length)
})

const handlers: DesktopAcpEventHandlers = {
  onBatch: vi.fn(),
  onFailure: vi.fn(),
}

function capabilities(mode: DesktopDeliveryMode): DesktopDeliveryCapabilities {
  const batching = mode === "batched"
  return {
    mode,
    flags: {
      desktop_acp_event_batching: batching,
      incremental_live_transcript: false,
      deferred_streaming_rich_content: false,
    },
    perf_replay_available: false,
    failure_event: "acp://delivery-failed",
  }
}

function fakeTransport() {
  const events: string[] = []
  let unsubscribeCount = 0
  return {
    subscribe: vi.fn(async (event: string) => {
      events.push(event)
      return () => {
        unsubscribeCount += 1
      }
    }),
    subscribedEvents: () => events.slice(),
    subscribedDataEvents: () =>
      events.filter((event) => event !== "acp://delivery-failed"),
    unsubscribeCount: () => unsubscribeCount,
  }
}
```

- [ ] **Step 2: Run transport tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/transport/desktop-acp-events.test.ts
```

Expected: import failure because the subscription module does not exist.

- [ ] **Step 3: Mirror wire types and install an immutable capability store**

Add the Cross-Task Interface types to `src/lib/types.ts`, API calls for metrics/capabilities/replay, and this normalization assertion:

```ts
export function normalizeStreamingPerformanceCapabilities(
  value: DesktopDeliveryCapabilities
): DesktopDeliveryCapabilities {
  const batching = value.mode === "batched" && value.flags.desktop_acp_event_batching
  const incremental = batching && value.flags.incremental_live_transcript
  return {
    ...value,
    flags: {
      desktop_acp_event_batching: batching,
      incremental_live_transcript: incremental,
      deferred_streaming_rich_content:
        incremental && value.flags.deferred_streaming_rich_content,
    },
  }
}
```

`streaming-performance-config.ts` exposes `initializeStreamingPerformanceConfig`, `getStreamingPerformanceConfig`, `subscribeStreamingPerformanceConfig`, and `useStreamingPerformanceFlag`. Initialization is idempotent and rejects a second different startup snapshot.

- [ ] **Step 4: Implement exact-one-event subscription**

```ts
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
```

Do not subscribe to the failure signal in legacy mode; it cannot be emitted there.

- [ ] **Step 5: Write failing ingestor tests with a manual frame scheduler**

```ts
it("deduplicates, compacts, and commits once on the next frame", () => {
  const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 10 } })
  h.pushBatch(batch(4, [content("c1", 10, "old"), content("c1", 11, "a"), content("c1", 12, "b")]))
  h.pushBatch(batch(5, [thinking("c1", 13, "x"), thinking("c1", 14, "y")]))
  expect(h.commits).toHaveLength(0)
  h.runFrame()
  expect(h.commits).toHaveLength(1)
  expect(h.commits[0].connections[0].applyEvents).toMatchObject([
    { type: "content_delta", text: "ab", seq: 12 },
    { type: "thinking", text: "xy", seq: 14 },
  ])
  expect(h.commits[0].connections[0].rawEvents.map((event) => event.seq)).toEqual([11, 12, 13, 14])
  expect(h.commits[0].connections[0].highestSeq).toBe(14)
})

it("stops a connection at the first sequence gap", () => {
  const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 20 } })
  h.pushBatch(batch(1, [content("c1", 22, "missing-21")]))
  h.runFrame()
  expect(h.commits).toHaveLength(0)
  expect(h.gaps).toEqual([
    { contextKey: "tab-1", connectionId: "c1", expectedSeq: 21, receivedSeq: 22 },
  ])
  expect(h.cursor("c1")).toBe(20)
})

it("never compacts across a tool append boundary", () => {
  const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
  h.pushBatch(batch(1, [content("c1", 1, "a"), toolAppend("c1", 2, "x"), content("c1", 3, "b")]))
  h.runFrame()
  expect(h.appliedTypes()).toEqual(["content_delta", "tool_call_update", "content_delta"])
  expect(h.rawSeqs()).toEqual([1, 2, 3])
})

function batch(batch_id: number, events: EventEnvelope[]): DesktopAcpEventBatch {
  return { batch_id, events }
}

function event(
  connection_id: string,
  seq: number,
  payload: AcpEvent
): EventEnvelope {
  return { connection_id, seq, ...payload }
}

function content(connectionId: string, seq: number, text: string): EventEnvelope {
  return event(connectionId, seq, { type: "content_delta", text })
}

function thinking(connectionId: string, seq: number, text: string): EventEnvelope {
  return event(connectionId, seq, { type: "thinking", text })
}

function toolAppend(
  connectionId: string,
  seq: number,
  raw_output: string
): EventEnvelope {
  return event(connectionId, seq, {
    type: "tool_call_update",
    tool_call_id: "tool-1",
    title: null,
    status: null,
    content: null,
    raw_input: null,
    raw_output,
    raw_output_append: true,
  })
}

function createIngestorHarness(
  initial: Record<string, { key: string; cursor: number }>
) {
  const connectionToKey = new Map(
    Object.entries(initial).map(([id, value]) => [id, value.key])
  )
  const cursorByKey = new Map(
    Object.values(initial).map((value) => [value.key, value.cursor])
  )
  const commits: AcceptedEventFrame[] = []
  const gaps: SequenceGap[] = []
  let scheduled: FrameRequestCallback | null = null
  const ingestor = new EventIngestor({
    resolveContextKey: (connectionId) => connectionToKey.get(connectionId) ?? null,
    readCursor: (contextKey) => cursorByKey.get(contextKey) ?? 0,
    commit: (frame) => {
      commits.push(frame)
      for (const connection of frame.connections) {
        cursorByKey.set(connection.contextKey, connection.highestSeq)
      }
    },
    onGap: (gap) => gaps.push(gap),
    onUnmapped: vi.fn(),
    scheduleFrame: (callback) => {
      scheduled = callback
      return 1
    },
    cancelFrame: () => {
      scheduled = null
    },
  })
  return {
    commits,
    gaps,
    pushBatch: (value: DesktopAcpEventBatch) => ingestor.pushBatch(value),
    runFrame: () => {
      const callback = scheduled
      scheduled = null
      callback?.(16)
    },
    cursor: (connectionId: string) =>
      cursorByKey.get(connectionToKey.get(connectionId) ?? "") ?? 0,
    appliedTypes: () =>
      commits.flatMap((frame) =>
        frame.connections.flatMap((connection) =>
          connection.applyEvents.map((item) => item.type)
        )
      ),
    rawSeqs: () => commits.flatMap((frame) => frame.rawEventsInDeliveryOrder.map((item) => item.seq)),
  }
}
```

- [ ] **Step 6: Run ingestor tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/acp/event-ingestor.test.ts
```

Expected: import failure because `EventIngestor` does not exist.

- [ ] **Step 7: Implement per-connection cursors and frame draining**

Constructor dependencies remain explicit and testable:

```ts
export interface EventIngestorDeps {
  resolveContextKey(connectionId: string): string | null
  readCursor(contextKey: string): number
  commit(frame: AcceptedEventFrame): void
  onGap(gap: SequenceGap): void
  onUnmapped(event: EventEnvelope): void
  scheduleFrame(callback: FrameRequestCallback): number
  cancelFrame(handle: number): void
}

export class EventIngestor {
  pushBatch(batch: DesktopAcpEventBatch): void
  pushMapped(contextKey: string, events: readonly EventEnvelope[]): void
  resumeConnection(connectionId: string, cursor: number): void
  pauseConnection(connectionId: string): void
  flushNow(): void
  dispose(): void
}
```

Maintain a pending array of `{ deliveryId, event }` in original delivery order and one scheduled RAF. `pushMapped` allocates from a separate process-local monotonic synthetic delivery counter because attach events have no desktop batch ID. During drain, filter `seq <= cursor`; require every accepted per-connection sequence to equal the provisional cursor plus one; stop and retain later envelopes for that connection on a gap. Each `AcceptedConnectionFrame.deliveryIds` contains the first-seen unique IDs that contributed accepted events to that connection, while the outer list preserves global delivery order. Update only provisional cursors during preparation. The provider's successful `commit` is the point after which real cursors are considered advanced.

- [ ] **Step 8: Compact store events without changing raw callback events**

`compactAdjacentDeltas` may merge only adjacent `content_delta` events or adjacent `thinking` events for the same connection. The merged event carries the last envelope's `seq` and concatenated text; `rawEventsInDeliveryOrder` and each connection's `rawEvents` retain every original envelope.

Never compact `tool_call_update`, even when `raw_output_append` is false: title/status/image fields are partial patches and ordered application is part of the contract.

- [ ] **Step 9: Add unmapped buffering, resume, failure, and disposal tests**

Cover these exact cases:

- unmapped events call `onUnmapped` in original order and do not advance a cursor;
- events arriving while a connection is paused remain buffered;
- `resumeConnection(id, snapshotSeq)` drops buffered duplicates, then requires contiguity;
- two connections with interleaved events retain independent cursors;
- `dispose()` cancels the pending RAF and makes later pushes no-ops;
- one thrown `commit` leaves cursors unadvanced and pauses affected connections for recovery.

- [ ] **Step 10: Run focused tests and commit**

```powershell
pnpm exec vitest run src/lib/transport/desktop-acp-events.test.ts src/lib/acp/event-ingestor.test.ts
pnpm eslint src/lib/transport/desktop-acp-events.ts src/lib/acp/streaming-performance-config.ts src/lib/acp/event-ingestor.ts
git add src/lib/types.ts src/lib/api.ts src/lib/transport/desktop-acp-events.ts src/lib/transport/desktop-acp-events.test.ts src/lib/acp/streaming-performance-config.ts src/lib/acp/event-ingestor.ts src/lib/acp/event-ingestor.test.ts
git commit -m "feat(perf): ingest ACP batches per frame"
```

Expected: focused tests and ESLint pass before commit.

---

### Task 7: Apply One Connection-Store Transaction Per Browser Frame

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/stores/conversation-runtime-store.ts`
- Modify: `src/lib/perf/streaming-perf-recorder.ts`

**Interfaces:**
- Consumes: `AcceptedEventFrame` from Task 6.
- Produces: reducer action `APPLY_EVENT_FRAME` that clones the outer map at most once.
- Produces: one live-message sink call and one render-relevant key notification per changed connection per frame.
- Preserves: raw `useAcpEvent` callback order after committed state is observable.

- [ ] **Step 1: Add failing provider transaction-count tests**

Extend the existing provider harness rather than constructing a second mock provider:

```ts
it("publishes one store transaction and one live sink for a 200-event frame", async () => {
  const handlers = await connectOwner()
  const sink = vi.fn()
  h.actions!.registerLiveMessageSink(TAB, sink)
  const notify = vi.fn()
  const unsubscribe = h.store!.subscribeKey(TAB, notify)

  act(() => {
    h.emitDesktopBatch(
      batch(1, Array.from({ length: 200 }, (_, index) =>
        content("owner-conn", index + 1, "x")
      ))
    )
    h.runAnimationFrame()
  })

  expect(h.publishedConnectionMaps()).toBe(1)
  expect(sink).toHaveBeenCalledTimes(1)
  expect(notify).toHaveBeenCalledTimes(1)
  expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(200)
  expect(h.store!.getConnection(TAB)?.liveMessage?.content[0]).toMatchObject({
    type: "text",
    text: "x".repeat(200),
  })
  unsubscribe()
})

it("raw subscribers run after commit in original envelope order", async () => {
  await connectOwner()
  const seen: Array<{ seq: number; cursor: number }> = []
  h.subscribeRaw((event) => {
    seen.push({
      seq: event.seq,
      cursor: h.store!.getConnection(TAB)!.lastAppliedSeq,
    })
  })
  act(() => {
    h.emitDesktopBatch(batch(9, [content("owner-conn", 1, "a"), thinking("owner-conn", 2, "b")]))
    h.runAnimationFrame()
  })
  expect(seen).toEqual([
    { seq: 1, cursor: 2 },
    { seq: 2, cursor: 2 },
  ])
})
```

- [ ] **Step 2: Run focused provider tests and confirm RED**

```powershell
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx -t "one store transaction|raw subscribers run after commit"
```

Expected: the batch helper/action is missing or current per-envelope dispatch produces more than one publication/notification.

- [ ] **Step 3: Make the existing reducer mutate only an unpublished frame map**

Add a private write-mode argument. Normal calls retain immutable behavior; `APPLY_EVENT_FRAME` creates one unpublished map and reuses it for every existing key-local case:

```ts
type MapLevelAction = Extract<
  Action,
  {
    type:
      | "CONNECTION_CREATED"
      | "CONNECTION_REMOVED"
      | "REMOVE_ALL"
      | "REKEY_CONNECTION"
      | "DELEGATION_CHILD_ATTACH"
      | "DELEGATION_CHILD_DETACH"
  }
>

type FrameAction = Exclude<
  Action,
  MapLevelAction | { type: "APPLY_EVENT_FRAME" }
>

interface PreparedConnectionFrame {
  contextKey: string
  deliveryIds: readonly number[]
  actions: readonly FrameAction[]
  highestSeq: number
}

function writableConnections(
  state: ConnectionsMap,
  mutateUnpublished: boolean
): ConnectionsMap {
  return mutateUnpublished ? state : new Map(state)
}

function reduceSingleAction(
  state: ConnectionsMap,
  action: Exclude<Action, { type: "APPLY_EVENT_FRAME" }>,
  mutateUnpublished = false
): ConnectionsMap {
  // This is the existing connectionsReducer switch, renamed. Every current
  // `const next = new Map(state)` becomes
  // `const next = writableConnections(state, mutateUnpublished)`; all field
  // calculations, guards, set/delete calls, and return values stay unchanged.
}

function connectionsReducer(state: ConnectionsMap, action: Action): ConnectionsMap {
  if (action.type === "APPLY_EVENT_FRAME") {
    const next = new Map(state)
    let changed = false
    for (const frame of action.frames) {
      const before = next.get(frame.contextKey)
      if (!before) continue
      for (const item of frame.actions) {
        reduceSingleAction(next, item, true)
      }
      const reduced = next.get(frame.contextKey)
      if (reduced && frame.highestSeq > reduced.lastAppliedSeq) {
        next.set(frame.contextKey, {
          ...reduced,
          lastAppliedSeq: frame.highestSeq,
        })
      }
      if (next.get(frame.contextKey) !== before) changed = true
    }
    return changed ? next : state
  }
  return reduceSingleAction(state, action, false)
}
```

Do not create new field reducers such as `appendContentDelta`; the renamed switch remains the source of truth. Keep map-level actions out of frames. Add a table-driven parity test containing one valid fixture for every `FrameAction["type"]`; each fixture must produce deep-equal state through the normal single-action route and a one-item frame, and a test-only `writableConnections` counter must equal one for the frame route.

- [ ] **Step 4: Convert event handling into actions plus deferred effects**

Replace `handleMappedEvent`'s direct dispatches with:

```ts
interface PreparedEnvelope {
  actions: FrameAction[]
  afterCommit: Array<() => void>
}

function prepareMappedEnvelope(
  contextKey: string,
  envelope: EventEnvelope,
  snapshot: ConnectionState
): PreparedEnvelope
```

Move OS notifications, selector-cache writes, background-overlay writes/refetches, logging, and alerts into `afterCommit`. Preserve their existing conditions and ordering. `turn_complete`, permission/question/error/cancellation-related events must remain individual actions in envelope order even when text deltas around them compact.

Use a runtime default for forward-compatible unknown types. It returns no store action, logs only `{ type }`, still lets the frame advance its cursor, and leaves the unmodified envelope in post-commit raw callbacks:

```ts
const unknown = envelope as EventEnvelope & { type: string }
console.warn("[acp-context] unknown ACP event type", { type: unknown.type })
return { actions: [], afterCommit: [] }
```

- [ ] **Step 5: Commit the complete frame once, then publish in fixed order**

Implement this order in the provider callback:

```ts
const commitEventFrame = (frame: AcceptedEventFrame): void => {
  const prepared = prepareEventFrame(frame, storeRef.current.connections)
  const previous = storeRef.current.connections
  const next = connectionsReducer(previous, {
    type: "APPLY_EVENT_FRAME",
    frames: prepared.connections,
  })
  storeRef.current.connections = next
  streamingPerfRecorder.markConnectionFrameCommitted(
    frame.deliveryIds,
    prepared.changedConnections.length,
    next !== previous
  )

  for (const connection of prepared.changedConnections) {
    mirrorLiveMessageOnce(
      connection.contextKey,
      previous,
      next,
      connection.deliveryIds
    )
  }
  for (const effect of prepared.afterCommit) effect()
  for (const connection of prepared.renderChangedConnections) {
    notifyKeyListeners(connection.contextKey)
  }
  for (const event of frame.rawEventsInDeliveryOrder) notifyRawSubscribers(event)
}
```

`markConnectionFrameCommitted` timestamps each unique delivery ID once, increments one frame counter, records whether the outer map was published, and adds the number of changed connection transactions. The mirror passes the connection-local `deliveryIds` to both canonical and transcript sinks; the recorder marks one live publication per changed connection and associates all contributing IDs with the next React commit. The mirror runs before effects/listeners, and raw subscribers run last. A cursor-only frame publishes the cursor but does not notify render-relevant key listeners or fire the live sink.

- [ ] **Step 6: Replace both desktop and attach paths with the ingestor**

At provider startup:

1. When `getEventStream() === null` and `getTransport().isDesktop()` is true, query/normalize desktop capabilities before subscribing. For Web/remote attach transports, initialize the immutable capability store with an explicit legacy/all-disabled snapshot and do not call the Tauri-only command.
2. Initialize the immutable capability store exactly once from that selected snapshot.
3. Construct one `EventIngestor` with reverse-map and store cursor callbacks.
4. Subscribe through `subscribeDesktopAcpEvents` only when `getEventStream() === null`.
5. Route attach `onReplay` and `onEvent` through `pushMapped(contextKey, events)` so legacy/web paths share the transaction helper without changing the wire protocol.

Delete `streamingQueueRef`, `flushTimerRef`, `flushStreamingQueue`, `enqueueStreamingAction`, the 256-item synchronous flush, `pendingToolCallUpdates`, and tool-forced timer flushes after parity tests are green.

- [ ] **Step 7: Implement sequence-gap and runtime-failure recovery**

For desktop gaps, pause that connection, call `acpGetSessionSnapshot(connectionId)`, dispatch `HYDRATE_FROM_SNAPSHOT`, then `resumeConnection(connectionId, snapshot.event_seq)`. If the snapshot is absent, dispatch `CONNECTION_REMOVED`.

For `DesktopDeliveryFailure`, dispose the ingestor, recover every affected connection from snapshots, surface one existing error alert that a restart is required, and do not subscribe to `acp://event`. This obeys the no-hot-switch rule.

For attach streams, retain the current detach/re-attach behavior; feed the recovered snapshot/replay cursor back to the same ingestor.

- [ ] **Step 8: Add control, rekey, buffering, and exception regressions**

Add tests for:

- permission request → resolved, question request → resolved, error, cancellation/status, and turn completion order in one batch;
- `raw_output_append` chunks remain ordered and concatenated;
- rekey between receipt and frame commit routes to the new key exactly once;
- unmapped desktop events buffer, snapshot hydrates, then drain without duplicates;
- snapshot cursor racing a queued batch drops old events and applies the contiguous suffix;
- one raw subscriber throwing does not stop later subscribers;
- an unknown event advances the contiguous cursor, reaches raw subscribers, and logs no payload fields;
- one connection gap does not block another connection in the same batch;
- runtime failure never starts the legacy listener.

- [ ] **Step 9: Run the full focused P1 frontend suite**

```powershell
pnpm exec vitest run src/lib/acp/event-ingestor.test.ts src/lib/transport/desktop-acp-events.test.ts src/contexts/acp-connections-context.test.tsx src/stores/runtime-live-message-slice-decoupling.test.ts
pnpm eslint src/lib/acp src/lib/transport/desktop-acp-events.ts src/contexts/acp-connections-context.tsx
```

Expected: all tests pass; the existing sink-before-notify and cross-client viewer tests remain green.

- [ ] **Step 10: Commit transactional provider ingestion**

```powershell
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/stores/conversation-runtime-store.ts src/lib/perf/streaming-perf-recorder.ts
git commit -m "refactor(perf): commit ACP frames once"
```

---

### Task 8: Run The P1 Integrity And Performance Gate

**Files:**
- Create: `docs/superpowers/performance/webview-streaming/p1-100eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p1-500eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p1-1000eps.json`
- Modify: `docs/superpowers/performance/webview-streaming/comparison.md`

**Interfaces:**
- Consumes: P0 baselines and P1 batch/transaction counters.
- Produces: the decision evidence required before P2.
- Gate: callback/store-commit counts must scale with emitted batches rather than 1,223 raw envelopes.

- [ ] **Step 1: Run P1 correctness suites in both runtimes**

```powershell
pnpm exec vitest run src/lib/acp/event-ingestor.test.ts src/lib/transport/desktop-acp-events.test.ts src/contexts/acp-connections-context.test.tsx
cd src-tauri
cargo test --features test-utils desktop_event_batcher
cargo test --features test-utils event_bridge
cargo test --no-default-features --bin codeg-server --lib internal_bus
```

Expected: all commands pass with zero integrity, order, or server-path failures.

- [ ] **Step 2: Build with batching enabled and dependent flags disabled**

```powershell
$env:CODEG_DESKTOP_ACP_EVENT_BATCHING='1'
$env:CODEG_INCREMENTAL_LIVE_TRANSCRIPT='0'
$env:CODEG_DEFERRED_STREAMING_RICH_CONTENT='0'
pnpm exec tauri build --debug --features test-utils
```

Expected: the report environment shows `deliveryMode: "batched"`, batching true, and both later flags false.

- [ ] **Step 3: Capture median-of-three P1 reports**

Repeat the exact Task 4 seed/profile expressions three times each. Select the median by batch-to-paint P95 and save as `p1-100eps.json`, `p1-500eps.json`, and `p1-1000eps.json`.

Every selected report must satisfy:

- event integrity and final checksum pass;
- desktop raw envelope count delta is 1,223;
- desktop batch event count delta is 1,223;
- emitted batch count is less than raw envelope count;
- frontend transaction count is no greater than rendered browser frames plus control flushes;
- per-connection live sink count is no greater than transaction count;
- server/InternalEventBus tests remain per-envelope.

- [ ] **Step 4: Add the P0→P1 comparison and review the gate**

Add P0 and P1 rows for callback count, batch count, connection-map publications, receipt-to-transaction P50/P95/max, input-to-paint P95, long-task max, and batch-to-paint P95. Calculate percentage changes as `(P1 - P0) / P0 * 100` with one decimal place.

P1 exits only if IPC plus state-apply cost materially decreases on at least the 500 and 1,000 eps profiles, no input/control metric regresses by more than 10%, and any remaining failed final target is attributed to render/Markdown/layout stages.

- [ ] **Step 5: Force-stage and commit P1 evidence**

```powershell
git add -f docs/superpowers/performance/webview-streaming/p1-100eps.json docs/superpowers/performance/webview-streaming/p1-500eps.json docs/superpowers/performance/webview-streaming/p1-1000eps.json docs/superpowers/performance/webview-streaming/comparison.md
git commit -m "docs(perf): record desktop batching gains"
```

---

## P2: Stable History And Incremental Live Projection

### Task 9: Split The Stable Historical Timeline Selector

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`
- Create: `src/stores/conversation-runtime-store.test.ts`
- Modify: `src/stores/runtime-live-message-slice-decoupling.test.ts`

**Interfaces:**
- Produces: `selectHistoricalTimelineTurns(state, conversationId)` with no streaming-phase entries.
- Preserves: `selectTimelineTurns` as a compatibility selector that appends canonical live turns for consumers not yet migrated.
- Invariant: content-only `SET_LIVE_MESSAGE` updates return the exact same historical array and historical turn references.
- Invalidates on: persisted detail, local, background, optimistic, live identity/start/end, delegation ownership/kickoff, or completion promotion changes.

- [ ] **Step 1: Write failing reference-stability tests**

Create a complete seeded session using the same shape as `runtime-live-message-slice-decoupling.test.ts`:

```ts
it("keeps historical arrays and entries identical across 500 live appends", () => {
  seedRuntimeSession({
    detail: detailWithTurns([userTurn("u1"), assistantTurn("a1")]),
    optimisticTurns: [userTurn("u2")],
  })
  const stateBefore = useConversationRuntimeStore.getState()
  const before = selectHistoricalTimelineTurns(stateBefore, CID)

  for (let index = 0; index < 500; index += 1) {
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage("live-1", "x".repeat(index + 1)), true)
    const current = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(current).toBe(before)
    expect(current[0]).toBe(before[0])
    expect(current[1]).toBe(before[1])
  }
})

it.each(["detail", "local", "background", "optimistic"] as const)(
  "invalidates when %s history changes",
  (kind) => {
    seedRuntimeSession(baseSeed())
    const before = selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    mutateHistoricalInput(kind)
    const after = selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    expect(after).not.toBe(before)
  }
)

it("never includes a streaming phase", () => {
  seedRuntimeSession({ liveMessage: liveMessage("live-1", "answer") })
  expect(
    selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
  ).not.toContainEqual(expect.objectContaining({ phase: "streaming" }))
})
```

- [ ] **Step 2: Run the focused store tests and confirm RED**

```powershell
pnpm exec vitest run src/stores/conversation-runtime-store.test.ts src/stores/runtime-live-message-slice-decoupling.test.ts
```

Expected: import failure for `selectHistoricalTimelineTurns`, or current session-keyed caching returns a new array on every live-message replacement.

- [ ] **Step 3: Define a scalar/reference cache key that excludes live content**

```ts
interface HistoricalTimelineCacheKey {
  detail: DbConversationDetail | null
  localTurns: MessageTurn[]
  backgroundTurns: BackgroundOverlayEntry[]
  optimisticTurns: MessageTurn[]
  liveOwnsActiveTurn: boolean
  delegationKickoffText: string | null
  liveMessageId: string | null
  liveStartedAt: number | null
}

interface HistoricalTimelineCacheEntry {
  key: HistoricalTimelineCacheKey
  value: ConversationTimelineTurn[]
}

const historicalTimelineCache = new Map<number, HistoricalTimelineCacheEntry>()

function sameHistoricalKey(
  left: HistoricalTimelineCacheKey,
  right: HistoricalTimelineCacheKey
): boolean {
  return (
    left.detail === right.detail &&
    left.localTurns === right.localTurns &&
    left.backgroundTurns === right.backgroundTurns &&
    left.optimisticTurns === right.optimisticTurns &&
    left.liveOwnsActiveTurn === right.liveOwnsActiveTurn &&
    left.delegationKickoffText === right.delegationKickoffText &&
    left.liveMessageId === right.liveMessageId &&
    left.liveStartedAt === right.liveStartedAt
  )
}
```

The live ID/start fields invalidate once at turn start and once at handoff, but appended text/tool changes do not.

- [ ] **Step 4: Extract historical phases without changing edge-case semantics**

Move these existing blocks verbatim into `computeHistoricalTimeline`:

- persisted assistant suppression for delegation children;
- in-flight persisted-partial suppression keyed by `in_flight_user_turn_id`;
- synthetic delegation kickoff insertion;
- persisted/local/background timestamp merge;
- optimistic turns;
- role-aware duplicate-ID retention.

Remove only Phase 4 (`buildStreamingTurnsFromLiveMessage`) from the historical computation. Use `liveMessageId !== null` wherever the existing algorithm checks `session.liveMessage !== null`; use `liveStartedAt` for the synthetic kickoff timestamp.

- [ ] **Step 5: Keep the compatibility selector explicit**

```ts
export function selectHistoricalTimelineTurns(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  return computeHistoricalTimeline(state, conversationId)
}

export function selectTimelineTurns(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  const historical = computeHistoricalTimeline(state, conversationId)
  const session = state.byConversationId.get(conversationId)
  if (!session?.liveMessage) return historical
  return appendCanonicalStreamingTurns(
    historical,
    conversationId,
    session.liveMessage
  )
}

function appendCanonicalStreamingTurns(
  historical: ConversationTimelineTurn[],
  conversationId: number,
  liveMessage: LiveMessage
): ConversationTimelineTurn[] {
  const built = buildStreamingTurnsFromLiveMessage(conversationId, liveMessage)
  const result = historical.slice()
  for (const [index, turn] of built.turns.entries()) {
    result.push({
      key: `streaming-${conversationId}-${liveMessage.id}-${index}`,
      turn,
      phase: "streaming",
      inProgressToolCallIds: built.inProgressToolCallIds,
    })
  }
  const retainKey = (turn: MessageTurn) => `${turn.role} ${turn.id}`
  const retainIndexByKey = new Map<string, number>()
  result.forEach((entry, index) => {
    const key = retainKey(entry.turn)
    if (!retainIndexByKey.has(key) || entry.turn.role !== "user") {
      retainIndexByKey.set(key, index)
    }
  })
  return retainIndexByKey.size === result.length
    ? result
    : result.filter(
        (entry, index) =>
          retainIndexByKey.get(retainKey(entry.turn)) === index
      )
}
```

Do not mutate the cached historical array.

- [ ] **Step 6: Add all current timeline edge cases to the focused suite**

Port or add explicit assertions for persisted partial suppression, optimistic user dedup, delegation kickoff, background timestamp ordering, user keep-first/assistant keep-last collisions, cross-conversation isolation, and live start/completion invalidation. Each assertion compares both semantic output and reference identity where applicable.

- [ ] **Step 7: Reset and remove cache entries with store lifecycle**

Delete `historicalTimelineCache` entries in `REMOVE_CONVERSATION`, migrate them from old to new ID only by recomputation, and clear the map in `resetConversationRuntimeStore`. Do not retain session objects through cache values after removal.

- [ ] **Step 8: Run focused and existing runtime tests**

```powershell
pnpm exec vitest run src/stores/conversation-runtime-store.test.ts src/stores/runtime-live-message-slice-decoupling.test.ts src/contexts/conversation-runtime-context.test.tsx src/components/message/message-list-view.test.tsx
pnpm eslint src/stores/conversation-runtime-store.ts src/stores/conversation-runtime-store.test.ts
```

Expected: all tests pass; 500 content updates reuse one historical array while real historical changes invalidate it.

- [ ] **Step 9: Commit the historical selector**

```powershell
git add src/stores/conversation-runtime-store.ts src/stores/conversation-runtime-store.test.ts src/stores/runtime-live-message-slice-decoupling.test.ts
git commit -m "refactor(perf): isolate stable message history"
```

---

### Task 10: Add The Pure Projector And `LiveTranscriptStore`

**Files:**
- Create: `src/lib/acp/live-transcript-projector.ts`
- Create: `src/lib/acp/live-transcript-projector.test.ts`
- Create: `src/stores/live-transcript-store.ts`
- Create: `src/stores/live-transcript-store.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/components/conversations/conversation-detail-panel.tsx`
- Modify: `src/components/message/sub-agent-session-dialog.tsx`

**Interfaces:**
- Consumes: committed `AcceptedConnectionFrame` and final canonical `LiveMessage` from Task 7.
- Produces: Cross-Task `LiveTranscriptSnapshot` plus per-conversation, per-segment, and per-tool subscriptions.
- Produces: `createLiveTranscriptFrameSink(conversationId, connectionId)` registered by context key.
- Recovery: a projector exception rebuilds from the already-committed canonical message at the same cursor.

- [ ] **Step 1: Write failing pure projector parity tests**

```ts
it("keeps segment ids stable for text append and isolates tool updates", () => {
  let projection = projectLiveSnapshot(42, "c1", liveMessageWithText("hello"), 1)
  const ids = projection.segmentIds
  const firstText = projection.segments.get(ids[0])

  projection = applyLiveTranscriptEvents(projection, [
    envelope(2, "c1", { type: "content_delta", text: " world" }),
  ])
  expect(projection.segmentIds).toBe(ids)
  expect(projection.segments.get(ids[0])).not.toBe(firstText)

  projection = applyLiveTranscriptEvents(projection, [toolCreate("c1", 3, "t1")])
  const idsAfterTool = projection.segmentIds
  const textAfterTool = projection.segments.get(ids[0])
  projection = applyLiveTranscriptEvents(projection, [toolUpdate("c1", 4, "t1", "done")])
  expect(projection.segmentIds).toBe(idsAfterTool)
  expect(projection.segments.get(ids[0])).toBe(textAfterTool)
})

it.each(agentLiveFixtures)("matches canonical completed turns for $name", ({ conversationId, snapshot, events }) => {
  let projection = projectLiveSnapshot(conversationId, "c1", snapshot, 0)
  projection = applyLiveTranscriptEvents(projection, events)
  const projectedCanonical = liveTranscriptToCanonicalMessage(projection)
  expect(
    buildStreamingTurnsFromLiveMessage(conversationId, projectedCanonical)
  ).toEqual(
    buildStreamingTurnsFromLiveMessage(
      conversationId,
      applyEventsToCanonicalLiveMessage(snapshot, events)
    )
  )
})
```

`agentLiveFixtures` must cover Claude text/thinking/tools, Codex child-agent metadata and generated images, CodeBuddy delegation metadata, Kimi plan replacement, and Grok rich text/tool appends.

- [ ] **Step 2: Run projector tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/acp/live-transcript-projector.test.ts
```

Expected: module import failure.

- [ ] **Step 3: Implement stable IDs and snapshot projection**

Derive IDs only from canonical structure, never array indexes that can be renumbered by appends:

```ts
function segmentId(messageId: string, ordinal: number, kind: string): string {
  return `${messageId}:${kind}:${ordinal}`
}

export function projectLiveSnapshot(
  conversationId: number,
  connectionId: string,
  canonical: LiveMessage,
  lastAppliedSeq: number
): LiveTranscriptSnapshot
```

Walk canonical content once. Consecutive text/thinking blocks retain their canonical boundaries; a tool segment points to `tool_call_id`; a plan uses one stable `:plan:0` ID and appears at the canonical end. Classify an image-bearing tool as `generated-image` but keep its full `ToolCallInfo` in `tools`.

- [ ] **Step 4: Implement event-local updates and parity conversion**

Rules:

- `content_delta`: append to the final text segment only when it is structurally last; otherwise add one new text segment.
- `thinking`: same rule for thinking segments.
- `tool_call`: upsert the tool and add one tool/generated-image segment only if no segment references that ID.
- `tool_call_update`: patch only that tool; honor `raw_output_append`, preserve omitted fields/images, and never reorder segment IDs.
- `plan_update`: remove the prior plan ID from its old position, replace its entries, and append the same stable plan ID at the end.
- other events: leave projection references unchanged except `turn_complete`, which is handled by the store's `markCompleting`.

`liveTranscriptToCanonicalMessage` rebuilds the context-layer `LiveMessage` in segment order and is used only for tests/recovery compatibility, not every render.

- [ ] **Step 5: Write failing store notification tests**

```ts
it("notifies only the changed segment and conversation once", () => {
  const store = createLiveTranscriptStore()
  store.rebuild(42, "c1", liveMessageWithText("a"), 1)
  const snapshot = store.getConversation(42)!
  const textId = snapshot.segmentIds[0]
  const conversationListener = vi.fn()
  const textListener = vi.fn()
  const unrelatedToolListener = vi.fn()
  store.subscribeConversation(42, conversationListener)
  store.subscribeSegment(42, textId, textListener)
  store.subscribeTool(42, "other", unrelatedToolListener)

  store.publish(42, frame([content("c1", 2, "b")]), liveMessageWithText("ab"))
  expect(conversationListener).toHaveBeenCalledTimes(1)
  expect(textListener).toHaveBeenCalledTimes(1)
  expect(unrelatedToolListener).not.toHaveBeenCalled()
  expect(store.getConversation(42)!.segmentIds).toBe(snapshot.segmentIds)
})

it("rebuilds from canonical state without advancing a false cursor", () => {
  const store = createLiveTranscriptStore({ projector: throwingProjector })
  store.rebuild(42, "c1", liveMessageWithText("a"), 10)
  store.publish(42, frame([content("c1", 11, "b")]), liveMessageWithText("ab"))
  expect(store.getConversation(42)?.lastAppliedSeq).toBe(11)
  expect(store.getDebugStats().rebuildCount).toBe(1)
  expect(Array.from(store.getConversation(42)?.segments.values() ?? [])[0]).toMatchObject({
    type: "text",
    text: "ab",
  })
})
```

- [ ] **Step 6: Implement the external store and narrow hooks**

Expose this non-React API plus hooks backed by `useSyncExternalStore`:

```ts
export interface LiveTranscriptStoreApi {
  getConversation(conversationId: number): LiveTranscriptSnapshot | null
  getSegment(conversationId: number, segmentId: string): LiveTranscriptSegment | null
  getTool(conversationId: number, toolCallId: string): ToolCallInfo | null
  subscribeConversation(conversationId: number, callback: () => void): () => void
  subscribeSegment(conversationId: number, segmentId: string, callback: () => void): () => void
  subscribeTool(conversationId: number, toolCallId: string, callback: () => void): () => void
  rebuild(conversationId: number, connectionId: string, canonical: LiveMessage, cursor: number): void
  publish(conversationId: number, frame: AcceptedConnectionFrame, canonical: LiveMessage): void
  markCompleting(conversationId: number, messageId: string): void
  removeIfMessage(conversationId: number, messageId: string): void
  remove(conversationId: number): void
  migrate(fromConversationId: number, toConversationId: number): void
  getDebugStats(): {
    rebuildCount: number
    conversations: number
    segments: number
    tools: number
  }
  reset(): void
}
```

Keep stable null and empty-array snapshots for hooks. `removeIfMessage` compares the current message ID before deleting, so a late completion cannot clear a newer turn. Notify conversation listeners once per publish; notify only changed segment/tool IDs. Do not notify when an event produces no projection change.

- [ ] **Step 7: Extend the provider sink boundary**

Replace `LiveMessageSink` registration with a backward-compatible registration object:

```ts
export interface ConnectionLiveSinks {
  canonical(
    liveMessage: LiveMessage,
    isLive: boolean,
    deliveryIds?: readonly number[]
  ): void
  transcript?: LiveTranscriptFrameSink
}

registerLiveSinks(contextKey: string, sinks: ConnectionLiveSinks): () => void
```

Task 7's frame commit calls `canonical` once, then `transcript.publish` once with the accepted connection frame. Registration immediately replays the current canonical message through `rebuild`, preserving close/reopen and permission-blocked behavior. Snapshot hydrate also calls `rebuild` before listeners.

- [ ] **Step 8: Register roots, rekey, removal, and reset lifecycle**

`ConversationDetailPanel` and `SubAgentSessionDialog` register sinks bound to their runtime conversation IDs. Migrate the live store when the runtime conversation migrates; remove it alongside `removeConversation`; reset it through `registerBackendScopedStoreReset`. On rekey, the context-key registration moves without changing projection segment/tool IDs.

- [ ] **Step 9: Run projector/store/provider tests**

```powershell
pnpm exec vitest run src/lib/acp/live-transcript-projector.test.ts src/stores/live-transcript-store.test.ts src/contexts/acp-connections-context.test.tsx src/components/message/sub-agent-session-dialog.test.tsx
pnpm eslint src/lib/acp/live-transcript-projector.ts src/stores/live-transcript-store.ts src/contexts/acp-connections-context.tsx src/components/conversations/conversation-detail-panel.tsx src/components/message/sub-agent-session-dialog.tsx
```

Expected: all tests pass, including canonical parity for every agent fixture and immediate rebuild on snapshot/reopen.

- [ ] **Step 10: Commit live projection ownership**

```powershell
git add src/lib/acp/live-transcript-projector.ts src/lib/acp/live-transcript-projector.test.ts src/stores/live-transcript-store.ts src/stores/live-transcript-store.test.ts src/contexts/acp-connections-context.tsx src/components/conversations/conversation-detail-panel.tsx src/components/message/sub-agent-session-dialog.tsx
git commit -m "feat(perf): project live transcripts incrementally"
```

---

### Task 11: Render A Live Footer Outside Virtua And Gate P2

**Files:**
- Create: `src/components/message/live-transcript-row.tsx`
- Create: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/components/message/virtualized-message-thread.tsx`
- Create: `src/components/message/virtualized-message-thread.test.tsx`
- Modify: `src/components/message/message-scroll-context.tsx` only if required by the footer coordinator
- Modify: `src/stores/conversation-runtime-store.ts`
- Modify: `src/components/conversations/conversation-detail-panel.tsx`
- Modify: `src/components/message/sub-agent-session-dialog.tsx`
- Create: `docs/superpowers/performance/webview-streaming/p2-100eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p2-500eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p2-1000eps.json`
- Modify: `docs/superpowers/performance/webview-streaming/comparison.md`

**Interfaces:**
- Consumes: stable history from Task 9 and live store from Task 10.
- Produces: `VirtualizedMessageThread.footer?: ReactNode` inside the shared scroll content and outside `Virtualizer` items.
- Produces: `completeLiveTranscriptTurn(conversationId, liveMessage?)` for one no-blank/no-duplicate handoff.
- Gate: historical thread and historical rows render zero additional times during active fixture output.

- [ ] **Step 1: Write failing footer and render-isolation tests**

```tsx
it("keeps the footer outside the Virtua item array", () => {
  render(
    <VirtualizedMessageThread
      items={["history"]}
      getItemKey={(item) => item}
      renderItem={(item) => <div data-testid="history">{item}</div>}
      footer={<div data-testid="live-footer">live</div>}
    />
  )
  expect(virtualizerItems()).toHaveLength(1)
  expect(screen.getByTestId("live-footer")).toBeInTheDocument()
  expect(screen.getByTestId("live-footer")).not.toHaveAttribute("data-virtua-item")
})

it("renders no historical row during 500 live publications", () => {
  const historicalRender = vi.fn()
  const liveRender = vi.fn()
  renderMessageList({ historicalRender, liveRender })
  act(() => {
    for (let index = 0; index < 500; index += 1) {
      publishLiveText(CID, `chunk-${index}`)
    }
  })
  expect(historicalRender).toHaveBeenCalledTimes(1)
  expect(liveRender.mock.calls.length).toBeGreaterThan(1)
})

it("hands off without an empty or duplicate assistant row", () => {
  const view = renderMessageListWithLive("final answer")
  expect(view.assistantTexts()).toEqual(["final answer"])
  act(() => completeLiveTranscriptTurn(CID))
  expect(view.assistantTexts()).toEqual(["final answer"])
})
```

- [ ] **Step 2: Run focused React tests and confirm RED**

```powershell
pnpm exec vitest run src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx
```

Expected: missing footer/live-row interfaces or historical renders increase with live publications.

- [ ] **Step 3: Add a stable footer slot without changing historical item keys**

Extend props:

```ts
interface VirtualizedMessageThreadProps<T> {
  // existing props
  footer?: ReactNode
  footerClassName?: string
}
```

Inside the existing `MessageThreadContent`, render the current empty state only when both `items.length === 0` and `footer == null`; otherwise render Virtua for history and then:

```tsx
{footer ? (
  <div
    data-message-live-footer
    className={cn("mx-auto w-full max-w-3xl px-4 pb-4", footerClassName)}
  >
    {footer}
  </div>
) : null}
```

Do not add the footer to `items`, `getItemKey`, navigation indices, Virtua measurement callbacks, or item padding calculations.

- [ ] **Step 4: Render live segments through narrow subscriptions**

`LiveTranscriptRow` subscribes once to `segmentIds`, then each child subscribes only to its segment/tool:

```tsx
export const LiveTranscriptRow = memo(function LiveTranscriptRow({
  conversationId,
  agentType,
}: LiveTranscriptRowProps) {
  const segmentIds = useLiveTranscriptSegmentIds(conversationId)
  if (segmentIds.length === 0) return <PendingTypingIndicator />
  return (
    <Message from="assistant" data-testid="live-transcript-row">
      <MessageContent>
        <div className="space-y-4">
          {segmentIds.map((segmentId) => (
            <LiveTranscriptSegmentView
              key={segmentId}
              conversationId={conversationId}
              segmentId={segmentId}
              agentType={agentType}
            />
          ))}
        </div>
      </MessageContent>
    </Message>
  )
})
```

P2 text may still use full `MessageResponse`; P3 replaces it. Thinking, plan, tool, and generated-image segments reuse existing visual components through focused adapted parts. Count live-row/tool renders through the P0 recorder.

- [ ] **Step 5: Stop `MessageListView` from subscribing to the full runtime session/live message**

Replace the session-object and full timeline selectors with:

```ts
const timelineTurns = useConversationRuntimeStore((state) =>
  selectHistoricalTimelineTurns(state, conversationId)
)
const sessionSyncState = useConversationRuntimeStore(
  (state) => state.byConversationId.get(conversationId)?.syncState ?? "idle"
)
const hasLiveTranscript = useHasLiveTranscript(conversationId)
```

Build/adapt only historical turns. Supply `<LiveTranscriptRow>` as `footer` when the normalized `incremental_live_transcript` flag is enabled; retain the old compatibility selector/render path behind the false flag. `hasRenderableContent` becomes history-or-live. Pending typing remains a footer concern, not a Virtua item.

Move live stats, plan, and delegation overlays to hooks/selectors that read only their live segment/tool data. Historical overlay data remains derived from stable historical adapted messages. Do not pass a rebuilt live turn through `threadItems`.

- [ ] **Step 6: Coordinate live-to-local completion in one call stack**

Add:

```ts
export function completeLiveTranscriptTurn(
  conversationId: number,
  liveMessage?: LiveMessage | null
): void {
  const live = liveTranscriptStore.getConversation(conversationId)
  if (live) liveTranscriptStore.markCompleting(conversationId, live.messageId)
  useConversationRuntimeStore
    .getState()
    .actions.completeTurn(conversationId, liveMessage)
  if (live) liveTranscriptStore.removeIfMessage(conversationId, live.messageId)
}
```

Call it from the main panel status-edge effect, sub-agent completion/adoption paths, and background `turn_complete` handler. React external-store notifications issued in one synchronous call stack must produce one committed tree; the handoff test verifies no blank/duplicate frame. Keep existing idempotency guards.

- [ ] **Step 7: Preserve navigation, exports, selection, and overlays**

Add tests proving:

- `scrollToIndex` and message-navigation indices refer only to historical items;
- the footer is selectable/copyable and retains `role="log"` ancestry;
- export uses canonical/historical turns, not the UI projection;
- plan/stats/delegation overlay updates do not render historical rows;
- sub-agent dialog uses the same footer and completion coordinator;
- RTL positions and existing logical overlay classes remain unchanged.

- [ ] **Step 8: Run P2 focused tests and commit implementation**

```powershell
pnpm exec vitest run src/stores/conversation-runtime-store.test.ts src/lib/acp/live-transcript-projector.test.ts src/stores/live-transcript-store.test.ts src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx src/components/message/sub-agent-session-dialog.test.tsx
pnpm eslint src/stores/conversation-runtime-store.ts src/stores/live-transcript-store.ts src/components/message/live-transcript-row.tsx src/components/message/message-list-view.tsx src/components/message/virtualized-message-thread.tsx
git add src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/components/message/virtualized-message-thread.tsx src/components/message/virtualized-message-thread.test.tsx src/components/message/message-scroll-context.tsx src/stores/conversation-runtime-store.ts src/components/conversations/conversation-detail-panel.tsx src/components/message/sub-agent-session-dialog.tsx
git commit -m "feat(perf): render live replies outside history"
```

Expected: all tests/lint pass before commit.

- [ ] **Step 9: Capture and review P2 reports**

Build with batching and incremental transcript enabled, rich deferral disabled:

```powershell
$env:CODEG_DESKTOP_ACP_EVENT_BATCHING='1'
$env:CODEG_INCREMENTAL_LIVE_TRANSCRIPT='1'
$env:CODEG_DEFERRED_STREAMING_RICH_CONTENT='0'
pnpm exec tauri build --debug --features test-utils
```

Capture/select median-of-three reports for all profiles as in Task 4. P2 exits only when `historicalRow` and historical-thread render counters add zero during active output, integrity/parity pass, handoff has no blank/duplicate row, and remaining measured cost is localized to the live footer/Markdown/layout.

- [ ] **Step 10: Force-stage and commit P2 evidence**

```powershell
git add -f docs/superpowers/performance/webview-streaming/p2-100eps.json docs/superpowers/performance/webview-streaming/p2-500eps.json docs/superpowers/performance/webview-streaming/p2-1000eps.json docs/superpowers/performance/webview-streaming/comparison.md
git commit -m "docs(perf): record live footer isolation"
```

---

## P3: Incremental Rich Content And Tool Isolation

### Task 12: Add Bounded Caches And Incremental Markdown Documents

**Files:**
- Create: `src/lib/cache/weighted-lru.ts`
- Create: `src/lib/cache/weighted-lru.test.ts`
- Create: `src/lib/markdown/incremental-stream-blocks.ts`
- Create: `src/lib/markdown/incremental-stream-blocks.test.ts`
- Create: `src/components/message/streaming-markdown-document.tsx`
- Create: `src/components/message/streaming-markdown-document.test.tsx`
- Modify: `src/lib/acp/live-transcript-projector.ts`
- Modify: `src/lib/acp/live-transcript-projector.test.ts`
- Modify: `src/stores/live-transcript-store.ts`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify: `src/components/message/content-parts-renderer.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/stores/conversation-runtime-store.ts`

**Interfaces:**
- Produces: `WeightedLruCache<K, V>` with entry and byte budgets, used by Markdown and Task 13 highlighting.
- Produces: Cross-Task `appendStreamingMarkdown` and `completeStreamingMarkdown` functions.
- Extends: text live segments with `document: IncrementalStreamBlocks` while retaining canonical `text` for parity/recovery.
- Produces: a 32-entry/2 MiB memory-only completion partition cache consumed once during live-to-history handoff.

- [ ] **Step 1: Write failing weighted-LRU tests**

```ts
describe("WeightedLruCache", () => {
  it("evicts least-recent entries by entry and byte budgets", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 2,
      maxWeight: 6,
      weightOf: (value) => value.length,
    })
    expect(cache.set("a", "aa")).toBe(true)
    expect(cache.set("b", "bb")).toBe(true)
    expect(cache.get("a")).toBe("aa")
    expect(cache.set("c", "cccc")).toBe(true)
    expect(cache.has("a")).toBe(true)
    expect(cache.has("b")).toBe(false)
    expect(cache.totalWeight).toBe(6)
  })

  it("rejects an entry larger than the entire budget", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 4,
      maxWeight: 3,
      weightOf: (value) => value.length,
    })
    expect(cache.set("oversize", "1234")).toBe(false)
    expect(cache.size).toBe(0)
  })
})
```

- [ ] **Step 2: Run LRU tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/cache/weighted-lru.test.ts
```

Expected: module import failure.

- [ ] **Step 3: Implement deterministic recency, replacement, and reset**

```ts
export class WeightedLruCache<K, V> {
  private readonly entries = new Map<K, { value: V; weight: number }>()
  private weight = 0

  constructor(
    private readonly options: {
      maxEntries: number
      maxWeight: number
      weightOf(value: V, key: K): number
    }
  ) {}

  get size(): number {
    return this.entries.size
  }

  get totalWeight(): number {
    return this.weight
  }

  get(key: K): V | undefined {
    const entry = this.entries.get(key)
    if (!entry) return undefined
    this.entries.delete(key)
    this.entries.set(key, entry)
    return entry.value
  }

  peek(key: K): V | undefined {
    return this.entries.get(key)?.value
  }

  has(key: K): boolean {
    return this.entries.has(key)
  }

  keys(): IterableIterator<K> {
    return this.entries.keys()
  }

  take(key: K): V | undefined {
    const entry = this.entries.get(key)
    if (!entry) return undefined
    this.delete(key)
    return entry.value
  }

  set(key: K, value: V): boolean {
    const weight = Math.max(0, this.options.weightOf(value, key))
    if (weight > this.options.maxWeight) return false
    this.delete(key)
    this.entries.set(key, { value, weight })
    this.weight += weight
    while (
      this.entries.size > this.options.maxEntries ||
      this.weight > this.options.maxWeight
    ) {
      const oldest = this.entries.keys().next().value as K | undefined
      if (oldest === undefined) break
      this.delete(oldest)
    }
    return true
  }

  delete(key: K): boolean {
    const entry = this.entries.get(key)
    if (!entry) return false
    this.entries.delete(key)
    this.weight -= entry.weight
    return true
  }

  clear(): void {
    this.entries.clear()
    this.weight = 0
  }
}
```

`peek` does not change recency; `take` supports the one-use Markdown handoff. Keep all accounting in `set/delete/clear`.

- [ ] **Step 4: Write failing arbitrary-boundary Markdown tests**

Use Streamdown's exported `parseMarkdownIntoBlocks` in production and an injected spy in bounded-work tests:

```ts
it.each([
  ["paragraphs", "one\n\ntwo\n\nthree"],
  ["backtick fence", "before\n\n```ts\nconst x = 1\n```\n\nafter"],
  ["tilde fence", "~~~js\nalert(1)\n~~~\n\nend"],
  ["table", "| a | b |\n| - | - |\n| 1 | 2 |\n\nend"],
  ["math", "text $x$\n\n$$\ny = x\n$$\n"],
  ["CJK", "第一段。\n\n第二段。"],
  ["HTML", "<details>\n<summary>x</summary>\ny\n</details>\n\nend"],
] as const)("reconstructs exact source for %s", (_name, source) => {
  let document = createIncrementalStreamBlocks("segment-1")
  for (const chunk of splitAtEveryCodeUnit(source)) {
    document = appendStreamingMarkdown(document, chunk)
  }
  document = completeStreamingMarkdown(document)
  expect(joinStreamingMarkdown(document)).toBe(source)
  expect(document.tail).toBe("")
  expect(document.valid).toBe(true)
})

it("does not repeatedly parse a long unclosed fence", () => {
  const split = vi.fn(parseMarkdownIntoBlocks)
  let document = createIncrementalStreamBlocks("segment-1", split)
  document = appendStreamingMarkdown(document, "```ts\n")
  for (let index = 0; index < 2_000; index += 1) {
    document = appendStreamingMarkdown(document, `line-${index}\n`)
  }
  expect(split.mock.calls.length).toBeLessThanOrEqual(1)
  expect(document.sealed).toHaveLength(0)
  expect(document.tail).toContain("line-1999")
})

it("seals safe blocks but keeps an unmatched tail at a tool boundary", () => {
  let document = createIncrementalStreamBlocks("segment-1")
  document = appendStreamingMarkdown(document, "done\n\n**unfinished")
  document = sealStreamingMarkdownBoundary(document)
  expect(document.sealed.map((block) => block.markdown).join(""))
    .toBe("done\n\n")
  expect(document.tail).toBe("**unfinished")
})

it("seals a closed fence without waiting for a following block", () => {
  let document = createIncrementalStreamBlocks("segment-1")
  document = appendStreamingMarkdown(document, "```ts\nconst x = 1\n```\n")
  expect(document.sealed.map((block) => block.markdown).join("")).toBe(
    "```ts\nconst x = 1\n```\n"
  )
  expect(document.tail).toBe("")
})

it("does not treat a backtick-prefixed code line as a closing fence", () => {
  let document = createIncrementalStreamBlocks("segment-1")
  document = appendStreamingMarkdown(
    document,
    "```ts\n```not-a-close\nconst x = 1\n"
  )
  expect(document.scanner.fence).not.toBeNull()
  expect(document.sealed).toHaveLength(0)
})

function splitAtEveryCodeUnit(source: string): string[] {
  return Array.from({ length: source.length }, (_, index) => source.slice(index, index + 1))
}
```

- [ ] **Step 5: Run Markdown tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/markdown/incremental-stream-blocks.test.ts
```

Expected: module import failure.

- [ ] **Step 6: Implement an incremental line/fence scanner**

```ts
export interface MarkdownLineScanner {
  pendingLine: string
  fence: null | {
    marker: "`" | "~"
    length: number
    language: string
    openingOffset: number
    bodyOffset: number
  }
  safeBoundarySeen: boolean
  safeBoundaryOffset: number
  safeBoundaryKind: "blank" | "closed_fence" | null
  closedFenceBoundaryOffset: number
  scannedLength: number
}

function scanCompleteLine(
  scanner: MarkdownLineScanner,
  line: string,
  lineOffset: number,
  nextLineOffset: number
): MarkdownLineScanner {
  const marker = line.match(/^ {0,3}(`{3,}|~{3,})([^\r\n]*)$/)
  if (marker) {
    const token = marker[1]
    const kind = token[0] as "`" | "~"
    if (!scanner.fence) {
      return {
        ...scanner,
        fence: {
          marker: kind,
          length: token.length,
          language: marker[2].trim().split(/\s+/, 1)[0] || "text",
          openingOffset: lineOffset,
          bodyOffset: nextLineOffset,
        },
      }
    }
    if (
      scanner.fence.marker === kind &&
      token.length >= scanner.fence.length &&
      marker[2].trim().length === 0
    ) {
      return {
        ...scanner,
        fence: null,
        safeBoundarySeen: true,
        safeBoundaryOffset: nextLineOffset,
        safeBoundaryKind: "closed_fence",
        closedFenceBoundaryOffset: nextLineOffset,
      }
    }
  }
  if (!scanner.fence && line.trim().length === 0) {
    return {
      ...scanner,
      safeBoundarySeen: true,
      safeBoundaryOffset: nextLineOffset,
      safeBoundaryKind: "blank",
    }
  }
  return scanner
}

function scanMarkdownFromScratch(text: string): MarkdownLineScanner {
  return scanMarkdownAppend(
    {
      pendingLine: "",
      fence: null,
      safeBoundarySeen: false,
      safeBoundaryOffset: 0,
      safeBoundaryKind: null,
      closedFenceBoundaryOffset: 0,
      scannedLength: 0,
    },
    text
  )
}

function scanMarkdownAppend(
  scanner: MarkdownLineScanner,
  delta: string
): MarkdownLineScanner {
  const buffered = scanner.pendingLine + delta
  const bufferedOffset = scanner.scannedLength - scanner.pendingLine.length
  let next: MarkdownLineScanner = {
    ...scanner,
    pendingLine: "",
    scannedLength: scanner.scannedLength + delta.length,
  }
  let cursor = 0
  while (cursor < buffered.length) {
    const newline = buffered.indexOf("\n", cursor)
    if (newline === -1) {
      next.pendingLine = buffered.slice(cursor)
      return next
    }
    const line = buffered.slice(cursor, newline).replace(/\r$/, "")
    next = scanCompleteLine(
      next,
      line,
      bufferedOffset + cursor,
      bufferedOffset + newline + 1
    )
    cursor = newline + 1
  }
  next.pendingLine = ""
  return next
}

export function createIncrementalStreamBlocks(
  segmentId: string,
  splitBlocks = parseMarkdownIntoBlocks
): IncrementalStreamBlocks {
  return {
    segmentId,
    sealed: [],
    tail: "",
    sourceLength: 0,
    nextBlockIndex: 0,
    scanner: scanMarkdownFromScratch(""),
    splitBlocks,
    valid: true,
  }
}

export function joinStreamingMarkdown(
  document: IncrementalStreamBlocks
): string {
  return document.sealed.map((block) => block.markdown).join("") + document.tail
}

export function appendStreamingMarkdown(
  document: IncrementalStreamBlocks,
  delta: string
): IncrementalStreamBlocks {
  if (delta.length === 0) return document
  if (!document.valid) {
    return {
      ...document,
      tail: document.tail + delta,
      sourceLength: document.sourceLength + delta.length,
      scanner: scanMarkdownAppend(document.scanner, delta),
    }
  }
  return sealAvailableBlocks({
    ...document,
    tail: document.tail + delta,
    sourceLength: document.sourceLength + delta.length,
    scanner: scanMarkdownAppend(document.scanner, delta),
  })
}
```

Process only complete newly-arrived lines plus `pendingLine`; `scannedLength` and fence offsets are UTF-16 offsets matching `String.slice`. A closing fence accepts only whitespace after a same-kind marker of sufficient length, so `` ```not-a-close `` remains code. While a fence is open, append without calling Streamdown's splitter unless an earlier closed fence can already be sealed. Outside a fence, call the splitter only after a blank line or closed fence set the boundary fields.

- [ ] **Step 7: Seal only complete blocks from the current tail**

```ts
function sealAvailableBlocks(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  const closedFenceOffset = document.scanner.closedFenceBoundaryOffset
  if (closedFenceOffset > 0) {
    const prefix = document.tail.slice(0, closedFenceOffset)
    const tail = document.tail.slice(closedFenceOffset)
    const blocks = document.splitBlocks(prefix)
    if (blocks.join("") !== prefix) {
      return { ...document, valid: false }
    }
    const sealed = blocks.map((markdown, offset) => ({
      id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
      markdown,
    }))
    return {
      ...document,
      sealed: [...document.sealed, ...sealed],
      tail,
      nextBlockIndex: document.nextBlockIndex + sealed.length,
      scanner: scanMarkdownFromScratch(tail),
    }
  }
  if (!document.scanner.safeBoundarySeen || document.scanner.fence) {
    return document
  }
  const blocks = document.splitBlocks(document.tail)
  if (blocks.join("") !== document.tail) {
    return { ...document, valid: false }
  }
  if (blocks.length <= 1) {
    return {
      ...document,
      scanner: {
        ...document.scanner,
        safeBoundarySeen: false,
        safeBoundaryOffset: 0,
        safeBoundaryKind: null,
      },
    }
  }
  const tail = blocks[blocks.length - 1] ?? ""
  const sealed = blocks.slice(0, -1).map((markdown, offset) => ({
    id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
    markdown,
  }))
  return {
    ...document,
    sealed: [...document.sealed, ...sealed],
    tail,
    nextBlockIndex: document.nextBlockIndex + sealed.length,
    scanner: scanMarkdownFromScratch(tail),
  }
}

export function sealStreamingMarkdownBoundary(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  if (document.scanner.fence) return document
  return sealAvailableBlocks({
    ...document,
    scanner: { ...document.scanner, safeBoundarySeen: true },
  })
}

export function completeStreamingMarkdown(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  if (!document.valid || document.tail.length === 0) return document
  const original = joinStreamingMarkdown(document)
  const blocks = document.splitBlocks(document.tail)
  const appended = blocks.map((markdown, offset) => ({
    id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
    markdown,
  }))
  const completed: IncrementalStreamBlocks = {
    ...document,
    sealed: [...document.sealed, ...appended],
    tail: "",
    nextBlockIndex: document.nextBlockIndex + appended.length,
    scanner: scanMarkdownFromScratch(""),
  }
  if (
    original.length === document.sourceLength &&
    joinStreamingMarkdown(completed) === original
  ) {
    return completed
  }
  return {
    ...document,
    sealed: [],
    tail: original,
    scanner: scanMarkdownFromScratch(original),
    valid: false,
  }
}
```

For a closed-fence boundary, split only the prefix through that fence, validate `blocks.join("") === prefix`, and seal every returned block; this lets a completed code block upgrade immediately. For a blank/external boundary, validate the entire tail and seal every block except the last, which may still be extended by a list/table/paragraph. On completion, call the splitter once on the remaining tail and seal every returned block. Verify both source length and `sealed.map(markdown).join("") + tail` against the accumulated source; if any splitter invariant fails, preserve the full joined source with `valid: false` so the caller uses canonical full Markdown.

- [ ] **Step 8: Write failing React render/copy tests**

```tsx
it("renders sealed blocks once while only the tail changes", () => {
  const blockRender = vi.fn()
  const { rerender } = render(
    <StreamingMarkdownDocument document={doc("first\n\ntail")} onBlockRender={blockRender} />
  )
  rerender(
    <StreamingMarkdownDocument document={doc("first\n\ntail grows")} onBlockRender={blockRender} />
  )
  expect(blockRender).toHaveBeenCalledTimes(1)
  expect(screen.getByTestId("streaming-markdown-tail")).toHaveTextContent("tail grows")
})

it("keeps an open code fence plain, selectable, and unhighlighted", () => {
  render(<StreamingMarkdownDocument document={doc("```ts\nconst x = 1")} />)
  const tail = screen.getByTestId("streaming-code-tail")
  expect(tail).toHaveTextContent("const x = 1")
  expect(tail.querySelector("[data-highlighted]")).toBeNull()
  expect(tail).toHaveClass("select-text")
})

it("falls back to visible canonical source when partition validation fails", () => {
  render(<StreamingMarkdownDocument document={invalidDoc("**visible") } />)
  expect(screen.getByText("visible")).toBeInTheDocument()
})

function doc(source: string): IncrementalStreamBlocks {
  return appendStreamingMarkdown(createIncrementalStreamBlocks("segment-1"), source)
}

function invalidDoc(source: string): IncrementalStreamBlocks {
  const document = doc(source)
  return { ...document, sealed: [], tail: source, valid: false }
}
```

- [ ] **Step 9: Implement sealed and lightweight tail rendering**

```tsx
interface Props {
  document: IncrementalStreamBlocks
  onBlockRender?: (blockId: string) => void
}

const SealedBlock = memo(
  function SealedBlock({
    block,
    onRender,
  }: {
    block: SealedMarkdownBlock
    onRender?: (blockId: string) => void
  }) {
    streamingPerfRecorder.countRender("markdownBlock")
    onRender?.(block.id)
    return <MessageResponse mode="static">{block.markdown}</MessageResponse>
  },
  (previous, next) =>
    previous.block.id === next.block.id &&
    previous.block.markdown === next.block.markdown &&
    previous.onRender === next.onRender
)

function getOpenFenceTail(document: IncrementalStreamBlocks): {
  language: string
  prefix: string
  code: string
} | null {
  const fence = document.scanner.fence
  if (!fence) return null
  return {
    language: fence.language || "text",
    prefix: document.tail.slice(0, fence.openingOffset),
    code: document.tail.slice(fence.bodyOffset),
  }
}

export function StreamingMarkdownDocument({ document, onBlockRender }: Props) {
  if (!document.valid) {
    return <MessageResponse mode="streaming">{joinStreamingMarkdown(document)}</MessageResponse>
  }
  const openFence = getOpenFenceTail(document)
  return (
    <div className="space-y-4">
      {document.sealed.map((block) => (
        <SealedBlock
          key={block.id}
          block={block}
          onRender={onBlockRender}
        />
      ))}
      {openFence ? (
        <>
          {openFence.prefix ? (
            <div className="whitespace-pre-wrap break-words text-sm select-text">
              {openFence.prefix}
            </div>
          ) : null}
          <CodeBlockContainer language={openFence.language}>
            <pre data-testid="streaming-code-tail" className="m-0 whitespace-pre-wrap break-words p-4 font-mono text-sm select-text"><code>{openFence.code}</code></pre>
          </CodeBlockContainer>
        </>
      ) : document.tail ? (
        <div data-testid="streaming-markdown-tail" className="whitespace-pre-wrap break-words text-sm select-text">{document.tail}</div>
      ) : null}
    </div>
  )
}
```

React text nodes provide escaping; do not use `dangerouslySetInnerHTML` or imperative DOM append.

- [ ] **Step 10: Attach documents to live text segments and handoff cache**

Extend text segments to `{ id, type: "text", text, document }`. Snapshot projection builds the document exactly once with `appendStreamingMarkdown(createIncrementalStreamBlocks(id), text)`; `content_delta` calls `appendStreamingMarkdown` with only the delta. Before adding a thinking, tool, plan, or generated-image segment, call `sealStreamingMarkdownBoundary` on the preceding text segment. An open fence/unmatched construct stays in its lightweight tail until a later close or turn completion. `LiveTranscriptRow` renders `StreamingMarkdownDocument` when `deferred_streaming_rich_content` is enabled and retains P2 `MessageResponse` behind the false flag.

Create a module-local cache:

```ts
const completedPartitions = new WeightedLruCache<string, IncrementalStreamBlocks>({
  maxEntries: 32,
  maxWeight: 2 * 1024 * 1024,
  weightOf: (document, canonicalTextKey) =>
    utf8Bytes(canonicalTextKey) + utf8Bytes(joinStreamingMarkdown(document)),
})

const utf8Encoder = new TextEncoder()
function utf8Bytes(value: string): number {
  return utf8Encoder.encode(value).byteLength
}
```

At `completeLiveTranscriptTurn`, complete and cache each valid text document by exact canonical text before promoting. Historical `TextPart` calls `completedPartitions.take(text)` so a partition is consumed once; cache miss, invalid document, or joined-text mismatch uses normal `MessageResponse`. Counting both the key and document source makes the 2 MiB budget conservative despite the temporary duplicate text. Clear this cache on backend reset and expose content-free size/weight only to tests/perf reports.

- [ ] **Step 11: Run focused P3 Markdown tests**

```powershell
pnpm exec vitest run src/lib/cache/weighted-lru.test.ts src/lib/markdown/incremental-stream-blocks.test.ts src/components/message/streaming-markdown-document.test.tsx src/lib/acp/live-transcript-projector.test.ts src/stores/live-transcript-store.test.ts src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx
pnpm eslint src/lib/cache src/lib/markdown src/components/message/streaming-markdown-document.tsx src/lib/acp/live-transcript-projector.ts src/stores/live-transcript-store.ts
```

Expected: exact-source parity passes for one-code-unit chunking; the unclosed-fence splitter call count remains bounded; sealed block render counts do not grow with tail updates.

- [ ] **Step 12: Commit incremental Markdown**

```powershell
git add src/lib/cache/weighted-lru.ts src/lib/cache/weighted-lru.test.ts src/lib/markdown/incremental-stream-blocks.ts src/lib/markdown/incremental-stream-blocks.test.ts src/components/message/streaming-markdown-document.tsx src/components/message/streaming-markdown-document.test.tsx src/lib/acp/live-transcript-projector.ts src/lib/acp/live-transcript-projector.test.ts src/stores/live-transcript-store.ts src/components/message/live-transcript-row.tsx src/components/message/content-parts-renderer.tsx src/components/message/message-list-view.test.tsx src/stores/conversation-runtime-store.ts
git commit -m "feat(perf): render streaming Markdown incrementally"
```

---

### Task 13: Defer Code, Math, And Mermaid Engines

**Files:**
- Create: `src/lib/scheduling/idle-work.ts`
- Create: `src/lib/scheduling/idle-work.test.ts`
- Modify: `src/components/ai-elements/heavy-plugins-warmup.tsx`
- Modify: `src/components/ai-elements/heavy-plugins-warmup.test.tsx`
- Modify: `src/components/ai-elements/code-block.tsx`
- Create: `src/components/ai-elements/code-block.test.tsx`
- Modify: `src/components/ai-elements/streamdown-plugins.ts`
- Modify: `src/components/ai-elements/streamdown-plugins.test.ts`
- Modify: `src/components/ai-elements/message.tsx`
- Modify: `src/components/ai-elements/message.test.tsx`
- Modify: `src/components/message/streaming-markdown-document.tsx`

**Interfaces:**
- Consumes: `WeightedLruCache` from Task 12.
- Produces: `scheduleIdleWork(callback, { timeoutMs }) -> () => void` with WebView fallback.
- Produces: `MessageResponse.richContentState?: "complete" | "sealed-streaming"`.
- Policy: live tail uses no rich engines; sealed blocks may render math and idle-highlighted code; Mermaid renders only for completed near-viewport content.

- [ ] **Step 1: Write failing idle scheduler tests**

```ts
it("uses requestIdleCallback and cancels it", () => {
  const request = vi.fn(() => 7)
  const cancel = vi.fn()
  vi.stubGlobal("requestIdleCallback", request)
  vi.stubGlobal("cancelIdleCallback", cancel)
  const dispose = scheduleIdleWork(vi.fn(), { timeoutMs: 1_000 })
  dispose()
  expect(request).toHaveBeenCalledWith(expect.any(Function), { timeout: 1_000 })
  expect(cancel).toHaveBeenCalledWith(7)
})

it("falls back to a cancellable timeout on WKWebView", () => {
  vi.useFakeTimers()
  vi.stubGlobal("requestIdleCallback", undefined)
  const work = vi.fn()
  scheduleIdleWork(work, { timeoutMs: 50 })
  vi.advanceTimersByTime(49)
  expect(work).not.toHaveBeenCalled()
  vi.advanceTimersByTime(1)
  expect(work).toHaveBeenCalledTimes(1)
})
```

- [ ] **Step 2: Run scheduler tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/scheduling/idle-work.test.ts
```

Expected: module import failure.

- [ ] **Step 3: Implement and reuse the scheduler**

```ts
export function scheduleIdleWork(
  callback: () => void,
  options: { timeoutMs: number }
): () => void {
  let active = true
  const run = () => {
    if (!active) return
    active = false
    callback()
  }
  if (typeof window.requestIdleCallback === "function") {
    const handle = window.requestIdleCallback(run, {
      timeout: options.timeoutMs,
    })
    return () => {
      active = false
      if (typeof window.cancelIdleCallback === "function") {
        window.cancelIdleCallback(handle)
      }
    }
  }
  const handle = window.setTimeout(run, options.timeoutMs)
  return () => {
    active = false
    window.clearTimeout(handle)
  }
}
```

Refactor `HeavyPluginsWarmup` to use this helper while preserving all existing pointer/keyboard/idle tests and the current 1,500 ms fallback.

- [ ] **Step 4: Write failing one-inflight and eviction tests for local code blocks**

```tsx
it("starts one highlight for one code-language version", async () => {
  const engine = deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
  const tokenize = vi.fn((code: string) => shikiResult(`${code}-token`))
  const factory = vi.fn(() => engine.promise)
  __setHighlighterFactoryForTest(factory)
  const callbackA = vi.fn()
  const callbackB = vi.fn()
  expect(highlightCode("const x = 1", "ts", callbackA)).toBeNull()
  expect(highlightCode("const x = 1", "ts", callbackB)).toBeNull()
  expect(factory).toHaveBeenCalledTimes(1)
  engine.resolve(fakeHighlighter(tokenize))
  await vi.waitFor(() => expect(callbackA).toHaveBeenCalledTimes(1))
  expect(tokenize).toHaveBeenCalledTimes(1)
  expect(callbackA).toHaveBeenCalledTimes(1)
  expect(callbackB).toHaveBeenCalledTimes(1)
})

it("evicts completed tokens by 128-entry or 8MiB budget", () => {
  for (let index = 0; index < 129; index += 1) {
    __putHighlightCacheForTest(`entry-${index}`, tokenized("x"))
  }
  expect(__getHighlightCacheStatsForTest().entries).toBe(128)
  __resetHighlightCachesForTest()
  __putHighlightCacheForTest("large-a", tokenized("x".repeat(5 * 1024 * 1024)))
  __putHighlightCacheForTest("large-b", tokenized("y".repeat(5 * 1024 * 1024)))
  expect(__getHighlightCacheStatsForTest().bytes).toBeLessThanOrEqual(
    8 * 1024 * 1024
  )
})

it("ignores a stale async result after props change", async () => {
  const engine = deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
  __setHighlighterFactoryForTest(() => engine.promise)
  const { rerender } = render(<CodeBlockContent code="old" language="ts" />)
  rerender(<CodeBlockContent code="new" language="ts" />)
  engine.resolve(fakeHighlighter((code) => shikiResult(`${code}-token`)))
  await vi.waitFor(() => expect(screen.getByText("new-token")).toBeInTheDocument())
  expect(screen.queryByText("old-token")).not.toBeInTheDocument()
})

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

function tokenized(content: string): TokenizedCode {
  return {
    bg: "transparent",
    fg: "inherit",
    tokens: [[{ content, color: "inherit" } as ThemedToken]],
  }
}

function shikiResult(content: string) {
  return { bg: "transparent", fg: "inherit", tokens: tokenized(content).tokens }
}

function fakeHighlighter(
  tokenize: (code: string) => ReturnType<typeof shikiResult>
): HighlighterGeneric<BundledLanguage, BundledTheme> {
  return {
    getLoadedLanguages: () => ["ts"],
    codeToTokens: (code: string) => tokenize(code),
  } as unknown as HighlighterGeneric<BundledLanguage, BundledTheme>
}
```

- [ ] **Step 5: Run code-block tests and confirm RED**

```powershell
pnpm exec vitest run src/components/ai-elements/code-block.test.tsx
```

Expected: duplicate highlight starts or missing cache-test hooks fail.

- [ ] **Step 6: Share in-flight operations and bound completed results**

Use one in-flight map and one weighted LRU:

```ts
const completedTokens = new WeightedLruCache<string, TokenizedCode>({
  maxEntries: 128,
  maxWeight: 8 * 1024 * 1024,
  weightOf: (value, key) =>
    tokenCacheUtf8Bytes(key) + estimateTokenizedCodeBytes(value),
})
const inflightTokens = new Map<string, Promise<TokenizedCode>>()
const subscribers = new Map<string, Set<(result: TokenizedCode) => void>>()
const tokenCacheUtf8Encoder = new TextEncoder()

function tokenCacheUtf8Bytes(value: string): number {
  return tokenCacheUtf8Encoder.encode(value).byteLength
}

function estimateTokenizedCodeBytes(value: TokenizedCode): number {
  return tokenCacheUtf8Bytes(JSON.stringify(value))
}

function getTokensCacheKey(code: string, language: BundledLanguage): string {
  return `github-light+github-dark\0${language}\0${code}`
}
```

The full-source key replaces the current length/first-100/last-100 key, which can collide when only a code block's middle changes. `highlightCode` checks completed, registers the callback, and starts work only when `inflightTokens` lacks the key. On settle, delete in-flight state exactly once; on success cache/notify; on failure retain raw visible tokens and remove that key's subscribers so no stale callback fires. `CodeBlockContent` uses an incrementing request version in addition to the code/language equality check. Export `TokenizedCode` plus exact test hooks `__setHighlighterFactoryForTest`, `__putHighlightCacheForTest`, `__getHighlightCacheStatsForTest`, and `__resetHighlightCachesForTest`; reset all overrides/maps in `afterEach`, and call the production cache reset on backend reset.

- [ ] **Step 7: Write failing rich-policy tests**

```tsx
it("does not request Mermaid for a sealed streaming block", async () => {
  render(
    <MessageResponse richContentState="sealed-streaming">
      {"```mermaid\ngraph TD; A-->B\n```"}
    </MessageResponse>
  )
  await act(async () => {})
  expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
  expect(screen.getByText(/graph TD/)).toBeVisible()
})

it("renders sealed math but never parses a lightweight tail", async () => {
  render(<MessageResponse richContentState="sealed-streaming">{"$x$"}</MessageResponse>)
  await vi.waitFor(() =>
    expect(__getStreamdownPluginDebugStateForTest().requests.math).toBe(1)
  )
  const tail = appendStreamingMarkdown(
    createIncrementalStreamBlocks("tail-1"),
    "$unfinished"
  )
  render(<StreamingMarkdownDocument document={tail} />)
  expect(__getStreamdownPluginDebugStateForTest().requests.math).toBe(1)
})

it("loads completed Mermaid only near the viewport", async () => {
  const observer = installIntersectionObserver(false)
  render(
    <MessageResponse richContentState="complete">
      {"```mermaid\ngraph TD; A-->B\n```"}
    </MessageResponse>
  )
  expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
  observer.enter()
  await vi.waitFor(() =>
    expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(1)
  )
})

function installIntersectionObserver(initiallyVisible: boolean) {
  let callback: IntersectionObserverCallback | null = null
  class FakeIntersectionObserver {
    constructor(next: IntersectionObserverCallback) {
      callback = next
    }
    observe = vi.fn(() => {
      if (initiallyVisible) this.enter()
    })
    disconnect = vi.fn()
    unobserve = vi.fn()
    takeRecords = () => []
    root = null
    rootMargin = "600px 0px"
    thresholds = [0]
    enter() {
      callback?.(
        [{ isIntersecting: true } as IntersectionObserverEntry],
        this as unknown as IntersectionObserver
      )
    }
  }
  let instance: FakeIntersectionObserver | null = null
  vi.stubGlobal(
    "IntersectionObserver",
    class extends FakeIntersectionObserver {
      constructor(next: IntersectionObserverCallback) {
        super(next)
        instance = this
      }
    }
  )
  return {
    enter: () => {
      act(() => instance?.enter())
    },
  }
}
```

- [ ] **Step 8: Implement explicit plugin policy and viewport gating**

Extend `MessageResponseProps` and pass policy to `useStreamdownPlugins`:

```ts
export interface RichContentPolicy {
  code: "disabled" | "idle"
  math: boolean
  mermaid: boolean
}

export function policyFor(
  state: "complete" | "sealed-streaming",
  nearViewport: boolean
): RichContentPolicy {
  return state === "sealed-streaming"
    ? { code: "idle", math: true, mermaid: false }
    : { code: "idle", math: true, mermaid: nearViewport }
}

export type RichContentState = "complete" | "sealed-streaming"

function useNearViewport(enabled: boolean): {
  ref: RefObject<HTMLDivElement | null>
  nearViewport: boolean
} {
  const ref = useRef<HTMLDivElement>(null)
  const [observedNearViewport, setObservedNearViewport] = useState(false)
  useEffect(() => {
    if (!enabled) return
    const element = ref.current
    if (!element) return
    if (typeof IntersectionObserver !== "function") {
      return scheduleIdleWork(() => setObservedNearViewport(true), {
        timeoutMs: 1_000,
      })
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setObservedNearViewport(true)
          observer.disconnect()
        }
      },
      { rootMargin: "600px 0px" }
    )
    observer.observe(element)
    return () => observer.disconnect()
  }, [enabled])
  return { ref, nearViewport: !enabled || observedNearViewport }
}
```

Redefine `MessageResponseProps` as `ComponentProps<typeof Streamdown> & { richContentState?: RichContentState }`; destructure the custom prop so it never reaches Streamdown. Detect whether the normalized text contains Mermaid, call `useNearViewport(richContentState === "complete" && needsMermaid)`, and wrap Streamdown in one stable `<div ref={ref} className="min-w-0">`. Pass `policyFor(richContentState ?? "complete", nearViewport)` into the extended `useStreamdownPlugins(text, policy)`. Its loader must require both syntax detection and policy permission before calling `ensure`. Keep `richContentState` in `MessageResponse`'s memo comparator alongside `children`, so a sealed-to-complete policy change cannot be skipped. Track content-free per-kind `ensure` request counts and expose them through `__getStreamdownPluginDebugStateForTest`; clear them in the existing `__resetStreamdownPluginsForTest`.

Use the observer above with `rootMargin: "600px 0px"`. When unavailable, schedule completed Mermaid through `scheduleIdleWork` with a 1,000 ms timeout. Until enabled or after an engine error, keep source text visible. Do not gate ordinary links/prose/CJK. Add an unmount-before-idle test proving the scheduled state update is canceled.

Wrap the Streamdown code plugin so its first `highlight` call queues underlying work through `scheduleIdleWork`; share pending requests by code/language/theme key. A completed message becomes visible immediately with raw code and upgrades later.

Update `StreamingMarkdownDocument`'s memoized sealed block to pass `richContentState="sealed-streaming"`; historical/default `MessageResponse` calls omit the prop and therefore use `"complete"`.

- [ ] **Step 9: Run rich-engine and existing Markdown tests**

```powershell
pnpm exec vitest run src/lib/scheduling/idle-work.test.ts src/components/ai-elements/heavy-plugins-warmup.test.tsx src/components/ai-elements/code-block.test.tsx src/components/ai-elements/streamdown-plugins.test.ts src/components/ai-elements/message.test.tsx src/components/message/streaming-markdown-document.test.tsx
pnpm eslint src/lib/scheduling src/components/ai-elements/code-block.tsx src/components/ai-elements/streamdown-plugins.ts src/components/ai-elements/message.tsx
```

Expected: all tests pass; one source version starts one highlight; Mermaid source is always visible before/after failure.

- [ ] **Step 10: Commit deferred rich engines**

```powershell
git add src/lib/scheduling/idle-work.ts src/lib/scheduling/idle-work.test.ts src/components/ai-elements/heavy-plugins-warmup.tsx src/components/ai-elements/heavy-plugins-warmup.test.tsx src/components/ai-elements/code-block.tsx src/components/ai-elements/code-block.test.tsx src/components/ai-elements/streamdown-plugins.ts src/components/ai-elements/streamdown-plugins.test.ts src/components/ai-elements/message.tsx src/components/ai-elements/message.test.tsx src/components/message/streaming-markdown-document.tsx
git commit -m "perf(markdown): defer rich rendering engines"
```

---

### Task 14: Add Per-Tool Subscriptions, Lazy Bodies, And Gate P3

**Files:**
- Modify: `src/stores/live-transcript-store.ts`
- Modify: `src/stores/live-transcript-store.test.ts`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/message/content-parts-renderer.tsx`
- Create: `src/components/message/content-parts-renderer.test.tsx`
- Modify focused tests for terminal, diff, delegation, and background-task components reached by the live renderer
- Create: `docs/superpowers/performance/webview-streaming/p3-100eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p3-500eps.json`
- Create: `docs/superpowers/performance/webview-streaming/p3-1000eps.json`
- Modify: `docs/superpowers/performance/webview-streaming/comparison.md`

**Interfaces:**
- Consumes: `useLiveTranscriptTool(conversationId, toolCallId)` from Task 10.
- Produces: cheap live group summaries and per-tool card subscriptions.
- Preserves: canonical completed tool output and existing 200,000-character backend/frontend cap.
- Gate: unrelated live tools and all historical rows render zero times for a one-tool update.

- [ ] **Step 1: Write failing per-tool render and append tests**

```tsx
it("updates one tool card without rendering siblings", () => {
  const renders = new Map<string, number>()
  seedLiveTools(CID, [tool("a"), tool("b"), tool("c")])
  render(<LiveTranscriptRow conversationId={CID} agentType="grok" onToolRender={(id) => renders.set(id, (renders.get(id) ?? 0) + 1)} />)
  renders.clear()
  act(() => publishToolUpdate(CID, "b", { status: "completed" }))
  expect(renders).toEqual(new Map([["b", 1]]))
})

it("preserves ordered append chunks and visible tail cap", () => {
  seedLiveTools(CID, [tool("a", { raw_output: "head\n" })])
  act(() => {
    publishToolAppend(CID, "a", "one\n")
    publishToolAppend(CID, "a", "two\n")
  })
  const record = liveTranscriptStore.getTool(CID, "a")!
  expect(record.raw_output).toBe("head\none\ntwo\n")
  expect(selectRunningOutputTail(record, 8)).toBe("ne\ntwo\n")
})
```

- [ ] **Step 2: Write failing collapsed/lazy-body tests**

```tsx
it("does not construct collapsed group children", () => {
  render(<ContentPartsRenderer parts={[groupOf50Tools()]} />)
  expect(screen.getAllByRole("button")).toHaveLength(1)
  fireEvent.click(screen.getByRole("button"))
  expect(screen.getAllByRole("button")).toHaveLength(51)
})

it("defers structured input and diff parsing until expansion", () => {
  render(<ToolCallPart part={completedEditTool()} />)
  expect(parseStructuredInputSpy).not.toHaveBeenCalled()
  expect(generateUnifiedDiffSpy).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole("button"))
  expect(parseStructuredInputSpy).toHaveBeenCalledTimes(1)
  expect(generateUnifiedDiffSpy).toHaveBeenCalledTimes(1)
})

it("keeps running command output plain and bounded", () => {
  render(<ToolCallPart part={runningCommandWithOutput("x".repeat(30_000))} />)
  expect(screen.getByRole("log").textContent?.length).toBeLessThanOrEqual(24_000)
  expect(screen.queryByTestId("markdown-response")).not.toBeInTheDocument()
})
```

- [ ] **Step 3: Run tool tests and confirm RED**

```powershell
pnpm exec vitest run src/stores/live-transcript-store.test.ts src/components/message/live-transcript-row.test.tsx src/components/message/content-parts-renderer.test.tsx
```

Expected: sibling render counts increase, collapsed children mount, or expensive parsers run before expansion.

- [ ] **Step 4: Maintain incremental group summaries in the live store**

Add one record per structural live tool group:

```ts
export interface LiveToolGroupSummary {
  id: string
  toolCallIds: readonly string[]
  counts: Readonly<Record<ToolKindLabel, number>>
  runningCount: number
  errorCount: number
}
```

On tool create, append the ID and increment its classified count. On status/error update, adjust only running/error totals. Expose `getToolGroup`/`subscribeToolGroup`; collapsed rendering reads this summary and never maps full tool records.

- [ ] **Step 5: Render each live tool through its own subscription**

```tsx
const LiveToolCard = memo(function LiveToolCard({
  conversationId,
  toolCallId,
}: LiveToolCardProps) {
  const tool = useLiveTranscriptTool(conversationId, toolCallId)
  if (!tool) return null
  streamingPerfRecorder.countRender("toolCard")
  return <ToolCallPart part={adaptLiveTool(tool)} live />
})
```

`LiveTranscriptSegmentView` passes only `conversationId` and `toolCallId`; it must not subscribe to the complete transcript/tool map. Generated image and delegation specializations follow the same per-tool selector.

- [ ] **Step 6: Make hidden bodies genuinely unmounted**

Split the current monolithic tool component into a cheap header model and body component. For generic completed tools, mount `ToolCallBody` only while open. For running command tools, mount only the bounded plain terminal tail. For specialized always-visible cards (permission/question/goal/delegation/image), preserve current semantics but move structured parsing into the visible child.

Change group rendering to:

```tsx
{open ? (
  <CollapsibleContent>
    <div className="mt-3 w-full space-y-3">
      {part.items.map((item) => (
        <ToolCallPart key={item.toolCallId} part={item} />
      ))}
    </div>
  </CollapsibleContent>
) : null}
```

For a live group, map IDs to `LiveToolCard` only inside the `open` branch. Summary text uses the aggregate counts, status, and error state.

- [ ] **Step 7: Preserve completion and expand-during-stream behavior**

Add tests proving:

- expanding mid-stream shows the full currently-capped canonical output, then receives ordered appends;
- collapsing mid-stream stops body renders while header status still updates;
- completion while collapsed does not parse Markdown/JSON/diff;
- first expansion after completion parses each body once;
- truncation indicators remain visible and authoritative;
- permission/question/error/nested-agent states match the current renderer;
- completed adapted tool output deep-equals the canonical historical renderer.

- [ ] **Step 8: Run the focused tool/Markdown/live suite and commit**

```powershell
pnpm exec vitest run src/stores/live-transcript-store.test.ts src/components/message/live-transcript-row.test.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/agent-tool-call.test.tsx src/components/message/delegation-status-group-card.test.tsx src/components/message/background-task-card.test.tsx src/components/diff/unified-diff-preview.test.tsx
pnpm eslint src/stores/live-transcript-store.ts src/components/message/live-transcript-row.tsx src/components/message/content-parts-renderer.tsx
git add src/stores/live-transcript-store.ts src/stores/live-transcript-store.test.ts src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/message/content-parts-renderer.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/agent-tool-call.test.tsx src/components/message/delegation-status-group-card.test.tsx src/components/message/background-task-card.test.tsx src/components/diff/unified-diff-preview.test.tsx
git commit -m "perf(tools): isolate live tool updates"
```

Expected: all existing files that actually exist in the touched renderer path pass. If a named focused test file is introduced by this task, create it in the same RED/GREEN cycle before staging; do not stage unrelated test files.

- [ ] **Step 9: Capture the P3 reports with all three flags enabled**

```powershell
$env:CODEG_DESKTOP_ACP_EVENT_BATCHING='1'
$env:CODEG_INCREMENTAL_LIVE_TRANSCRIPT='1'
$env:CODEG_DEFERRED_STREAMING_RICH_CONTENT='1'
pnpm exec tauri build --debug --features test-utils
```

Capture/select median-of-three reports for all profiles. P3 passes only when:

- batch-to-paint P95 is `< 100 ms` for 100/500/1,000 eps;
- input-to-paint P95 is `< 50 ms` for all profiles;
- no recorded/fallback-inferred main-thread task exceeds 200 ms;
- queued visual cadence is at least 30 updates/second;
- historical rows and unrelated tools add zero renders;
- final Markdown and tool parity tests pass;
- integrity/checksum remain exact.

- [ ] **Step 10: Decide worker follow-up from evidence, not speculation**

If the selected P3 report attributes any `> 50 ms` long task to Shiki tokenization after idle deferral, add one measured follow-up row to `comparison.md` with trace timestamps and estimated worker scope. Do not implement a worker in this plan. If no such task exists, record `Shiki worker: not justified by P3 trace`.

- [ ] **Step 11: Force-stage and commit P3 evidence**

```powershell
git add -f docs/superpowers/performance/webview-streaming/p3-100eps.json docs/superpowers/performance/webview-streaming/p3-500eps.json docs/superpowers/performance/webview-streaming/p3-1000eps.json docs/superpowers/performance/webview-streaming/comparison.md
git commit -m "docs(perf): record incremental rendering gains"
```

---

## P4: Scroll, Layout, Platform Hardening, And Release Gate

### Task 15: Coordinate Streaming Scroll, Bound Memory, And Harden Recovery

**Files:**
- Modify: `src/components/ai-elements/message-thread.tsx`
- Modify: `src/components/message/virtualized-message-thread.tsx`
- Modify: `src/components/message/virtualized-message-thread.test.tsx`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/components/message/message-scroll-context.tsx`
- Modify conditionally under the measured decision rule: `src/app/globals.css`
- Modify: `src/lib/cache/weighted-lru.ts`
- Modify: `src/lib/markdown/incremental-stream-blocks.ts`
- Modify: `src/components/ai-elements/code-block.tsx`
- Modify: `src/stores/live-transcript-store.ts`
- Modify: `src/lib/acp/streaming-performance-config.ts`
- Modify: `src-tauri/src/acp/streaming_performance.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify relevant recovery tests from Tasks 5-14

**Interfaces:**
- Produces: explicit `isStreaming` resize/follow behavior and one footer correction per committed publication.
- Produces: `resetStreamingPerformanceCaches()` and cache stats for soak assertions.
- Changes: all three internal flags default on only after P3 passes; environment variables remain able to disable them independently subject to downward normalization.
- Preserves: user escape from bottom, visible anchor, selection, navigation indices, and legacy fallbacks.

- [ ] **Step 1: Write failing scroll-follow and escape tests**

```tsx
it("uses one instant correction for one live publication while following", () => {
  const scroll = createStickToBottomHarness({ atBottom: true })
  renderLiveThread(scroll)
  act(() => publishLiveText(CID, "next"))
  act(() => runAnimationFrames())
  expect(scroll.scrollToBottom).toHaveBeenCalledTimes(1)
  expect(scroll.scrollToBottom).toHaveBeenCalledWith({
    animation: "instant",
    preserveScrollPosition: true,
  })
})

it.each(["wheel", "touchstart", "pointerdown", "PageUp"])(
  "stops following immediately on %s",
  (escape) => {
    const scroll = createStickToBottomHarness({ atBottom: true })
    renderLiveThread(scroll)
    act(() => dispatchScrollEscape(escape))
    act(() => publishLiveText(CID, "growth"))
    act(() => runAnimationFrames())
    expect(scroll.stopScroll).toHaveBeenCalled()
    expect(scroll.scrollToBottom).not.toHaveBeenCalled()
    expect(scroll.scrollTop()).toBe(scroll.anchorScrollTop)
  }
)

it("coalesces text and sealed-block height changes into one correction", () => {
  const scroll = createStickToBottomHarness({ atBottom: true })
  renderLiveThread(scroll)
  act(() => {
    publishLiveText(CID, "paragraph\n\n")
    sealLatestMarkdownBlock(CID)
  })
  act(() => runAnimationFrames())
  expect(scroll.scrollToBottom).toHaveBeenCalledTimes(1)
})
```

- [ ] **Step 2: Run focused scroll tests and confirm RED**

```powershell
pnpm exec vitest run src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx -t "instant correction|stops following|coalesces"
```

Expected: current smooth resize or repeated footer growth produces wrong/multiple corrections.

- [ ] **Step 3: Make resize behavior explicit at the thread root**

Keep the component default for non-streaming callers, but pass an exact value from the message list:

```tsx
<MessageThread
  className="flex-1 min-h-0"
  resize={hasLiveTranscript ? "instant" : "smooth"}
>
```

Remove `shouldUseSmoothResize`; streaming state now changes only at turn start/end and does not subscribe to text. Keep `initial="instant"` in `MessageThread`.

- [ ] **Step 4: Implement follow intent independent of post-growth `isAtBottom`**

The footer coordinator owns a boolean intent that becomes false only on user escape and true again only when a scroll event reaches bottom or the user presses the existing down button. This avoids reading `isAtBottom === false` after footer growth and incorrectly treating it as user intent.

```ts
export interface LiveFooterScrollCoordinator {
  scheduleFollow(publicationVersion: number): void
  cancelForUserInput(): void
  markAtBottom(): void
  dispose(): void
}
```

Keep one pending RAF. `scheduleFollow` replaces the pending publication version without scheduling a second RAF. The RAF calls:

```ts
scrollToBottom({
  animation: "instant",
  preserveScrollPosition: true,
})
```

only when follow intent is true. Register passive `wheel`/`touchstart`, non-interactive primary-pointer selection, and keyboard PageUp/Home/ArrowUp handlers on the scroll viewport; call `stopScroll` and cancel the pending RAF immediately. Reuse the thread's existing interactive-element selector so pointerdown on buttons, links, inputs, tool expanders, or the down button does not cancel follow intent. Do not prevent native scrolling or selection.

- [ ] **Step 5: Prove Virtua history and navigation remain stable**

Add tests that:

- 500 footer height changes do not change Virtua `items`, keys, or historical measurement callbacks;
- `scrollToIndex(0)` and the last historical index still target the same turns with a footer present;
- the down button follows to the footer, not merely the final Virtua item;
- expanding a live tool while at bottom produces one correction;
- expanding while scrolled away preserves `scrollTop` and visible history;
- reopening mid-stream rebuilds the footer and follows only when the restored viewport was at bottom;
- keyboard scrolling and RTL layout retain existing behavior.

- [ ] **Step 6: Add cache reset ownership and memory-pressure capability fallback**

```ts
export interface StreamingPerformanceCacheStats {
  markdownEntries: number
  markdownBytes: number
  highlightEntries: number
  highlightBytes: number
}

export function resetStreamingPerformanceCaches(): void {
  resetCompletedMarkdownPartitions()
  resetHighlightCaches()
}
```

Call reset on backend-scoped reset/logout tests. Remove per-conversation documents/tools on `removeConversation`; migrate without duplication. If `"onmemorypressure" in window`, register a listener that clears completed Markdown/highlight caches but not active live/canonical state; otherwise register nothing. Clean up the listener on provider teardown.

Add tests that reset returns all entry/byte counts to zero, active live state survives completed-cache eviction, and 100 sequential completed conversations leave no live-store entries after removal.

- [ ] **Step 7: Run the measured layout/overscan experiment with a fixed rule**

On the Windows 1,000 eps fixture and a 500-row synthetic history, capture three runs for each Virtua `bufferSize` candidate: 400, 800, and 1,200. Select the smallest candidate with zero blank-frame assertions during fast PageUp/PageDown/wheel scrolling. Change the default from 800 only when the candidate also improves layout+paint P95 by at least 10% over 800.

Evaluate historical-row `content-visibility: auto` plus `contain-intrinsic-size: auto 240px` in the same harness. Add the CSS only when it improves layout+paint P95 by at least 10% and every Virtua measurement, selection, sticky overlay, accessibility, and navigation test passes. Otherwise retain no new historical-row containment and record that decision in `comparison.md`.

This is a closed decision rule: no unmeasured CSS or overscan tuning is permitted.

- [ ] **Step 8: Add a deterministic 20-turn soak assertion**

Run `grok_rich_v1` twenty times at 1,000 eps in one process, completing/removing each disposable conversation before the next. The resulting report must assert:

- completion cache `entries <= 32` and `bytes <= 2 MiB`;
- highlight cache `entries <= 128` and `bytes <= 8 MiB`;
- live transcript conversation/tool/segment counts return to their pre-run values;
- no event gaps/duplicates across any run;
- when `performance.memory` exists, post-quiet used heap is no more than the first post-run heap plus max(20%, 32 MiB); when absent, record `heapMeasurement: "unsupported"` and rely on deterministic store/cache bounds.

Do not require or call a non-standard forced GC API.

- [ ] **Step 9: Write failing recovery/fallback matrix tests**

Add focused tests for:

```text
startup batcher unavailable -> legacy only, snapshot succeeds
runtime batch emit failure -> stop, recover affected snapshots, restart alert
frontend seq gap -> pause only that connection, snapshot, contiguous resume
projection throw -> canonical rebuild, no false cursor
invalid Markdown partition -> visible canonical source
Shiki rejection -> raw code remains visible
math/Mermaid rejection -> source remains visible
no PerformanceObserver longtask -> RAF gap + drift metrics
no requestIdleCallback -> timeout scheduling
no IntersectionObserver -> completed Mermaid idle fallback
```

For every error log assertion, verify it contains event type/reason but not event payload text or tool fields.

- [ ] **Step 10: Flip defaults only after the P3 gate and keep opt-out fallbacks**

Change the default flags in `StreamingPerformanceFlags::from_env` to all true. Keep explicit environment false values and downward normalization. Add tests:

```rust
#[test]
fn release_defaults_enable_the_complete_path() {
    let flags = StreamingPerformanceFlags::from_lookup(|_| None);
    assert!(flags.desktop_acp_event_batching);
    assert!(flags.incremental_live_transcript);
    assert!(flags.deferred_streaming_rich_content);
}

#[test]
fn disabling_batching_disables_dependent_paths() {
    let flags = StreamingPerformanceFlags::from_lookup(|name| {
        (name == "CODEG_DESKTOP_ACP_EVENT_BATCHING").then_some("0".into())
    });
    assert_eq!(flags, StreamingPerformanceFlags::legacy());
}
```

In `lib.rs`, enable main-window devtools only under `#[cfg(feature = "test-utils")]` so release-like reference builds can invoke the harness while ordinary release builds do not expose it:

```rust
#[cfg(feature = "test-utils")]
let builder = builder.devtools(true);
```

- [ ] **Step 11: Run focused P4 tests and all recovery suites**

```powershell
pnpm exec vitest run src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx src/lib/cache/weighted-lru.test.ts src/lib/markdown/incremental-stream-blocks.test.ts src/components/ai-elements/code-block.test.tsx src/components/ai-elements/streamdown-plugins.test.ts src/lib/acp/event-ingestor.test.ts src/stores/live-transcript-store.test.ts
cd src-tauri
cargo test --features test-utils desktop_event_batcher
cargo test --features test-utils streaming_performance
```

Expected: every scroll, bound, flag, failure, and fallback test passes.

- [ ] **Step 12: Commit P4 hardening**

Inspect `git diff -- src/app/globals.css`; stage it only if Step 7's evidence selected containment. Then stage exact changed files and commit:

```powershell
git add src/components/ai-elements/message-thread.tsx src/components/message/virtualized-message-thread.tsx src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/components/message/message-scroll-context.tsx src/lib/cache/weighted-lru.ts src/lib/markdown/incremental-stream-blocks.ts src/components/ai-elements/code-block.tsx src/stores/live-transcript-store.ts src/lib/acp/streaming-performance-config.ts src-tauri/src/acp/streaming_performance.rs src-tauri/src/lib.rs
git diff --quiet -- src/app/globals.css
if ($LASTEXITCODE -ne 0) { git add src/app/globals.css }
git commit -m "perf(streaming): harden scroll and recovery"
```

---

### Task 16: Run Final Verification And Produce Release Evidence

**Files:**
- Create: `docs/superpowers/performance/webview-streaming/final-100eps.json`
- Create: `docs/superpowers/performance/webview-streaming/final-500eps.json`
- Create: `docs/superpowers/performance/webview-streaming/final-1000eps.json`
- Modify: `docs/superpowers/performance/webview-streaming/comparison.md`
- Create: `docs/superpowers/performance/webview-streaming/platform-smoke.md`
- Create: `docs/superpowers/performance/webview-streaming/rollout.md`
- Modify only implementation files required by failures directly caused by Tasks 1-15.

**Interfaces:**
- Verifies every acceptance criterion in the approved design.
- Produces: final Windows absolute-gate reports, cross-platform functional evidence, failure/privacy evidence, and one-release rollout/removal criteria.
- Does not remove legacy listener code or internal flags.

- [ ] **Step 1: Run all focused performance/correctness tests together**

```powershell
pnpm exec vitest run src/lib/perf/streaming-perf-report.test.ts src/lib/perf/streaming-perf-recorder.test.ts src/lib/acp/event-ingestor.test.ts src/lib/acp/live-transcript-projector.test.ts src/stores/conversation-runtime-store.test.ts src/stores/live-transcript-store.test.ts src/lib/cache/weighted-lru.test.ts src/lib/markdown/incremental-stream-blocks.test.ts src/components/message/streaming-markdown-document.test.tsx src/components/ai-elements/code-block.test.tsx src/components/ai-elements/streamdown-plugins.test.ts src/components/message/content-parts-renderer.test.tsx src/components/message/virtualized-message-thread.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx src/contexts/acp-connections-context.test.tsx src/components/chat/message-input.test.tsx
```

Expected: every listed file passes with zero failed tests and no unhandled promise rejection.

- [ ] **Step 2: Run full frontend repository verification**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: ESLint, the complete Vitest suite, TypeScript/static export, and Next build all exit 0.

- [ ] **Step 3: Run full desktop Rust verification**

```powershell
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all commands exit 0 with no warnings. Batcher paused-time, backpressure, fixture, metrics, shutdown, and failure tests are included.

- [ ] **Step 4: Run server and MCP compatibility verification**

```powershell
cd src-tauri
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all commands exit 0; neither server nor MCP requires Tauri desktop delivery, and server attach tests remain per-envelope.

- [ ] **Step 5: Build the release reference binary with replay capability**

Clear all overrides so release defaults are measured:

```powershell
Remove-Item Env:CODEG_DESKTOP_ACP_EVENT_BATCHING -ErrorAction SilentlyContinue
Remove-Item Env:CODEG_INCREMENTAL_LIVE_TRANSCRIPT -ErrorAction SilentlyContinue
Remove-Item Env:CODEG_DEFERRED_STREAMING_RICH_CONTENT -ErrorAction SilentlyContinue
pnpm exec tauri build --features test-utils
```

Expected: a release binary is built with a production static frontend; its capability report shows batched/all-three flags true and test replay available. A normal build without `test-utils` remains the shipping configuration and omits replay/devtools.

- [ ] **Step 6: Capture final Windows enabled-acceleration reports**

Confirm `disable_hardware_acceleration: false`, restart the app, then capture three runs per profile with seed 49374 and select medians exactly as Tasks 4/8/11/14. Save the selected reports as `final-100eps.json`, `final-500eps.json`, and `final-1000eps.json`.

Every selected report must show:

- batch-to-paint P95 `< 100 ms`;
- input-to-paint P95 `< 50 ms`;
- long-task max `< 200 ms` or equivalent fallback max when unsupported;
- visual cadence `>= 30` updates/second while queued;
- `integrity.ok === true`, 1,223 applied events, zero gaps/duplicates, exact checksum;
- historical active-output render delta 0;
- unrelated tool render delta 0;
- cache/store bounds within Task 15 budgets.

If any target fails, do not label the work complete. Return to the owning task identified by the report, add a focused RED/GREEN correction, rerun all affected phase gates, and replace the final reports only after passing.

- [ ] **Step 7: Run Windows disabled-acceleration qualitative smoke**

Enable the existing Disable Hardware Acceleration setting, restart, and run the 1,000 eps fixture once. Record integrity, interaction usability, fallback metrics, and any severe regression in `platform-smoke.md`; do not apply the enabled-acceleration absolute timing gate to this run. Restore the setting afterward.

- [ ] **Step 8: Run macOS WKWebView and Linux WebKitGTK functional smoke**

On each platform, build/run the same test-utils fixture and record:

- all 1,223 events and final checksum;
- no missing capability crash for longtask, idle callback, or IntersectionObserver;
- typing/probe, selection/copy, keyboard scroll, wheel/touch escape, permission/question/cancel, expanded tools, RTL, and reopen-mid-stream behavior;
- startup legacy override and snapshot recovery;
- no severe pause/jump observed at 1,000 eps.

Absolute Windows timings are reported separately and are not imposed on WKWebView/WebKitGTK.

- [ ] **Step 9: Exercise the complete failure/privacy matrix in a test build**

Run injected startup batcher failure, runtime emit failure, sequence gap, projector throw, invalid Markdown partition, highlighter rejection, math/Mermaid rejection, and missing browser-capability tests. For each, record recovery result in `platform-smoke.md`.

Scan all report files:

```powershell
rg -n 'prompt|response|raw_input|raw_output|tool_call_id|中文流式输出|Fast Grok output' docs/superpowers/performance/webview-streaming/*.json
```

Expected: no matches. Field names such as `inputToPaint` are camelCase and do not match lowercase `prompt/input` terms; any content-bearing match is a release blocker.

- [ ] **Step 10: Complete before/after and phase attribution**

In `comparison.md`, include P0/P1/P2/P3/final rows for each rate with exact P50/P95/max timings, callbacks, transactions, live publications, historical/live/Markdown/tool renders, long tasks/frame gaps, and cadence. Attribute gains only when adjacent phase reports show them.

State the final result for each acceptance criterion explicitly:

1. no observed pause-then-jump on the reference workload;
2. backend/apply/commit/paint costs separated;
3. bounded ordered desktop batches;
4. server/remote semantics unchanged;
5. one transaction/publication per connection/frame;
6. stable history references;
7. unfinished tail is the only streaming Markdown change;
8. heavy engines/tool bodies deferred without final-content change;
9. bottom-follow respects user escape;
10. control/reconnect/completion paths correct;
11. reports contain no user content;
12. Windows targets and repository checks pass.

- [ ] **Step 11: Document rollout ownership and removal criteria**

`rollout.md` must contain this exact ownership table:

| Flag | Owner | Disabled fallback | Removal condition |
| --- | --- | --- | --- |
| `desktop_acp_event_batching` | `DesktopAcpDelivery` | `acp://event` legacy emit/listener | one stable release with zero delivery integrity/recovery incidents |
| `incremental_live_transcript` | `MessageListView` + `LiveTranscriptStore` | canonical live turn in timeline | batching stable and projection parity telemetry/tests clean for one release |
| `deferred_streaming_rich_content` | `StreamingMarkdownDocument` | current full `MessageResponse` path | incremental transcript stable and rich fallback errors clean for one release |

Document environment variable names, metric fields, failure signal, snapshot recovery, report command, and the rule that flags/listener are not removed in this change.

- [ ] **Step 12: Review final diff and generated artifacts**

```powershell
git diff --check
git status --short
git diff --stat f296e5cd..HEAD
git diff -- src-tauri/src/web/event_bridge.rs src/contexts/acp-connections-context.tsx src/stores/conversation-runtime-store.ts src/components/message/message-list-view.tsx
```

Expected: no whitespace errors or generated build artifacts are staged; changes are confined to the plan's file map plus scoped failure corrections. Preserve all pre-existing unrelated dirty/untracked files.

- [ ] **Step 13: Commit any verification-only corrections**

Only if Steps 1-9 required a scoped correction, stage each exact implementation/test path and run:

```powershell
git diff --cached --check
git commit -m "test(perf): complete streaming hardening"
```

Do not create an empty commit.

- [ ] **Step 14: Format, force-stage, and commit release evidence**

```powershell
pnpm exec prettier --write docs/superpowers/performance/webview-streaming/README.md docs/superpowers/performance/webview-streaming/comparison.md docs/superpowers/performance/webview-streaming/platform-smoke.md docs/superpowers/performance/webview-streaming/rollout.md
git add -f docs/superpowers/performance/webview-streaming/final-100eps.json docs/superpowers/performance/webview-streaming/final-500eps.json docs/superpowers/performance/webview-streaming/final-1000eps.json docs/superpowers/performance/webview-streaming/README.md docs/superpowers/performance/webview-streaming/comparison.md docs/superpowers/performance/webview-streaming/platform-smoke.md docs/superpowers/performance/webview-streaming/rollout.md
git diff --cached --check
git commit -m "docs(perf): publish WebView optimization evidence"
```

Expected: final evidence is committed; legacy flags/listener remain for the observation release.

---

## Phase Review Checkpoints

- **P0 review:** deterministic fixture integrity, local content-free reports, and measured stage attribution are present before any optimization.
- **P1 review:** desktop callback/store work scales with batches/frames; server and raw subscribers retain order and parity.
- **P2 review:** active output causes zero historical-row renders; snapshot/reconnect/rekey/handoff parity passes.
- **P3 review:** incremental Markdown/tool isolation meets the Windows responsiveness targets with final rich-content parity.
- **P4 review:** scroll escape, memory bounds, recovery, privacy, cross-platform smoke, full repository checks, and release reports all pass.

No checkpoint may be waived because a later phase appears visually faster. Any changed boundary or removed acceptance criterion requires returning to the approved design.
