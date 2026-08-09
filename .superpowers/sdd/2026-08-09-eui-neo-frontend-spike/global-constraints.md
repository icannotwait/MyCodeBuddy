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
- All headless C++ suites use the repository-owned `CODEG_EUI_TEST_HARNESS_VERSION=1` in `codeg-eui/tests/test_harness.h` plus `test_main.cpp`; do not fetch GoogleTest/Catch2 or require EUI/GLFW/OpenGL in contracts-only mode. Every C++ RED source is added through `codeg_eui_add_contract_test(<exact-ctest-name> <source>)` no later than its RED step, builds that target, proves `ctest -N -R '^<exact-name>$'` selects exactly one test through `assert_ctest_registered.sh`, and then proves the named assertion fails. A compile failure or zero-test selection is an invalid RED result.
- Work from `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` using POSIX shell syntax. Create local commits only; do not push, merge, rebase, or open a pull request.

### Risk Policy

Policy version: `b2d_task_risk_v1`.

- Hard triggers always produce `high`: `concurrency_lifecycle`, `security_trust_boundary`, `migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`, `update_rollback`.
- Soft signals sum once each: `cross_runtime_or_process=2`; `broad_production_surface=1`; `multiple_ownership_modules=1`; `shared_interface=1`; `dependency_or_build=1`; `multi_layer_without_test_seam=1`.
- Soft total `>=3` produces `high`; totals `0-2` produce `normal` when no hard trigger applies.
- Route `normal` Tasks to implementer `grok` with reviewer `[codex]`.
- Route `high` Tasks to implementer `codex` with reviewers `[codex (separate reviewer thread, not the Author or implementer), grok]`.
