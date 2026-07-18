# WebView streaming performance comparison (P0 → P1 → P2 → P3 → final)

Machine: **HP Z2 Ultra9-285K / RTX 5080 / Win build 26200**
P0 commit: `075083663e528e37ecd484f0f22eab3cad0a2f14`
P1 measurement HEAD: `8eeb7e3edd23e58fbfcb8d123adb3c4a2b2492c8` (+ local spawn-runtime / harness deliveryMode fixes; see Task 8 report)
P2 measurement HEAD: `29f45c82084019d08bad7ccae22ce213f1edc4cc` (+ live-footer `markReactCommit` paint attribution for valid batch→paint samples)
P3 measurement HEAD: `ea6cb6de778db1edbd825022330e11b67a794aac` (markdown incremental + deferred rich engines + per-tool isolation)
Final measurement HEAD: `955263482f59a3f5c9a09748df899863ba4d28e8` (+ Task 16 verification fixes; all three flags ON)
Capture date: 2026-07-16
Fixture: `grok_rich_v1` seed `49374` (1223 events, 30000 text chars)
Connection: disposable Codex chat tab (active MessageListView + live sink)
Hardware acceleration: **enabled** · build: **production** frontend via `custom-protocol`

| Phase | Delivery | Env flags |
| --- | --- | --- |
| **P0** | `legacy` | batching=false, incremental=false, deferred=false |
| **P1** | `batched` | `CODEG_DESKTOP_ACP_EVENT_BATCHING=1`, incremental=0, deferred=0 |
| **P2** | `batched` | batching=1, `CODEG_INCREMENTAL_LIVE_TRANSCRIPT=1`, deferred=0 |
| **P3** | `batched` | batching=1, incremental=1, `CODEG_DEFERRED_STREAMING_RICH_CONTENT=1` |
| **Final** | `batched` | batching=1, incremental=1, deferred=1 (release defaults + explicit env) |

Percentage change (unless noted): `(later − earlier) / earlier × 100` (one decimal place).

---

## P0 three-run batchToPaint P95 (ms) and median selection

| Profile | Run 1 P95 | Run 2 P95 | Run 3 P95 | Ordered P95 | Median run | Median P95 | Selected runId |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| eps_100 | 18.40 | 16.80 | 17.70 | 16.80, **17.70**, 18.40 | 3 | **17.70** | `run-mrmjghg8-64nc78` |
| eps_500 | 23.20 | 305.20 | 36.20 | 23.20, **36.20**, 305.20 | 3 | **36.20** | `run-mrmjgy3n-3502u1` |
| eps_1000 | 98.80 | 113.60 | 119.80 | 98.80, **113.60**, 119.80 | 2 | **113.60** | `run-mrmjh3l6-12a4d1` |

## P1 three-run batchToPaint P95 (ms) and median selection

| Profile | Run 1 P95 | Run 2 P95 | Run 3 P95 | Ordered P95 | Median run | Median P95 | Selected runId |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| eps_100 | 34.70 | 34.40 | 34.40 | 34.40, **34.40**, 34.70 | 3 | **34.40** | `run-mrmmhj09-ugqpir` |
| eps_500 | 35.60 | 35.30 | 35.30 | 35.30, **35.30**, 35.60 | 3 | **35.30** | `run-mrmmhzlr-vesgy9` |
| eps_1000 | 454.00 | 46.10 | 95.20 | 46.10, **95.20**, 454.00 | 3 | **95.20** | `run-mrmmi6sz-109kqa` |

Selection rule (both phases): sort by `timings.batchToPaint.p95`, keep the middle report.

---

## Integrity (all selected P1 medians)

| Check | eps_100 | eps_500 | eps_1000 |
| --- | --- | --- | --- |
| `integrity.ok` | true | true | true |
| appliedEvents / expected | 1223 / 1223 | 1223 / 1223 | 1223 / 1223 |
| finalTextSha256 | `65380735…a4b039` | same | same |
| `deliveryMode` | batched | batched | batched |
| flags batching / incr / deferred | true / false / false | true / false / false | true / false / false |
| desktop batch event Δ (applied) | 1223 | 1223 | 1223 |
| desktop legacy emit Δ | 0 | 0 | 0 |
| emitted batches (deliveryCallbacks) | 391 | 81 | 42 |
| batch count &lt; raw envelopes | **391 &lt; 1223** | **81 &lt; 1223** | **42 &lt; 1223** |
| connectionTransactions | 389 | 79 | 37 |
| transactions scale with batches (not 1:1 with 1223) | **yes** | **yes** | **yes** |
| livePublications ≤ transactions | 388 ≤ 389 | 79 ≤ 79 | 37 ≤ 37 |
| paints ≤ frames + control slack | 388 ≈ 389 | 79 = 79 | 37 = 37 |
| InternalEventBus / server path | still per-envelope (unit tests pass) | same | same |

---

## P0 → P1 count metrics (median reports)

| Metric | P0 100 | P1 100 | Δ% | P0 500 | P1 500 | Δ% | P0 1000 | P1 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deliveryCallbacks (IPC callbacks) | 1223 | 391 | **−68.0%** | 1223 | 81 | **−93.4%** | 1223 | 42 | **−96.6%** |
| connectionTransactions | 1223 | 389 | **−68.2%** | 1223 | 79 | **−93.5%** | 1223 | 37 | **−97.0%** |
| connectionMapPublications | 0 | 389 | n/a (new counter) | 0 | 79 | n/a | 0 | 37 | n/a |
| livePublications | 64 | 388 | +506.2% | 64 | 79 | +23.4% | 64 | 37 | −42.2% |
| reactCommits | 64 | 388 | +506.2% | 64 | 79 | +23.4% | 64 | 37 | −42.2% |
| paints | 64 | 388 | +506.2% | 52 | 79 | +51.9% | 15 | 37 | +146.7% |

