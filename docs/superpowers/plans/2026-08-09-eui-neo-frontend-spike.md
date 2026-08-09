# EUI-NEO Frontend Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an optional Linux `codeg-eui` shell that reuses the existing Rust ACP/database core for Grok and Codex streaming and produces comparable native-versus-WebView performance evidence without changing default product builds.

**Architecture:** A standalone `src-tauri/codeg-eui-core` static library owns an isolated EUI bootstrap profile, Tokio runtime, async request queue, immutable polled frames, and per-connection snapshot/subscription recovery. A CMake-led C++17 EUI-NEO application owns the GLFW/OpenGL UI thread, copies each Rust frame before the next poll, and renders shell, chat, and settings pages. The bridge reuses public Rust facades over existing folder, conversation, ACP, and agent-config cores; Tauri/Axum handlers remain transport adapters rather than dependencies of the native shell.

**Tech Stack:** Rust 2021, Tokio, SeaORM + SQLite, hand-written C ABI, C++17, CMake 3.20+, EUI-NEO v0.5.5, GLFW + OpenGL, Bash, Vitest, Next.js 16, React 19, pnpm.

## Global Constraints

- Approved baseline: `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`, SHA-256 `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`. Do not modify the design during delivery.
- Linux is the only required native-shell platform. The EUI backend is GLFW + OpenGL; Vulkan, Windows, and macOS are outside this spike.
- Keep `codeg`, `codeg-server`, `codeg-mcp`, the React application, and all default build commands independent of EUI-NEO sources and native UI dependencies.
- Add `src-tauri/codeg-eui-core` as an independent `staticlib` crate with `codeg = { package = "codeg", path = "..", default-features = false }`. Do not add an EUI feature or crate type to the existing `codeg` package.
- Pin the EUI-NEO submodule at tag `v0.5.5`, commit `cb70ea8bea263efa7805a40c07135df028ad44b1`, under `codeg-eui/third_party/EUI-NEO`.
- The public ABI is version `1`, hand-written `extern "C"`, pointer-plus-length for every string, and UI-thread-only. Rust panics never unwind across it.
- Use a two-phase public shutdown: `codeg_eui_begin_shutdown()` moves `running -> stopping`, rejects new work, and starts cancellation/terminalization; `codeg_eui_poll()` remains legal in `stopping`; `int codeg_eui_shutdown(void)` performs the final join/free only after a successful poll exposed `shutdown_ready=1`. Calls in any other order return stable non-zero errors.
- Lifecycle states are `uninitialized -> starting -> running -> stopping -> stopped`. Re-init after `stopped` is legal only with the same process-pinned data root. Every accepted request, including work cancelled by drain, must be observed exactly once in a successful `running`/`stopping` poll before final shutdown frees its bytes.
- All non-lifecycle work is async. Accepted requests receive a monotonic non-zero `request_id` and exactly one terminal completion; DB, probe, config, and ACP work never run on the UI thread.
- Freeze input bounds at `CODEG_EUI_MAX_PATH_BYTES=32768`, `CODEG_EUI_MAX_MESSAGE_BYTES=1048576`, and `CODEG_EUI_MAX_SETTINGS_JSON_BYTES=2097152`.
- Freeze queue bounds at 256 pending commands, 256 terminal completions, and 128 control-class live events. Reject an enqueue before acceptance when capacity is unavailable; never drop an accepted request's terminal completion.
- A successful poll atomically transfers/drains the completions included in that frame. The immutable frame backing and drained completion bytes remain valid until the next successful poll or completed shutdown. A failed poll leaves the prior successful frame valid.
- Resolve one absolute data root before logging, Tokio, DB, credential helpers, or agent processes. A non-empty `codeg_eui_init(data_dir,len)` argument is authoritative; when it is empty, a non-empty `CODEG_EUI_DATA_DIR` wins; otherwise use `$XDG_DATA_HOME/codeg-eui` or `~/.local/share/codeg-eui`. Ignore ambient `CODEG_DATA_DIR`; do not require the argument and environment variable to agree.
- Before any core initialization, remove ambient `CODEG_HOME`, then overwrite `CODEG_DATA_DIR` with the resolved EUI root. This makes logs, uploads, pets, timing, ACP transcripts, credentials, SQLite, `AppState.data_dir`, and child inheritance use one root. Native `~/.codex` and `~/.grok` files remain shared agent-owned configuration.
- Do not start the embedded web server, pet mapper/window, updater, chat-channel workers, automation engine, auto-title workers, document-translation workers, reference-search sweeper, or delegation listener in the EUI profile.
- `EventEmitter::WebOnly` is required for shared-core compatibility, but the canonical EUI live stream is a per-connection `SessionState::to_snapshot()` plus `event_stream().subscribe()` pair acquired under one state read lock.
- On a sequence gap, receiver lag, or local control-queue saturation, mark `needs_resync`, replace the projection from an authoritative snapshot, and resume strictly after its `event_seq`. Producers never wait for the EUI poll cadence.
- Through M4, ACP permissions choose a reject/deny option when supplied or cancel the active turn; `ask_user_question` and plan approvals are immediately declined/cancelled. No unsupported interaction may remain parked.
- Settings read and write only Grok and Codex through a narrow public Rust DTO facade over existing ACP config helpers. Do not call Axum handlers and do not create a second config schema.
- The native UI is fixed-locale English and includes only recognizable shell, sessions, workspace, Grok/Codex selection, chat, composer, global error strip, settings, one-line tool summaries, and P1 cancel/settings controls.
- Use EUI `components::markdown` for assistant content and plain text fallback when markdown is disabled. Rebuild markdown at most every 75 ms while streaming and immediately at generation/turn boundaries.
- Freeze the long-frame threshold at `50 ms` for both shells. `t0` is send acceptance; `t_first_presented` uses the first callback after an eligible presentation in both shells (EUI next-update `onFrame`, WebView second RAF); the active sample window ends at `t_end` or hard error.
- Run one warm-up discard and 3 measured runs by default. Report median `t_first_presented - t0`, presented-frame interval p95, count of intervals `>50 ms`, and peak shell-process RSS only.
- CI and ordinary developer checks must not require EUI native packages. Headless Rust/ABI/projector tests are mandatory; the real EUI build and real-agent run are producer evidence on a prepared Linux host.
- Follow RED-GREEN-REFACTOR for behavior changes. Each producer Task writes a focused failing test, observes the intended failure, implements the minimum behavior, runs its automated verification, commits only owned files, and prepares its review package before the next Task.
- Do not insert human UAT, manual sign-off, or user approval between Tasks. Real-agent/native-window evidence is producer-run and recorded; human acceptance is listed only as post-delivery residual work.
- Final product-loop acceptance requires successful real workspace/session/send/stream evidence for both Grok and Codex on the prepared producer host. Missing installation, credentials, or launchability for either agent blocks delivery unless the approved design scope is explicitly revised; performance-only skip notation does not satisfy this gate.
- Generated Cargo/CMake output, screenshots, and raw/local performance artifacts are local ignored evidence. Producer commits stage exact source paths only; recursive parent-directory staging is forbidden. Intentionally ignored `.superpowers` delivery reports use `git add -f`.
- Work from `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` using POSIX shell syntax. Create local commits only; do not push, merge, rebase, or open a pull request.

### Risk Policy

Policy version: `b2d_task_risk_v1`.

- Hard triggers always produce `high`: `concurrency_lifecycle`, `security_trust_boundary`, `migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`, `update_rollback`.
- Soft signals sum once each: `cross_runtime_or_process=2`; `broad_production_surface=1`; `multiple_ownership_modules=1`; `shared_interface=1`; `dependency_or_build=1`; `multi_layer_without_test_seam=1`.
- Soft total `>=3` produces `high`; totals `0-2` produce `normal` when no hard trigger applies.
- Route `normal` Tasks to implementer `grok` with reviewer `[codex]`.
- Route `high` Tasks to implementer `codex` with reviewers `[codex (separate reviewer thread, not the Author or implementer), grok]`.

## Task Routing Matrix

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Establish the optional EUI build spine and hello window | `.gitmodules`, EUI gitlink, CMake, bridge header, staticlib skeleton, app entry | `unsafe_ffi`: first exported C ABI and Rust/C++ layout; `public_compatibility`: ABI version and symbols become the native-shell contract | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `5` | `high`: hard ABI triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 2 | Pin the isolated data root and construct the EUI AppState profile | data-root resolver, logging/bootstrap, `AppState::new_eui`, isolation tests | `security_trust_boundary`: ambient main-app roots and credential/support paths must not cross into EUI; `concurrency_lifecycle`: env is pinned before any worker or logger starts | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: data-root trust and startup ordering are hard triggers | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 3 | Implement lifecycle, async requests, completions, and immutable polled frames | ABI/runtime/queue/frame modules and contract tests | `unsafe_ffi`: pointer ownership, panic containment, validation; `concurrency_lifecycle`: UI thread, Tokio workers, bounded queues, shutdown drain/join | `cross_runtime_or_process=2`, `shared_interface=1`, `multi_layer_without_test_seam=1`; total `4` | `high`: two hard triggers and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 4 | Add the Grok and Codex settings facade and async bridge operations | shared ACP facade, settings DTO, bridge command handlers, tests | `security_trust_boundary`: auth/config files and launch credentials are read/written; `public_compatibility`: the facade is a shared public Rust contract | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: credential/config boundary and public facade hard triggers | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 5 | Add workspace, conversation, connection, history, and send operations | session facade, bridge commands, DB/manager tests | `concurrency_lifecycle`: agent process spawn, selection epochs, linked sends, cancellation, and connection ownership | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `broad_production_surface=1`; total `5` | `high`: agent lifecycle hard trigger; soft threshold also reached | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 6 | Project live state with snapshot recovery and deterministic interaction decline | live projector, permission policy, frame projection, recovery tests | `concurrency_lifecycle`: snapshot/subscribe race, lag, overflow, session switches; `security_trust_boundary`: permission/question/plan decisions must fail closed | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`; total `4` | `high`: lifecycle and permission hard triggers | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 7 | Build the native shell, chat, and P0 settings UI | C++ frame copy/model, shell/chat/settings pages, headless C++ tests | none | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `5` | `high`: soft threshold reached across ABI, EUI, and native build | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 8 | Add M5 error recovery, session switching, cancel, and P1 settings controls | Rust selection/cancel behavior, C++ UI controls, integration tests | `concurrency_lifecycle`: stale completions, active-turn cancel, disconnect/switch ownership, recovery to a new session | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`; total `4` | `high`: lifecycle hard trigger and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 9 | Add comparable performance instrumentation and reproducible evidence | Rust/C++/React markers, perf scripts, fixture, README/results | none | `cross_runtime_or_process=2`, `broad_production_surface=1`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `6` | `high`: soft threshold reached across both shells and build/run tooling | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |
| 10 | Aggregate the pre-Final delivery and scope audit | committed Task 1-9 diffs, submodule pin, review packages | none | `multiple_ownership_modules=1`; total `1` | `normal`: read-only aggregation with one ownership signal | `grok` | `codex` | `b2d_task_risk_v1` |
| 11 | Run Final automated verification, independent review, and delivery | Rust, C++, default regressions, native smoke, real-agent evidence, final diff | none | `broad_production_surface=1`, `multiple_ownership_modules=1`, `dependency_or_build=1`; total `3` | `high`: aggregate verification crosses the soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

## File Structure

| File | Responsibility in this change |
| --- | --- |
| `.gitmodules` and `codeg-eui/third_party/EUI-NEO` | Pin EUI-NEO v0.5.5 without affecting default builds. |
| `codeg-eui/.gitignore` | Ignore native build trees, screenshots, and raw/local performance results while keeping source, fixture, and README evidence trackable. |
| `src-tauri/codeg-eui-core/.gitignore` | Ignore the independent crate's nested Cargo `target/` directory. |
| `src-tauri/codeg-eui-core/Cargo.toml` | Standalone `staticlib` manifest with `codeg` path dependency and no default features. |
| `src-tauri/codeg-eui-core/src/lib.rs` | Export ABI version and public C entry points only. |
| `src-tauri/codeg-eui-core/src/data_root.rs` | Resolve, absolutize, and process-pin the EUI data root before core startup. |
| `src-tauri/codeg-eui-core/src/runtime.rs` | Own lifecycle state, UI-thread identity, Tokio runtime, command worker, shutdown, and global bridge slot. |
| `src-tauri/codeg-eui-core/src/abi.rs` | Stable repr(C) error codes, slices, frame/completion structs, input validation, panic boundaries, and pointer views. |
| `src-tauri/codeg-eui-core/src/model.rs` | Rust-owned display projection, selection epoch, bounded terminal completions, and immutable `OwnedFrame` construction. |
| `src-tauri/codeg-eui-core/src/commands.rs` | Async command enum and handlers for workspace/session/send/cancel/settings/probe. |
| `src-tauri/codeg-eui-core/src/live.rs` | Snapshot-and-subscribe attach loop, sequence checks, coalescing, resync, and interaction decline. |
| `src-tauri/codeg-eui-core/src/perf.rs` | Native `t0`, first-token, turn-end markers supplied to C++ presentation instrumentation. |
| `src-tauri/codeg-eui-core/tests/*.rs` | Headless ABI, data isolation, queue, completion, shutdown, settings, session, and live-recovery contracts. |
| `src-tauri/src/app_state.rs` | Add a production `AppState::new_eui` profile with `WebOnly` emission and no unrelated workers. |
| `src-tauri/src/document_translate/service.rs` | Expose a disabled/inert service constructor for production profiles that do not expose translation. |
| `src-tauri/src/logging/init.rs` | Add `init_eui()` so native-shell logs use the pinned EUI root and `codeg-eui` file prefix. |
| `src-tauri/src/commands/eui_facade.rs` | Public, typed Grok/Codex settings and session operations over existing shared cores. |
| `src-tauri/src/commands/mod.rs` | Export the narrow EUI facade. |
| `codeg-eui/CMakeLists.txt` | CMake-led EUI executable, Rust staticlib import, EUI assets, and headless contract-test mode. |
| `codeg-eui/app/app.cpp` | EUI `dslAppConfig`, startup/shutdown guard, 60 Hz dirty poll, copied model, and page routing. |
| `codeg-eui/app/bridge/codeg_eui_bridge.h` | C mirror of ABI v1 and bounds/error constants. |
| `codeg-eui/app/bridge/ui_snapshot.h` | Deep-copy `CodegEuiFrame` into C++ value types before the next poll. |
| `codeg-eui/app/bridge/client.h` | Typed enqueue methods, request tracking, completion dispatch, and error conversion. |
| `codeg-eui/app/pages/shell.h` | Dark shell chrome, sidebar, workspace/agent selectors, status, and global error strip. |
| `codeg-eui/app/pages/chat.h` | Transcript, throttled markdown, tool summaries, streaming indicator, composer, send/cancel. |
| `codeg-eui/app/pages/settings.h` | Grok/Codex probe, P0 launch fields, P1 structured fields, and advanced raw editors. |
| `codeg-eui/app/perf_metrics.h` | UI presentation markers, active frame intervals, fixed 50 ms count, and JSON result output. |
| `codeg-eui/tests/*.cpp` | Header layout, deep-copy lifetime, completion dispatch, stale-result, and metric aggregation tests. |
| `codeg-eui/tests/perf_compare_test.sh` | Exercise all performance-script subcommands, usage failures, metadata validation, and self-test behavior. |
| `codeg-eui/scripts/build.sh` | Build Rust staticlib, configure CMake, build/link EUI, and print the runnable binary path. |
| `codeg-eui/scripts/perf_compare.sh` | Run/aggregate three measurements, sample shell RSS, validate metadata, and render comparison rows. |
| `codeg-eui/fixtures/perf-workspace/README.md` | Fixed small workspace and prompt identifiers for comparison runs. |
| `codeg-eui/README.md` | Dependencies, build/run/env contracts, agent setup, comparison procedure, skips, and filled local results. |
| `src/lib/perf/eui-comparison-recorder.ts` | WebView-side send/presentation/end markers and the same 50 ms frame metric. |
| `src/lib/perf/eui-comparison-recorder.test.ts` | Deterministic WebView metric semantics and aggregation tests. |
| `src/contexts/acp-connections-context.tsx` | Mark WebView send acceptance and terminal events at existing authoritative points. |
| `src/components/message/live-transcript-row.tsx` | Arm the first-assistant marker at DOM commit and record it only in RAF2 after RAF1 gave the eligible DOM a paint opportunity. |

## Design Traceability

