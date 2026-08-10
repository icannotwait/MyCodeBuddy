# codeg-eui Windows First-Class Support Design

## Status

Draft for review (2026-08-10). Extends the approved
[EUI-NEO Frontend Spike Design](./2026-08-09-eui-neo-frontend-spike-design.md)
with a **Windows first-class local preview** path. Does not replace the spike;
it removes the Linux-only product-preview gate for the optional `codeg-eui`
binary.

## Summary

Make the existing hybrid native shell (`codeg-eui` = EUI-NEO UI thread +
`codeg-eui-core` Rust staticlib) buildable and runnable on **Windows with
MSVC + Visual Studio 2022**, so developers can:

1. Build and launch `codeg-eui.exe` on Windows.
2. Complete real **Grok** and **Codex** streaming product loops against the
   existing Rust core.
3. Keep default data isolation from the main Codeg desktop database.

Linux remains fully supported. Performance comparison against WebView is
**adapted and optional** on Windows (does not block product-loop acceptance).
Default `codeg` / `codeg-server` / `codeg-mcp` / React paths stay free of
EUI-NEO.

## Problem

The spike deliberately chose **Linux first**:

| Surface | Current behavior | Windows impact |
| --- | --- | --- |
| `codeg-eui/scripts/build.sh` | Exits if `uname != Linux` | No official build entry |
| Rust artifact | Expects `libcodeg_eui_core.a` | MSVC uses `.lib` |
| CMake link line | Unconditionally links `m`, `${CMAKE_DL_LIBS}` | MSVC link failure |
| Default data root | `XDG_DATA_HOME` / `HOME/.local/share/codeg-eui` | Typical Windows env lacks these → hard fail |
| Perf RSS sampling | `/proc/<pid>/status` `VmRSS` | Unavailable on native Windows |
| Design Non-Goals | Windows/macOS not first delivery | Product preview blocked on the primary Windows dev host |

EUI-NEO itself is cross-platform (GLFW/OpenGL, Windows CI packages). The gap is
**Codeg integration packaging**, not the UI framework.

## Goals

1. **Windows first-class build and run** via MSVC + VS2022, documented next to
   Linux in `codeg-eui/README.md`.
2. **Real product loops on Windows**: workspace → new session → send → visible
   streaming assistant text → authoritative turn completion for **Grok** and
   **Codex**.
3. **Isolated default data root** on Windows
   (`%LOCALAPPDATA%\codeg-eui` or equivalent), with `CODEG_EUI_DATA_DIR`
   remaining the highest-priority override.
4. **No regression** of the Linux `build.sh` path and existing headless
   contract tests that already pass on Windows.
5. **CI smoke on Windows**: at least configure/build of contracts and/or native
   binary plus automated tests that do not require a display.
6. **Keep optionality**: default product binaries and lockfiles do not gain an
   EUI-NEO dependency.

## Non-Goals

- Replacing Tauri/WebView as the default desktop entrypoint.
- Making Windows performance numbers a **hard migration decision gate** against
  Linux (Windows WorkingSet sampling may ship, but product acceptance does not
  wait on a full eight-run comparison matrix).
- macOS as a first-class delivery target in this design (do not break it if
  cheap; do not accept on it).
- Expanding spike UI depth (file tree, terminal, delegation, workflow overlay,
  full Markdown fidelity).
- MinGW/MSYS as a co-equal maintained toolchain (MSVC only for Windows
  acceptance).
- Floating the EUI-NEO submodule pin without a separate change.

## Decisions

| Topic | Choice |
| --- | --- |
| Approach | Platform adaptation of existing hybrid spike (Approach A) |
| Windows toolchain | **MSVC + Visual Studio 2022** (CMake multi-config) |
| Windows product status | **First-class** alongside Linux |
| Product-loop acceptance | Required on Windows for Grok and Codex |
| Performance protocol | **Optional / adapted** on Windows; not a Final product-loop blocker |
| Default backends | Unchanged: **GLFW + OpenGL** |
| Data root | Platform-aware defaults; same pin semantics |
| Build orchestration | Symmetric scripts: `build.sh` (Linux) + `build.ps1` (Windows) |

## Architecture

Unchanged host model from the spike:

```text
codeg-eui  (single process; Linux + Windows)

┌─ Main / UI thread ─────────────────┐    ┌─ Tokio workers ──────────────┐
│ EUI-NEO (GLFW + OpenGL)            │FFI │ codeg-eui-core (staticlib)  │
│  app main + compose pages          │◄──►│  AppState / ACP / DB         │
│  shell / chat / settings           │    │  EventEmitter::WebOnly       │
└────────────────────────────────────┘    └──────────────────────────────┘
```

### Where platform differences live

Concentrate OS-specific code in four places. Do **not** fork page logic or
ABI semantics per OS.