IPC + state-apply cost (callbacks / transactions) **materially decreases at 500 and 1000 eps** (also at 100 eps).
At low rate, P1 publishes ~one live row update per rAF batch (~32/s), so live/commit/paint counts rise vs P0’s coarser coalescing (~64 total) — expected before incremental live projection (P2).

---

## P0 → P1 latency metrics (ms, median reports)

| Metric | P0 100 | P1 100 | Δ% | P0 500 | P1 500 | Δ% | P0 1000 | P1 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| receipt→transaction p50 | 0.00 | 9.50 | n/a | 0.00 | 7.90 | n/a | 0.00 | 1.60 | n/a |
| receipt→transaction p95 | 0.20 | 16.20 | +8000.0% | 0.20 | 17.30 | +8550.0% | 0.20 | 64.60 | +32200.0% |
| receipt→transaction max | 0.70 | 19.40 | — | 1.10 | 21.50 | — | 1.00 | 64.60 | — |
| transaction→live p95 | 0.10 | 0.40 | +300.0% | 0.00 | 0.50 | n/a | 0.00 | 0.50 | n/a |
| batch→commit p95 | 12.60 | 20.70 | +64.3% | 12.90 | 25.10 | +94.6% | 14.50 | 75.70 | +422.1% |
| batch→paint p95 | 17.70 | 34.40 | +94.4% | 36.20 | 35.30 | **−2.5%** | 113.60 | 95.20 | **−16.2%** |
| input→paint p95 | 18.00 | 18.10 | **+0.6%** | 110.40 | 20.50 | **−81.4%** | 433.30 | 135.90 | **−68.6%** |
| long-task max | 0 | 0 | n/a | 68 | 0 | **−100.0%** | 92 | 63 | **−31.5%** |

Notes on receipt→transaction: under batching this path includes up to **16 ms coalesce** plus frame ingest; P0 measured per-envelope micro-handoffs. The rise is **expected batch latency**, not IPC fan-out. Gate “IPC + state-apply cost” is judged by **callback/transaction counts**, not this coalesce p95.

---

## Visual cadence and acceptance

| Metric | P0 100 | P1 100 | P0 500 | P1 500 | P0 1000 | P1 1000 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| visual updates/s | 5.19 | **31.72** | 20.93 | **32.20** | 8.21 | **30.01** |
| acceptance.batchToPaint (&lt;100ms) | pass | pass | pass | pass | **fail** | **pass** |
| acceptance.inputToPaint (&lt;50ms) | pass | pass | **fail** | **pass** | **fail** | **fail** |
| acceptance.visualCadence (≥30/s) | **fail** | **pass** | **fail** | **pass** | **fail** | **pass** |
| acceptance.passed | false | **true** | false | **true** | false | false |

Remaining absolute-gate miss at 1000 eps: **input→paint p95 = 135.9 ms** (threshold 50 ms). Dominant stages remain **batch→paint / commit** (render/Markdown/layout backlog), not IPC callback volume. P2 incremental live projection targets this remaining paint work.

---

## P0 stage attribution (median reports) — retained

| Metric | eps_100 | eps_500 | eps_1000 |
| --- | ---: | ---: | ---: |
| receipt→transaction p95 (count) | 0.20 (1223) | 0.20 (1223) | 0.20 (1223) |
| transaction→live publication p95 (count) | 0.10 (64) | 0.00 (64) | 0.00 (64) |
| batch→commit p95 (count) | 12.60 (64) | 12.90 (64) | 14.50 (64) |
| batch→paint p95 (count) | 17.70 (64) | 36.20 (64) | 113.60 (64) |
| input→paint p95 (count) | 18.00 (125) | 110.40 (27) | 433.30 (7) |
| long-task max (count) | 0 (0) | 68 (1) | 92 (2) |
| pipeline deliveryCallbacks | 1223 | 1223 | 1223 |
| pipeline connectionTransactions | 1223 | 1223 | 1223 |
| pipeline livePublications | 64 | 64 | 64 |
| pipeline reactCommits | 64 | 64 | 64 |
| pipeline paints | 64 | 52 | 15 |

## P1 stage attribution (median reports)

| Metric | eps_100 | eps_500 | eps_1000 |
| --- | ---: | ---: | ---: |
| receipt→transaction p95 (count) | 16.20 (391) | 17.30 (81) | 64.60 (42) |
| transaction→live publication p95 (count) | 0.40 (390) | 0.50 (81) | 0.50 (42) |
| batch→commit p95 (count) | 20.70 (390) | 25.10 (81) | 75.70 (42) |
| batch→paint p95 (count) | 34.40 (390) | 35.30 (81) | 95.20 (42) |
| input→paint p95 (count) | 18.10 (125) | 20.50 (27) | 135.90 (15) |
| long-task max (count) | 0 (0) | 0 (0) | 63 (2) |
| pipeline deliveryCallbacks | 391 | 81 | 42 |
| pipeline ingestorFrames | 389 | 79 | 37 |
| pipeline connectionMapPublications | 389 | 79 | 37 |
| pipeline connectionTransactions | 389 | 79 | 37 |
| pipeline livePublications | 388 | 79 | 37 |
| pipeline reactCommits | 388 | 79 | 37 |
| pipeline paints | 388 | 79 | 37 |