| Design requirement / milestone | Producer Task | Final evidence |
| --- | --- | --- |
| M0: optional staticlib, pinned EUI, CMake hello window, init/poll/two-phase-shutdown link | Task 1 | ABI smoke, CMake contract test, prepared-host native build |
| M1: isolated DB, `WebOnly` AppState, empty sessions | Task 2 | ambient-root isolation and bootstrap-profile tests |
| Async request/completion, frame ownership, shutdown drain | Task 3 | ABI contract and in-flight shutdown tests |
| M2: Grok/Codex P0 settings and probe | Task 4 | facade round-trip and bridge completion tests |
| M3: workspace, create/select session, history | Task 5 | DB/session facade and request pipeline tests |
| M4: send, live assistant buffer, mandatory resync, P0 decline | Task 6 | race/lag/overflow/parity/terminal-decline tests |
| Recognizable shell/chat/settings and markdown throttle | Task 7 | headless model tests plus prepared-host native smoke |
| M5: error strip, switch, cancel, P1 settings | Task 8 | stale epoch, cancel, crash recovery, and C++ control tests |
| M6: common performance anchors and RSS comparison | Task 9 | deterministic metric tests, script validation, filled README table |
| Grok and Codex real streaming each present non-empty text and complete successfully | Tasks 6 and 11 | mandatory producer-run evidence for both agents; either unavailable or unsuccessful agent blocks delivery |
| Default product paths have no EUI dependency | Tasks 1, 10, and 11 | default desktop/server/MCP checks with EUI submodule temporarily unavailable |

---

### Task 1: Establish the Optional EUI Build Spine and Hello Window

**Milestone:** M0.

**Files:**

- Modify: `.gitmodules`
- Create gitlink: `codeg-eui/third_party/EUI-NEO`
- Create: `codeg-eui/.gitignore`
- Create: `src-tauri/codeg-eui-core/.gitignore`
- Create: `src-tauri/codeg-eui-core/Cargo.toml`
- Create: `src-tauri/codeg-eui-core/src/lib.rs`
- Create: `src-tauri/codeg-eui-core/src/abi.rs`
- Create: `src-tauri/codeg-eui-core/tests/abi_smoke.rs`
- Create: `codeg-eui/CMakeLists.txt`
- Create: `codeg-eui/app/app.cpp`
- Create: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Create: `codeg-eui/tests/abi_layout_test.cpp`
- Create: `codeg-eui/scripts/build.sh`

**Interfaces:**

- Consumes: `codeg_lib` from `src-tauri` with `default-features = false`; EUI `glfw_app_main.cpp`, `eui_neo_configure_app`, and `app::{dslAppConfig,compose}`.
- Produces: ABI version constant `CODEG_EUI_API_VERSION=1`; exported `codeg_eui_api_version`, `codeg_eui_init`, `codeg_eui_poll`, initial `codeg_eui_begin_shutdown`, and `codeg_eui_shutdown`; CMake target `codeg-eui`; Rust artifact `libcodeg_eui_core.a`; explicit generated-output ignores.
- Preserves: no root Cargo workspace and no reference from `src-tauri/Cargo.toml`, `package.json`, or default CMake/build paths to the new crate or submodule.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Establish the optional EUI build spine and hello window | submodule, CMake, bridge header, staticlib skeleton, app entry | `unsafe_ffi`: exported layout/symbols; `public_compatibility`: ABI v1 starts here | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `5` | `high`: hard ABI triggers | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Add the pinned EUI-NEO submodule**

```bash
git submodule add https://github.com/sudoevolve/EUI-NEO.git codeg-eui/third_party/EUI-NEO
git -C codeg-eui/third_party/EUI-NEO checkout cb70ea8bea263efa7805a40c07135df028ad44b1
test "$(git -C codeg-eui/third_party/EUI-NEO rev-parse HEAD)" = cb70ea8bea263efa7805a40c07135df028ad44b1
git diff --submodule=short -- .gitmodules codeg-eui/third_party/EUI-NEO
```

Expected: `.gitmodules` names the GitHub URL and the gitlink points exactly at the peeled `v0.5.5` commit.

- [ ] **Step 2: Write the failing Rust ABI smoke test**

Create `src-tauri/codeg-eui-core/tests/abi_smoke.rs`:

```rust
use codeg_eui_core::{
    codeg_eui_api_version, codeg_eui_poll, CodegEuiFrame, CODEG_EUI_API_VERSION,
    CODEG_EUI_ERR_NULL_POINTER,
};

#[test]
fn abi_version_and_null_poll_are_stable() {
    assert_eq!(codeg_eui_api_version(), CODEG_EUI_API_VERSION);
    assert_eq!(CODEG_EUI_API_VERSION, 1);
    assert_eq!(codeg_eui_poll(std::ptr::null_mut::<CodegEuiFrame>()),
               CODEG_EUI_ERR_NULL_POINTER);
}
```

- [ ] **Step 3: Run the Rust test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
```

Expected: FAIL because the crate and ABI symbols do not exist.

- [ ] **Step 4: Add the independent staticlib manifest and ABI skeleton**

Create `src-tauri/codeg-eui-core/Cargo.toml` with this dependency boundary:

```toml
[package]
name = "codeg-eui-core"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
name = "codeg_eui_core"
crate-type = ["staticlib", "rlib"]

[dependencies]
codeg = { package = "codeg", path = "..", default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
thiserror = "2"

[dev-dependencies]
temp-env = "0.3"
tempfile = "3"
```

Define the initial stable surface in `abi.rs` and re-export it from `lib.rs`:

```rust
pub const CODEG_EUI_API_VERSION: u32 = 1;
pub const CODEG_EUI_OK: i32 = 0;
pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 7],
}

#[no_mangle]
pub extern "C" fn codeg_eui_api_version() -> u32 {
    CODEG_EUI_API_VERSION
}

#[no_mangle]
pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
    if out.is_null() {
        return CODEG_EUI_ERR_NULL_POINTER;
    }
    CODEG_EUI_ERR_NOT_READY
}
```

The M0 lifecycle already follows the public call order used by the final bridge: `init` enters `running`, `begin_shutdown` enters `stopping` and rejects new work, the first successful stopping `poll` returns `shutdown_ready=1`, and only then does `shutdown` enter `stopped`. Because M0 accepts no async requests, that stopping poll is immediately ready. Task 3 replaces the minimal process-local state with the full worker/drain state machine without changing these symbols or weakening their ordering.

- [ ] **Step 5: Mirror ABI v1 in C and write the headless layout test**

The header must use only fixed-width C types and include compile-time assertions:

```c
#define CODEG_EUI_API_VERSION 1u
#define CODEG_EUI_OK 0
#define CODEG_EUI_ERR_INVALID_STATE 1
#define CODEG_EUI_ERR_NULL_POINTER 2
#define CODEG_EUI_ERR_NOT_READY 9

typedef struct CodegEuiFrame {
  uint32_t api_version;
  uint32_t lifecycle_state;
  uint64_t generation;
  uint8_t shutdown_ready;
  uint8_t reserved[7];
} CodegEuiFrame;

uint32_t codeg_eui_api_version(void);
int codeg_eui_init(const uint8_t *data_dir_utf8, size_t data_dir_len);
int codeg_eui_poll(CodegEuiFrame *out);
int codeg_eui_begin_shutdown(void);
int codeg_eui_shutdown(void);

