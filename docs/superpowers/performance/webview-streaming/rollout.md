# WebView streaming performance — one-release rollout / removal

This document owns the **observation release** after Tasks 1–16. Flags and the legacy listener remain for one stable release; they are **not** removed in this change.

## Ownership

| Flag | Owner | Disabled fallback | Removal condition |
| --- | --- | --- | --- |
| `desktop_acp_event_batching` | `DesktopAcpDelivery` | `acp://event` legacy emit/listener | one stable release with zero delivery integrity/recovery incidents |
| `incremental_live_transcript` | `MessageListView` + `LiveTranscriptStore` | canonical live turn in timeline | batching stable and projection parity telemetry/tests clean for one release |
| `deferred_streaming_rich_content` | `StreamingMarkdownDocument` | current full `MessageResponse` path | incremental transcript stable and rich fallback errors clean for one release |

## Environment variable names

| Flag (capability / report) | Process env override |
| --- | --- |
| `desktop_acp_event_batching` | `CODEG_DESKTOP_ACP_EVENT_BATCHING` (`1`/`true` enable, `0`/`false` disable) |
| `incremental_live_transcript` | `CODEG_INCREMENTAL_LIVE_TRANSCRIPT` |
| `deferred_streaming_rich_content` | `CODEG_DEFERRED_STREAMING_RICH_CONTENT` |

Release defaults (after Task 15): all three **true**, with downward normalization (incremental/deferred require batching). Env false still disables.

**Known validation limits (merge caveats):** absolute gates and fixture integrity are **Windows-only** in this branch. macOS WKWebView / Linux WebKitGTK smoke were not executed in-session. Frontend integrity now counts **accepted envelopes + SHA-256 of committed content_delta text** (not backend emit counters). Prefer env opt-out (`=0`) if a platform shows delivery/integrity incidents before those smokes land.

## Metric fields (content-free)

Use reports from `window.__codegStreamingPerf` / desktop metrics snapshot only:

| Area | Fields |
| --- | --- |
| Delivery | `desktop_batch_count`, `desktop_batch_event_count`, `desktop_legacy_emit_count`, `desktop_emit_failure_count`, `desktop_startup_fallback_count`, `desktop_runtime_failure_count` |
| Pipeline | `deliveryCallbacks`, `ingestorFrames`, `connectionTransactions`, `livePublications`, `reactCommits`, `paints` |
| Timings | `receiptToTransaction`, `transactionToLivePublication`, `batchToCommit`, `batchToPaint`, `inputToPaint`, `longTasks` (p50/p95/max) |
| Isolation | `renders.historicalThread`, `historicalRow`, `liveRow`, `markdownBlock`, `toolCard` |
| Integrity | `integrity.ok`, `appliedEvents`, `gapCount`, `duplicateCount`, `finalTextSha256` |
| Cadence | `cadence.updatesPerSecond` |
| Resources | heap / cache counters when supported (no content bodies) |

## Failure signals

| Signal | Meaning | First response |
| --- | --- | --- |
| `integrity.ok === false` or gap/duplicate &gt; 0 | Frontend apply integrity break (accepted count / text hash / gap / dup) | Disable `CODEG_DESKTOP_ACP_EVENT_BATCHING=0` (forces legacy + dependents off) |
| Capability query fails / listener stays not-ready | FE cannot invent delivery mode; no subscribe | Restart app; check `acp_get_desktop_delivery_capabilities`; do **not** hot-switch |
| Runtime delivery failure alert / prompts blocked | Batch emit terminal; stream fail-closed | Reload conversation; inspect `desktop_runtime_failure_count` |
| `desktop_startup_fallback_count` rising (backend) | Batcher start failed server-side | Confirm backend legacy emit + FE capability mode still match; file incident |
| Projector rebuild storms / parity test fail | Live transcript projection drift | Disable `CODEG_INCREMENTAL_LIVE_TRANSCRIPT=0` |
| Rich engine / Shiki / Mermaid fallback error rate spike | Deferred rich path unhealthy | Disable `CODEG_DEFERRED_STREAMING_RICH_CONTENT=0` |
| Content leakage in reports | Privacy blocker | Stop publishing reports; scrub pipeline |

## Snapshot recovery

- On attach/reconnect, ordered snapshot + live tail must re-establish without inventing sequences.
- Seq-gap: pause ordered apply, resume after contiguous events; logs content-free.
- Projector throw: rebuild from canonical runtime state; no false cursor advance.
- Backend-scoped store reset clears completed Markdown/highlight caches under memory pressure.

## Report command (local harness)

```powershell
# Flags ON (or rely on release defaults)
$env:CODEG_DESKTOP_ACP_EVENT_BATCHING = "1"
$env:CODEG_INCREMENTAL_LIVE_TRANSCRIPT = "1"
$env:CODEG_DEFERRED_STREAMING_RICH_CONTENT = "1"

# test-utils binary loads production frontend via custom-protocol
pnpm build
cd src-tauri
cargo build --features test-utils --features tauri/custom-protocol

# In WebView (DevTools or temporary CDP automation only):
# await window.__codegStreamingPerf.run({ rateProfile: "eps_1000", seed: 49374, download: true })
```

Median selection: three runs per profile; order by `timings.batchToPaint.p95`; keep middle report.

## Removal rule (this change)

- **Do not** remove `desktop_acp_event_batching`, `incremental_live_transcript`, `deferred_streaming_rich_content`, or the legacy `acp://event` listener in this release.
- Remove only after the ownership table’s removal conditions are met for one stable release with zero related integrity/recovery incidents and clean parity telemetry.