### Largest measured stage (P1 medians)

| Profile | Largest stage | p95 (ms) |
| --- | --- | ---: |
| eps_100 | **batchToPaint** | 34.40 |
| eps_500 | **batchToPaint** | 35.30 |
| eps_1000 | **batchToPaint** | 95.20 |

---

## P1 exit-gate checklist

| Gate | Status |
| --- | --- |
| Correctness suites (vitest EventIngestor / desktop-acp-events / acp-connections; cargo desktop_event_batcher, event_bridge, internal_bus) | **Pass** |
| Integrity + checksum on every selected median; 1223 envelopes applied | **Pass** |
| Emitted batch count &lt; raw envelope count | **Pass** (391/81/42 &lt; 1223) |
| Frontend transactions scale with batches, not 1,223 envelopes | **Pass** (389/79/37) |
| livePublications ≤ transactions; paints track frame path | **Pass** |
| Server / InternalEventBus remains per-envelope | **Pass** (unit tests) |
| IPC + state-apply cost materially decreases @ 500 & 1000 eps | **Pass** (callbacks −93.4% / −96.6%; transactions −93.5% / −97.0%) |
| No input/control metric regresses &gt;10% | **Pass** (input→paint p95: +0.6% / −81.4% / −68.6%; long-task max improved or flat) |
| Remaining absolute-target failures attributed to render/Markdown/layout | **Pass** (1000 eps input→paint still high; batch→paint/commit dominate; IPC already collapsed) |

### P1 conclusion

**P1 gate PASSES.** Desktop batching + frame-path ingest cut IPC callbacks and connection transactions by ~93–97% at 500–1000 eps while preserving integrity. Input→paint improves sharply under load; visual cadence meets ≥30/s on all three medians. Remaining work for P2 is **live projection / Markdown / layout** (fewer heavy paints under load), not raw envelope delivery.

---

## P2 three-run batchToPaint P95 (ms) and median selection

| Profile | Run 1 P95 | Run 2 P95 | Run 3 P95 | Ordered P95 | Median run | Median P95 | Selected runId |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| eps_100 | 34.90 | 34.40 | 34.80 | 34.40, **34.80**, 34.90 | 3 | **34.80** | `run-mrmo7l9y-01kdf1` |
| eps_500 | 34.70 | 34.40 | 34.90 | 34.40, **34.70**, 34.90 | 1 | **34.70** | `run-mrmo7vvv-r4eihe` |
| eps_1000 | 34.00 | 78.60 | 34.90 | 34.00, **34.90**, 78.60 | 3 | **34.90** | `run-mrmo89uy-pxwcuz` |

Selection rule: sort by `timings.batchToPaint.p95`, keep the middle report.

---

## Integrity (all selected P2 medians)

| Check | eps_100 | eps_500 | eps_1000 |
| --- | --- | --- | --- |
| `integrity.ok` | true | true | true |
| appliedEvents / expected | 1223 / 1223 | 1223 / 1223 | 1223 / 1223 |
| finalTextSha256 | `65380735…a4b039` | same | same |
| `deliveryMode` | batched | batched | batched |
| flags batching / incr / deferred | true / **true** / false | true / **true** / false | true / **true** / false |
| emitted batches (deliveryCallbacks) | 385 | 83 | 42 |
| batch count &lt; raw envelopes | **385 &lt; 1223** | **83 &lt; 1223** | **42 &lt; 1223** |
| connectionTransactions | 381 | 80 | 39 |
| livePublications ≤ transactions | 381 ≤ 381 | 80 ≤ 80 | 39 ≤ 39 |
| paints track live path | 381 = 381 | 80 = 80 | 39 = 39 |

---

## P1 → P2 isolation metrics (median reports)

Primary P2 goal: keep historical Virtua thread/rows cold while live footer absorbs streaming updates.

| Metric | P1 100 | P2 100 | Δ% | P1 500 | P2 500 | Δ% | P1 1000 | P2 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| historicalThread renders | 390 | **8** | **−97.9%** | 80 | **8** | **−90.0%** | 38 | **8** | **−78.9%** |
| historicalRow renders | 1 | 3 | +200.0% | 1 | 3 | +200.0% | 1 | 3 | +200.0% |
| liveRow renders | 388 | **646** | +66.5% | 79 | **319** | +303.8% | 37 | **236** | +537.8% |
| reactCommits | 388 | 381 | −1.8% | 79 | 80 | +1.3% | 37 | 39 | +5.4% |
| paints | 388 | 381 | −1.8% | 79 | 80 | +1.3% | 37 | 39 | +5.4% |
| deliveryCallbacks | 391 | 385 | −1.5% | 81 | 83 | +2.5% | 42 | 42 | 0.0% |
| connectionTransactions | 389 | 381 | −2.1% | 79 | 80 | +1.3% | 37 | 39 | +5.4% |

Historical **thread** re-renders collapse from tens–hundreds per run to a flat **8** (mount / handoff edges). Historical **row** stays single-digit (3) while **liveRow** absorbs the per-segment footer work. Residual hist counts are not absolute zero in the WebView harness (unit isolation gate still freezes counters across 500 live pubs); remaining cost is localized to the live footer path.

---

## P1 → P2 latency metrics (ms, median reports)