#if defined(__cplusplus)
static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI drift");
#endif
```

`abi_layout_test.cpp` asserts version `1`, size `24`, `offsetof(generation)==8`, and `offsetof(shutdown_ready)==16` without linking the EUI application. The Rust smoke test also drives `init -> begin_shutdown -> stopping poll with shutdown_ready=1 -> shutdown` and rejects final shutdown before the ready poll.

- [ ] **Step 6: Add CMake and the hello window**

Use the EUI integration contract exactly:

```cmake
cmake_minimum_required(VERSION 3.20)
project(codeg_eui LANGUAGES C CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
option(CODEG_EUI_CONTRACTS_ONLY "Build ABI tests without EUI/native deps" OFF)
option(CODEG_EUI_ABI_LINK_TESTS "Link black-box ABI tests to the Rust archive" OFF)
set(CODEG_EUI_RUST_LIB "" CACHE FILEPATH "Absolute libcodeg_eui_core.a path")

enable_testing()
add_executable(codeg_eui_abi_layout_test tests/abi_layout_test.cpp)
target_include_directories(codeg_eui_abi_layout_test PRIVATE app/bridge)
add_test(NAME codeg_eui_abi_layout COMMAND codeg_eui_abi_layout_test)

if(CODEG_EUI_ABI_LINK_TESTS OR NOT CODEG_EUI_CONTRACTS_ONLY)
  if(NOT IS_ABSOLUTE "${CODEG_EUI_RUST_LIB}" OR
     NOT EXISTS "${CODEG_EUI_RUST_LIB}")
    message(FATAL_ERROR "CODEG_EUI_RUST_LIB must name an existing absolute archive")
  endif()
  find_package(Threads REQUIRED)
  add_library(codeg_eui_core STATIC IMPORTED GLOBAL)
  set_target_properties(codeg_eui_core PROPERTIES
    IMPORTED_LOCATION "${CODEG_EUI_RUST_LIB}")
endif()

if(CODEG_EUI_ABI_LINK_TESTS)
  add_executable(codeg_eui_shutdown_drain_test tests/shutdown_drain_test.cpp)
  target_compile_definitions(codeg_eui_shutdown_drain_test PRIVATE
    CODEG_EUI_TEST_HOOKS=1)
  target_include_directories(codeg_eui_shutdown_drain_test PRIVATE app/bridge)
  target_link_libraries(codeg_eui_shutdown_drain_test PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
  add_test(NAME codeg_eui_shutdown_drain COMMAND
    codeg_eui_shutdown_drain_test)
endif()

if(NOT CODEG_EUI_CONTRACTS_ONLY)
  add_subdirectory(third_party/EUI-NEO)
  add_executable(codeg-eui
    third_party/EUI-NEO/core/app/glfw_app_main.cpp
    app/app.cpp)
  target_include_directories(codeg-eui PRIVATE app/bridge)
  target_link_libraries(codeg-eui PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
  eui_neo_configure_app(codeg-eui)
endif()
```

`app.cpp` defines a 1180x760, 60 fps `Codeg EUI Spike` window, calls `codeg_eui_init(nullptr, 0)` once before rendering, calls `codeg_eui_poll` during compose, draws `Codeg EUI / bridge v1`, and owns an RAII guard that calls `codeg_eui_begin_shutdown`, polls until the minimal frame exposes `shutdown_ready=1`, then calls `codeg_eui_shutdown`. Task 3 expands the same guard to dispatch terminal completions while draining.

Create exact ignore files before any build:

```gitignore
# codeg-eui/.gitignore
/build/
/build-*/
/results/
/screenshots/
```

```gitignore
# src-tauri/codeg-eui-core/.gitignore
/target/
```

- [ ] **Step 7: Add the deterministic build orchestrator**

`codeg-eui/scripts/build.sh` must use `set -eu`, resolve the absolute repository root from the script path, reject non-Linux hosts, verify the submodule commit, build `codeg-eui-core --release`, pass `-DCODEG_EUI_RUST_LIB="${repo_root}/src-tauri/codeg-eui-core/target/release/libcodeg_eui_core.a"`, and print the absolute `codeg-eui` binary path as its final output line. It must not run `git submodule update` implicitly or mutate the pin.

- [ ] **Step 8: Run automated verification and a prepared-host native build**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract --output-on-failure
codeg-eui/scripts/build.sh
```

Expected: Rust and headless C++ tests pass. On a Linux host with CMake, a C++17 compiler, GLFW/OpenGL development libraries, and initialized submodules, `build.sh` produces a runnable hello window. Missing native packages are recorded in the review package and do not weaken the Rust/headless gates.

- [ ] **Step 9: Commit and prepare the Task 1 review package**

```bash
git add --dry-run -- .gitmodules codeg-eui/.gitignore codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/scripts/build.sh codeg-eui/tests/abi_layout_test.cpp codeg-eui/third_party/EUI-NEO src-tauri/codeg-eui-core/.gitignore src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/abi_smoke.rs
git add -- .gitmodules codeg-eui/.gitignore codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/scripts/build.sh codeg-eui/tests/abi_layout_test.cpp codeg-eui/third_party/EUI-NEO src-tauri/codeg-eui-core/.gitignore src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/abi_smoke.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add optional native shell build spine"
git show --stat --oneline HEAD
git diff HEAD^ -- .gitmodules codeg-eui src-tauri/codeg-eui-core
```

Expected package: one commit with the exact submodule pin, independent crate, ABI v1 skeleton, CMake target, hello app, headless tests, and ignore ownership. The staged list contains no `target`, `build`, `results`, screenshot, binary, or object path. Attach Task 1 command evidence and route it to both high-risk reviewers, then continue directly to Task 2.

### Task 2: Pin the Isolated Data Root and Construct the EUI AppState Profile

**Milestone:** M1.

**Files:**

- Create: `src-tauri/codeg-eui-core/src/data_root.rs`
- Create: `src-tauri/codeg-eui-core/src/bootstrap.rs`
- Modify: `src-tauri/codeg-eui-core/src/lib.rs`
- Modify: `src-tauri/codeg-eui-core/Cargo.toml`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/document_translate/service.rs`
- Modify: `src-tauri/src/logging/init.rs`
- Test: `src-tauri/codeg-eui-core/tests/data_root_isolation.rs`
- Test: `src-tauri/codeg-eui-core/tests/bootstrap_profile.rs`

**Interfaces:**

- Consumes: `codeg_lib::{db::init_database,logging::init::init_eui}`, `InternalAgentSessionRegistry::load`, `EventEmitter::web_only`, and dormant core constructors.
- Produces: `resolve_eui_data_root(&EuiRootInputs) -> Result<PathBuf, DataRootError>`, process-once `pin_eui_data_root(PathBuf) -> Result<(), DataRootError>`, `logging::init::init_eui() -> LogGuard`, `AppState::new_eui(db, data_dir) -> Result<AppState, AppCommandError>`, and `EuiBootstrap::start() -> Result<Self, BootstrapError>`.
- Invariant: the first successful pin is immutable; re-init with the same normalized path succeeds, while a different path returns `DataRootError::AlreadyPinned`.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2 | Pin isolated root and construct EUI AppState | resolver, bootstrap, AppState profile, isolation tests | `security_trust_boundary`: ambient app roots/credentials; `concurrency_lifecycle`: pin before logging/runtime | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: both hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write data-root precedence and isolation tests**

Use a pure input struct so precedence tests do not race process environment:

```rust
#[test]
fn ambient_main_data_dir_and_codeg_home_never_choose_the_eui_root() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: None,
        xdg_data_home: Some(PathBuf::from("/tmp/xdg")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };
    assert_eq!(resolve_eui_data_root(&inputs).unwrap(),
               PathBuf::from("/tmp/xdg/codeg-eui"));
}

#[test]
fn explicit_eui_root_is_absolutized() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: Some(PathBuf::from("relative-eui")),
        xdg_data_home: Some(PathBuf::from("/tmp/ignored")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };
    assert_eq!(resolve_eui_data_root(&inputs).unwrap(),
               PathBuf::from("/work/relative-eui"));
}
```

The integration test serializes process-env mutation with one static mutex, sets `CODEG_DATA_DIR=<main>`, `CODEG_HOME=<main-home>`, and `CODEG_EUI_DATA_DIR=<eui>`, starts bootstrap, and asserts only `<eui>/codeg.db` plus `<eui>/logs` are created; `<main>/codeg.db` and `<main-home>/logs` remain absent. A second RED case calls `codeg_eui_init(<argument-root>, len)` while `CODEG_EUI_DATA_DIR=<env-root>` and expects `<argument-root>/codeg.db`, no write under `<env-root>`, a successful two-phase shutdown, and a stable init error when later re-init supplies a different normalized argument root.

- [ ] **Step 2: Run the isolation test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test data_root_isolation
```

Expected: FAIL because the resolver/bootstrap do not exist.

- [ ] **Step 3: Implement the pure resolver and one-time process pin**

Use this exact precedence and process mutation order:

```rust
pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootError> {
    let candidate = input.codeg_eui_data_dir.as_ref().filter(|p| !p.as_os_str().is_empty())
        .cloned()
        .or_else(|| input.xdg_data_home.as_ref().map(|p| p.join("codeg-eui")))
        .or_else(|| input.home.as_ref().map(|p| p.join(".local/share/codeg-eui")))
        .ok_or(DataRootError::HomeUnavailable)?;
    Ok(if candidate.is_absolute() { candidate } else { input.cwd.join(candidate) })
}

pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
    let absolute = absolutize_without_requiring_existence(&root)?;
    verify_or_set_process_pin(&absolute)?;
    std::env::remove_var("CODEG_HOME");
    std::env::set_var("CODEG_DATA_DIR", &absolute);
    Ok(())
}
```

If the public ABI data-dir argument is non-empty, validate UTF-8/bounds, absolutize it against the captured startup working directory, and use it as the authoritative EUI root even when `CODEG_EUI_DATA_DIR` differs. Only an empty argument consults `CODEG_EUI_DATA_DIR` and then the documented XDG/home defaults. Both paths call `pin_eui_data_root`, remove ambient `CODEG_HOME`, and overwrite `CODEG_DATA_DIR`. The C++ product entrypoint passes empty; ABI tests exercise the authoritative non-empty path.

- [ ] **Step 4: Write the failing EUI AppState profile test**

`bootstrap_profile.rs` must assert:

```rust
let bootstrap = EuiBootstrap::start_for_test(temp.path()).await.unwrap();
assert_eq!(bootstrap.state.data_dir, temp.path());
assert!(matches!(bootstrap.state.emitter, EventEmitter::WebOnly { .. }));
assert_eq!(bootstrap.state.connection_manager.list_connections().await.len(), 0);
assert!(!bootstrap.started_services.web_server);
assert!(!bootstrap.started_services.auto_title);
assert!(!bootstrap.started_services.automation);
assert!(!bootstrap.started_services.chat_channels);
assert!(!bootstrap.started_services.pet_mapper);
```

- [ ] **Step 5: Add `AppState::new_eui` with disabled auxiliary services**

Refactor the test constructor only enough to share field assembly. Add `DocumentTranslationService::new_disabled` as the production-visible replacement for the test-only `new_inert`, and retain `new_inert` as a test alias. Build the dormant auto-title coordinator with `build_production_coordinator` but never call `recover_and_start`; construct the reference registry but never spawn `run_reference_search_sweeper`; construct the completion dispatcher but never call `spawn_completion_outbox_dispatcher`. `new_eui` must use this complete field map:

```rust
pub async fn new_eui(db: AppDatabase, data_dir: PathBuf)
    -> Result<Self, AppCommandError>
{
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let metrics = Arc::new(crate::acp::EventBusMetrics::default());
    let bus = Arc::new(InternalEventBus::new(metrics));
    let emitter = EventEmitter::web_only(broadcaster.clone(), bus.clone());
    let manager = ConnectionManager::new();
    let internal_sessions = InternalAgentSessionRegistry::load(
        db.conn.clone(), &data_dir).await.map_err(AppCommandError::from)?;
    let chat_channel_manager = default_chat_channel_manager();
    let conversation_experience_gate =
        Arc::new(ConversationExperienceMutationGate::default());
    let db_handle = Arc::new(AppDatabase { conn: db.conn.clone() });
    let auto_title_coordinator = crate::auto_title::build_production_coordinator(
        Arc::clone(&db_handle),
        manager.clone_ref(),
        chat_channel_manager.clone_ref(),
        EventEmitter::Noop,
        Arc::clone(&conversation_experience_gate),
    );
    let document_translation = DocumentTranslationService::new_disabled(
        Arc::clone(&db_handle),
    );
    let reference_search_registry = ReferenceSearchRegistry::new(
        crate::commands::conversation_experience::DEFAULT_REFERENCE_SEARCH_LIMIT,
        Arc::new(crate::reference_search::ProductionReferenceSourceFactory {
            db: db.conn.clone(),
        }),
    );
    let stack = build_delegation_stack(&manager, db.conn.clone(), data_dir.clone());
    let completion_protocol_rollout = Arc::new(
        crate::acp::delegation::workflow::CompletionProtocolRolloutConfig::default(),
    );
    manager.install_completion_protocol_runtime(
        Arc::clone(&completion_protocol_rollout),
        Arc::clone(&stack.metrics),
    );
    let completion_outbox_dispatcher = Arc::new(
        CompletionOutboxDispatcher::new(db_handle, emitter.clone())
            .with_metrics(Arc::clone(&stack.metrics)),
    );

    Ok(Self {
        db,
        connection_manager: manager,
        terminal_manager: default_terminal_manager(),
        event_broadcaster: broadcaster,
        acp_event_bus: bus,
        emitter,
        data_dir,
        internal_sessions,
        auto_title_coordinator,
        document_translation,
        conversation_experience_gate,
        reference_search_registry,
        web_server_state: WebServerState::new(),
        chat_channel_manager,
        workspace_transfer: Arc::new(WorkspaceTransferManager::new_from_env()),
        pet_state: crate::pet_state_mapper::new_pet_state_handle(),
        delegation_broker: stack.broker,
        continuation_coordinator: stack.continuation_coordinator,
        delegation_metrics: stack.metrics,
        completion_protocol_rollout,
        completion_outbox_dispatcher,
        delegation_runtime_settings: stack.runtime_settings,
        delegation_tokens: stack.tokens,
        delegation_leases: stack.leases,
        delegation_socket_path: stack.socket_path,
        feedback_config: stack.feedback,
        question_config: stack.ask,
        session_info_config: stack.sessions,
        system_op_lock: default_system_op_lock(),
        update_state: default_update_state(),
    })
}
```

The delegation objects exist because shared launch helpers require them, but do not call the listener, supervisor, outbox-dispatcher, chat-channel, auto-title, translation, reference-search, pet, web-server, or updater start functions from this profile.

- [ ] **Step 6: Implement bootstrap ordering**

Add this production logging entry point, then have `EuiBootstrap::start` run synchronously on the eventual UI thread in this order: resolve root, pin `CODEG_HOME`/`CODEG_DATA_DIR`, create directories, call `init_eui`, create Tokio runtime, run `init_database`, apply persisted log level, call `AppState::new_eui`, then return the state/runtime/log guard. No `tokio::spawn` occurs before the env pin.

```rust
pub fn init_eui() -> LogGuard {
    init_with_file("codeg-eui")
}
```

- [ ] **Step 7: Run M1 verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test data_root_isolation -- --test-threads=1
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bootstrap_profile -- --test-threads=1
cargo check --manifest-path src-tauri/codeg-eui-core/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --lib
```

Expected: the EUI root is absolute and exclusive, SQLite/logs use it, the state is `WebOnly` with zero sessions, no excluded service is started, and no Tauri dependency is enabled.

- [ ] **Step 8: Commit and prepare the Task 2 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/data_root.rs src-tauri/codeg-eui-core/src/bootstrap.rs src-tauri/codeg-eui-core/tests/data_root_isolation.rs src-tauri/codeg-eui-core/tests/bootstrap_profile.rs src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
git add -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/data_root.rs src-tauri/codeg-eui-core/src/bootstrap.rs src-tauri/codeg-eui-core/tests/data_root_isolation.rs src-tauri/codeg-eui-core/tests/bootstrap_profile.rs src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add isolated core bootstrap profile"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
```

Expected package: one commit proving root precedence, `CODEG_HOME` clearing, SQLite/log isolation, `WebOnly` construction, and excluded-service non-start. Route it to both high-risk reviewers, then continue directly to Task 3.

### Task 3: Implement Lifecycle, Async Requests, Completions, and Immutable Frames

**Milestone:** M0/M1 bridge completion.

**Files:**

- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Expand: `src-tauri/codeg-eui-core/src/runtime.rs`
- Create: `src-tauri/codeg-eui-core/src/model.rs`
- Create: `src-tauri/codeg-eui-core/src/commands.rs`
- Modify: `src-tauri/codeg-eui-core/src/lib.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Create: `codeg-eui/app/bridge/ui_snapshot.h`
- Modify: `codeg-eui/CMakeLists.txt`
- Create: `codeg-eui/tests/ui_snapshot_test.cpp`
- Create: `codeg-eui/tests/shutdown_drain_test.cpp`
- Test: `src-tauri/codeg-eui-core/tests/bridge_contract.rs`
- Test: `src-tauri/codeg-eui-core/tests/shutdown_contract.rs`

**Interfaces:**

- Consumes: `EuiBootstrap`, Tokio bounded `mpsc`, and `AppState`.
- Produces: stable error constants `0..9`, lifecycle enum, request op/status enums, `CodegEuiSlice`, `CodegEuiSessionSummary`, `CodegEuiCompletion`, complete `CodegEuiFrame`, generic enqueue helper, monotonic request IDs, immutable `OwnedFrame` retention, and `begin_shutdown -> stopping polls -> final shutdown`.
- Completion transfer: pending completions are removed only while constructing a successful frame; the constructed `OwnedFrame` retains all copied bytes until replacement/shutdown.
- Shutdown visibility: `shutdown_ready=1` is returned only by a successful `stopping` poll that also exposes every remaining accepted request completion. Final `codeg_eui_shutdown` is rejected with `NOT_READY` until that exact frame has been observed.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 3 | Lifecycle, async requests, completions, immutable frames | ABI, runtime, queues, model, contract tests | `unsafe_ffi`: pointers/panics; `concurrency_lifecycle`: threads/queues/shutdown | `cross_runtime_or_process=2`, `shared_interface=1`, `multi_layer_without_test_seam=1`; total `4` | `high`: hard triggers and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write ABI layout, lifecycle-order, and input RED tests**

Cover this exact table in `bridge_contract.rs`:

```rust
#[test]
fn lifecycle_rejects_invalid_order_and_wrong_thread() {
    assert_eq!(shutdown(), ERR_INVALID_STATE);
    assert_eq!(init_empty(), OK);
    assert_eq!(init_empty(), ERR_INVALID_STATE);
    assert_eq!(poll_frame().unwrap().lifecycle_state, RUNNING);
    assert_eq!(std::thread::spawn(poll_code).join().unwrap(), ERR_WRONG_THREAD);
    assert_eq!(begin_shutdown(), OK);
    assert_eq!(enqueue_noop(), ERR_INVALID_STATE);
    assert_eq!(shutdown(), ERR_NOT_READY);
    assert_eq!(poll_frame().unwrap().lifecycle_state, STOPPING);
    drain_until_ready();
    assert_eq!(shutdown(), OK);
    assert_eq!(shutdown(), ERR_INVALID_STATE);
}

#[test]
fn strings_reject_null_invalid_utf8_and_bounds_without_accepting_a_request() {
    assert_eq!(enqueue_message(std::ptr::null(), 1), ERR_NULL_POINTER);
    assert_eq!(enqueue_message([0xff].as_ptr(), 1), ERR_INVALID_UTF8);
    assert_eq!(enqueue_message(vec![b'x'; MAX_MESSAGE + 1].as_ptr(), MAX_MESSAGE + 1),
               ERR_TOO_LARGE);
    assert_eq!(accepted_request_count(), 0);
}
```

The C header layout test also asserts the `shutdown_ready` offset and declares `codeg_eui_begin_shutdown`. Keep queue behavior for its own RED step below.

- [ ] **Step 2: Run focused bridge tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
```

Expected: FAIL on the first missing expanded session/completion ABI symbol or unimplemented wrong-thread/input contract; the M0 `shutdown_ready` field and basic two-phase order remain intact.

- [ ] **Step 3: Define the complete ABI v1 layout in Rust and C**

Use matching repr(C) definitions and discriminants:

```rust
#[repr(C)]
pub struct CodegEuiSlice { pub ptr: *const u8, pub len: usize }

#[repr(C)]
pub struct CodegEuiCompletion {
    pub request_id: u64,
    pub op: u32,
    pub status: u32, // ok=0, error=1, stale=2, cancelled=3
    pub result_payload: CodegEuiSlice,
    pub error: CodegEuiSlice,
}

#[repr(C)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub selection_epoch: u64,
    pub sessions: *const CodegEuiSessionSummary,
    pub sessions_len: usize,
    pub connection_id: CodegEuiSlice,
    pub event_seq: u64,
    pub transcript_json: CodegEuiSlice,
    pub live_assistant: CodegEuiSlice,
    pub stream_active: u8,
    pub needs_resync: u8,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 5],
    pub error_strip: CodegEuiSlice,
    pub completions: *const CodegEuiCompletion,
    pub completions_len: usize,
    pub t0_ns: u64,
    pub t_first_token_ns: u64,
    pub t_end_ns: u64,
}
```

Run C `static_assert` checks for every struct size/alignment and Rust `size_of/align_of/offset_of` tests. Keep all booleans as `uint8_t`; do not expose Rust `bool`, enums, `String`, `Vec`, or references. Mirror these lifecycle declarations exactly:

```c
int codeg_eui_begin_shutdown(void);
int codeg_eui_poll(CodegEuiFrame *out); /* legal in running and stopping */
int codeg_eui_shutdown(void);           /* final join/free after ready poll */
```

- [ ] **Step 4: Add the process-global state skeleton and panic boundary**

Use `OnceLock<Mutex<BridgeSlot>>`, capture `std::thread::ThreadId` on successful init, and wrap every exported function:

```rust
fn ffi_guard(body: impl FnOnce() -> i32 + UnwindSafe) -> i32 {
    match std::panic::catch_unwind(body) {
        Ok(code) => code,
        Err(_) => {
            record_panic_diagnostic("Rust panic contained at codeg-eui ABI");
            CODEG_EUI_ERR_PANIC
        }
    }
}
```

Stable errors are: `OK=0`, `INVALID_STATE=1`, `NULL_POINTER=2`, `INVALID_UTF8=3`, `TOO_LARGE=4`, `QUEUE_FULL=5`, `WRONG_THREAD=6`, `PANIC=7`, `INTERNAL=8`, `NOT_READY=9`. `codeg_eui_api_version` is the only call allowed from any thread and lifecycle state.

Use these concrete ownership types; methods are filled in by the following RED/GREEN pairs:

```rust
#[repr(u32)]
enum LifecycleState { Uninitialized = 0, Starting = 1, Running = 2,
                      Stopping = 3, Stopped = 4 }

struct BridgeSlot {
    lifecycle: LifecycleState,
    ui_thread: Option<std::thread::ThreadId>,
    runtime: Option<RuntimeOwner>,
    model: SharedModel,
    last_frame: Option<OwnedFrame>,
    shutdown_ready_observed: bool,
}

static BRIDGE: OnceLock<Mutex<BridgeSlot>> = OnceLock::new();
```

- [ ] **Step 5: Run layout/lifecycle tests to verify GREEN**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract lifecycle -- --nocapture
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R abi_layout --output-on-failure
```

Expected: complete layout, lifecycle order, wrong-thread, panic, and input cases pass; queue/frame/drain-with-in-flight-work cases remain unimplemented.

- [ ] **Step 6: Write bounded acceptance and exactly-once RED tests**

```rust
#[tokio::test]
async fn the_257th_request_is_rejected_before_acceptance() {
    let bridge = TestBridge::running_with_blocked_worker();
    let ids = (0..256).map(|_| bridge.enqueue_noop().unwrap()).collect::<Vec<_>>();
    assert!(ids.iter().all(|id| *id != 0));
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 256);
    assert_eq!(bridge.enqueue_noop(), Err(CODEG_EUI_ERR_QUEUE_FULL));
    assert_eq!(bridge.accepted_count(), 256);
}
```

Add worker error, worker panic, selection-stale, and duplicate-terminalization cases. Each accepted ID must have one reserved completion slot and exactly one terminal state.

- [ ] **Step 7: Run queue tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract queue -- --nocapture
```

Expected: FAIL because command admission and completion reservation are absent.

- [ ] **Step 8: Implement bounded async acceptance and terminalization**

`RuntimeCommand` carries `{request_id, selection_epoch, op}`. The UI call validates input, reserves completion capacity, allocates the next ID with checked increment, and `try_send`s into a 256-slot channel. The worker catches task errors/panics and calls one terminalization function guarded by a `HashSet<u64>`:

```rust
fn terminalize(&mut self, completion: OwnedCompletion) {
    assert!(self.accepted.remove(&completion.request_id),
            "accepted request terminalized more than once");
    self.completions.push_back(completion);
}
```

Capacity accounting reserves one terminal completion slot per accepted request; the 256 bound therefore cannot overflow after acceptance. Mark a result `stale` when its captured `selection_epoch` differs from the model's current epoch, but still emit it exactly once.

Define the queue boundary explicitly:

```rust
struct RuntimeCommand {
    request_id: NonZeroU64,
    selection_epoch: u64,
    op: Operation,
    payload: CommandPayload,
}

struct CompletionLedger {
    accepted: HashSet<NonZeroU64>,
    ready: VecDeque<OwnedCompletion>,
    reserved: usize,
}
```