| Area | Linux | Windows (this design) |
| --- | --- | --- |
| Orchestration | `scripts/build.sh` | `scripts/build.ps1` |
| Rust staticlib filename | `libcodeg_eui_core.a` | `codeg_eui_core.lib` (and search path under `target/release`) |
| CMake system libs | `Threads`, `dl`/`m` as needed | `Threads`; **no** unconditional `m`/`dl`; Windows system libs via EUI helpers |
| Default data root | XDG / `~/.local/share/codeg-eui` | `%LOCALAPPDATA%\codeg-eui` (see Data directory) |
| Perf RSS (optional) | `/proc/<pid>/status` VmRSS | Process WorkingSet via Win32 (or skip with explicit SKIPPED row) |

ABI (`codeg_eui_*`), frame snapshot rules, settings schema, and session/send
paths stay shared.

### Relationship to existing binaries

| Binary | After this design |
| --- | --- |
| `codeg` | Unchanged Tauri + WebView desktop |
| `codeg-server` | Unchanged |
| `codeg-mcp` | Unchanged |
| `codeg-eui` | Optional native shell; **Linux + Windows** first-class |

## Build and link (MSVC)

### Prerequisites (Windows)

Documented in `codeg-eui/README.md`:

- Rust stable (MSVC toolchain: `x86_64-pc-windows-msvc`)
- Visual Studio 2022 with **Desktop development with C++**
- CMake 3.20+ (or VS-bundled CMake)
- Node/pnpm only if running WebView comparison or frontend regression suite
- OpenGL-capable GPU drivers (typical Windows desktop)
- Initialized EUI-NEO submodule at the **same pin** as the spike design
  (`cb70ea8bea263efa7805a40c07135df028ad44b1` / v0.5.5 unless a later approved
  pin supersedes it)

### `scripts/build.ps1`

PowerShell entry point, symmetric to `build.sh`:

1. Fail clearly if not on Windows or if MSVC environment is missing
   (recommend invoking from “x64 Native Tools” or using
   `vswhere` + `Launch-VsDevShell` / `vcvars64`).
2. Verify EUI-NEO submodule pin matches expected commit.
3. `cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --release`
4. Resolve absolute path of the MSVC static library
   (`.../target/release/codeg_eui_core.lib` or the crate’s actual staticlib
   output name; pin the exact name in implementation and tests).
5. Configure CMake:

   ```text
   cmake -S codeg-eui -B codeg-eui/build
     -G "Visual Studio 17 2022" -A x64
     -DEUI_WINDOW_BACKEND=glfw
     -DEUI_RENDER_BACKEND=opengl
     -DCODEG_EUI_RUST_LIB=<absolute path to .lib>
   ```

6. `cmake --build codeg-eui/build --config Release --parallel`
7. Print the runnable path (e.g. `codeg-eui/build/Release/codeg-eui.exe`).

### CMake changes

In `codeg-eui/CMakeLists.txt` (and any ABI-link test targets):

1. **Stop unconditional** `target_link_libraries(... m ${CMAKE_DL_LIBS})` on the
   native executable path.
2. Use platform conditionals:

   - Always: `Threads::Threads` when required by the Rust archive.
   - Linux/Unix: `m`, `${CMAKE_DL_LIBS}` as today if still required.
   - Windows: rely on `eui_neo_configure_app` + Windows system libraries; link
     the IMPORTED `codeg_eui_core` `.lib` with correct MSVC whole-archive /
     `/WHOLEARCHIVE` or equivalent **only if** the staticlib needs force-include
     of ctor sections (decide by link error; document the chosen flag).

3. Multi-config generators: place or copy runtime assets next to
   `Release/codeg-eui.exe` the same way EUI-NEO already copies `assets/` for
   Windows examples.
4. Keep `CODEG_EUI_CONTRACTS_ONLY=ON` building **without** EUI/native deps so
   Windows CI can run headless contracts even when GPU/native deps are thin.

### `build.sh` (Linux)

- Remove or narrow the “only Linux” message only if the script remains
  Linux-oriented: **keep Linux-only shell orchestration**, but README must
  point Windows users to `build.ps1` instead of implying the product is
  Linux-exclusive forever.
- Prefer wording: “This script supports Linux; on Windows use `build.ps1`.”

### CRT / panic / ABI notes

- Prefer a single CRT strategy: Rust MSVC + MSVC C++ both use the dynamic CRT
  (`/MD` Release) to avoid double-CRT heaps across FFI.
- Existing panic containment at the C ABI boundary remains required on both
  OSes.
- Path strings across FFI remain UTF-8 as today; Windows absolute paths are
  converted at the Rust boundary from `OsString`/`PathBuf`.

## Data directory and environment

### Resolution order (updated)