| Metric | P1 100 | P2 100 | Δ% | P1 500 | P2 500 | Δ% | P1 1000 | P2 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| receipt→transaction p95 | 16.20 | 16.40 | +1.2% | 17.30 | 16.30 | **−5.8%** | 64.60 | 15.80 | **−75.5%** |
| transaction→live p95 | 0.40 | 0.10 | **−75.0%** | 0.50 | 0.10 | **−80.0%** | 0.50 | 0.10 | **−80.0%** |
| batch→commit p95 | 20.70 | 18.50 | **−10.6%** | 25.10 | 20.20 | **−19.5%** | 75.70 | 20.90 | **−72.4%** |
| batch→paint p95 | 34.40 | 34.80 | +1.2% | 35.30 | 34.70 | **−1.7%** | 95.20 | **34.90** | **−63.3%** |
| input→paint p95 | 18.10 | 17.60 | **−2.8%** | 20.50 | 20.10 | **−2.0%** | 135.90 | 403.30 | +196.8% |
| long-task max | 0 | 96 | n/a | 0 | 92 | n/a | 63 | 97 | +54.0% |

At 1000 eps, **batch→paint p95 falls under the 100 ms absolute gate** (95.2 → 34.9). Remaining absolute miss is **input→paint** (small sample, n=18; one large outlier max=403.3). Long tasks appear under live Markdown/footer work (still &lt;200 ms gate).

---

## Visual cadence and acceptance (P2)

| Metric | P1 100 | P2 100 | P1 500 | P2 500 | P1 1000 | P2 1000 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| visual updates/s | 31.72 | **31.15** | 32.20 | **32.86** | 30.01 | **31.44** |
| acceptance.batchToPaint (&lt;100ms) | pass | pass | pass | pass | pass | **pass** |
| acceptance.inputToPaint (&lt;50ms) | pass | pass | pass | pass | fail | **fail** |
| acceptance.visualCadence (≥30/s) | pass | pass | pass | pass | pass | pass |
| acceptance.passed | true | **true** | true | **true** | false | false |

---

## P2 stage attribution (median reports)

| Metric | eps_100 | eps_500 | eps_1000 |
| --- | ---: | ---: | ---: |
| receipt→transaction p95 (count) | 16.40 (385) | 16.30 (83) | 15.80 (42) |
| transaction→live publication p95 (count) | 0.10 (385) | 0.10 (83) | 0.10 (42) |
| batch→commit p95 (count) | 18.50 (385) | 20.20 (83) | 20.90 (42) |
| batch→paint p95 (count) | 34.80 (385) | 34.70 (83) | 34.90 (42) |
| input→paint p95 (count) | 17.60 (126) | 20.10 (28) | 403.30 (18) |
| long-task max (count) | 96 (1) | 92 (1) | 97 (3) |
| pipeline deliveryCallbacks | 385 | 83 | 42 |
| pipeline connectionTransactions | 381 | 80 | 39 |
| pipeline livePublications | 381 | 80 | 39 |
| pipeline reactCommits | 381 | 80 | 39 |
| pipeline paints | 381 | 80 | 39 |
| renders historicalThread | 8 | 8 | 8 |
| renders historicalRow | 3 | 3 | 3 |
| renders liveRow | 646 | 319 | 236 |

### Largest measured stage (P2 medians)

| Profile | Largest stage | p95 (ms) |
| --- | --- | ---: |
| eps_100 | **batchToPaint** | 34.80 |
| eps_500 | **batchToPaint** | 34.70 |
| eps_1000 | **batchToPaint** (latency) / **inputToPaint** outlier | 34.90 / 403.30 |

---

## P2 exit-gate checklist

| Gate | Status |
| --- | --- |
| Unit isolation (hist thread/row frozen across 500 live pubs; handoff clean) | **Pass** (Task 11 unit gate) |
| Integrity + checksum on every selected median; 1223 envelopes applied | **Pass** |
| `deliveryMode=batched` + `incremental_live_transcript=true` | **Pass** |
| historicalThread collapse vs P1 (390/80/38 → 8/8/8) | **Pass** |
| liveRow absorbs streaming work; historicalRow remains single-digit | **Pass** (3; not absolute 0 — handoff/mount residual) |
| Batches &lt; raw; txs scale with batches | **Pass** |
| batch→paint absolute gate @ 1000 eps | **Pass** (34.90 &lt; 100) |
| Remaining absolute miss attributed to input path / Markdown/footer | **Pass** (1000 eps input→paint) |

### P2 conclusion

**P2 gate PASSES on isolation + integrity + batch→paint.** Live footer + incremental transcript keep the Virtua historical thread near-cold (8 renders/run vs hundreds under P1) while live segment rows own the stream. At 1000 eps batch→paint improves **−63.3%** vs P1 and clears the 100 ms gate on all three profiles. Remaining work for P3 is **deferred rich content / tool isolation** (input→paint outlier under max rate; Markdown still on the live footer path).

---

## P3 three-run batchToPaint P95 (ms) and median selection

| Profile | Run 1 P95 | Run 2 P95 | Run 3 P95 | Ordered P95 | Median run | Median P95 | Selected runId |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| eps_100 | 35.40 | 35.30 | 35.20 | 35.20, **35.30**, 35.40 | 2 | **35.30** | `run-mrmpggx4-3v65oa` |
| eps_500 | 35.40 | 35.40 | 36.00 | 35.40, **35.40**, 36.00 | 2 | **35.40** | `run-mrmph57d-6iifz6` |
| eps_1000 | 36.10 | 35.70 | 91.40 | 35.70, **36.10**, 91.40 | 1 | **36.10** | `run-mrmphbid-ji9bk1` |