- [ ] **Step 9: Run queue tests to verify GREEN**

Run the Step 7 command. Expected: 256 requests are accepted, request 257 is rejected before acceptance, and all released requests terminalize once.

- [ ] **Step 10: Write immutable-frame and completion-transfer RED tests**

Prove enqueue/model mutation cannot invalidate frame A, a failed poll leaves frame A valid, successful poll B drains each ready completion once, and B's bytes stay readable through later enqueue calls. Assert the same request ID is absent from frame C.

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract frame -- --nocapture
```

Expected: FAIL because `OwnedFrame` retention and atomic completion transfer are absent.

- [ ] **Step 11: Implement immutable frame construction and completion transfer**

`OwnedFrame` owns `Vec<u8>` for every string, `Vec<OwnedCompletion>`, and parallel repr(C) views. Build every pointer only after all owning vectors have final capacity. On successful poll: lock model, move at most all currently pending completions into a new `OwnedFrame`, increment frame generation, swap it into `BridgeSlot.last_frame`, then copy only the top-level repr(C) value to `*out`. Enqueue and model updates never touch `last_frame`.

- [ ] **Step 12: Run frame tests to verify GREEN**

Run the Step 10 command. Expected: all pointer-lifetime and completion-transfer cases pass.

- [ ] **Step 13: Write public two-phase shutdown RED tests**

`shutdown_contract.rs` uses barriers to accept a controllable blocked request, calls `codeg_eui_begin_shutdown`, and asserts: new enqueue is rejected; poll remains legal in `stopping`; final shutdown returns `NOT_READY` before a ready frame; cancellation terminalizes the blocked ID; one successful poll exposes that completion and `shutdown_ready=1`; only then final shutdown returns `OK`, joins the runtime, frees the frame, and enters `stopped`.

Create `shutdown_drain_test.cpp` as the black-box consumer. Build the staticlib with Cargo feature `ffi-test-hooks`; the feature exposes only `codeg_eui_test_enqueue_blocked(uint64_t*)` as a stimulus. The C++ test uses the ordinary public lifecycle/poll ABI and must copy the result before final free:

```cpp
ASSERT_EQ(codeg_eui_init(root.data(), root.size()), CODEG_EUI_OK);
uint64_t requestId = 0;
ASSERT_EQ(codeg_eui_test_enqueue_blocked(&requestId), CODEG_EUI_OK);
ASSERT_EQ(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);

std::vector<Completion> seen;
for (int attempt = 0; attempt < 200; ++attempt) {
  CodegEuiFrame frame{};
  ASSERT_EQ(codeg_eui_poll(&frame), CODEG_EUI_OK);
  appendCopiedCompletions(frame, seen);
  if (frame.shutdown_ready == 1) break;
  std::this_thread::sleep_for(std::chrono::milliseconds(5));
}
ASSERT_EQ(countCompletion(seen, requestId, CODEG_EUI_COMPLETION_CANCELLED), 1);
ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_OK);
```

The test hook is absent from normal builds and normal headers unless `CODEG_EUI_TEST_HOOKS` is defined.

Add this opt-in feature to `src-tauri/codeg-eui-core/Cargo.toml`; do not include it in `default`:

```toml
[features]
default = []
ffi-test-hooks = []
```

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test shutdown_contract
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-abi-link -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a"
cmake --build codeg-eui/build-abi-link --parallel
ctest --test-dir codeg-eui/build-abi-link -R shutdown_drain --output-on-failure
```

Expected: RED because stopping polls, `shutdown_ready`, and finalization gating are absent.

- [ ] **Step 14: Implement begin-drain, stopping polls, and final shutdown**

`codeg_eui_begin_shutdown` is UI-thread-only and legal only in `running`. It changes state to `stopping`, closes command admission, cancels all accepted work, and starts `disconnect_all(ApplicationShutdown)` without joining or freeing. `codeg_eui_poll` remains non-blocking in `stopping`; after workers/connections quiesce and `accepted` is empty, its next successful frame transfers the final completions, sets `shutdown_ready=1`, and records `shutdown_ready_observed=true` only after copying the frame to `*out`. `codeg_eui_shutdown` requires `stopping && shutdown_ready_observed`, joins Tokio, drops `last_frame`, and sets `stopped`; it never creates an unobservable completion.

```rust
fn final_shutdown(slot: &mut BridgeSlot) -> i32 {
    if slot.lifecycle != LifecycleState::Stopping || !slot.shutdown_ready_observed {
        return CODEG_EUI_ERR_NOT_READY;
    }
    slot.runtime.take().expect("runtime while stopping").join();
    slot.last_frame = None;
    slot.lifecycle = LifecycleState::Stopped;
    CODEG_EUI_OK
}
```

- [ ] **Step 15: Run public shutdown tests to verify GREEN**

Run every command from Step 13. Expected: the C++ consumer observes the cancelled request once before final shutdown, and Rust verifies disconnect/join/free ordering without timing sleeps.

- [ ] **Step 16: Write the C++ deep-copy RED test**

`ui_snapshot_test.cpp` copies frame A, triggers successful frame B, completes the two-phase shutdown, and then reads only the copied A value strings. It also rejects `(ptr=null,len>0)` and accepts `(ptr=null,len=0)`.

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R ui_snapshot --output-on-failure
```

Expected: FAIL because `copy_frame` is not implemented.

- [ ] **Step 17: Implement and verify the C++ deep-copy boundary**

`ui_snapshot.h` defines value-owned C++ strings/vectors and performs the copy immediately after `codeg_eui_poll` returns `OK`. Implement the null/length checks and copy every nested slice/vector exercised by the Step 16 test. C++ never stores any raw Rust pointer in page state.

Run the Step 16 command. Expected: PASS.

- [ ] **Step 18: Run Task 3 automated verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test shutdown_contract
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a"
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract --output-on-failure
```

Expected: lifecycle order, UI affinity, panic containment, input bounds, queue backpressure, one completion per accepted ID, stale marking, frame lifetime, externally observable drain, and final shutdown all pass.

- [ ] **Step 19: Commit and prepare the Task 3 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/tests/bridge_contract.rs src-tauri/codeg-eui-core/tests/shutdown_contract.rs codeg-eui/CMakeLists.txt codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/tests/ui_snapshot_test.cpp codeg-eui/tests/shutdown_drain_test.cpp
git add -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/tests/bridge_contract.rs src-tauri/codeg-eui-core/tests/shutdown_contract.rs codeg-eui/CMakeLists.txt codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/tests/ui_snapshot_test.cpp codeg-eui/tests/shutdown_drain_test.cpp
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): implement async bridge lifecycle"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core codeg-eui/app/bridge codeg-eui/tests codeg-eui/CMakeLists.txt
```

Expected package: one ABI/lifecycle commit with matching Rust/C layouts, public stopping-poll evidence, and no generated build/archive path staged. Route it to both high-risk reviewers, then continue directly to Task 4.

### Task 4: Add the Grok and Codex Settings Facade and Async Bridge Operations

**Milestone:** M2.

**Files:**

- Create: `src-tauri/src/commands/eui_facade.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Expand: `src-tauri/codeg-eui-core/src/commands.rs`
- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Test: `src-tauri/codeg-eui-core/tests/settings_contract.rs`
- Test: unit tests in `src-tauri/src/commands/eui_facade.rs`

**Interfaces:**

- Consumes: `acp_list_agents_core`, `acp_update_agent_config_and_refresh`, `acp_update_agent_env_and_refresh`, `acp_preflight_core`, `AcpAgentInfo`, `CodexSandboxStructuredConfig`, and `GrokStructuredConfig`.
- Produces: public `EuiAgentSettings`, `EuiAgentSettingsPatch`, `EuiAgentProbe`, `get_eui_agent_settings`, `set_eui_agent_settings`, and `probe_eui_agent`; async C functions `codeg_eui_get_agent_settings`, `codeg_eui_set_agent_settings`, and `codeg_eui_probe_agent`.
- Restricts: only wire values `"codex"` and `"grok"`; every other agent returns `EuiFacadeError::UnsupportedAgent` before file or DB access.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 4 | Grok/Codex settings facade and async operations | ACP facade, DTOs, bridge handlers, tests | `security_trust_boundary`: auth/config writes; `public_compatibility`: new shared facade | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write facade round-trip tests before adding the facade**

Use isolated `CODEX_HOME`, `GROK_HOME` when supported by the existing helpers, a temporary OS home otherwise under a serialized env mutex, and a fresh disk DB. Assert the DTO contains only backend-owned fields:

```rust
#[tokio::test]
async fn codex_settings_round_trip_through_existing_native_files() {
    let fixture = SettingsFixture::new(AgentType::Codex).await;
    let patch = EuiAgentSettingsPatch {
        enabled: Some(true),
        env: Some(BTreeMap::from([("OPENAI_API_KEY".into(), "test-key".into())])),
        model_provider_id: None,
        config_json: None,
        codex_auth_json: Some(r#"{"OPENAI_API_KEY":"test-key"}"#.into()),
        codex_config_toml: Some("model = \"gpt-5\"\napproval_policy = \"never\"\n".into()),
        codex_model_catalog: None,
        codex_sandbox: None,
        grok_config_toml: None,
        grok_structured: None,
    };
    set_eui_agent_settings(fixture.state(), AgentType::Codex, patch).await.unwrap();
    let got = get_eui_agent_settings(fixture.state(), AgentType::Codex).await.unwrap();
    assert_eq!(got.agent_type, AgentType::Codex);
    assert_eq!(got.codex_config_toml.as_deref(),
               Some("model = \"gpt-5\"\napproval_policy = \"never\"\n"));
    assert!(got.grok_config_toml.is_none());
}
```

Add the equivalent Grok raw/structured round-trip and a test proving `ClaudeCode` is rejected before any filesystem path is touched.

- [ ] **Step 2: Run the facade tests to verify RED**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
```

Expected: FAIL because `commands::eui_facade` does not exist.

- [ ] **Step 3: Define the narrow backend-aligned DTOs**

Use serde camelCase only at this public Rust boundary:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiAgentSettings {
    pub agent_type: AgentType,
    pub available: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
    pub env: BTreeMap<String, String>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxSettings>,
    pub grok_config_toml: Option<String>,
    pub grok_settings: Option<GrokSettings>,
    pub model_provider_id: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EuiAgentSettingsPatch {
    pub enabled: Option<bool>,
    pub env: Option<BTreeMap<String, String>>,
    pub model_provider_id: Option<i32>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxStructuredConfig>,
    pub grok_config_toml: Option<String>,
    pub grok_structured: Option<GrokStructuredConfig>,
}
```

Do not expose OpenCode, Cursor, Cline, Hermes, installation/download mutation, Axum parameter structs, or transport-specific status codes.

- [ ] **Step 4: Implement the facade over existing ACP cores**

Keep the existing ACP helpers `pub(crate)`: `eui_facade` is a sibling module in the same `codeg` crate and can call them without exposing a wider API or editing `commands/acp.rs` visibility. `get` calls `acp_list_agents_core`, selects exactly one row, and projects fields. `set` first validates agent-specific field exclusivity, then applies env/preferences and config through the existing refresh helpers. It never writes TOML/JSON directly. `probe` calls `acp_preflight_core(agent, Some(true), db)` and returns `{launchable, installed_version, message}`.

- [ ] **Step 5: Write failing async completion tests**

Inject a `CoreOps` test implementation into the command worker. Prove get/probe run off the UI thread and result JSON arrives through a later frame:

```rust
#[tokio::test]
async fn slow_probe_never_blocks_poll_and_completes_once() {
    let gate = Arc::new(Notify::new());
    let bridge = TestBridge::with_ops(SlowProbeOps::new(gate.clone()));
    let request_id = bridge.enqueue_probe("codex").unwrap();
    assert!(bridge.poll_within(Duration::from_millis(20)).completions.is_empty());
    gate.notify_one();
    let completion = bridge.wait_completion(request_id).await;
    assert_eq!(completion.status, CompletionStatus::Ok);
    assert_eq!(completion.op, Operation::ProbeAgent);
    assert_eq!(bridge.completion_count(request_id), 1);
}
```

- [ ] **Step 6: Implement settings/probe ABI entry points**

Each entry uses the generic validated enqueue helper. `set_agent_settings` parses JSON with `deny_unknown_fields` after the 2 MiB bound and before acceptance. Result payloads are UTF-8 JSON serialized from the facade DTO; errors are diagnostic strings with no secret values. Redact auth/env values from tracing.

- [ ] **Step 7: Run M2 verification**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test settings_contract -- --test-threads=1
cargo check --manifest-path src-tauri/codeg-eui-core/Cargo.toml
```

Expected: Codex and Grok read/write/probe paths round-trip through existing helpers, unsupported agents fail closed, malformed/oversized JSON is rejected before acceptance, slow probe does not block poll, and every accepted settings request completes once.

- [ ] **Step 8: Commit and prepare the Task 4 review package**

```bash
git add --dry-run -- src-tauri/src/commands/eui_facade.rs src-tauri/src/commands/mod.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/tests/settings_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git add -- src-tauri/src/commands/eui_facade.rs src-tauri/src/commands/mod.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/tests/settings_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): expose Grok and Codex settings facade"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/src/commands src-tauri/codeg-eui-core codeg-eui/app/bridge/codeg_eui_bridge.h
```

Expected package: one settings/probe commit, with no new config persistence implementation and no secret-bearing logs. Route it to both high-risk reviewers, then continue directly to Task 5.

### Task 5: Add Workspace, Conversation, Connection, History, and Send Operations

**Milestone:** M3 plus send admission needed by M4.

**Files:**

- Expand: `src-tauri/src/commands/eui_facade.rs`
- Expand: `src-tauri/codeg-eui-core/src/commands.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Test: unit tests in `src-tauri/src/commands/eui_facade.rs`
- Test: `src-tauri/codeg-eui-core/tests/session_contract.rs`

**Interfaces:**

- Consumes: `open_folder_core`, `create_project_conversation_core`, `get_folder_conversation_with_live_core`, `build_acp_launch_inputs`, `verify_agent_installed`, `ConnectionManager::spawn_agent`, and `send_prompt_linked_with_message_id`.
- Produces: `EuiWorkspace`, `EuiSessionSummary`, `EuiSessionSelection`, `set_eui_workspace`, `create_eui_conversation`, `create_eui_session`, `select_eui_session`, `send_eui_message`; all corresponding ABI enqueue functions; model `selection_epoch` increments on workspace/session change.
- Session ownership: EUI connections use owner label `"eui"`, user launch context from DB, no delegation route override, and only Grok/Codex.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 5 | Workspace, conversation, connection, history, send | session facade, bridge commands, DB/manager tests | `concurrency_lifecycle`: process spawn, linked send, selection epochs | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `broad_production_surface=1`; total `5` | `high`: lifecycle hard trigger and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write workspace and persisted-session facade tests**

Use a fresh DB and a real temporary directory:

```rust
#[tokio::test]
async fn workspace_and_conversation_reuse_existing_database_cores() {
    let state = eui_test_state().await;
    let workspace = set_eui_workspace(&state, fixture_dir()).await.unwrap();
    assert_eq!(workspace.path, fixture_dir().canonicalize().unwrap());
    let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Grok)
        .await.unwrap();
    assert!(row.conversation_id > 0);
    assert_eq!(row.agent_type, AgentType::Grok);
    assert_eq!(state.db_count_regular_conversations().await, 1);
}
```

Add invalid/non-directory workspace tests, Codex/Grok acceptance, unsupported agent rejection, and history projection from `get_folder_conversation_with_live_core` using `HistoryLoadOpts { user_turn_limit: Some(100), before_turn_id: None }`.