`resolve_eui_data_root` (and `EuiRootInputs`) must become platform-aware while
preserving pin semantics:

1. `CODEG_EUI_DATA_DIR` if non-empty (absolute or relative to startup CWD) —
   **unchanged highest priority**.
2. Else platform default:
   - **Windows:** `%LOCALAPPDATA%\codeg-eui`
     (if `LOCALAPPDATA` missing, fall back to
     `%USERPROFILE%\AppData\Local\codeg-eui`; if still missing, error with a
     clear message).
   - **Linux/macOS:** existing chain
     `XDG_DATA_HOME/codeg-eui` → `$HOME/.local/share/codeg-eui`.
3. On pin: clear ambient `CODEG_HOME`, set `CODEG_DATA_DIR` to the pinned root
   (unchanged trust boundary).

### Tests

Extend `data_root` unit/integration tests with Windows-shaped inputs
(`LOCALAPPDATA` / `USERPROFILE`) using the existing injectable `EuiRootInputs`
pattern so tests do not require a real Windows host when run on Linux CI, and
vice versa.

### User-facing env vars

| Variable | Meaning after this design |
| --- | --- |
| `CODEG_EUI_DATA_DIR` | Override isolated data root (all platforms) |
| `CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES` | Unchanged automated smoke exit |
| `CODEG_EUI_PERF_*` | Unchanged metadata for comparison runs |

Document Windows default path in README next to XDG paths.

## Product loop (Windows acceptance)

Same functional loop as the spike, run natively on Windows:

1. Launch `codeg-eui.exe` with optional `CODEG_EUI_DATA_DIR` under
   `codeg-eui/results/data/...` (gitignored).
2. Settings: configure Grok or Codex using the **same backend schema** as the
   main app; Probe succeeds.
3. Select a workspace (fixture or real folder).
4. Create session → send message → observe **non-empty assistant text** in the
   EUI-presented UI → observe turn completion.
5. Unsupported interaction still fail-closed / terminalizes (spike M5 behavior).

Both Grok and Codex must pass once on a prepared Windows machine for this
design’s product acceptance. Credentials stay out of the repo and out of
committed evidence.

### Smoke (non-agent)

Bounded smoke without agents remains valuable:

- Window opens non-blank shell.
- Settings and Chat navigable.
- Optional `CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES=N` for automation when a display
  session exists.

Headless CI **must not** claim product-loop success; product loops are
developer-host or dedicated UI-capable runners only unless a future design adds
GPU CI.

## Performance protocol (optional / adapted)

### Status relative to acceptance

| Gate | Windows requirement |
| --- | --- |
| Product loop Grok/Codex | **Required** |
| Full perf matrix (warm-up + 3× per shell × agent) | **Optional** |
| Synthetic self-test of comparison CLI | **Required** to keep tooling honest |
| Claiming a performance winner | **Forbidden** without real dual-shell captures on the same host |

### RSS sampling adaptation

- Abstract “read shell RSS KiB for PID” behind a small helper used by
  `perf_compare` (or a Windows companion script).
- Linux: keep `/proc/<pid>/status` `VmRSS`.
- Windows: sample peak **WorkingSetSize** (or Private Working Set if documented
  consistently) for the shell PID only; still **shell-process-only**, never
  child agent processes.
- If Windows sampling is not ready in the first implementation slice, scripts
  must emit explicit `SKIPPED` rows and refuse to aggregate a winner—not silent
  zeros.

### Presentation anchors

`t0` / `tFirstPresented` / frame interval semantics remain shared. WebView
RAF2 recorder on Windows Tauri remains valid when running comparison; EUI
ticker/post-presentation proxy remains valid once the native binary runs.

## Testing strategy

### Automated (must stay green)

| Layer | Windows | Linux |
| --- | --- | --- |
| `codeg-eui-core` unit + focused integrations | Yes | Yes |
| CMake `CODEG_EUI_CONTRACTS_ONLY` CTest set | Yes (current 10 Windows-compatible names) | Yes |
| Linux-only ABI-link shutdown CTest | N/A | Yes when configured |
| Frontend tests that touch comparison recorder | Yes (Vitest) | Yes |
| `perf_compare` self-test | Yes (Git Bash or adapted) | Yes |
| Full default `codeg` cargo suite | Not required by this design | Not required by this design |

### Manual / host evidence (product)

- Windows: build via `build.ps1`, smoke, Grok loop, Codex loop; record binary
  path, data dir, and short notes in SDD delivery package when implementing.
- Linux: smoke regression after CMake link conditionals change.

### Regression risks to cover with tests

1. Default data root on Windows does not resolve to the main app database.
2. CMake does not link `m` on MSVC.
3. Build script documents and fails fast on wrong toolchain.
4. Contracts-only configure still works without `CODEG_EUI_RUST_LIB` native app
   target.