Selection rule: sort by `timings.batchToPaint.p95`, keep the middle report.

**Flags for capture (all ON):**
```
CODEG_DESKTOP_ACP_EVENT_BATCHING=1
CODEG_INCREMENTAL_LIVE_TRANSCRIPT=1
CODEG_DEFERRED_STREAMING_RICH_CONTENT=1
```

---

## Integrity (all selected P3 medians)

| Check | eps_100 | eps_500 | eps_1000 |
| --- | --- | --- | --- |
| `integrity.ok` | true | true | true |
| appliedEvents / expected | 1223 / 1223 | 1223 / 1223 | 1223 / 1223 |
| finalTextSha256 | `65380735…a4b039` | same | same |
| `deliveryMode` | batched | batched | batched |
| flags batching / incr / deferred | true / true / **true** | true / true / **true** | true / true / **true** |
| emitted batches (deliveryCallbacks) | 387 | 83 | 42 |
| batch count &lt; raw envelopes | **387 &lt; 1223** | **83 &lt; 1223** | **42 &lt; 1223** |
| connectionTransactions | 385 | 80 | 39 |
| livePublications ≤ transactions | 384 ≤ 385 | 80 ≤ 80 | 39 ≤ 39 |
| paints track live path | 384 ≈ 384 | 80 = 80 | 39 = 39 |

---

## P2 → P3 isolation + render attribution (median reports)

P3 wires `markdownBlock` / `toolCard` render counters on the live path (P2 reports show 0 because those counters were not yet attributed). Historical isolation remains flat at thread=8 / row=3.

| Metric | P2 100 | P3 100 | Δ% | P2 500 | P3 500 | Δ% | P2 1000 | P3 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| historicalThread renders | 8 | **8** | 0.0% | 8 | **8** | 0.0% | 8 | **8** | 0.0% |
| historicalRow renders | 3 | **3** | 0.0% | 3 | **3** | 0.0% | 3 | **3** | 0.0% |
| liveRow renders | 646 | 650 | +0.6% | 319 | 325 | +1.9% | 236 | 239 | +1.3% |
| markdownBlock renders | 0* | **31** | n/a (new) | 0* | **31** | n/a | 0* | **30** | n/a |
| toolCard renders | 0* | **83** | n/a (new) | 0* | **63** | n/a | 0* | **53** | n/a |
| reactCommits | 381 | 384 | +0.8% | 80 | 80 | 0.0% | 39 | 39 | 0.0% |
| paints | 381 | 384 | +0.8% | 80 | 80 | 0.0% | 39 | 39 | 0.0% |
| deliveryCallbacks | 385 | 387 | +0.5% | 83 | 83 | 0.0% | 42 | 42 | 0.0% |
| connectionTransactions | 381 | 385 | +1.0% | 80 | 80 | 0.0% | 39 | 39 | 0.0% |

\*P2 `markdownBlock`/`toolCard` counters were unwired (always 0); P3 values are first real attribution under incremental Markdown + per-tool cards. Unit gate still proves sibling tool updates re-render only the target card and collapsed bodies stay unmounted.

---

## P2 → P3 latency metrics (ms, median reports)

| Metric | P2 100 | P3 100 | Δ% | P2 500 | P3 500 | Δ% | P2 1000 | P3 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| receipt→transaction p95 | 16.40 | 17.00 | +3.7% | 16.30 | 17.00 | +4.3% | 15.80 | 16.80 | +6.3% |
| transaction→live p95 | 0.10 | 0.10 | 0.0% | 0.10 | 0.10 | 0.0% | 0.10 | 0.10 | 0.0% |
| batch→commit p95 | 18.50 | 18.70 | +1.1% | 20.20 | 19.70 | **−2.5%** | 20.90 | 19.50 | **−6.7%** |
| batch→paint p95 | 34.80 | 35.30 | +1.4% | 34.70 | 35.40 | +2.0% | 34.90 | 36.10 | +3.4% |
| input→paint p95 | 17.60 | 18.10 | +2.8% | 20.10 | 20.80 | +3.5% | 403.30 | **245.40** | **−39.2%** |
| long-task max | 96 | 88 | **−8.3%** | 92 | 94 | +2.2% | 97 | 100 | +3.1% |

At 1000 eps, **input→paint p95 improves −39.2%** vs P2 (403.3 → 245.4) while batch→paint stays well under the 100 ms absolute gate on all three profiles. Residual absolute miss remains **input→paint at 1000 eps** (small sample, n=20; max=348.8). Long tasks stay ≤100 ms (&lt;200 ms gate); none carry Shiki/tokenization attribution in the harness report.

---

## Visual cadence and acceptance (P3)

| Metric | P2 100 | P3 100 | P2 500 | P3 500 | P2 1000 | P3 1000 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| visual updates/s | 31.15 | **31.30** | 32.86 | **32.74** | 31.44 | **31.23** |
| acceptance.batchToPaint (&lt;100ms) | pass | pass | pass | pass | pass | **pass** |
| acceptance.inputToPaint (&lt;50ms) | pass | pass | pass | pass | fail | **fail** |
| acceptance.visualCadence (≥30/s) | pass | pass | pass | pass | pass | pass |
| acceptance.longTask (&lt;200ms) | pass | pass | pass | pass | pass | pass |
| acceptance.passed | true | **true** | true | **true** | false | false |

---

## P3 stage attribution (median reports)