- [ ] **Step 2: Run facade tests to verify RED**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests::workspace_and_conversation -- --nocapture
cd ..
```

Expected: FAIL because the session facade functions do not exist.

- [ ] **Step 3: Implement workspace and conversation DTOs**

Canonicalize and verify an existing directory before `open_folder_core`. Create the DB row with `create_project_conversation_core(&state.db.conn, workspace.folder_id, agent_type, None, None)`. The DTOs carry only `folder_id`, absolute path, `conversation_id`, title, agent, status, external session ID, and transcript turns serialized as backend `MessageTurn` JSON. Do not expose `AppState`, DB connections, or parser objects.

- [ ] **Step 4: Write spawn/send tests with deterministic manager seams**

Use `test-utils` manager connections or an injected `EuiSessionOps` implementation to prove the exact orchestration:

```rust
#[tokio::test]
async fn create_session_builds_launch_inputs_before_spawn_and_binds_on_send() {
    let ops = RecordingSessionOps::default();
    let bridge = TestBridge::with_session_ops(ops.clone());
    let create_id = bridge.enqueue_create_session("codex").unwrap();
    let create = bridge.wait_completion(create_id).await;
    assert_eq!(create.status, CompletionStatus::Ok);
    assert_eq!(ops.calls(), ["verify_installed", "build_launch_inputs", "spawn_agent"]);
    let send_id = bridge.enqueue_send("hello").unwrap();
    assert_eq!(ops.last_send().unwrap().conversation_id, create.conversation_id());
    assert_eq!(bridge.wait_completion(send_id).await.status, CompletionStatus::Ok);
}
```

Add a test where session selection changes during slow create/send and the old completion arrives once with `stale`.

- [ ] **Step 5: Implement create/select/send through shared core paths**

`create_eui_session` verifies installation, builds launch inputs with `AcpRouteRequest::root(Some(conversation_id), None)`, loads `user_launch_context_from_db`, and calls `spawn_agent` with workspace path and owner `"eui"`. `select_eui_session` loads the persisted row/history, reuses `find_connection_by_conversation_id` when live, or spawns with the row's `external_id` when resuming. `send_eui_message` builds exactly one `PromptInputBlock::Text`, uses a UUID client message ID, and calls `send_prompt_linked_with_message_id` with folder/conversation IDs.

- [ ] **Step 6: Expose all async session ABI calls**

Implement `set_workspace`, `create_session`, `select_session`, and `send_user_message` with Task 3 validation/acceptance. On successful create, the completion JSON contains `conversationId` and `connectionId`. On selection, update the model's transcript/session list and increment `selection_epoch` before launching slow work so prior operations become stale. On send acceptance, record native `t0_ns` immediately after enqueue succeeds; Task 9 consumes the marker.

- [ ] **Step 7: Run M3/session verification**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test session_contract
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
```

Expected: invalid workspace fails without a row, only Grok/Codex create, history is backend-derived, launch order is recorded, linked sends carry the selected IDs, poll remains non-blocking, and selection changes mark old completions stale exactly once.

- [ ] **Step 8: Commit and prepare the Task 5 review package**

```bash
git add --dry-run -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/session_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git add -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/session_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add workspace and session command loop"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core codeg-eui/app/bridge/codeg_eui_bridge.h
```

Expected package: one session-loop commit with DB, launch, selection, history, and send tests. Route it to both high-risk reviewers, then continue directly to Task 6.

### Task 6: Project Live State with Snapshot Recovery and Deterministic Interaction Decline

**Milestone:** M4 backend/live path.

**Files:**

- Create: `src-tauri/codeg-eui-core/src/live.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Expand: `src-tauri/codeg-eui-core/src/runtime.rs`
- Expand: `src-tauri/codeg-eui-core/src/perf.rs`
- Test: `src-tauri/codeg-eui-core/tests/live_recovery.rs`
- Test: `src-tauri/codeg-eui-core/tests/interaction_decline.rs`

**Interfaces:**

- Consumes: `ConnectionManager::get_state`, `SessionState::{to_snapshot,event_stream}`, `EventEnvelope.seq`, `AcpEvent`, `respond_permission`, `cancel`, `cancel_question`, and `cancel_plan_approvals_by_parent`.
- Produces: `LiveProjector::attach(connection_id, selection_epoch)`, `snapshot_and_subscribe`, `Projection::replace_from_snapshot`, `Projection::apply_envelope`, `reconcile_snapshot_interactions`, `needs_resync`, generation-counted assistant/transcript output, and `decline_interaction`.
- Recovery invariant: snapshot and subscribe happen under one `SessionState` read lock; after replacement at sequence `S`, pending snapshot interactions are declined exactly once before the receiver resumes, and only envelopes `S+1` onward may mutate the projection.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 6 | Live snapshot recovery and deterministic decline | projector, permission policy, frame projection, tests | `concurrency_lifecycle`: attach/lag/overflow/switch; `security_trust_boundary`: permission/question/plan fail closed | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`; total `4` | `high`: both hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write the atomic attach race RED test**

Drive a real test `SessionState` and stream with barriers, not timing sleeps:

```rust
#[tokio::test]
async fn attach_cannot_miss_event_between_snapshot_and_subscribe() {
    let fixture = LiveFixture::new().await;
    fixture.pause_attach_while_read_locked();
    let attach = tokio::spawn(fixture.projector().attach(fixture.connection_id(), 1));
    fixture.emit_after_attach_attempt(AcpEvent::ContentDelta {
        text: "hello".into(), parent_tool_use_id: None,
    }).await;
    fixture.release_attach();
    let projector = attach.await.unwrap().unwrap();
    assert_eq!(projector.snapshot().live_assistant, "hello");
    assert_eq!(projector.snapshot().event_seq, 1);
}

```

- [ ] **Step 2: Run the attach test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery attach_cannot_miss -- --nocapture
```

Expected: FAIL because the attach API and lock seam do not exist.

- [ ] **Step 3: Add the projector skeleton and atomic snapshot/subscription helper**

```rust
pub struct AttachPoint {
    pub snapshot: LiveSessionSnapshot,
    pub receiver: tokio::sync::broadcast::Receiver<EventEnvelope>,
}

async fn snapshot_and_subscribe(state: &Arc<RwLock<SessionState>>) -> AttachPoint {
    let guard = state.read().await;
    AttachPoint {
        snapshot: guard.to_snapshot(),
        receiver: guard.event_stream().subscribe(),
    }
}

pub struct LiveProjector {
    connection_id: String,
    selection_epoch: u64,
    cursor: u64,
    projection: Arc<Mutex<Projection>>,
    declined: HashSet<InteractionKey>,
}
```

`attach` obtains `AttachPoint`, releases the read lock, and replaces the projection at `snapshot.event_seq`. Once the interaction policy below is present, it reconciles all pending interactions in that snapshot before consuming the already-subscribed receiver. No await or event emission occurs while holding the state read lock.

- [ ] **Step 4: Run the attach test to verify GREEN**

Run the Step 2 command. Expected: the event attempted during attach is either in the snapshot or receiver, never missed or duplicated.

- [ ] **Step 5: Write gap, lag, overflow, switch, and parity RED tests**

Add the sequence-gap test plus broadcast `Lagged`, a full 128-control-event queue containing permission plus `TurnComplete`, text delta coalescing, session switch during stream, and final JSON parity between `Projection` and `SessionState::to_snapshot()`. The old selection task must terminate without overwriting the new selection, while its accepted request completion still arrives stale.

```rust
#[tokio::test]
async fn sequence_gap_replaces_projection_from_authoritative_snapshot() {
    let mut projector = projector_at_seq(4, "partial");
    projector.apply(envelope(6, delta("wrong"))).await;
    assert!(projector.snapshot().needs_resync);
    projector.resync(authoritative_snapshot(6, "final")).await.unwrap();
    assert_eq!(projector.snapshot().live_assistant, "final");
    assert_eq!(projector.snapshot().event_seq, 6);
    assert!(!projector.snapshot().needs_resync);
}
```

- [ ] **Step 6: Run recovery tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery
```

Expected: FAIL on the first unimplemented gap/lag/overflow recovery path.

- [ ] **Step 7: Implement merge and authoritative resubscription**

Coalesce consecutive text into the live assistant buffer; reduce tools to `{name,status}` summaries; carry errors/turn state explicitly. Any `seq != cursor+1`, `RecvError::Lagged`, or control enqueue failure sets `needs_resync`, discards the old receiver, calls `snapshot_and_subscribe` again, replaces from that authoritative snapshot, reconciles snapshot interactions through the shared decline policy below, then resumes from the new receiver. Producers never await the projector.

- [ ] **Step 8: Run recovery tests to verify GREEN**

Run the Step 6 command. Expected: attach, gap, lag, overflow, coalescing, switch isolation, and final parity pass without sleeps.

- [ ] **Step 9: Write live-event and snapshot-only interaction RED tests**

```rust
#[tokio::test]
async fn permission_uses_reject_option_or_cancels_turn() {
    let manager = RecordingManager::default();
    decline_permission(&manager, "c1", "r1", &[
        option("allow", "Allow once", "allow_once"),
        option("deny", "Deny", "reject_once"),
    ]).await.unwrap();
    assert_eq!(manager.permission_response(), Some(("r1", "deny")));

    decline_permission(&manager, "c1", "r2", &[]).await.unwrap();
    assert_eq!(manager.cancel_count(), 1);
}
```

Add question and plan-approval event cases that resolve the parked receiver, emit the resolved event, surface `Interactive prompts require the main app` in the EUI error strip, and reach `TurnComplete` or hard error within a bounded test timeout.

Add initial-attach and resync fixtures for each snapshot field with no corresponding request event after the snapshot cursor: `pending_permission.request_id`, `pending_question.question_id`, and `pending_plan_approval.approval_id`. For each fixture, assert the appropriate manager method is invoked once before `receive_next` begins, a second resync containing the same ID does not invoke it again, and the turn reaches `t_end` or a surfaced hard error.

```rust
#[tokio::test]
async fn snapshot_pending_interactions_decline_before_event_resume_once() {
    for case in [PendingCase::Permission, PendingCase::Question, PendingCase::Plan] {
        let fixture = InteractionFixture::snapshot_only(case).await;
        let projector = fixture.attach().await.unwrap();
        assert_eq!(fixture.decline_count(case), 1);
        assert!(!projector.has_started_receive_before_decline());
        projector.resync(fixture.same_pending_snapshot()).await.unwrap();
        assert_eq!(fixture.decline_count(case), 1);
        fixture.assert_terminal_within(Duration::from_secs(2)).await;
    }
}
```

- [ ] **Step 10: Run interaction tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test interaction_decline -- --nocapture
```

Expected: live-event policy and snapshot-only reconciliation cases fail before implementation.

- [ ] **Step 11: Implement one deduplicated decline function for events and snapshots**

Choose a permission option whose normalized `kind`, then `name`, then `option_id` contains `reject` or `deny`; call `respond_permission`. If none exists, call `ConnectionManager::cancel`. For a question, call `cancel_question(connection_id, question_id)`. For a plan approval, call `cancel_plan_approvals_by_parent(connection_id)`. Do not mutate persisted question/feedback settings.

Both event and snapshot paths create the same key and call the same function:

```rust
#[derive(Clone, Hash, Eq, PartialEq)]
enum InteractionKey { Permission(String), Question(String), Plan(String) }

async fn decline_once(
    manager: &ConnectionManager,
    connection_id: &str,
    interaction: PendingInteraction,
    seen: &mut HashSet<InteractionKey>,
) -> Result<(), AcpError> {
    let key = interaction.key();
    if !seen.insert(key) { return Ok(()); }
    decline_interaction(manager, connection_id, interaction).await
}
```

Immediately after every `replace_from_snapshot`, call `decline_once` for `pending_permission`, `pending_question`, and `pending_plan_approval` in that order, then begin receiving at `snapshot.event_seq + 1`. If decline fails, cancel the active turn, set a hard error, and do not resume with a parked responder.

- [ ] **Step 12: Run interaction tests to verify GREEN**

Run the Step 10 command. Expected: event and snapshot-only interactions resolve once and terminate without parking.

- [ ] **Step 13: Write native marker/frame RED tests**

Assert `t_first_token_ns` is set once on the first authoritative non-empty assistant buffer, resync does not move it, `t_end_ns` is set by `TurnComplete` or hard error, and `needs_resync` remains visible until replacement commits.

- [ ] **Step 14: Run native marker tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery marker -- --nocapture
```

Expected: FAIL because frame marker propagation is absent.

- [ ] **Step 15: Wire projector output and native markers into frames**

The projector updates the shared model without touching `last_frame`; poll picks it up on the next dirty generation. Set `t_first_token_ns` exactly once when the authoritative/coalesced live assistant first becomes non-empty after `t0`. Set `t_end_ns` on `TurnComplete` or hard error. `needs_resync` remains true in frames until the replacement snapshot is committed.

- [ ] **Step 16: Run marker tests to verify GREEN**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery marker -- --nocapture
```

- [ ] **Step 17: Run M4 live-path verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test interaction_decline
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test session_contract
```

Expected: race-free attach, lag/overflow resync, non-blocking producers, session-switch isolation, final snapshot parity, snapshot-before-resume decline, exactly-once completions, and terminal decline all pass.

- [ ] **Step 18: Commit and prepare the Task 6 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/tests/live_recovery.rs src-tauri/codeg-eui-core/tests/interaction_decline.rs
git add -- src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/tests/live_recovery.rs src-tauri/codeg-eui-core/tests/interaction_decline.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add recoverable live stream projection"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core
```

Expected package: one live-path commit with recovery and fail-closed interaction evidence. Route it to both high-risk reviewers, then continue directly to Task 7.

### Task 7: Build the Native Shell, Chat, and P0 Settings UI

**Milestone:** M0-M4 visible product loop.

**Files:**

- Expand: `codeg-eui/app/app.cpp`
- Expand: `codeg-eui/app/bridge/ui_snapshot.h`
- Create: `codeg-eui/app/bridge/client.h`
- Create: `codeg-eui/app/model.h`
- Create: `codeg-eui/app/pages/shell.h`
- Create: `codeg-eui/app/pages/chat.h`
- Create: `codeg-eui/app/pages/settings.h`
- Modify: `codeg-eui/CMakeLists.txt`
- Test: `codeg-eui/tests/client_completion_test.cpp`
- Test: `codeg-eui/tests/app_lifecycle_test.cpp`
- Test: `codeg-eui/tests/shell_page_test.cpp`
- Test: `codeg-eui/tests/chat_page_test.cpp`
- Test: `codeg-eui/tests/settings_page_test.cpp`

**Interfaces:**

- Consumes: copied `UiSnapshot`, ABI enqueue functions, request completions, and EUI `components::{button,input,scrollView,markdown}`.
- Produces: `BridgeClient`, `ShellPage`, `ChatPage`, `SettingsPage`, `AppModel`, 60 Hz maximum poll cadence, generation-driven page updates, and a recognizable Codeg dark shell.
- UI state rule: pages hold C++ values and pending request IDs only; every Rust pointer dies at the `copy_frame` boundary.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 7 | Native shell, chat, and P0 settings UI | C++ bridge/model/pages/tests | none | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `5` | `high`: soft threshold reached | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write typed client/request-state RED tests**

`client_completion_test.cpp` verifies accepted enqueue tracking, immediate ABI rejection without a pending entry, operation mismatch rejection, duplicate completion rejection, and stale completion behavior:

```cpp
TEST(BridgeClient, stale_completion_finishes_request_without_mutating_selection) {
  AppModel model;
  model.selectionEpoch = 4;
  model.pending.emplace(9, PendingRequest{Operation::SelectSession, 3});
  model.apply(Completion{9, Operation::SelectSession,
                         CompletionStatus::Stale, "{}", ""});
  EXPECT_TRUE(model.pending.empty());
  EXPECT_EQ(model.selectionEpoch, 4u);
  EXPECT_TRUE(model.currentConnectionId.empty());
}
```

- [ ] **Step 2: Run client tests to verify RED**

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R client_completion --output-on-failure
```

Expected: configure or compile FAIL because `BridgeClient`, `PendingRequest`, and `AppModel` are absent.

- [ ] **Step 3: Implement the typed client and value-owned model skeleton**

`copy_frame` converts every slice with `(ptr==nullptr && len==0)` as empty and rejects `(ptr==nullptr && len>0)` as an internal bridge error. `BridgeClient` owns `std::unordered_map<uint64_t, PendingRequest>`, rejects duplicate completion IDs, dispatches JSON only after checking expected op, and turns ABI enqueue errors into the global error strip without inserting a pending request.

```cpp
struct PendingRequest {
  Operation op;
  uint64_t selectionEpoch;
};

struct AppModel {
  UiSnapshot snapshot;
  uint64_t selectionEpoch = 0;
  std::unordered_map<uint64_t, PendingRequest> pending;
  std::string errorStrip;
  void apply(const Completion& completion);
};

class BridgeClient {
 public:
  explicit BridgeClient(AppModel& model) : model_(model) {}
  bool pollIfDue(std::chrono::steady_clock::time_point now);
  void dispatch(const UiSnapshot& frame);
 private:
  AppModel& model_;
};
```

- [ ] **Step 4: Run client tests to verify GREEN**

Run the Step 2 command. Expected: all request-state and copied-value cases pass.

