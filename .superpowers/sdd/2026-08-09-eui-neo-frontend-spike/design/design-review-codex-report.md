# EUI-NEO Frontend Spike Design Review (Codex)

## Review Basis

- Requirements baseline:
  `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`
- Verified SHA-256:
  `85d985e7adb02a9e1547ea7e4ac21aca301fa8cb9ab526dba50c4eda0b49d5b2`
- Review focus: feasibility, concurrency and FFI safety, data isolation,
  settings parity, scope, build isolation, and readiness for `writing-plans`.
- Repository evidence reviewed includes `AppState`, `EventEmitter::WebOnly`,
  `InternalEventBus`, the per-connection snapshot/replay path, server bootstrap,
  Grok/Codex settings helpers, permission handling, and the Cargo feature graph.
- Upstream feasibility was checked against EUI-NEO's integration guide and
  current `main` (`cb70ea8bea263efa7805a40c07135df028ad44b1`). Its framework-owned GLFW
  main and `eui::neo` static-library integration both support the proposed host
  split.

## Overall Assessment

The same-process host is feasible: EUI can own the main/UI thread while a Rust
`staticlib` owns a Tokio runtime and the existing core. The repository also
already proves that the shared core compiles without `tauri-runtime`, and
`EventEmitter::WebOnly` correctly emits typed ACP envelopes to
`InternalEventBus` without a `tauri::AppHandle`.

The design is not yet ready for an implementation plan, however. Several
bridge contracts are currently expressed as alternatives or contain internal
contradictions. Those choices affect the public C ABI, the event-pump
architecture, tests, and milestone ordering, so deferring them to a plan author
would produce materially different implementations.

## Blocking Findings And Required Design Edits

### 1. Async commands contradict the non-blocking UI-thread rule

**Severity: BLOCKING**

The threading rule says synchronous C entry points only enqueue work or copy a
ready snapshot (design lines 192-193), but the illustrative ABI returns an
immediate conversation ID and immediate settings/probe JSON (lines 203-215).
Those operations perform SQLite, filesystem, subprocess, and agent work in the
existing core. They cannot both return their real result synchronously and
remain non-blocking.

**Required design edit:** Replace the command-surface text with one chosen
asynchronous completion contract. Commands other than a documented startup
`init` phase must return only immediate validation/enqueue status plus a
monotonic `request_id`. Define a command-completion collection in
`CodegEuiFrame` containing at least `request_id`, operation, terminal status,
typed result (including `conversation_id` or settings/probe payload), and error.
Specify ordering and stale-result handling when workspace/session selection
changes. Remove synchronous `out_conversation_id` and output-buffer semantics
from async operations. Add contract tests proving a slow DB/probe does not block
the polling/UI thread and every accepted request reaches exactly one terminal
completion.

### 2. FFI buffer ownership and shutdown safety are undecided

**Severity: BLOCKING**

The snapshot lifetime is currently "for the duration of the poll call (or until
the next poll)" (lines 220-224). The first option is unusable because C++ can
only consume the returned pointers after the call returns; the two options also
require different storage. The design does not define concurrent-call rules,
reinitialization, shutdown versus in-flight workers, UTF-8/length validation,
or panic containment.

**Required design edit:** Choose one ABI ownership model. For the proposed
polled model, require Rust to retain one immutable frame backing store until the
next successful poll or completed shutdown, require C++ to copy any retained
display data before that point, and state that no other command invalidates the
frame. Define a bridge lifecycle (`uninitialized`, `starting`, `running`,
`stopping`, `stopped`), UI-thread affinity for all public calls, and shutdown
ordering: reject new commands, cancel/drain workers, join the runtime, then free
the last frame. Inputs must be pointer/length pairs with null, overflow, maximum
size, and UTF-8 checks. State how Rust panics are prevented from unwinding over
the C boundary. Add ABI tests for undersized buffers, invalid UTF-8, double
init/shutdown, poll during startup/stopping, and shutdown with in-flight work.

### 3. The event overflow rules cannot guarantee convergence as written

**Severity: BLOCKING**

Lines 239-244 say control events are not dropped, use a bounded queue, and
survive overflow, but no bounded non-blocking queue can promise all three.
Moreover, the current `InternalEventBus` is a 4096-entry broadcast channel whose
receivers can receive `Lagged`; its separate critical lane is owned by the
lifecycle subscriber, not by arbitrary UI subscribers. The existing robust UI
path instead snapshots and subscribes to each `SessionState` under one lock,
tracks sequence numbers, and reattaches to snapshot/replay after a gap
(`web/ws_attach.rs`).

**Required design edit:** Make per-connection snapshot-and-subscribe semantics,
including an event sequence cursor, the canonical EUI live path. Require the
pump to detect a sequence gap, receiver lag, or local control-queue saturation,
mark that connection `needs_resync`, and replace its projection from an
authoritative `SessionState` snapshot before applying further events. The
`InternalEventBus` may still support discovery and shared core consumers, but
must not be described as the sole lossless frontend stream. Replace the
"bounded but never dropped" claim with bounded/coalesced delivery plus mandatory
snapshot recovery; producer paths must not block on UI consumption. Add tests
for subscribe/snapshot race freedom, lag, overflow containing turn completion
and permission events, session switching mid-stream, and final snapshot parity.