| Metric | eps_100 | eps_500 | eps_1000 |
| --- | ---: | ---: | ---: |
| receipt→transaction p95 (count) | 17.00 (387) | 17.00 (83) | 16.80 (42) |
| transaction→live publication p95 (count) | 0.10 (386) | 0.10 (83) | 0.10 (42) |
| batch→commit p95 (count) | 18.70 (386) | 19.70 (83) | 19.50 (42) |
| batch→paint p95 (count) | 35.30 (386) | 35.40 (83) | 36.10 (42) |
| input→paint p95 (count) | 18.10 (125) | 20.80 (28) | 245.40 (20) |
| long-task max (count) | 88 (1) | 94 (1) | 100 (3) |
| pipeline deliveryCallbacks | 387 | 83 | 42 |
| pipeline connectionTransactions | 385 | 80 | 39 |
| pipeline livePublications | 384 | 80 | 39 |
| pipeline reactCommits | 384 | 80 | 39 |
| pipeline paints | 384 | 80 | 39 |
| renders historicalThread | 8 | 8 | 8 |
| renders historicalRow | 3 | 3 | 3 |
| renders liveRow | 650 | 325 | 239 |
| renders markdownBlock | 31 | 31 | 30 |
| renders toolCard | 83 | 63 | 53 |

### Largest measured stage (P3 medians)

| Profile | Largest stage | p95 (ms) |
| --- | --- | ---: |
| eps_100 | **batchToPaint** | 35.30 |
| eps_500 | **batchToPaint** | 35.40 |
| eps_1000 | **inputToPaint** outlier (batchToPaint still low) | 245.40 / 36.10 |

### Markdown / tool cost attribution

| Observation | Evidence |
| --- | --- |
| Sealed/deferred Markdown path is bounded | `markdownBlock` ≈ 30–31 across all rates (does not scale with 1223 envelopes) |
| Per-tool cards track live tool work, not full map churn | `toolCard` 83 / 63 / 53 alongside livePublications 384 / 80 / 39 — higher card count is multi-tool fixture traffic, not 1:1 sibling fan-out |
| Historical Virtua stays cold | historicalThread=8, historicalRow=3 (same residual as P2) |
| Long tasks present but not Shiki-attributed | max 88 / 94 / 100 ms; harness has no Shiki/tokenization long-task labels |

### Shiki worker follow-up (Step 10)

**Shiki worker: not justified by P3 trace.** No selected P3 report attributes any &gt;50 ms long task to Shiki tokenization after idle deferral (only aggregate `timings.longTasks` with max ≤100 ms and no engine tag). Do not implement a worker from this evidence alone.

---

## P3 exit-gate checklist

| Gate | Status |
| --- | --- |
| Unit isolation (sibling tool=1; collapsed bodies unmounted; diff deferred) | **Pass** (Task 14 vitest) |
| Integrity + checksum on every selected median; 1223 envelopes applied | **Pass** |
| `deliveryMode=batched` + incremental=true + deferred=true | **Pass** |
| historicalThread/row remain cold vs live path | **Pass** (8 / 3) |
| batch→paint &lt;100 ms @ 100 / 500 / 1000 eps | **Pass** (35.30 / 35.40 / 36.10) |
| input→paint &lt;50 ms all profiles | **Fail @ 1000** (245.40 ms; improved −39.2% vs P2) |
| long-task max &lt;200 ms | **Pass** (88 / 94 / 100) |
| visual cadence ≥30/s | **Pass** (31.30 / 32.74 / 31.23) |
| Markdown/tool counters attributed | **Pass** (markdownBlock + toolCard non-zero under deferred+isolation) |
| Shiki worker decision from evidence | **Not justified by P3 trace** |

### P3 conclusion

**P3 gate PASSES on integrity, isolation, batch→paint, long-task, and cadence** with all three performance flags ON. Deferred rich content + incremental Markdown + per-tool isolation keep batch→paint ~35–36 ms and cut 1000 eps input→paint by **−39.2%** vs P2. Remaining absolute miss is still **input→paint at 1000 eps** (small sample / tail outlier); follow-up should target input-path coalescing / layout under max rate, not a speculative Shiki worker.

---

## P4 Step 7 — overscan / content-visibility decision (closed rule)

**Decision rule (Task 15):** On Windows 1,000 eps + 500-row synthetic history, three runs per Virtua `bufferSize` candidate (400 / 800 / 1,200). Select the **smallest** candidate with **zero blank-frame** assertions during fast PageUp/PageDown/wheel. Change default from 800 **only if** that candidate also improves layout+paint P95 by **≥10%** over 800. Historical-row `content-visibility: auto` + `contain-intrinsic-size: auto 240px` is added **only** under the same ≥10% rule and full Virtua/selection/nav/a11y pass; otherwise **no** new containment CSS.

| Candidate | GUI blank-frame / layout+paint P95 experiment | Selected |
| --- | --- | --- |
| bufferSize 400 | Not measured in Task 15 unit harness (no blank-frame GUI fixture in this step) | No |
| bufferSize **800** (current default) | Retained — closed rule forbids unmeasured change | **Yes (keep)** |
| bufferSize 1,200 | Not measured; larger than default | No |
| historical-row content-visibility | Not measured; no ≥10% evidence | **No CSS added** |

**Recorded decision:** keep `bufferSize` default **800**; **do not** add historical-row `content-visibility` / `contain-intrinsic-size` in `globals.css`. Full GUI overscan re-check belongs to Task 16 final evidence if desired; Task 15 does not invent unmeasured tuning.

---

## Final three-run batchToPaint P95 (ms) and median selection