- [ ] **Step 5: Write lifecycle, drain, poll-cadence, and smoke-hook RED tests**

`app_lifecycle_test.cpp` uses a fake `BridgeApi` to prove: init occurs once; poll is called no faster than 16 ms; close calls begin-shutdown once; stopping frames continue dispatching completions; final shutdown is attempted only after a copied `shutdownReady` frame; invalid/zero smoke values disable; values `1` and `3` request GLFW close on exactly callback N.

```cpp
TEST(ShutdownDriver, dispatches_stopping_completions_before_final_free) {
  FakeBridgeApi api;
  api.frames = {stoppingFrame(cancelled(41), false), stoppingFrame({}, true)};
  ShutdownDriver driver(api);
  driver.drainAndShutdown();
  EXPECT_EQ(api.calls, std::vector<std::string>({
      "begin_shutdown", "poll", "dispatch:41", "poll", "shutdown"}));
}
```

- [ ] **Step 6: Run lifecycle tests to verify RED**

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R app_lifecycle --output-on-failure
```

Expected: FAIL because `ShutdownDriver`, cadence control, and `SmokeFrameExit` are absent.

- [ ] **Step 7: Implement app lifecycle, stopping-poll drain, and smoke hook**

Use EUI's public app main and this ownership shape:

```cpp
const DslAppConfig& dslAppConfig() {
  static const DslAppConfig config = DslAppConfig{}
      .title("Codeg EUI Spike")
      .pageId("codeg_eui")
      .windowSize(1180, 760)
      .fps(60.0);
  return config;
}

void compose(eui::Ui& ui, const eui::Screen& screen) {
  AppModel& model = appModel();
  model.bridge.pollIfDue(std::chrono::steady_clock::now());
  model.applyNewFrame();
  model.shell.compose(ui, screen.width, screen.height, model);
}
```

Initialize before the first compose and redraw page content only when copied frame generation or local input state changes. Poll no faster than every 16 ms in the visible loop. The one RAII owner calls this exact close sequence on the UI thread after the GLFW loop asks to close:

```cpp
void ShutdownDriver::drainAndShutdown() {
  requireOk(api_.beginShutdown());
  for (;;) {
    UiSnapshot frame = copy_frame(requirePoll(api_));
    client_.dispatch(frame);
    if (frame.shutdownReady) break;
    std::this_thread::sleep_for(std::chrono::milliseconds(5));
  }
  requireOk(api_.shutdown());
}
```

Use a bounded 5-second deadline; timeout is a surfaced fatal shutdown error and non-zero process exit, never an unobserved forced free.

Parse `CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES` once at startup as a positive decimal `uint64_t`; unset, empty, zero, or invalid values disable the hook. When enabled, compose a persistent 1x1 transparent ticker with `onFrame`. Count callbacks only after the first nonblank shell composition, and on callback N call `glfwSetWindowShouldClose(glfwGetCurrentContext(), GLFW_TRUE)`. The public GLFW app main then leaves its loop normally, and the bridge RAII owner calls `codeg_eui_shutdown`; never call `_Exit`, `quick_exit`, or `abort` from the smoke hook.

- [ ] **Step 8: Run lifecycle tests to verify GREEN**

Run the Step 6 command. Expected: cadence, stopping completion dispatch, finalization order, deadline, and smoke counter pass.

- [ ] **Step 9: Write shell layout/navigation RED tests**

`shell_page_test.cpp` exercises pure `ShellLayout::calculate(width,height)` and route state. Assert fixed 248 px sidebar, 48 px header, 44 px composer, non-negative content at 800x600 and 1440x900, global error-strip placement, New enablement from workspace+agent, and Chat/Settings navigation.

```cpp
TEST(ShellLayout, remains_valid_at_minimum_supported_size) {
  const auto layout = ShellLayout::calculate(800, 600);
  EXPECT_EQ(layout.sidebar.width, 248);
  EXPECT_EQ(layout.header.height, 48);
  EXPECT_GE(layout.content.width, 0);
  EXPECT_FALSE(layout.overlaps());
}
```

- [ ] **Step 10: Run shell tests to verify RED**

Run the contract configure/build command from Step 6, then `ctest --test-dir codeg-eui/build-contract -R shell_page --output-on-failure`. Expected: FAIL because shell layout/route types are absent.

- [ ] **Step 11: Implement the shell page**

Use a 248 px sidebar, full-height content, 48 px header, 44 px composer row, 8 px maximum card radius, fixed button/input heights, and responsive minimum content widths. The sidebar owns `New`, session rows, workspace input, Grok/Codex selector, and Settings navigation. The content owns header/status, chat/settings route, and a full-width error strip. Use EUI theme tokens based on dark neutral backgrounds plus restrained green status and red errors; do not introduce a one-hue blue/purple palette.

```cpp
class ShellPage {
 public:
  void compose(eui::Ui& ui, float width, float height, AppModel& model);
 private:
  Route route_ = Route::Chat;
  ShellLayout layout_{};
};
```

- [ ] **Step 12: Run shell tests to verify GREEN**

Run the Step 10 test command. Expected: layout and navigation cases pass.

- [ ] **Step 13: Write chat/composer/markdown RED tests**

`chat_page_test.cpp` proves send enablement, composer clear-on-accept and retain-on-reject, pending request disablement, bottom-follow threshold, plain user text, one-line tool projection, and markdown rebuild at 0 ms, not at 74 ms, at 75 ms, and immediately on turn end.

```cpp
TEST(ChatState, throttles_streaming_markdown_but_flushes_turn_end) {
  ChatState state;
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(0), Generation::Delta));
  EXPECT_FALSE(state.shouldRebuildMarkdown(ms(74), Generation::Delta));
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(75), Generation::Delta));
  EXPECT_TRUE(state.shouldRebuildMarkdown(ms(76), Generation::TurnEnd));
}
```

- [ ] **Step 14: Run chat tests to verify RED**

Run contract configure/build, then `ctest --test-dir codeg-eui/build-contract -R chat_page --output-on-failure`. Expected: FAIL because `ChatState` and `ChatPage` are absent.

- [ ] **Step 15: Implement chat rendering and composer behavior**

Render user text plainly, assistant messages with `components::markdown`, and tools as `tool: <name> - <status>`. During streaming, update the markdown source no more than every 75 ms; flush immediately when frame generation reports turn end. `Send` is disabled for empty input, missing workspace/session, unavailable agent, or an active send request. Successful enqueue clears the composer; failed enqueue retains it. Keep the transcript in `components::scrollView` with stable width and automatic bottom follow only when already near the bottom.

```cpp
class ChatPage {
 public:
  void compose(eui::Ui& ui, const Rect& bounds, AppModel& model);
 private:
  ChatState state_;
  std::string composer_;
  std::chrono::steady_clock::time_point lastMarkdownBuild_{};
};
```

- [ ] **Step 16: Run chat tests to verify GREEN**

Run the Step 14 test command. Expected: chat state and rendering projections pass.

- [ ] **Step 17: Write P0 settings RED tests**

`settings_page_test.cpp` proves Grok/Codex-only tabs, probe/save pending IDs, launch-required field mapping, unsupported-agent rejection, backend JSON field names, credential redaction in errors, and raw editor round-trip.

```cpp
TEST(SettingsState, save_payload_contains_only_facade_fields) {
  SettingsState state = codexSettingsFixture();
  const auto json = state.buildPatchJson();
  EXPECT_TRUE(hasOnlyKeys(json, codexFacadeKeys()));
  EXPECT_FALSE(json.contains("OPENAI_API_KEY=test-secret"));
}
```

- [ ] **Step 18: Run settings tests to verify RED**

Run contract configure/build, then `ctest --test-dir codeg-eui/build-contract -R settings_page --output-on-failure`. Expected: FAIL because `SettingsState` and `SettingsPage` are absent.

- [ ] **Step 19: Implement P0 settings**

The page exposes Grok/Codex tabs; installed/probe status; enabled state; model/provider/env fields returned by the facade; launch-required Codex auth, sandbox/approval values, and raw `config.toml`; Grok structured values and raw TOML. `Probe` and `Save` show pending state keyed by request ID. The page never parses or writes agent files directly and never displays credential values in the global error strip.

```cpp
class SettingsPage {
 public:
  void compose(eui::Ui& ui, const Rect& bounds, AppModel& model);
  std::string buildPatchJson(Agent agent) const;
 private:
  SettingsState codex_;
  SettingsState grok_;
};
```

- [ ] **Step 20: Run settings tests to verify GREEN**

Run the Step 18 test command. Expected: P0 mapping, pending state, and redaction cases pass.

- [ ] **Step 21: Run C++ and native-shell verification**

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract --output-on-failure
codeg-eui/scripts/build.sh
```

Expected: all client/lifecycle/shell/chat/settings tests pass. On the prepared Linux host, the window opens with nonblank shell, navigable settings, workspace/agent/session controls, and no overlap at 800x600 or 1440x900. Record two ignored local screenshots and the binary path in the review package; this is producer evidence, not a human gate.

- [ ] **Step 22: Commit and prepare the Task 7 review package**

```bash
git add --dry-run -- codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/model.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/app/bridge/client.h codeg-eui/app/pages/shell.h codeg-eui/app/pages/chat.h codeg-eui/app/pages/settings.h codeg-eui/tests/client_completion_test.cpp codeg-eui/tests/app_lifecycle_test.cpp codeg-eui/tests/shell_page_test.cpp codeg-eui/tests/chat_page_test.cpp codeg-eui/tests/settings_page_test.cpp
git add -- codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/model.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/app/bridge/client.h codeg-eui/app/pages/shell.h codeg-eui/app/pages/chat.h codeg-eui/app/pages/settings.h codeg-eui/tests/client_completion_test.cpp codeg-eui/tests/app_lifecycle_test.cpp codeg-eui/tests/shell_page_test.cpp codeg-eui/tests/chat_page_test.cpp codeg-eui/tests/settings_page_test.cpp
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): build native chat and settings shell"
git show --stat --oneline HEAD
git diff HEAD^ -- codeg-eui/app codeg-eui/tests codeg-eui/CMakeLists.txt
```

Expected package: one native UI commit with test-first client/lifecycle/page state and prepared-host evidence; ignored screenshots/build trees are not staged. Route it to both high-risk reviewers, then continue directly to Task 8.

### Task 8: Add M5 Error Recovery, Session Switching, Cancel, and P1 Settings Controls

**Milestone:** M5.

**Files:**

- Expand: `src-tauri/codeg-eui-core/src/commands.rs`
- Expand: `src-tauri/codeg-eui-core/src/live.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Expand: `codeg-eui/app/bridge/client.h`
- Expand: `codeg-eui/app/pages/shell.h`
- Expand: `codeg-eui/app/pages/chat.h`
- Expand: `codeg-eui/app/pages/settings.h`
- Test: `src-tauri/codeg-eui-core/tests/m5_contract.rs`
- Create: `codeg-eui/tests/m5_controls_test.cpp`
- Expand test: `codeg-eui/tests/settings_page_test.cpp`

**Interfaces:**

- Consumes: `ConnectionManager::{cancel,disconnect_if_owner,find_connection_by_conversation_id}`, selection epoch, settings P1 DTO fields, and terminal errors.
- Produces: `codeg_eui_cancel_active_turn`, active selection teardown/reattach, retryable UI state, persistent streamed text after error, P1 structured settings controls, and session list refresh after create/switch.
- Cancel invariant: only the currently selected connection at the captured epoch is cancelled; a late cancel request cannot target the next selected session.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 8 | Error recovery, switch, cancel, P1 settings | Rust lifecycle/model plus C++ controls/tests | `concurrency_lifecycle`: cancel/switch/disconnect and stale completions | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`; total `4` | `high`: lifecycle hard trigger and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write cancel/switch fencing RED tests**

`m5_contract.rs` must cover:

```rust
#[tokio::test]
async fn cancel_is_fenced_to_the_selected_connection_epoch() {
    let bridge = TestBridge::running();
    bridge.select("conn-a", 10);
    let request = bridge.enqueue_cancel().unwrap();
    bridge.select("conn-b", 11);
    bridge.release_cancel_worker();
    let completion = bridge.wait_completion(request).await;
    assert_eq!(completion.status, CompletionStatus::Stale);
    assert_eq!(bridge.cancelled_connections(), ["conn-a"]);
    assert!(!bridge.cancelled_connections().contains(&"conn-b"));
}
```

Also test session switch mid-stream, old projector termination, duplicate cancel completion prevention, and selection completion staleness.

- [ ] **Step 2: Run the M5 tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test m5_contract
```

Expected: FAIL on missing cancel/switch fencing behavior.

- [ ] **Step 3: Implement epoch-fenced cancel and switch teardown**

Capture `{connection_id,selection_epoch}` at cancel acceptance, call `ConnectionManager::cancel(&db.conn, &connection_id)`, and mark completion stale if the current epoch changed. Session switch cancels the old projector task, drops its receiver, keeps the agent connection available for later reattach, loads new history, and attaches one new projector. Shutdown still disconnects all EUI-owned connections.

- [ ] **Step 4: Run cancel/switch tests to verify GREEN**

Run the Step 2 command. Expected: cancel is fenced to the captured connection/epoch and old projectors cannot mutate new selection state.

- [ ] **Step 5: Write crash/recovery model RED tests**

Add cases for agent crash retaining partial assistant text/tool summaries, `stream_active=false`, error strip ownership, immediate New Session enablement, successful recovery in a new session, and scoped error replacement.

```rust
#[tokio::test]
async fn hard_error_retains_partial_text_and_allows_new_session() {
    let mut model = model_streaming("partial answer");
    model.apply_terminal_error("agent exited");
    assert_eq!(model.live_assistant(), "partial answer");
    assert!(!model.stream_active());
    assert!(model.can_create_session());
    assert_eq!(model.error_strip(), "agent exited");
}
```

- [ ] **Step 6: Run crash/recovery tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test m5_contract hard_error -- --nocapture
```

Expected: FAIL because terminal error ownership/recovery rules are absent.

- [ ] **Step 7: Implement error/crash recovery state**

On terminal agent error, stop the active stream, retain already copied assistant text/tool summaries, set the error strip, and allow New Session immediately. Non-terminal turn errors do not disconnect the selection. Successful new selection/send clears only errors scoped to the superseded operation; unsupported-interaction notices remain until dismiss or session switch.

- [ ] **Step 8: Run crash/recovery tests to verify GREEN**

Run the Step 6 command. Expected: partial content, recovery enablement, and error ownership pass.

- [ ] **Step 9: Write cancel/session/error control RED tests**

`m5_controls_test.cpp` proves the 36x36 stop-square visibility, tooltip text, pending disablement, stale cancel no-op on the new selection, partial transcript after crash, session-row active/streaming/error status, and scoped error-strip clearing.

```cpp
TEST(M5Controls, stale_cancel_does_not_change_new_selection) {
  M5ControlsState state = streamingSelection("conn-a", 10);
  state.beginCancel(71);
  state.select("conn-b", 11);
  state.apply(cancelledStale(71));
  EXPECT_EQ(state.connectionId(), "conn-b");
  EXPECT_FALSE(state.cancelPending());
}
```

- [ ] **Step 10: Run control tests to verify RED**

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R m5_controls --output-on-failure
```

Expected: FAIL because `M5ControlsState` and control rendering are absent.

- [ ] **Step 11: Implement cancel/session/error controls**

```cpp
struct M5ControlsState {
  std::string connectionId;
  uint64_t selectionEpoch = 0;
  std::optional<uint64_t> cancelRequestId;
  bool streamActive = false;
  void apply(const Completion& completion);
};
```

Add a stop-square icon button while streaming, with tooltip `Cancel active turn`, fixed 36x36 dimensions, and disabled while cancel is pending. Session rows show active/streaming/error state and switch on click without nested cards. Agent errors retain the copied transcript and keep New Session available.

- [ ] **Step 12: Run control tests to verify GREEN**

Run the Step 10 command. Expected: cancel, selection, crash, error, and row-status cases pass.

- [ ] **Step 13: Write P1 settings RED tests**

Extend `settings_page_test.cpp` with Codex model/provider/reasoning and sandbox/approval fields, Grok model/reasoning/structured fields, raw-editor precedence, and exact facade JSON keys. Assert no UI-only field is serialized.

- [ ] **Step 14: Run P1 settings tests to verify RED**

Run contract configure/build, then `ctest --test-dir codeg-eui/build-contract -R settings_page --output-on-failure`. Expected: FAIL on missing P1 field projection.

- [ ] **Step 15: Implement P1 structured settings**

Expose remaining backend-provided Codex model/provider/reasoning and sandbox/approval structured controls plus Grok model/reasoning/structured config fields. Keep raw editors as the authoritative advanced escape hatch. Serialize only the exact facade keys asserted by the RED test; cancel, session-row, and error controls remain owned by the preceding RED/GREEN pair.

- [ ] **Step 16: Run P1 settings tests to verify GREEN**

Run the Step 14 command. Expected: all P1 mapping/raw-precedence cases pass.

- [ ] **Step 17: Run M5 integrated verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test m5_contract
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test interaction_decline
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test settings_contract -- --test-threads=1
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R 'm5_controls|settings_page' --output-on-failure
```

