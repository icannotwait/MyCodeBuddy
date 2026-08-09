# EUI-NEO Frontend Spike Design

## Status

Approved in conversation on 2026-08-09.

## Summary

Add an optional, parallel native desktop shell powered by
[EUI-NEO](https://github.com/sudoevolve/EUI-NEO) (C++17, GLFW + OpenGL/Vulkan)
to validate two things without replacing the existing Tauri WebView + React
frontend:

1. **End-to-end product loop**: create a Grok or Codex session against the
   existing Rust core, send a message, and render a streaming assistant reply.
2. **Reproducible performance comparison**: collect a small set of metrics
   against the current WebView desktop shell on the same machine.

The spike is a new binary (`codeg-eui`), same process hybrid host (EUI owns the
UI thread; Rust owns AppState / ACP / DB), CMake-led EUI app linked to a Rust
`staticlib`, and an isolated default data directory.

## Problem

The current desktop UI path is:

```text
ACP update → Rust state → Tauri emit → WebView JS → React reducers → Streamdown
```

High-rate streaming has already required substantial WebView-path optimization
work. EUI-NEO offers a no-WebView, dirty-rect GPU UI stack that may reduce
paint and main-thread cost for tool-style chat UIs. Before any product
migration, the project needs a **minimum viable proof** that:

- the existing agent core can drive a native UI, and
- the performance difference is measurable enough to justify further investment.

## Goals

1. Ship a Linux-first optional binary `codeg-eui` that does not change the
   default `codeg` / `codeg-server` / React build paths.
2. Complete a real streaming loop with **Grok** and **Codex**:
   workspace path → new session → send message → visible streaming reply.
3. Provide a simple settings surface that reads/writes the **same backend
   agent config schema** used by the existing Grok/Codex settings (not a second
   config system).
4. Collect three reproducible comparison metrics versus the WebView shell:
   first-visible assistant latency, stream jank (or frame-interval p95), and
   peak RSS.
5. Keep data fully isolated from the main app database by default.

## Non-Goals

- Replacing Tauri, removing React, or changing the default desktop entrypoint.
- Windows/macOS as first delivery targets.
- File tree, terminal, split panes, delegation UI, workflow overlay, pet,
  full settings IA.
- Full tool-card fidelity, plan approval UX, or browser-grade Markdown.
- Sharing the main application SQLite data directory by default.
- Matching the full
  `docs/superpowers/performance/webview-streaming/` report format.
- Full i18n for the spike UI (fixed locale strings are enough).

## Decisions

| Topic | Choice |
| --- | --- |
| Validation goals | End-to-end streaming + small performance metrics |
| Delivery shape | Parallel binary; existing shells unchanged |
| Host model | Same-process hybrid (A′): EUI UI loop + Rust core |
| Build approach | CMake-led EUI app + Rust `staticlib` bridge |
| Agents | Grok + Codex |
| Session model | Create minimal new session; streaming send/receive |
| UI depth | Recognizable Codeg chrome (sidebar + chat + input) |
| Settings depth | Align existing Grok/Codex **backend** config capability |
| Data | Isolated default data dir |
| Platform | Linux first |

### Host model clarification (A′)

EUI-NEO's recommended integration owns the window and event loop via its app
main (`compose` + GLFW). It also supports embedding via `eui::neo` and
`core::dsl::Runtime` in a custom loop. The public facade is **C++**, not a
stable C ABI.

Therefore "Rust host" means **Rust owns business authority** (AppState, ACP,
DB, Tokio), not "Rust drives every GPU frame." In practice:

- Main/UI thread: EUI event loop + `compose`
- Background: Tokio multi-thread runtime + existing core
- Bridge: narrow C API (or equivalent) for commands and polled snapshots

This is feasible. The unsupported extreme is fine-grained Rust ownership of
GLFW/OpenGL frame submission without EUI's lifecycle helpers; that is out of
scope.

## Architecture

```text
codeg-eui  (single process, Linux)

┌─ Main / UI thread ─────────────────┐    ┌─ Tokio worker threads ──────────┐
│ EUI-NEO                            │    │ libcodeg_eui_core (Rust)        │
│  glfw app main + app::compose      │FFI │  AppState                       │
│  pages: shell / chat / settings    │◄──►│  EventEmitter::WebOnly          │
│  UI snapshot (display copy)        │    │  ConnectionManager / ACP        │
│                                    │    │  SQLite (isolated data_dir)     │
└────────────────────────────────────┘    │  Grok + Codex launch paths     │
                                          └────────────────────────────────┘
```

### Principles

1. **Authoritative state lives in Rust.** C++ holds display snapshots only.
2. **Reuse existing `_core` / ACP / DB paths.** Do not invent a second session
   protocol.
3. **Emit through `EventEmitter::WebOnly`** (broadcaster + `InternalEventBus`).
   Do not depend on Tauri `app.emit`.
4. **Keep the FFI surface narrow and versioned.** Do not leak `AppState` into
   C++.
5. **Default product builds must not require EUI-NEO** sources or native UI
   deps.

### Relationship to existing binaries

| Binary | Role after spike |
| --- | --- |
| `codeg` | Unchanged Tauri + WebView desktop |
| `codeg-server` | Unchanged HTTP/WebSocket server |
| `codeg-mcp` | Unchanged delegation companion |
| `codeg-eui` | New optional native shell spike |

## Repository and build layout

```text
MyCodeBuddy/
  codeg-eui/
    CMakeLists.txt
    README.md                 # build, run, perf steps
    app/
      app.cpp                 # dslAppConfig + compose routing
      pages/
        shell.h
        chat.h
        settings.h
      bridge/
        codeg_eui_bridge.h    # C API
        ui_snapshot.h
    scripts/
      build.sh                # cargo staticlib → cmake → link
      perf_compare.sh
    third_party/
      EUI-NEO/                # pinned submodule or FetchContent tag

  src-tauri/
    # preferred: workspace crate codeg-eui-core → staticlib
    # alternative: feature eui-core on codeg package
```

### Rust artifact

Preferred: workspace crate **`codeg-eui-core`** producing `staticlib`, depending
on shared library code **without** `tauri-runtime` / `server` default features.

Acceptable alternative: same package feature `eui-core` with extra crate-type.
Independent crate is preferred for link isolation.

### CMake artifact

- Add EUI-NEO as subdirectory or pinned FetchContent.
- Build `codeg-eui` with `eui_neo_configure_app` (or equivalent link + assets
  copy).
- Default backends: **GLFW + OpenGL** on Linux.
- `scripts/build.sh` orchestrates cargo then cmake.

### Data directory (single process-wide root)

EUI resolves **one absolute data root** once, **before** Tokio worker threads,
logging, credential helpers, or core init:

1. If `CODEG_EUI_DATA_DIR` is non-empty → use its absolute form.
2. Else → `$XDG_DATA_HOME/codeg-eui` or `~/.local/share/codeg-eui` (absolute).
3. Pin that exact path as this process’s effective `CODEG_DATA_DIR` (overwrite
   any pre-existing main-app `CODEG_DATA_DIR` in the environment). Only the
   explicit `CODEG_EUI_DATA_DIR` variable may opt into a non-default root;
   ambient main-app `CODEG_DATA_DIR` must **not** win over the EUI default.

That same root is used for: SQLite (`codeg.db`), `AppState.data_dir`, logs,
credential helper state, and child-process inheritance. Relationship to
`CODEG_HOME` follows existing core rules under the pinned `CODEG_DATA_DIR`.

Native agent configs under `~/.codex` / `~/.grok` remain the agents’ own files
(intentionally shared with the main app). They are **not** EUI application
data and are not relocated into the EUI root.

**Isolation test requirement:** with ambient `CODEG_DATA_DIR` pointing at a
main-app data dir, EUI must create/open only `<eui-root>/codeg.db` and its own
support directories.

## In-process bridge

### Threading and call affinity

| Thread | Responsibility |
| --- | --- |
| Main / UI | EUI loop, input, compose, draw; **all** public `codeg_eui_*` calls |
| Tokio multi-thread | ACP, DB, process launch, event production |

Public FFI entry points are **UI-thread-only**. They must only: validate
inputs, enqueue work, or copy a ready snapshot. Blocking agent/DB/probe work
stays on Tokio. Concurrent calls from non-UI threads are undefined and must
fail tests that assert single-threaded affinity where practical.

### Bridge lifecycle

States: `uninitialized` → `starting` → `running` → `stopping` → `stopped`.

- `init`: only legal from `uninitialized`/`stopped`; moves to `starting`, then
  `running` on success or back to `stopped` on failure.
- Command enqueue (below) is only legal in `running`.
- `shutdown`: reject new commands; cancel/drain in-flight workers; join Tokio
  runtime; free the last frame backing store; enter `stopped`.
- Double `init` / double `shutdown` / `poll` outside `running` return explicit
  error codes (no crash).

Rust panics must not unwind across the C boundary: use `catch_unwind` (or
equivalent) at each public entry and map panics to a stable error code plus a
diagnostic string in the next frame when possible.

### Asynchronous command contract (chosen)

**Decision:** All non-lifecycle operations are **async request/completion**.
They return only immediate validation/enqueue status plus a monotonic
`request_id`. Results appear in `CodegEuiFrame.completions[]`, never via
synchronous out-parameters for DB/agent work.

Lifecycle-only sync ops:

- `codeg_eui_init` / `codeg_eui_shutdown` / `codeg_eui_poll` (and version query)

Async ops (enqueue only):

- set_workspace, create_session, select_session, send_user_message,
  cancel_active_turn (P1), get/set_agent_settings, probe_agent, and any later
  list/reload helpers.

Illustrative C API (names may be adjusted; semantics fixed):

```c
/* Lifecycle / poll — may run work only for init path documented as startup */
int  codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
/* data_dir may be empty → resolve default root per Data directory rules */
void codeg_eui_shutdown(void);
uint32_t codeg_eui_api_version(void);

/* Async enqueue: returns 0 + *out_request_id on accept; non-zero error otherwise */
int  codeg_eui_set_workspace(const uint8_t* path_utf8, size_t path_len,
                             uint64_t* out_request_id);
int  codeg_eui_create_session(const uint8_t* agent_utf8, size_t agent_len,
                              uint64_t* out_request_id);
int  codeg_eui_select_session(int32_t conversation_id, uint64_t* out_request_id);
int  codeg_eui_send_user_message(const uint8_t* text_utf8, size_t text_len,
                                 uint64_t* out_request_id);
int  codeg_eui_cancel_active_turn(uint64_t* out_request_id); /* P1 */
int  codeg_eui_get_agent_settings(const uint8_t* agent_utf8, size_t agent_len,
                                  uint64_t* out_request_id);
int  codeg_eui_set_agent_settings(const uint8_t* agent_utf8, size_t agent_len,
                                  const uint8_t* json_utf8, size_t json_len,
                                  uint64_t* out_request_id);
int  codeg_eui_probe_agent(const uint8_t* agent_utf8, size_t agent_len,
                           uint64_t* out_request_id);

/* Non-blocking copy of current frame pointers into *out */
int  codeg_eui_poll(CodegEuiFrame* out);
```

**Input rules:** all strings are pointer+length; null pointer with len>0 is
error; max sizes (path/json/message) are documented constants; invalid UTF-8 is
rejected at enqueue; oversized payloads return error without crash.

**Completion rules:**

- Each accepted request produces **exactly one** terminal completion in a later
  frame: `{ request_id, op, status, result_payload?, error? }`.
- `create_session` result carries `conversation_id`; settings/probe carry JSON
  in the completion payload (Rust-owned until next successful poll).
- If workspace or session selection changes, in-flight completions for the
  previous selection still arrive once, marked `stale` when no longer applicable
  to the current UI selection; the UI ignores stale results.
- Contract tests must prove a slow DB/probe does not block the UI/`poll` path
  and every accepted request reaches exactly one terminal completion.

### Frame ownership (chosen)

**Decision:** Polled immutable frame backing store.

- On each successful `poll`, Rust exposes pointers into **one** immutable frame
  buffer retained until the **next successful poll** or completed `shutdown`.
- C++ must copy any data it needs past that point before the next poll.
- No other command invalidates the current frame (enqueue does not free it).
- Failed poll leaves the previous frame valid if still in `running`.

`CodegEuiFrame` includes at least: lifecycle state, session list summaries,
current connection id + event sequence cursor, transcript/live assistant
buffers (generation counter), stream flags, error strip text, `completions[]`,
and a `needs_resync` flag for the active connection.

### Event surface (Rust → UI) — snapshot + subscribe with recovery

**Canonical live path (chosen):** per-connection **snapshot-and-subscribe**,
mirroring the robust Web attach pattern (`SessionState` under lock + sequence
cursor + reattach after gap). Not a claim of lossless broadcast.

```text
SessionState (authoritative per connection)
  → snapshot at attach / resync
  → sequence-numbered event stream for that connection
  → eui bridge pump (UI model merge)
  → codeg_eui_poll → CodegEuiFrame

EventEmitter::WebOnly / InternalEventBus
  → optional discovery / shared core consumers only
  → NOT the sole lossless frontend stream for the EUI shell
```

Merge and recovery rules:

- Text deltas coalesce into the latest live assistant buffer.
- Control-class events (turn end, hard error, permission outcome) are applied
  when received; delivery is **bounded/coalesced**, not “never dropped.”
- Poll is dirty-driven, at most ~60 Hz.
- On sequence gap, receiver lag, or local control-queue saturation: mark the
  connection `needs_resync`, replace the projection from an authoritative
  `SessionState` snapshot, then resume events after the snapshot sequence.
  Producer paths must not block on UI consumption.
- Final turn state must converge via snapshot recovery even if intermediate
  text merges were dropped.
- Required tests: subscribe/snapshot race freedom; lag; overflow that includes
  turn completion and permission events; session switch mid-stream; final
  snapshot parity with core state.

### Core call mapping

| Bridge op | Existing core path (conceptual) | Delivery |
| --- | --- | --- |
| init | Pin data root; open SQLite; construct EUI AppState | sync lifecycle |
| set_workspace | Folder/workspace open core | async completion |
| create_session | Project conversation create + ACP prep | async completion |
| send | Linked send-prompt / ACP send path | async completion |
| list/select | Conversation list + transcript load | async completion |
| settings | Narrow public facade over Grok/Codex config helpers | async completion |
| probe | Existing agent install / readiness checks | async completion |
| poll | Copy current frame | sync |

Settings must use a **narrow public Rust facade/DTO** over existing
`acp_list_agents` / `acp_update_agent_config` family helpers (not Axum handlers
and not a duplicated TOML/JSON projection).

### AppState assembly

Prefer reusing `AppState` to avoid a second state graph, with an **EUI bootstrap
profile/factory** that starts only what streaming needs:

- Force `EventEmitter::web_only(...)`.
- Do **not** start: embedded public web server UI surface, pet window/mapper,
  auto-updater, chat-channel background tasks, or automation engine.
- Delegation/workflow subsystems may exist if constructors demand them, but the
  spike UI does not expose them.

**Hard constraint:** EUI path must compile and run with
`--no-default-features` relative to `tauri-runtime` (no Tauri dependency).
Default desktop/server/MCP commands must never require EUI/CMake sources.
`codeg-eui-core` is a standalone crate (or non-default workspace member) that
path-depends on shared code with `default-features = false`.
## UI design

### Layout

```text
┌──────────────┬────────────────────────────────────────┐
│ Sessions     │  Header (title / agent / status)       │
│ [+ New]      ├────────────────────────────────────────┤
│ • …          │  Message list                          │
│ • …          │   user text / assistant markdown-ish   │
│──────────────┤                                        │
│ Workspace    ├────────────────────────────────────────┤
│ Agent select │  input                         [Send]  │
│ [Settings]   │                                        │
└──────────────┴────────────────────────────────────────┘
```

Dark theme tokens roughly approach current Codeg; pixel parity is not required.

### Pages

1. **Shell** — layout, navigation, global error strip.
2. **Chat** — sessions, messages, composer, streaming indicator.
3. **Settings** — Grok + Codex only.

### Message rendering

- User: plain text.
- Assistant: EUI `components::markdown` when available; plain text fallback.
- Tools: one-line summary only (`tool: name — status`), not full cards.
- Throttle markdown re-parse during streaming (for example 50–100 ms or on
  generation boundaries) so the C++ side does not re-parse every token.

### Settings (backend-aligned)

Do not invent a second config schema. Read/write the same fields the existing
ACP agent config path persists for Grok and Codex.

**Codex field groups (capability parity target):**

- Binary / install probe results
- Auth modes already supported by core
- Model / provider / reasoning effort
- Sandbox / approval structured config
- Advanced raw `config.toml` editor (large text area is acceptable)

**Grok field groups:**

- Binary / install probe
- Structured config / toml projection already used by core
- Model and other existing Grok settings fields

**Settings delivery tiers:**

| Priority | Content |
| --- | --- |
| P0 | Everything required to probe, save, launch, and select a working model |
| P1 | Remaining structured form fields |
| P2 | Advanced raw toml/json editors |

Any field without which launch fails is P0 by definition.

### Primary user flow

```text
First launch
  → Settings: configure and probe Grok and/or Codex
  → Shell: set existing workspace directory
  → Select agent → New Session
  → Type message → Send
  → Status: connecting / streaming / idle / error
  → Assistant region updates until turn completes
```

## Error handling and degradation

| Scenario | Behavior |
| --- | --- |
| Agent missing / not launchable | Probe fails (async completion); block send with pointer to Settings |
| Invalid workspace path | Fail session create completion; show error strip |
| Agent crash mid-stream | Mark turn error; keep streamed text; allow new session |
| ACP permission | **P0 chosen policy:** immediately select a reject/deny option when the request supplies one; otherwise cancel the pending permission/turn through the existing core path. Then surface a read-only notice that interactive prompts need the main app. Never leave a parked responder. |
| ask_user_question / plan approval / other unsupported interactive | Same terminal decline: disable EUI `ask_user_question` injection in the process-local runtime profile **or** immediately cancel/decline each question without changing the persisted main-app setting. Plan approvals and any other unsupported interactive request use the same terminal decline. Optional P1 Approve/Deny UI is out of spike success criteria. |
| Oversized / invalid UTF-8 at FFI | Error code at enqueue; no crash |
| Sequence gap / lag / queue saturation | `needs_resync` + authoritative SessionState snapshot recovery |
| Init failure | Stay/return `stopped`; in-window error + stderr / log file |

**E2E/contract requirement:** Grok and Codex turns cannot remain parked on
unsupported interactive prompts; `t_end` is always reached or a hard error is
surfaced.

Logging: Rust tracing/log into the EUI data dir; critical errors also surface in
the UI error strip.

## Performance measurement

### Scenario

- Agents: Grok and Codex at least once each when installed; document skips.
- Prompt: medium-length continuous text stream; optional second run that asks
  for a code block.
- Workspace: small fixed fixture directory under
  `codeg-eui/fixtures/perf-workspace` (or equivalent).
- Runs: document warm-up discard (at least one) and N measured runs (default 3);
  aggregate as median for latency and p95 for frame intervals.

### Instrumentation (common anchors)

Shared `t0` semantics for both shells: moment the user send is **accepted** by
the product path (EUI: async send enqueue success; WebView: equivalent send
accepted).

| Marker | Where | Meaning |
| --- | --- | --- |
| `t0` | both | send accepted |
| `t_first_token` | Rust bridge (diagnostic only) | first non-empty live assistant buffer in core |
| `t_first_presented` | **primary** comparison | EUI: UI thread after presenting first frame that contains assistant text; WebView: equivalent first paint/presentation of assistant text (document exact DOM/paint hook used) |
| `t_end` | both | turn complete (or hard error) |

**Jank metric (chosen):** presented-frame interval p95 during the active stream
window (`t_first_presented` .. `t_end`), plus count of intervals above a fixed
threshold (e.g. 50 ms). Sample only while the stream is active; exclude idle
and startup frames.

**RSS (chosen):** peak RSS of the **shell process only** (EUI `codeg-eui` or
desktop `codeg`), via `/proc/self/status` sampling or wrapper; do **not**
attribute child agent process RSS to either side for the comparison table.
Record build type, backend (OpenGL), prompt id, agent, and skipped runs.

### Deliverable

`codeg-eui/README.md` includes build deps, env vars, comparison steps, and a
results table template with columns for agent, `t_first_presented - t0`,
frame-interval p95, threshold exceed count, peak shell RSS, build type, and
notes. One local filled table is enough for spike completion.

## Testing strategy

| Layer | What |
| --- | --- |
| Rust unit/integration | Isolated data root pin (ambient main `CODEG_DATA_DIR` ignored for default); async request completion; snapshot/resync; settings JSON round-trip via facade |
| Bridge contract tests | UI-thread poll/enqueue without real GUI; slow probe non-blocking; panic containment; double init/shutdown; invalid UTF-8/oversize; shutdown with in-flight work; every request_id completes once |
| Permission degrade tests | Parked permission / ask_user_question cannot stall a turn |
| Manual E2E | Linux real Grok + real Codex streaming |
| Regression | Default `cargo` / desktop / server / MCP builds do not require EUI sources |
CI must not hard-require EUI native deps. An optional job that skips when
dependencies are missing is acceptable later; not required for spike design
acceptance.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Dual build fragility | Single `build.sh`; pin EUI tag; API version constant |
| Heavy AppState construction | Start with web_only full construct; add `new_eui` trim only if needed |
| Large Grok/Codex settings surface | P0 launch-required only; expand forms in P1/P2 |
| Interactive permission blocks loop | Documented P0 degrade |
| C++ markdown thrash while streaming | Throttled re-parse + simple tool lines |
| Submodule size / license | EUI-NEO is Apache-2.0; shallow pin |

## Implementation milestones

1. **M0** — EUI Hello Window + empty `init`/`poll` link.
2. **M1** — Isolated DB + web_only AppState + empty session list.
3. **M2** — Settings P0 for Grok/Codex (probe + launchable config).
4. **M3** — Workspace + create session + history load.
5. **M4** — Send + live buffer + text/markdown render.
6. **M5** — Error strip, session switch, cancel (P1), settings P1.
7. **M6** — Instrumentation + README comparison run.

## Default implementation preferences

These are defaults for the implementation plan unless new evidence forces a
change:

1. Independent crate `codeg-eui-core` (`staticlib`), not a feature on the Tauri
   package (standalone crate or non-default workspace member; path-dep with
   `default-features = false`).
2. Hand-written `extern "C"` bridge (not required to adopt `cxx`) with the
   async request/completion + polled frame ownership contracts above.
3. EUI-NEO as a **pinned git submodule** under `codeg-eui/third_party/EUI-NEO`.
4. Permission / ask-user / plan-approval stay on the **deterministic P0 decline
   policy** through M4; no Approve/Deny requirement for spike success.
5. Live path is per-connection snapshot+subscribe with mandatory resync, not
   InternalEventBus-as-lossless-stream.

## Acceptance checklist

- [ ] Linux `scripts/build.sh` produces a runnable `codeg-eui`.
- [ ] Default data dir is isolated from the main app.
- [ ] With correct local agent setup, Grok and Codex can each create a session
      and stream a reply.
- [ ] Settings P0 can read/write/persist existing backend config fields needed
      to launch.
- [ ] Default `codeg` desktop/server builds and tests do not depend on EUI.
- [ ] README includes perf comparison steps and at least one local results
      table.

## References

- EUI-NEO: https://github.com/sudoevolve/EUI-NEO
- EUI-NEO integration guide (public header app main, static lib embed,
  custom GLFW loop): repository `docs/集成指南.md`
- Existing event abstraction: `src-tauri/src/web/event_bridge.rs`
  (`EventEmitter::WebOnly`)
- Existing agent config surfaces: `acp_update_agent_config` / related commands
  in `src-tauri/src/commands/acp.rs` and web handlers
- Prior WebView streaming work:
  `docs/superpowers/specs/2026-07-16-webview-streaming-performance-design.md`