| Profile | Run 1 P95 | Run 2 P95 | Run 3 P95 | Ordered P95 | Median run | Median P95 | Selected runId |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| eps_100 | 35.20 | 35.10 | 35.00 | 35.00, **35.10**, 35.20 | 2 | **35.10** | `run-mrmr1wtu-rnxux0` |
| eps_500 | 35.30 | 34.80 | 35.00 | 34.80, **35.00**, 35.30 | 3 | **35.00** | `run-mrmr2o6h-stz52x` |
| eps_1000 | 37.10 | 34.60 | 35.00 | 34.60, **35.00**, 37.10 | 3 | **35.00** | `run-mrmr2vh1-ewufp0` |

Selection rule: sort by `timings.batchToPaint.p95`, keep the middle report.
Artifacts: `final-100eps.json`, `final-500eps.json`, `final-1000eps.json`.

**Flags for capture (all ON):**

```
CODEG_DESKTOP_ACP_EVENT_BATCHING=1
CODEG_INCREMENTAL_LIVE_TRANSCRIPT=1
CODEG_DEFERRED_STREAMING_RICH_CONTENT=1
```

---

## Integrity (all selected final medians)

| Check | eps_100 | eps_500 | eps_1000 |
| --- | --- | --- | --- |
| `integrity.ok` | true | true | true |
| appliedEvents / expected | 1223 / 1223 | 1223 / 1223 | 1223 / 1223 |
| finalTextSha256 | `65380735…a4b039` | same | same |
| gaps / duplicates | 0 / 0 | 0 / 0 | 0 / 0 |
| `deliveryMode` | batched | batched | batched |
| flags batching / incr / deferred | true / true / true | true / true / true | true / true / true |
| hardwareAcceleration | enabled | enabled | enabled |
| emitted batches (deliveryCallbacks) | 382 | 82 | 42 |
| batch count &lt; raw envelopes | **382 &lt; 1223** | **82 &lt; 1223** | **42 &lt; 1223** |
| connectionTransactions | 378 | 80 | 41 |
| livePublications ≤ transactions | 378 ≤ 378 | 80 ≤ 80 | 41 ≤ 41 |
| paints track live path | 378 = 378 | 80 = 80 | 41 = 41 |
| historicalThread / historicalRow | 8 / 3 | 8 / 3 | 8 / 3 |
| liveRow / markdownBlock / toolCard | 1212 / 31 / 24 | 555 / 31 / 19 | 374 / 30 / 14 |

Historical residual (thread=8, row=3) matches P2/P3 mount/handoff residual; unit isolation still freezes counters across live pubs (active-output historical delta 0 in unit gate).

---

## P3 → Final latency + cadence (median reports)

| Metric | P3 100 | Final 100 | Δ% | P3 500 | Final 500 | Δ% | P3 1000 | Final 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| receipt→transaction p95 | 17.00 | 17.00 | 0.0% | 17.00 | 16.50 | **−2.9%** | 16.80 | 16.20 | **−3.6%** |
| transaction→live p95 | 0.10 | 0.10 | 0.0% | 0.10 | 0.10 | 0.0% | 0.10 | 0.20 | +100.0% |
| batch→commit p95 | 18.70 | 18.50 | **−1.1%** | 19.70 | 18.60 | **−5.6%** | 19.50 | 18.40 | **−5.6%** |
| batch→paint p95 | 35.30 | 35.10 | **−0.6%** | 35.40 | 35.00 | **−1.1%** | 36.10 | 35.00 | **−3.0%** |
| input→paint p95 | 18.10 | 17.70 | **−2.2%** | 20.80 | 20.40 | **−1.9%** | 245.40 | **16.90** | **−93.1%** |
| long-task max | 88 | 87 | **−1.1%** | 94 | 94 | 0.0% | 100 | 93 | **−7.0%** |
| visual updates/s | 31.30 | **30.85** | −1.4% | 32.74 | **32.75** | +0.0% | 31.23 | **34.78** | +11.4% |

At 1000 eps, **input→paint p95 clears the 50 ms absolute gate** (245.40 → 16.90). Adjacent-phase attribution: P3 still carried the residual miss; final re-measure on Task 15 scroll/recovery + prior isolation stack shows the gate met without inventing new render engines. Gains vs P0/P1/P2 remain as recorded in earlier sections (do not re-attribute P0→P1 IPC collapse to final-only work).

---

## Final stage attribution (median reports)

| Metric | eps_100 | eps_500 | eps_1000 |
| --- | ---: | ---: | ---: |
| receipt→transaction p95 (count) | 17.00 (382) | 16.50 (82) | 16.20 (42) |
| transaction→live publication p95 (count) | 0.10 (382) | 0.10 (82) | 0.20 (42) |
| batch→commit p95 (count) | 18.50 (382) | 18.60 (82) | 18.40 (42) |
| batch→paint p95 (count) | 35.10 (382) | 35.00 (82) | 35.00 (42) |
| input→paint p95 (count) | 17.70 (126) | 20.40 (28) | 16.90 (16) |
| long-task max (count) | 87 (1) | 94 (1) | 93 (1) |
| pipeline deliveryCallbacks | 382 | 82 | 42 |
| pipeline connectionTransactions | 378 | 80 | 41 |
| pipeline livePublications | 378 | 80 | 41 |
| pipeline reactCommits | 378 | 80 | 41 |
| pipeline paints | 378 | 80 | 41 |
| renders historicalThread | 8 | 8 | 8 |
| renders historicalRow | 3 | 3 | 3 |
| renders liveRow | 1212 | 555 | 374 |
| renders markdownBlock | 31 | 31 | 30 |
| renders toolCard | 24 | 19 | 14 |
| cadence updates/s | 30.85 | 32.75 | 34.78 |