Expected: cancel/switch/crash/settings regressions pass without parked work or cross-selection mutation.

- [ ] **Step 18: Commit and prepare the Task 8 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/m5_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/client.h codeg-eui/app/pages/shell.h codeg-eui/app/pages/chat.h codeg-eui/app/pages/settings.h codeg-eui/tests/m5_controls_test.cpp codeg-eui/tests/settings_page_test.cpp
git add -- src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/m5_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/client.h codeg-eui/app/pages/shell.h codeg-eui/app/pages/chat.h codeg-eui/app/pages/settings.h codeg-eui/tests/m5_controls_test.cpp codeg-eui/tests/settings_page_test.cpp
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add native session recovery controls"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core codeg-eui/app codeg-eui/tests
```

Expected package: one M5 commit with epoch-fenced lifecycle and visible controls. Route it to both high-risk reviewers, then continue directly to Task 9.

### Task 9: Add Comparable Performance Instrumentation and Reproducible Evidence

**Milestone:** M6.

**Files:**

- Expand: `src-tauri/codeg-eui-core/src/perf.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Create: `codeg-eui/app/perf_metrics.h`
- Expand: `codeg-eui/app/app.cpp`
- Create: `codeg-eui/tests/perf_metrics_test.cpp`
- Create: `src/lib/perf/eui-comparison-recorder.ts`
- Create: `src/lib/perf/eui-comparison-recorder.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify test: `src/components/message/live-transcript-row.test.tsx`
- Create: `codeg-eui/scripts/perf_compare.sh`
- Create: `codeg-eui/tests/perf_compare_test.sh`
- Create: `codeg-eui/fixtures/perf-workspace/README.md`
- Create: `codeg-eui/README.md`

**Interfaces:**

- Consumes: native `t0_ns/t_first_token_ns/t_end_ns`, EUI next-update `onFrame` post-presentation marker/frame clock, WebView send success, React DOM commit plus RAF1/eligible paint/RAF2 ordering, terminal ACP event, and Linux `/proc/<pid>/status`.
- Produces: common `ComparisonRun` JSON, `LONG_FRAME_MS=50`, equivalent native/WebView post-presentation proxies, RSS samples, median/p95 aggregation, CLI subcommands `record-eui`, `record-webview`, `aggregate`, `validate`, and `self-test`, plus one filled tracked README table.
- Common schema: `{shell,agent,promptId,buildType,backend,t0Ns,tFirstTokenNs,tFirstPresentedNs,tEndNs,frameIntervalsMs,longFrameThresholdMs,longFrameCount,peakShellRssKb,shellPid,rssScope,gitCommit,notes}`, where `rssScope` is exactly `"shell-process-only"`.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 9 | Comparable performance instrumentation and evidence | Rust/C++/React markers, scripts, fixture, README | none | `cross_runtime_or_process=2`, `broad_production_surface=1`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `6` | `high`: soft threshold reached | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write deterministic metric-arithmetic RED tests**

In TypeScript and C++, feed timestamps `[0,16,32,92,108]` with `t0=0`, `firstPresented=16`, `end=108`. Both implementations must produce active intervals `[16,60,16]`, p95 `60`, threshold `50`, and long-frame count `1`. The first-token marker is diagnostic and must not substitute for first-presented.

```ts
it("uses first presentation and the fixed 50 ms threshold", () => {
  const run = recorderFromFrames([0, 16, 32, 92, 108], {
    t0: 0,
    firstToken: 8,
    firstPresented: 16,
    end: 108,
  }).finish()
  expect(run.firstPresentedLatencyMs).toBe(16)
  expect(run.frameIntervalP95Ms).toBe(60)
  expect(run.longFrameThresholdMs).toBe(50)
  expect(run.longFrameCount).toBe(1)
})
```

- [ ] **Step 2: Run metric tests to verify RED**

```bash
pnpm test -- src/lib/perf/eui-comparison-recorder.test.ts
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R perf_metrics --output-on-failure
```

Expected: TypeScript file missing and/or C++ metric target missing.

- [ ] **Step 3: Implement common metric calculations**

Both implementations sample presented-frame intervals only from the frame that records `t_first_presented` through `t_end`; exclude startup and idle frames. Use nearest-rank p95: sort N intervals and choose index `ceil(0.95*N)-1`. Record raw intervals in artifacts so aggregation is auditable. Freeze `LONG_FRAME_MS`/`longFrameThresholdMs` to `50` and reject imported runs with any other value.

- [ ] **Step 4: Run metric-arithmetic tests to verify GREEN**

Run the Step 2 commands. Expected: TypeScript and C++ produce the same latency, intervals, p95, threshold, and count.

- [ ] **Step 5: Write EUI presentation-order RED tests**

In `perf_metrics_test.cpp`, drive an explicit fake phase scheduler. Composition with first assistant text arms but does not mark; the simulated `present` still does not mark; the first subsequent EUI `onFrame` marks exactly once and begins interval sampling.

```cpp
TEST(PresentationClock, eui_marks_on_update_after_eligible_present) {
  PresentationClock clock;
  clock.onCompose(true, ns(10));
  EXPECT_FALSE(clock.firstPresentedNs());
  clock.onPresentedForTest(ns(12));
  EXPECT_FALSE(clock.firstPresentedNs());
  clock.onFrame(ns(16));
  EXPECT_EQ(clock.firstPresentedNs(), ns(16));
  clock.onFrame(ns(32));
  EXPECT_EQ(clock.firstPresentedNs(), ns(16));
}
```

- [ ] **Step 6: Run EUI ordering tests to verify RED**

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract -R perf_metrics --output-on-failure
```

Expected: FAIL because `PresentationClock` phase behavior is absent.

- [ ] **Step 7: Implement the EUI post-presentation proxy**

Rust already supplies send acceptance, first-token, and end nanoseconds. EUI v0.5.5 owns `renderBackend.present()` inside its public GLFW app main and exposes no post-present application callback. In C++, when the compose pass first includes a non-empty copied assistant buffer, arm a one-shot marker without timestamping it. A persistent ticker element's first subsequent `onFrame` callback runs during the next update, after the prior frame was rendered and presented; timestamp that callback as `t_first_presented` and clear the marker. Use the same ticker callbacks for active-window frame timestamps through `t_end`. Write one JSON run to the path in `CODEG_EUI_PERF_OUT`; normal runs with the variable absent do no file I/O.

- [ ] **Step 8: Run EUI ordering tests to verify GREEN**

Run the Step 6 command. Expected: composition/presentation/callback ordering and one-shot behavior pass.

- [ ] **Step 9: Write WebView two-RAF ordering RED tests**

Use a manual RAF scheduler in `eui-comparison-recorder.test.ts` and the existing `live-transcript-row.test.tsx`. The test records phases `commit -> raf1 -> paint -> raf2` and proves no marker at commit or RAF1, then one marker at RAF2 after the eligible paint.

```ts
it("marks only in the second RAF after assistant DOM commit", () => {
  const raf = new ManualRafScheduler()
  const recorder = new ComparisonRecorder(metadata, raf.request)
  recorder.assistantCommitted("first token")
  expect(recorder.firstPresentedNs).toBeUndefined()
  raf.flushOne(10) // RAF1, before eligible paint
  expect(recorder.firstPresentedNs).toBeUndefined()
  raf.markPaint()
  raf.flushOne(16) // RAF2, after eligible paint
  expect(recorder.firstPresentedNs).toBe(16)
})
```

- [ ] **Step 10: Run WebView ordering tests to verify RED**

```bash
pnpm test -- src/lib/perf/eui-comparison-recorder.test.ts src/components/message/live-transcript-row.test.tsx
```

Expected: FAIL because the current single-RAF marker fires before paint.

- [ ] **Step 11: Implement the WebView post-presentation proxy**

`eui-comparison-recorder.ts` is opt-in under `CODEG_EUI_COMPARE` or this explicit API and metadata schema:

```ts
window.__codegEuiComparison.start({
  shell: "webview",
  agent: "codex",
  promptId: "continuous-text-v1",
  buildType: "release",
  backend: "tauri-webview",
  gitCommit: "066ce16401cbd5de0822f5f721806f6624f1eade",
  notes: "local comparison capture",
})
```

The implementation validates `shell === "webview"`, `agent` as `"grok" | "codex"`, non-empty prompt/build/backend/commit strings, and stores the supplied metadata unchanged in the run artifact. Mark `t0` immediately after the existing send invoke resolves successfully in `acp-connections-context.tsx`. In `LiveTranscriptRow`, a `useLayoutEffect` that observes the first non-empty assistant text schedules RAF1; RAF1 schedules RAF2; RAF2 marks `t_first_presented`, because the eligible DOM had an intervening paint opportunity. Cancel both handles on unmount/reset and enforce one marker per run. Mark `t_end` where the authoritative `TurnComplete`/hard-error event is applied. Expose `window.__codegEuiComparison.finish()` to return/download the common JSON and stop all RAF sampling.

- [ ] **Step 12: Run WebView ordering tests to verify GREEN**

Run the Step 10 command. Expected: scheduler and component tests distinguish commit, pre-paint RAF1, paint, and post-paint RAF2.

- [ ] **Step 13: Write performance CLI usage/validation/self-test RED tests**

Create `codeg-eui/tests/perf_compare_test.sh`. It asserts `--help` names all five subcommands; an unknown command exits `64`; `validate` accepts a complete fixture and rejects missing fields, `longFrameThresholdMs != 50`, inconsistent `longFrameCount`, and child-PID RSS attribution; `aggregate` rejects fewer than one warm-up plus three measured runs; both record commands merge metrics with RSS from the supplied shell PID and produce output accepted by `validate`; and `self-test` returns zero after exercising every public subcommand.

```bash
set -eu
script=codeg-eui/scripts/perf_compare.sh
usage=$($script --help)
for command in record-eui record-webview aggregate validate self-test; do
  printf '%s\n' "$usage" | rg -q "$command"
done
if $script unknown-command; then exit 1; else test "$?" -eq 64; fi
$script self-test
```

The test creates one temporary long-lived shell stub PID plus four synthetic EUI and four WebView metric JSON inputs (run 1 warm-up, runs 2-4 measured), then runs these public paths rather than sourcing private helpers:

```bash
for run in 1 2 3 4; do
  $script record-eui --pid "$stub_pid" --metrics "$tmp/eui-metrics-$run.json" --output "$tmp/eui-runs/run-$run.json"
  $script validate "$tmp/eui-runs/run-$run.json"
  $script record-webview --pid "$stub_pid" --metrics "$tmp/webview-metrics-$run.json" --output "$tmp/webview-runs/run-$run.json"
  $script validate "$tmp/webview-runs/run-$run.json"
done
$script aggregate --eui-dir "$tmp/eui-runs" --webview-dir "$tmp/webview-runs" --output "$tmp/aggregate.json"
$script validate "$tmp/aggregate.json"
$script self-test
```

- [ ] **Step 14: Run CLI tests to verify RED**

```bash
bash codeg-eui/tests/perf_compare_test.sh
```

Expected: FAIL because the script and `validate`/`self-test` commands are absent.

- [ ] **Step 15: Implement all performance CLI subcommands end to end**

`perf_compare.sh` accepts exactly `record-eui`, `record-webview`, `aggregate`, `validate`, and `self-test` plus `--help`. Both record commands use the exact interface `--pid <shell-pid> --metrics <shell-json> --output <run-json>`; they reject a missing/non-numeric PID or wrong-shell input, sample only `VmRSS` from `/proc/$pid/status` every 50 ms until the metrics file contains `tEndNs`, and never walk child processes. They preserve the shell metrics, add `shellPid`, `rssScope="shell-process-only"`, and peak RSS, atomically write the output, then invoke `validate` through the public dispatcher. `validate <run-or-aggregate.json>` checks the full schema, non-negative ordered timestamps, fixed threshold 50, recomputed interval/count values, shell PID scope metadata, and run count. `aggregate` calls `validate` on every input, validates same host/git/prompt/agent/build metadata, discards run 1 as warm-up, requires 3 measured runs, reports median first-presented latency, p95 across all active intervals, summed `>50 ms` count, and maximum shell RSS.

`self-test` creates a temporary synthetic EUI/WebView data set and shell stub, invokes `record-eui`, `record-webview`, `validate`, and `aggregate` through the public command dispatcher, verifies the deterministic median/p95/count, proves the shell sample excludes a spawned child RSS value, proves threshold/metadata corruption is rejected, and removes its trap-owned temporary directory.

- [ ] **Step 16: Run CLI tests to verify GREEN**

Run the Step 14 command, then:

```bash
codeg-eui/scripts/perf_compare.sh self-test
```

Expected: all five commands are advertised and executed end to end, validation failures are deterministic, and self-test covers both recorders, validation, aggregation, and shell-only RSS.

- [ ] **Step 17: Add the fixed fixture and README protocol**

`fixtures/perf-workspace/README.md` contains prompt ID `continuous-text-v1` and this prompt:

```text
Write a continuous 700-900 word technical explanation of how an event-driven
desktop chat client moves a streamed response from an agent process to pixels.
Use short paragraphs, no tools, no tables, and finish with exactly one fenced
Rust code block.
```

`codeg-eui/README.md` documents Ubuntu/Fedora dependencies, submodule init, build/run commands, all EUI env vars, data isolation, Grok/Codex setup, one warm-up plus three runs, DevTools/WebView two-RAF capture, RSS scope, all five script subcommands, exact JSON schema, skip notation, and a comparison table with columns required by the design. `codeg-eui/results/` is ignored local evidence; only aggregate values copied into the README table are tracked.

- [ ] **Step 18: Run deterministic and regression verification**

```bash
pnpm test -- src/lib/perf/eui-comparison-recorder.test.ts src/lib/perf/streaming-perf-recorder.test.ts
pnpm test -- src/components/message/live-transcript-row.test.tsx
pnpm eslint src/lib/perf/eui-comparison-recorder.ts src/lib/perf/eui-comparison-recorder.test.ts src/contexts/acp-connections-context.tsx src/components/message/live-transcript-row.tsx
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml perf
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract --output-on-failure
codeg-eui/scripts/perf_compare.sh self-test
bash codeg-eui/tests/perf_compare_test.sh
```

Expected: both shells calculate identical fixture metrics after an eligible presentation, existing streaming recorder tests remain green, lints pass, and script tests reject child RSS and mismatched threshold/metadata.

- [ ] **Step 19: Produce one local filled comparison table**

On the prepared Linux host, close other heavy foreground applications, use the Release/OpenGL builds, run one warm-up and three measurements for each installed agent on EUI and WebView, and aggregate. At least one installed agent must yield a fully filled EUI/WebView performance row; run both when available and use the design's performance skip notation for unavailable performance rows. This performance skip is not the Task 11 dual-agent product-loop acceptance gate.

```bash
codeg-eui/scripts/perf_compare.sh aggregate \
  --eui-dir codeg-eui/results/raw/eui \
  --webview-dir codeg-eui/results/raw/webview \
  --output codeg-eui/results/local-comparison.json
codeg-eui/scripts/perf_compare.sh validate codeg-eui/results/local-comparison.json
```

Copy the aggregate values, host label, date, git commit, backend, build type, and skip notes into the tracked README table. Keep raw and aggregate JSON under ignored `codeg-eui/results/`; do not stage it or claim a performance winner from fewer than three measured runs.