## CI

Add or extend a Windows job (GitHub Actions `windows-latest` or project
equivalent):

1. Checkout + submodule pin check for EUI-NEO.
2. Install Rust MSVC target if needed.
3. Prefer two stages when native GPU/OpenGL link is flaky on agents:
   - **Always:** `CODEG_EUI_CONTRACTS_ONLY=ON` configure + CTest.
   - **Best-effort or required if agent supports it:** full `build.ps1` Release
     build of `codeg-eui.exe` (no interactive product loop).
4. Run `cargo test` for `codeg-eui-core` on Windows.
5. Do **not** require real Grok/Codex credentials in CI.

Linux CI continues to run `build.sh` or contracts as already established by the
spike; CMake conditionals must not break Linux link.

## Documentation

Update `codeg-eui/README.md` support matrix:

| Platform | Build script | Product preview | Perf protocol |
| --- | --- | --- | --- |
| Linux | `scripts/build.sh` | Supported | Full (`/proc` RSS) |
| Windows (MSVC) | `scripts/build.ps1` | Supported | Adapted / optional |
| macOS | — | Not accepted | — |

Include VS2022 prerequisites, example PowerShell launch with
`CODEG_EUI_DATA_DIR`, and a short “Troubleshooting” for common MSVC/OpenGL
link errors.

Update the spike design’s Non-Goals note only by **reference** from this
document; do not silently rewrite historical spike acceptance without a
changelog section:

### Changelog relative to spike design

- **Removes** “Windows not a first delivery target” for **product preview** of
  `codeg-eui`.
- **Keeps** “Windows not required for Linux-first perf migration evidence.”
- Spike Final blockers that were “native binary unavailable on Windows” become
  implementable work under this design instead of permanent host excuses.

## Implementation slices (for planning)

Suggested order for a later implementation plan (not implementation itself):

1. **Data root Windows defaults** + unit tests (unblocks runtime even before UI).
2. **CMake platform link conditionals** + contracts-only CI green.
3. **`build.ps1` + MSVC staticlib path** → runnable `codeg-eui.exe`.
4. **README + smoke checklist** on a real Windows GPU host.
5. **Product-loop evidence** Grok + Codex.
6. **Optional:** Windows RSS helper in perf scripts + documentation of SKIPPED
   vs real rows.
7. **CI Windows job** wired to contracts + best-effort native build.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| MSVC staticlib + C++ link order / missing symbols | Start from EUI-NEO’s Windows gallery link flags; force-archive only if needed; record exact link line in README |
| CRT mismatch Rust/C++ | Standardize on `/MD` Release; fail CI if mixed |
| OpenGL context failures on remote CI | Contracts-only required; native build best-effort on CI |
| `LOCALAPPDATA` missing in unusual environments | Clear error; `CODEG_EUI_DATA_DIR` always works |
| Perf apples-to-oranges | Forbid winner claims; document WorkingSet vs VmRSS |
| Scope creep into full desktop IA | Non-goals remain enforced |

## Success criteria (acceptance checklist)

- [ ] `scripts/build.ps1` builds Release `codeg-eui.exe` on VS2022 x64.
- [ ] README documents Windows as supported alongside Linux.
- [ ] Default data root on Windows is under LocalAppData `codeg-eui`, isolated
      from main app; override via `CODEG_EUI_DATA_DIR` works.
- [ ] Window smoke: non-blank shell, Settings/Chat navigable.
- [ ] Real Grok streaming product loop succeeds on Windows.
- [ ] Real Codex streaming product loop succeeds on Windows.
- [ ] Linux `build.sh` (or equivalent) still builds and contracts pass.
- [ ] Windows CI runs contracts (+ core tests); native build at least attempted
      or required when feasible.
- [ ] Perf: self-test still passes; no false winner from synthetic/Windows-stub
      rows.
- [ ] Default `codeg` / server / MCP paths remain free of EUI-NEO.

## Open questions (resolved in this design)

| Question | Resolution |
| --- | --- |
| Scope | Local preview first (product loops), not full perf parity |
| Toolchain | MSVC + VS2022 only for Windows acceptance |
| Product status | First-class, not experimental-only |
| MinGW | Out of scope for acceptance |
| macOS | Not accepted in this design |

## References

- [2026-08-09 EUI-NEO Frontend Spike Design](./2026-08-09-eui-neo-frontend-spike-design.md)
- Worktree / branch baseline: `feat/eui-neo-frontend-spike` (spike delivery
  candidate; Windows product preview blocked until this design is implemented)
- EUI-NEO upstream: cross-platform GLFW/OpenGL; Windows PowerShell build
  examples in its README
- Spike delivery note: Windows host could run contracts and synthetic perf
  self-test only; native binary and real agent loops missing