### 4. The EUI data root is not connected to the core's process-wide path contract

**Severity: BLOCKING**

The design introduces `CODEG_EUI_DATA_DIR` (lines 177-181), but much of the
existing core resolves logs, credentials, transcripts, and subprocess state
through process-wide `CODEG_DATA_DIR`. Server startup deliberately absolutizes
and pins that variable before creating Tokio threads. Merely passing an EUI
path to `init_database`/`AppState` can therefore split state or inherit a main
app `CODEG_DATA_DIR`, despite the isolation goal.

**Required design edit:** Define startup resolution as a single-shot operation
before Rust worker threads and logging/core initialization: resolve an absolute
root from non-empty `CODEG_EUI_DATA_DIR`, otherwise the XDG EUI default, and pin
that exact root as the EUI process's effective `CODEG_DATA_DIR`. A pre-existing
main-app `CODEG_DATA_DIR` must not override the EUI default; only the explicit
EUI variable may opt into another root. The resolved path must be used for the
database, `AppState.data_dir`, logs, credential helpers, and child-process
inheritance. Document the relationship to `CODEG_HOME`. Add an isolation test
with ambient `CODEG_DATA_DIR` pointing at the main app and verify EUI creates
and opens only `<eui-root>/codeg.db` and its own support directories.

### 5. Read-only prompt degradation can leave turns permanently parked

**Severity: BLOCKING**

The P0 behavior allows either auto-deny or a read-only notice (line 353). In the
current core, an ACP permission request parks its responder until
`respond_permission`, and `ask_user_question` is enabled by default and parks a
one-shot until answer/cancel. Displaying a notice alone therefore does not
degrade: it can deadlock the primary streaming flow and prevent `t_end`.

**Required design edit:** Select deterministic P0 resolution, not an
alternative. On every ACP permission request, immediately choose a reject
option when supplied, otherwise cancel the pending request/turn through the
existing core path, and surface a read-only notice after resolution. Disable
EUI `ask_user_question` injection in the process-local runtime profile or
immediately cancel/decline each question without changing the persisted main
app setting. State that plan approvals and any other unsupported interactive
request receive the same terminal decline policy. Add E2E/contract tests proving
Grok and Codex turns cannot remain parked on unsupported interaction.

### 6. The proposed latency instrumentation does not measure first-visible UI

**Severity: BLOCKING**

The goal is first-visible assistant latency and stream jank/frame-interval p95,
but `t_first_token` is currently recorded when Rust first sees a non-empty live
buffer (lines 371-377). That excludes poll delay, C++ model merge, compose,
render, and presentation. No concrete frame-interval sampling window or WebView
equivalent is defined, so the primary comparison can report backend latency as
UI latency and compare unlike anchors.

**Required design edit:** Define common measurement anchors and collectors.
Keep `t_first_token` as a diagnostic, but add `t_first_presented` on the EUI UI
thread after presenting the first frame containing assistant text; define the
equivalent WebView paint marker and use the same `t0` semantics for both. Choose
one jank metric (recommended: presented-frame interval p95 plus count over a
fixed threshold), define the active-stream sampling window, warm-up/run count,
aggregation, and idle/startup treatment. State whether RSS includes child agent
processes (recommended: shell process only for both) and record build type,
backend, prompt, agent, and skipped runs in the results table.

## Non-Blocking Findings

- **MINOR:** The independent Rust crate is the right isolation boundary, but
  this repository currently has a single Cargo package rather than a workspace.
  The implementation plan must make `codeg-eui-core` a standalone crate or an
  explicitly excluded/non-default workspace member, path-depend on `codeg` with
  `default-features = false`, and verify existing default/desktop/server/MCP
  commands never traverse EUI or CMake sources.
- **MINOR:** Settings reuse is directionally correct. The Grok/Codex
  `acp_list_agents_core` and update helpers are currently `pub(crate)`, so the
  plan should add a narrow public Rust facade/DTO for EUI rather than duplicate
  their TOML/JSON projection or invoke Axum handlers. Native `~/.codex` and
  `~/.grok` files remain intentionally shared agent configuration; the design
  should avoid describing them as EUI-isolated application data.
- **MINOR:** Full `AppState` construction is feasible in no-Tauri mode but pulls
  in many unrelated required fields. The plan should enumerate the minimum
  startup tasks needed for conversation persistence and streaming and must not
  start the web server, pet mapper, automation engine, updater, or chat-channel
  background tasks. An EUI bootstrap profile/factory is preferable to copying
  the server bootstrap wholesale.
- **INFO:** Scope is otherwise disciplined: Linux-first, an optional binary,
  reduced message rendering, degraded interaction, no replacement of Tauri or
  React, and CI that does not require native EUI dependencies are appropriate
  spike boundaries.

## Readiness Gate

After the six blocking contracts above are made singular and testable, the
milestone structure is suitable for `writing-plans`. Until then, M0-M4 cannot
be decomposed reliably because the ABI, completion delivery, recovery path,
startup root, and performance acceptance instrumentation remain unsettled.

VERDICT: changes_requested