- [ ] **Step 20: Commit and prepare the Task 9 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/src/model.rs codeg-eui/app/app.cpp codeg-eui/app/perf_metrics.h codeg-eui/tests/perf_metrics_test.cpp codeg-eui/tests/perf_compare_test.sh codeg-eui/scripts/perf_compare.sh codeg-eui/fixtures/perf-workspace/README.md codeg-eui/README.md src/lib/perf/eui-comparison-recorder.ts src/lib/perf/eui-comparison-recorder.test.ts src/contexts/acp-connections-context.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx
git add -- src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/src/model.rs codeg-eui/app/app.cpp codeg-eui/app/perf_metrics.h codeg-eui/tests/perf_metrics_test.cpp codeg-eui/tests/perf_compare_test.sh codeg-eui/scripts/perf_compare.sh codeg-eui/fixtures/perf-workspace/README.md codeg-eui/README.md src/lib/perf/eui-comparison-recorder.ts src/lib/perf/eui-comparison-recorder.test.ts src/contexts/acp-connections-context.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "perf(eui): add native WebView comparison protocol"
git show --stat --oneline HEAD
git diff HEAD^ -- src/lib/perf src/contexts/acp-connections-context.tsx src/components/message/live-transcript-row.tsx src-tauri/codeg-eui-core codeg-eui
```

Expected package: one instrumentation/evidence commit with arithmetic and presentation-order tests, all CLI subcommands, script self-test/validation, and a filled README table. No `codeg-eui/results/`, build, screenshot, or nested Cargo target path is staged. Route it to both high-risk reviewers, then continue directly to Task 10.

### Task 10: Aggregate the Pre-Final Delivery and Scope Audit

**Phase:** Pre-Final aggregation.

**Files:**

- Read: committed Task 1-9 diffs and review packages
- Read: approved design and this plan
- Modify: none
- Test execution: none beyond diff/static metadata checks

**Interfaces:**

- Consumes: nine committed producer outputs, per-Task automated evidence, reviewer packages, and the pinned submodule state.
- Produces: exact changed-file allowlist evidence, ordered commit series, design-traceability checklist, submodule pin proof, whitespace check, and one consolidated Final review package.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 10 | Aggregate pre-Final delivery and scope audit | Task 1-9 commits and review packages | none | `multiple_ownership_modules=1`; total `1` | `normal`: read-only aggregation | `grok` | `codex` | `b2d_task_risk_v1` |

- [ ] **Step 1: Resolve the Plan commit as the delivery base**

```bash
delivery_base=$(git rev-list -n 1 --grep='^docs: revise EUI-NEO spike plan after review$' HEAD)
test -n "$delivery_base"
git log --reverse --format='%h %s' "$delivery_base..HEAD"
```

Expected: Task 1-9 producer commits appear in order. Focused repair commits are allowed only adjacent to or clearly attributed to the owning Task.

- [ ] **Step 2: Enforce the complete changed-file allowlist**

```bash
git diff --name-only "$delivery_base..HEAD" > /tmp/codeg-eui-changed-files.txt
awk '
  $0 == ".gitmodules" { next }
  $0 == "codeg-eui/third_party/EUI-NEO" { next }
  $0 ~ /^codeg-eui\// { next }
  $0 ~ /^src-tauri\/codeg-eui-core\// { next }
  $0 == "src-tauri/src/app_state.rs" { next }
  $0 == "src-tauri/src/document_translate/service.rs" { next }
  $0 == "src-tauri/src/logging/init.rs" { next }
  $0 == "src-tauri/src/commands/eui_facade.rs" { next }
  $0 == "src-tauri/src/commands/mod.rs" { next }
  $0 == "src/lib/perf/eui-comparison-recorder.ts" { next }
  $0 == "src/lib/perf/eui-comparison-recorder.test.ts" { next }
  $0 == "src/contexts/acp-connections-context.tsx" { next }
  $0 == "src/components/message/live-transcript-row.tsx" { next }
  $0 == "src/components/message/live-transcript-row.test.tsx" { next }
  { print; bad=1 }
  END { exit bad }
' /tmp/codeg-eui-changed-files.txt
cat /tmp/codeg-eui-changed-files.txt
```

Expected: `awk` prints nothing and exits zero. Any lockfile, existing manifest, locale, database migration, Tauri config, server route, CI, release, or unrelated frontend file is a scope failure that returns to its owning Task or is removed in a focused repair.

- [ ] **Step 3: Prove the optional-build boundary and submodule pin**

```bash
test "$(git -C codeg-eui/third_party/EUI-NEO rev-parse HEAD)" = cb70ea8bea263efa7805a40c07135df028ad44b1
test -z "$(rg -l 'codeg-eui|EUI-NEO|codeg_eui' src-tauri/Cargo.toml package.json pnpm-lock.yaml next.config.ts 2>/dev/null || true)"
git diff --submodule=short "$delivery_base..HEAD" -- .gitmodules codeg-eui/third_party/EUI-NEO
```

Expected: exact v0.5.5 pin and no default manifest/lock/config reference to EUI.

- [ ] **Step 4: Check the aggregate diff and commits without running suites**

```bash
git diff --check "$delivery_base..HEAD"
git diff --stat "$delivery_base..HEAD"
test -z "$(git status --porcelain --untracked-files=all)"
git status --short --ignored=matching -- codeg-eui/build codeg-eui/build-contract codeg-eui/build-abi-link codeg-eui/results codeg-eui/screenshots src-tauri/codeg-eui-core/target
```

Expected: no whitespace errors or uncommitted/untracked source files. Any listed generated path appears only as `!!` ignored evidence; a generated path outside the two explicit `.gitignore` files is a scope failure.

- [ ] **Step 5: Produce the pre-Final traceability checklist**

Attach these exact assertions and evidence locations to the consolidated package:

```text
1. Optionality: independent staticlib + pinned submodule; no default manifest dependency.
2. Isolation: ambient CODEG_DATA_DIR/CODEG_HOME cannot select or redirect EUI storage.
3. ABI: versioned repr(C), bounded inputs/queues, panic containment, UI affinity.
4. Lifecycle: begin-drain, stopping polls, observable exactly-once completion, final join/free.
5. Settings: Grok/Codex only, existing persistence helpers only, secrets redacted.
6. Sessions: existing folder/conversation/ACP paths, linked send, history from backend.
7. Live: atomic snapshot+subscribe, sequence recovery, bounded producer-independent merge.
8. Interactions: permissions/questions/plans always decline/cancel to terminal state.
9. UI: copied frames only, shell/chat/settings, 75 ms markdown throttle, M5 recovery.
10. Performance: identical anchors, fixed 50 ms threshold, shell-only RSS, filled local row.
11. E2E: both Grok and Codex complete a real presented streaming turn on the prepared host.
12. Evidence: each accepted producer request has one completion and each Task has a commit.
```

Queue the Task 10 package to the normal-route Codex reviewer. Continue to Task 11 without waiting for human acceptance.

### Task 11: Run Final Automated Verification, Independent Review, and Delivery

**Phase:** Final review and deliver.

**Files:**

- Verify: all Task 1-9 owned files and default product surfaces
- Create after green review: `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/delivery/final-delivery-report.md`
- Modify on failure: only through a focused repair returned to the owning producer Task

**Interfaces:**

- Consumes: clean committed Task 1-9 output and Task 10 aggregate package.
- Produces: targeted and broad command evidence, default-build isolation proof, prepared-host native smoke, mandatory successful Grok-and-Codex product-loop evidence, performance evidence, two independent Final reviews, clean worktree, final delivery report, and ordered commit list.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 11 | Final automated verification, review, and delivery | all new/changed surfaces plus default builds | none | `broad_production_surface=1`, `multiple_ownership_modules=1`, `dependency_or_build=1`; total `3` | `high`: aggregate soft threshold reached | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Run the complete headless EUI core suite**

```bash
cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml
cargo clippy --manifest-path src-tauri/codeg-eui-core/Cargo.toml --all-targets -- -D warnings
```

Expected: format check, every unit/integration contract, and clippy pass with no warnings.

- [ ] **Step 2: Run the shared-core tests touched by the EUI profile/facade**

```bash
cd src-tauri
cargo fmt -- --check
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
cd ..
```

Expected: EUI facade tests and shared-library clippy pass. The EUI bootstrap profile itself is covered by `codeg-eui-core/tests/bootstrap_profile.rs` in Step 1.

- [ ] **Step 3: Run all headless C++ contract/state/metric tests**

```bash
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a" -DCMAKE_BUILD_TYPE=Release
cmake --build codeg-eui/build-contract --parallel
ctest --test-dir codeg-eui/build-contract --output-on-failure
bash codeg-eui/tests/perf_compare_test.sh
```

Expected: ABI layout, black-box shutdown drain, frame copy, completion, lifecycle, page state, stale/cancel, performance ordering/arithmetic, and CLI tests all pass without a display server or EUI native packages.

- [ ] **Step 4: Run targeted and broad frontend verification**

```bash
pnpm test -- src/lib/perf/eui-comparison-recorder.test.ts src/lib/perf/streaming-perf-recorder.test.ts src/contexts/acp-connections-context.test.tsx src/components/message/live-transcript-row.test.tsx
pnpm test
pnpm eslint .
pnpm build
```

Expected: targeted metric/integration tests, full Vitest suite, lint, and static export build pass.

- [ ] **Step 5: Run broad default/server/MCP Rust gates with EUI sources unavailable**

Temporarily move only the explicit submodule directory, install a trap before running checks, and restore it even on failure:

```bash
repo_root=$(git rev-parse --show-toplevel)
eui_source="$repo_root/codeg-eui/third_party/EUI-NEO"
eui_hold=$(mktemp -d /tmp/codeg-eui-source-hold.XXXXXX)
mv "$eui_source" "$eui_hold/EUI-NEO"
restore_eui_source() {
  if test -e "$eui_hold/EUI-NEO"; then
    mv "$eui_hold/EUI-NEO" "$eui_source"
  fi
  rmdir "$eui_hold" 2>/dev/null || true
}
trap restore_eui_source EXIT HUP INT TERM
cd "$repo_root/src-tauri"
cargo check
cargo test --lib --features test-utils
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
cd "$repo_root"
restore_eui_source
trap - EXIT HUP INT TERM
git status --short
```

Expected: broad shared/default tests, full desktop regression, desktop clippy, server check/tests/clippy, and MCP check/clippy all pass while EUI sources are unavailable; restoring the submodule leaves no Git status change.

- [ ] **Step 6: Run the prepared-host native build and bounded window smoke**

```bash
eui_binary=$(codeg-eui/scripts/build.sh | tail -n 1)
test -x "$eui_binary"
smoke_root=$(mktemp -d /tmp/codeg-eui-smoke.XXXXXX)
ambient_root=$(mktemp -d /tmp/codeg-main-data.XXXXXX)
ambient_home=$(mktemp -d /tmp/codeg-main-home.XXXXXX)
CODEG_DATA_DIR="$ambient_root" \
CODEG_HOME="$ambient_home" \
CODEG_EUI_DATA_DIR="$smoke_root" \
CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES=3 \
xvfb-run -a "$eui_binary"
test -f "$smoke_root/codeg.db"
test -d "$smoke_root/logs"
test ! -e "$ambient_root/codeg.db"
test ! -e "$ambient_home/logs"
```

Expected: Release staticlib links to EUI/GLFW/OpenGL, the app presents at least three nonblank frames, begins drain, copies any stopping completions, finalizes through `codeg_eui_shutdown`, and creates its DB/logs only under the smoke root. `CODEG_EUI_SMOKE_EXIT_AFTER_FRAMES` is a documented producer-test hook handled by `app.cpp`; normal launches ignore it when unset.

- [ ] **Step 7: Run mandatory real Grok and Codex product-loop checks**

The prepared producer host must have working credentials and launchable installations for both Grok and Codex. For each agent, use an isolated EUI root and the fixed fixture: probe successfully, save/read settings, set workspace, create a session, send `continuous-text-v1`, observe non-empty assistant text in a presented frame, and reach a successful `TurnComplete`. Then trigger or fixture-inject one unsupported interaction per agent path and prove it reaches `TurnComplete` or a surfaced hard error with no parked responder. Record exact command, agent version, presented-text proof, terminal event, and outcome in the delivery report.

Expected: both Grok and Codex have successful real streaming product-loop evidence. Missing installation/credentials, probe/launch failure, an empty reply, or a hard error on the primary streaming turn blocks Final delivery. Do not convert either required agent to a skip; only an explicit approved design-scope revision can remove this gate.

- [ ] **Step 8: Re-run and validate the comparison evidence**

```bash
codeg-eui/scripts/perf_compare.sh self-test
codeg-eui/scripts/perf_compare.sh validate codeg-eui/results/local-comparison.json
```

Expected: one warm-up plus three measured runs, common metadata, 50 ms threshold, presentation-based latency, shell-only RSS, at least one filled EUI/WebView row, and README values matching the aggregate JSON.

- [ ] **Step 9: Apply the automated failure protocol**

If any Step 1-8 command fails, stop the Final sequence, assign the failure to the owning producer Task, add or tighten a focused regression when coverage is absent, make the minimum in-scope repair, commit it with an exact subject, refresh that Task's review package, and restart Task 11 at Step 1. Partial success is not Final evidence. A failure requiring a file outside Task 10's allowlist blocks delivery until the parent reroutes risk and updates the manifest. Missing required Grok or Codex capability is a delivery blocker, not a test skip or repair success.

- [ ] **Step 10: Run final diff, submodule, and worktree checks**

```bash
delivery_base=$(git rev-list -n 1 --grep='^docs: revise EUI-NEO spike plan after review$' HEAD)
test -n "$delivery_base"
git diff --check "$delivery_base..HEAD"
test "$(git -C codeg-eui/third_party/EUI-NEO rev-parse HEAD)" = cb70ea8bea263efa7805a40c07135df028ad44b1
test -z "$(git status --porcelain --untracked-files=all)"
git status --short --ignored=matching -- codeg-eui/build codeg-eui/build-contract codeg-eui/build-abi-link codeg-eui/results codeg-eui/screenshots src-tauri/codeg-eui-core/target
git log --reverse --format='%h %s' "$delivery_base..HEAD"
git diff --stat "$delivery_base..HEAD"
```

Expected: no whitespace errors, exact submodule pin, clean worktree before writing the delivery report, and only planned producer/repair commits.

- [ ] **Step 11: Complete both independent Final reviews**

Send the Task 10 aggregate diff, all Task 11 command output, native smoke record, real-agent outcomes, comparison artifact, and Task 11 risk row to both reviewers. The separate Codex reviewer must not be the Plan Author or Task 11 implementer thread.

```text
Codex review focus:
- ABI layout, pointer lifetime, panic containment, and shutdown ordering
- request reservation/exactly-once terminalization and stale epoch behavior
- snapshot/subscribe lock ordering, sequence recovery, and final parity
- data-root/credential isolation and config-facade scope
- default-build independence and evidence correctness

Grok review focus:
- end-to-end Grok/Codex workspace/session/send flow
- native shell control states, errors, cancel, switching, and settings parity
- unsupported interaction terminal decline
- performance anchor equivalence, RSS scope, reproducibility, and README usability
```

Expected: both reviewers cover the latest commit/digest and return no unresolved Critical or Important findings. Any such finding returns to the owning Task for a committed repair and restarts Task 11 from Step 1. Minor findings are fixed when in scope or recorded with concrete residual impact.

- [ ] **Step 12: Write and commit the Final delivery report**

Create `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/delivery/final-delivery-report.md` containing: design/plan digests; Task risk routes; ordered commit hashes; exact EUI pin; command/result table; native smoke path/root; successful Grok and Codex product-loop outcomes; performance table/artifact and performance-only skips; both reviewer identities/verdicts/latest digest; unresolved minors; and post-delivery human acceptance items.

```bash
git add -f -- .superpowers/sdd/2026-08-09-eui-neo-frontend-spike/delivery/final-delivery-report.md
test "$(git diff --cached --name-only)" = .superpowers/sdd/2026-08-09-eui-neo-frontend-spike/delivery/final-delivery-report.md
git commit -m "docs(eui): record final spike delivery evidence"
git show --stat --oneline HEAD
git status --short
```

Expected: the report commit contains only the delivery report and the worktree is clean.

- [ ] **Step 13: Deliver**

Return the plan/design digests, Task count and risk summary, producer/report commit list, verification summary, native binary path, successful Grok and Codex outcomes, performance artifact/table and performance-only skips, Final reviewer verdicts, and delivery report path. Do not request mid-flow UAT or perform push/merge/PR actions.

## Post-Delivery Human Acceptance and Residual Work

These items are not Task gates and occur only after delivery:

- Compare visual density, text selection, IME behavior, accessibility, and high-DPI/multi-monitor behavior on representative Linux desktops.
- Repeat the performance protocol on more hardware and longer conversations before making any product migration decision.
- Decide whether P2 raw-editor polish, interactive permission UI, Windows/macOS backends, or packaging deserve separate approved designs.
- Review EUI-NEO upgrades separately; never float the submodule pin during ordinary default product work.
