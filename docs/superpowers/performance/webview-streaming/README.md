# WebView streaming performance — Windows reference evidence

Reference-machine baselines and phase reports for ACP streaming through
WebView2 (P0 → P1 → P2 → P3 → **final**). Used by Tasks 8, 14, and 16.
All comparisons must run on this same named machine with foreground apps
closed (or document deviations).

## Reference machine

Captured on **2026-07-16** from `git rev-parse HEAD` =
`075083663e528e37ecd484f0f22eab3cad0a2f14` (`feat/webview-streaming-performance`).

### `Get-ComputerInfo` (selected fields)

| Field | Value |
| --- | --- |
| WindowsProductName | Windows 10 Pro |
| WindowsVersion | 2009 |
| OsBuildNumber | 26200 |
| CsManufacturer | HP |
| CsModel | HP Z2 Tower G1i Workstation Desktop PC |
| CsProcessors | Intel(R) Core(TM) Ultra 9 285K |
| CsTotalPhysicalMemory | 136831197184 (~127.4 GiB) |

### GPU (`Get-CimInstance Win32_VideoController`)

| Name | DriverVersion | AdapterRAM (reported) |
| --- | --- | --- |
| NVIDIA GeForce RTX 5080 | 32.0.15.8088 | 4293918720 |
| Intel(R) Graphics | 32.0.101.6129 | 2147479552 |

### Toolchain

```text
rustc 1.97.0 (2d8144b78 2026-07-07)
binary: rustc
commit-hash: 2d8144b7880597b6e6d3dfd63a9a9efae3f533d3
commit-date: 2026-07-07
host: x86_64-pc-windows-msvc
release: 1.97.0
LLVM version: 22.1.6

node: v24.14.0
pnpm: 11.9.0
```

### WebView / app settings (from median reports)

| Field | Value |
| --- | --- |
| Hardware acceleration (Settings) | **enabled** (`environment.hardwareAcceleration`) |
| Build mode | **production** static frontend (`NODE_ENV=production` export) |
| Delivery mode | `legacy` |
| WebView user agent | `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36 Edg/150.0.0.0` |
| WebView runtime | Edge WebView2 **150.0.4078.65** |

Named machine label: **HP Z2 Ultra9-285K / RTX 5080 / Win build 26200**.

## Build procedure (debug desktop + test-utils replay)

```powershell
# Repo checks (must pass before measuring)
pnpm exec vitest run src/lib/perf/streaming-perf-report.test.ts `
  src/lib/perf/streaming-perf-recorder.test.ts `
  src/contexts/acp-connections-context.test.tsx `
  src/components/message/message-list-view.test.tsx
cd src-tauri
cargo test --features test-utils perf_fixture
cargo test --features test-utils metrics_snapshot

# Production frontend + debug desktop binary with replay command registered
cd ..
pnpm exec tauri build --debug --features test-utils
# Note: MSI bundling may fail on pre-release version identifiers; the debug
# executable is still produced under src-tauri/target/debug/ (MyCodeBuddy.exe
# via tauri build, or codeg.exe via cargo build --features test-utils,tauri/custom-protocol).
```

For this capture, the measurement binary was built as:

```powershell
pnpm build
cd src-tauri
cargo build --features test-utils --features tauri/custom-protocol
# Binary: src-tauri/target/debug/codeg.exe
```

`custom-protocol` is required so the debug binary loads the production
`frontendDist` assets instead of `http://localhost:3000`.

Optional (automation only, not committed): enable WebView CDP by setting
`CODEG_PERF_CDP_PORT` and temporarily passing `--remote-debugging-port` through
wry `additional_browser_args` (wry overrides `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`).

## Capture procedure

1. Close other heavy foreground apps.
2. Launch the debug binary with test-utils replay registered.
3. Open a disposable conversation with a live ACP connection and the chat
   panel focused (MessageListView mounted). The fixture is connection-agnostic;
   this capture used a disposable **Codex** chat tab because the Grok agent
   preflight reported “SDK not installed” in the UI even though `grok` 0.2.98
   and `acp_preflight` passed.
4. In WebView DevTools (or CDP), run three times per profile, waiting for each
   completion:

```js
await window.__codegStreamingPerf.run({
  rateProfile: "eps_100",
  seed: 49374,
  download: true,
})
await window.__codegStreamingPerf.run({
  rateProfile: "eps_500",
  seed: 49374,
  download: true,
})
await window.__codegStreamingPerf.run({
  rateProfile: "eps_1000",
  seed: 49374,
  download: true,
})
```

5. Every run must have `integrity.ok === true`,
   `expectedEvents === appliedEvents === 1223`, no gaps/duplicates, and
   fixture checksum `65380735c9a752758c7bace17cc722d86400480a0ae1dff62759f37eafa4b039`.
   Latency gate failures are allowed for P0 baseline.

## Median selection rule

For each profile, order the three reports by `timings.batchToPaint.p95`
ascending and keep the **middle** report as `baseline-<rate>eps.json`.
All three P95 values are recorded in `comparison.md`.

## Artifacts

| File | Description |
| --- | --- |
| `baseline-100eps.json` | P0 median @ 100 events/s |
| `baseline-500eps.json` | P0 median @ 500 events/s |
| `baseline-1000eps.json` | P0 median @ 1000 events/s |
| `p1-*.json` / `p2-*.json` / `p3-*.json` | Phase medians |
| `final-100eps.json` | Final median @ 100 events/s (all flags ON) |
| `final-500eps.json` | Final median @ 500 events/s |
| `final-1000eps.json` | Final median @ 1000 events/s |
| `comparison.md` | Phase attribution + absolute-gate review |
| `platform-smoke.md` | Cross-platform smoke + failure/privacy matrix |
| `rollout.md` | Flag ownership + one-release removal criteria |

Reports contain fixture IDs, counts, digests, and timings only — no prompt,
response, or tool I/O content fields.
