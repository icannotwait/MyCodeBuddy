# Platform smoke + failure/privacy matrix (P4 / Task 16)

Machine (Windows absolute + disabled-accel qualitative): **HP Z2 Ultra9-285K / RTX 5080 / Win build 26200**
Measurement HEAD: `955263482f59a3f5c9a09748df899863ba4d28e8` (+ Task 16 verification fixes for `deriveToolTitle` lazy parse + clippy allow on desktop batcher flush)
Fixture: `grok_rich_v1` seed `49374` (1,223 events; checksum `65380735…a4b039`)
Flags for all smoke rows: batching=1, incremental=1, deferred=1
Capture date: 2026-07-16

---

## Windows — enabled hardware acceleration (absolute gate)

Reports: `final-100eps.json`, `final-500eps.json`, `final-1000eps.json`
Setting: `disable_hardware_acceleration: false` (`environment.hardwareAcceleration: enabled`)

| Profile | integrity.ok | applied | batch→paint p95 | input→paint p95 | long-task max | cadence (ups) | acceptance.passed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| eps_100 | true | 1223 | 35.10 | 17.70 | 87 | 30.85 | **true** |
| eps_500 | true | 1223 | 35.00 | 20.40 | 94 | 32.75 | **true** |
| eps_1000 | true | 1223 | 35.00 | 16.90 | 93 | 34.78 | **true** |

Absolute Windows gates (batch→paint &lt;100 ms, input→paint &lt;50 ms, long-task max &lt;200 ms, cadence ≥30/s, integrity) **all pass** on selected medians. See `comparison.md` for full phase attribution.

---

## Windows — disabled hardware acceleration (qualitative only)

Procedure: set `~/.codeg/preferences.json` → `disable_hardware_acceleration: true`, restart binary, one `eps_1000` fixture run, restore setting.

| Check | Result |
| --- | --- |
| `environment.hardwareAcceleration` | **disabled** |
| integrity.ok / applied 1223 / checksum | **true / 1223 / match** |
| Interaction usability | Chat tab + fixture run completed; no crash; no harness hang |
| batch→paint p95 / max (informational) | 55.40 / 124 (not gated) |
| input→paint p95 (informational) | 32.80 (not gated) |
| long-task max (informational) | 112 |
| cadence ups (informational) | 30.11 |
| Severe regression vs enabled | **Not observed** for integrity or basic interactivity; timings higher as expected without GPU compositing |

Absolute enabled-acceleration timing gates were **not** applied to this run. Summary artifact (local only): `.superpowers/sdd/final-runs/noaccel-1000eps-summary.json`.

---

## macOS WKWebView functional smoke

| Item | Status |
| --- | --- |
| Platform available in this agent session | **No** |
| Build / fixture run | **Skipped** |
| Skip reason | Task 16 executed on Windows-only host; no macOS runner or remote macOS desktop was available to the agent |

When a macOS host is available, record: 1,223 events + checksum; no capability crash (longtask / idle / IntersectionObserver missing); typing, selection/copy, keyboard scroll, wheel/touch escape, permission/question/cancel, expanded tools, RTL, reopen-mid-stream; startup legacy override + snapshot recovery; no severe pause/jump at 1,000 eps. Do **not** impose Windows absolute timings.

---

## Linux WebKitGTK functional smoke

| Item | Status |
| --- | --- |
| Platform available in this agent session | **No** |
| Build / fixture run | **Skipped** |
| Skip reason | Task 16 executed on Windows-only host; no Linux WebKitGTK desktop session was available to the agent |

Same recording checklist as macOS when a host becomes available.

---

## Failure / recovery matrix (test-build evidence)

Exercised via focused unit/integration suites on Task 16 HEAD (not live GUI injects for every cell). Results are **recovery OK** unless noted.

| Failure mode | Coverage | Recovery result |
| --- | --- | --- |
| Startup batcher failure → legacy emit | `desktop_event_batcher` / streaming_performance Rust tests; capability path | Falls back to legacy `acp://event`; counters content-free |
| Runtime emit failure | batcher failure / outstanding envelope tests | Failure signaled; metrics incremented without payload logs |
| Sequence gap (pause/resume) | `event-ingestor.test.ts` | Gap detected; ordered apply preserved after resume; content-free logs |
| Projector throw | `live-transcript-store.test.ts` | Rebuilds from canonical without false cursor advance |
| Invalid Markdown partition | `incremental-stream-blocks.test.ts` | Safe partition / no throw of full stream |
| Highlighter (Shiki) rejection | `code-block.test.tsx` | Raw code remains visible |
| Math / Mermaid rejection | `streamdown-plugins.test.ts` | Plugin error isolation; missing IntersectionObserver handled |
| Missing browser capabilities (longtask / idle / IO) | `streaming-performance-config` + existing suites | Feature-detect; no crash; fallback metrics path |

### Privacy scan

```text
rg -n 'prompt|response|raw_input|raw_output|tool_call_id|中文流式输出|Fast Grok output' docs/superpowers/performance/webview-streaming
```

**Result:** no content-bearing matches in committed report JSON under this directory (exit code indicates no matches). Field names such as `inputToPaint` are camelCase and do not match the lowercase content terms above.

---

## Notes

- Temporary CDP (`CODEG_PERF_CDP_PORT` + wry `additional_browser_args`) used for Windows automation only — **not committed**.
- Codex disposable chat tab used as live ACP connection (same as prior phases).
- macOS/Linux smoke must be filled on real hosts before claiming cross-platform GUI parity; Windows absolute gate is complete.