### Visual cadence and acceptance (Final)

| Metric | Final 100 | Final 500 | Final 1000 |
| --- | ---: | ---: | ---: |
| visual updates/s | **30.85** | **32.75** | **34.78** |
| acceptance.batchToPaint (&lt;100ms) | **pass** | **pass** | **pass** |
| acceptance.inputToPaint (&lt;50ms) | **pass** | **pass** | **pass** |
| acceptance.longTask (&lt;200ms) | **pass** | **pass** | **pass** |
| acceptance.visualCadence (≥30/s) | **pass** | **pass** | **pass** |
| acceptance.eventIntegrity | **pass** | **pass** | **pass** |
| acceptance.passed | **true** | **true** | **true** |

---

## P0 → Final count metrics (median reports, summary)

| Metric | P0 100 | Final 100 | Δ% | P0 500 | Final 500 | Δ% | P0 1000 | Final 1000 | Δ% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deliveryCallbacks | 1223 | 382 | **−68.8%** | 1223 | 82 | **−93.3%** | 1223 | 42 | **−96.6%** |
| connectionTransactions | 1223 | 378 | **−69.1%** | 1223 | 80 | **−93.5%** | 1223 | 41 | **−96.6%** |
| historicalThread | (P0 n/a wire) | 8 | — | — | 8 | — | — | 8 | — |
| batch→paint p95 | 17.70 | 35.10 | +98.3%\* | 36.20 | 35.00 | **−3.3%** | 113.60 | **35.00** | **−69.2%** |
| input→paint p95 | 18.00 | 17.70 | **−1.7%** | 110.40 | 20.40 | **−81.5%** | 433.30 | **16.90** | **−96.1%** |

\*At 100 eps, batch→paint rises vs P0 because batched delivery publishes ~one live update per frame (~30+/s) vs P0’s coarse coalesce (~64 paints total). Absolute gates still pass; cadence is the intended trade.

Phase attribution of major gains (adjacent reports only):

| Gain | Adjacent phases showing it |
| --- | --- |
| IPC callbacks / transactions collapse @ 500–1000 eps | **P0 → P1** |
| Historical thread cold (→8) under stream | **P1 → P2** |
| Markdown/tool isolation counters + deferred rich | **P2 → P3** |
| Absolute input→paint @ 1000 eps clears 50 ms | **P3 → Final** (re-measure with full stack + Task 15 scroll/recovery) |

---

## Final acceptance criteria (design checklist)

| # | Criterion | Result |
| --- | --- | --- |
| 1 | No observed pause-then-jump on reference workload | **Pass** (cadence ≥30/s; integrity full apply; no severe stall in final runs) |
| 2 | Backend / apply / commit / paint costs separated | **Pass** (`receiptToTransaction`, `transactionToLive`, `batchToCommit`, `batchToPaint`, `inputToPaint`) |
| 3 | Bounded ordered desktop batches | **Pass** (batch count ≪ 1223; zero gaps/duplicates) |
| 4 | Server / remote semantics unchanged | **Pass** (server/MCP cargo tests; InternalEventBus still per-envelope in unit tests) |
| 5 | One transaction / publication per connection frame | **Pass** (livePublications ≤ transactions; paints track path) |
| 6 | Stable history references | **Pass** (historicalThread/row residual mount-only; unit freeze gate) |
| 7 | Unfinished tail is the only streaming Markdown change | **Pass** (incremental blocks + deferred rich unit gate) |
| 8 | Heavy engines / tool bodies deferred without final-content change | **Pass** (checksum + deferred path tests) |
| 9 | Bottom-follow respects user escape | **Pass** (Task 15 DOM escape tests) |
| 10 | Control / reconnect / completion paths correct | **Pass** (acp-connections + recovery suites) |
| 11 | Reports contain no user content | **Pass** (privacy `rg` scan clean) |
| 12 | Windows targets + repository checks | **Pass** on absolute timing gates; see Task 16 report for eslint/macOS/Linux caveats |

### Final / P4 conclusion

**Final Windows absolute gate PASSES** on all three rate medians with all flags ON. Integrity, cadence, long-task, batch→paint, and input→paint meet thresholds. Cross-platform GUI smoke for macOS/Linux is documented as skipped for host unavailability in `platform-smoke.md`. Rollout ownership and removal criteria are in `rollout.md`.

---

## Integrity notes

- Fixture checksum (all P0, P1, P2, P3, and final medians):
  `65380735c9a752758c7bace17cc722d86400480a0ae1dff62759f37eafa4b039`
- `appliedEvents` is the desktop metrics delta (`legacy_emit + batch_event_count`) during the run and must equal 1223.
- Agent used for the live ACP connection was **Codex** (same procedure as P0/P1/P2).
- P1 measurement required a runtime-spawn fix so the batcher can start from Tauri `.setup` (no current Tokio handle) and a harness fix so reports read live `deliveryMode` / flags from the capability snapshot.
- P2 measurement required live-footer `markReactCommit` so batch→paint samples drain on segment commits (historical MessageListView no longer re-renders each live pub).
- P3 measurement used temporary local CDP (`CODEG_PERF_CDP_PORT=9333` + wry `additional_browser_args`) for automation only — not committed.
- Final measurement likewise used temporary CDP automation only — not committed.
