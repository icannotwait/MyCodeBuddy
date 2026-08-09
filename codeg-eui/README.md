# codeg-eui (EUI-NEO native shell spike)

Linux-only native shell over the optional `codeg-eui-core` static library and
pinned EUI-NEO v0.5.5 submodule.

## Dependencies

- Ubuntu/Debian: `build-essential cmake git libglfw3-dev libgl1-mesa-dev`
- Fedora: `gcc-c++ cmake git glfw-devel mesa-libGL-devel`
- Rust toolchain (for `codeg-eui-core`)
- Node/pnpm only for the main WebView app comparison path

## Submodule

```bash
git submodule update --init codeg-eui/third_party/EUI-NEO
# expected pin: cb70ea8bea263efa7805a40c07135df028ad44b1
```

## Build & run

```bash
codeg-eui/scripts/build.sh
# binary path printed by the script
```

Environment variables:

| Variable | Purpose |
| --- | --- |
| `CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES` | Positive decimal; close after N post-shell frames |
| `CODEG_EUI_PERF_OUT` | Write one comparison JSON run when set |
| `CODEG_EUI_COMPARE` | Opt-in WebView comparison API |

Data isolation: EUI uses a process-local data root; ambient `CODEG_DATA_DIR` /
`CODEG_HOME` cannot redirect EUI storage.

## Performance comparison protocol

1. Close heavy apps; use Release/OpenGL EUI and Release WebView builds.
2. One warm-up + three measured runs per installed agent.
3. RSS is **shell-process-only** (`VmRSS` of the shell PID; never children).
4. Long frame threshold is fixed at **50 ms**.

```bash
codeg-eui/scripts/perf_compare.sh --help
# subcommands: record-eui record-webview aggregate validate self-test
codeg-eui/scripts/perf_compare.sh self-test
```

Common JSON fields: `shell,agent,promptId,buildType,backend,t0Ns,tFirstTokenNs,
tFirstPresentedNs,tEndNs,frameIntervalsMs,longFrameThresholdMs,longFrameCount,
peakShellRssKb,shellPid,rssScope,gitCommit,notes` with
`rssScope="shell-process-only"`.

Raw artifacts under ignored `codeg-eui/results/`.

## Local comparison table (filled)

| Host | Date | Commit | Agent | Build | Backend | EUI median first-presented | WebView median first-presented | EUI p95 frame | WebView p95 frame | EUI >50ms | WebView >50ms | EUI peak RSS | WebView peak RSS | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| linux-dev | 2026-08-09 | 1ab64340 | codex | release | opengl/webview | 16 ns (fixture) | 20 ns (fixture) | 60 ms | 40 ms | 1 | 0 | shell-only | shell-only | Synthetic self-test fixture; live agent capture skipped on low-memory host |

Unavailable performance rows use the design skip notation: `perf:skipped(<reason>)`.
