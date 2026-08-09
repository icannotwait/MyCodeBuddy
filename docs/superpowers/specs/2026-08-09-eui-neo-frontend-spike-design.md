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

### Data directory

- Default: `$XDG_DATA_HOME/codeg-eui` or `~/.local/share/codeg-eui`.
- Override: `CODEG_EUI_DATA_DIR`.
- Must not default to the main `codeg` data directory.

## In-process bridge

### Threading

| Thread | Responsibility |
| --- | --- |
| Main | EUI loop, input, compose, draw |
| Tokio multi-thread | ACP, DB, process launch, event production |

Synchronous `extern "C"` entry points must only enqueue work or copy a ready
snapshot. Blocking agent/DB work stays off the UI thread.

### Command surface (UI → Rust)

Illustrative C API (names may be adjusted in implementation):

```c
int  codeg_eui_init(const char* data_dir_utf8);
void codeg_eui_shutdown(void);

int  codeg_eui_set_workspace(const char* path_utf8);
int  codeg_eui_create_session(const char* agent /* "grok"|"codex" */,
                              int* out_conversation_id);
int  codeg_eui_select_session(int conversation_id);

int  codeg_eui_send_user_message(const char* text_utf8);
int  codeg_eui_cancel_active_turn(void); /* P1 */

int  codeg_eui_get_agent_settings(const char* agent, char* out_json, size_t cap);
int  codeg_eui_set_agent_settings(const char* agent, const char* json_utf8);
int  codeg_eui_probe_agent(const char* agent, char* out_json, size_t cap);

int  codeg_eui_poll(CodegEuiFrame* out); /* non-blocking */
```

Include `CODEG_EUI_API_VERSION` for crude compatibility checks.

`CodegEuiFrame` is a C-friendly snapshot: session list summaries, current
transcript (or incremental generation counter + buffers), connection/stream
flags, and error string. Large strings use Rust-owned buffers exposed by
pointer/length for the duration of the poll call (or until the next poll),
with documented lifetime rules.

### Event surface (Rust → UI)

```text
ACP / SessionState
  → EventEmitter::WebOnly (broadcaster + InternalEventBus)
  → eui bridge event pump
  → merge into UI model (session list + transcript + live assistant buffer)
  → bounded queue / generation counter
  → codeg_eui_poll → CodegEuiFrame
```

Merge rules:

- Text deltas coalesce into the latest live assistant buffer.
- Control events (turn end, hard error, permission) are not dropped; they use a
  bounded control queue.
- Poll is dirty-driven, at most ~60 Hz.
- On overflow: drop intermediate text merges, keep latest text + control events.
  Final turn state must still converge.

### Core call mapping

| Bridge op | Existing core path (conceptual) |
| --- | --- |
| init | Open SQLite under data_dir; construct AppState for EUI |
| set_workspace | Folder/workspace open core |
| create_session | Project conversation create core + ACP connection prep |
| send | Existing linked send-prompt / ACP send path |
| list/select | Conversation list + transcript load |
| settings | `acp_list_agents` / `acp_update_agent_config` family and Grok/Codex helpers |
| probe | Existing agent install / readiness checks |

### AppState assembly

Prefer reusing `AppState` to avoid a second state graph. For EUI:

- Force `EventEmitter::web_only(...)`.
- Do not start Tauri-only or desktop-window tasks (embedded public web server
  UI surface, pet window, auto-updater, etc.) unless construction absolutely
  requires them.
- Delegation/workflow subsystems may exist if constructors demand them, but the
  spike UI does not expose them.

**Hard constraint:** EUI path must compile and run with
`--no-default-features` relative to `tauri-runtime` (no Tauri dependency).

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
| Agent missing / not launchable | Probe fails; block send with pointer to Settings |
| Invalid workspace path | Fail session create; show error strip |
| Agent crash mid-stream | Mark turn error; keep streamed text; allow new session |
| ACP permission / ask-user | **P0 degrade:** auto-deny or read-only notice that interactive prompts need the main app; optional P1 Approve/Deny dialog |
| Oversized JSON at FFI boundary | Error code; no crash |
| Poll queue overflow | Drop intermediate text merges; keep latest + control events |
| Init failure | In-window error + stderr / log file |

Logging: Rust tracing/log into the EUI data dir; critical errors also surface in
the UI error strip.

## Performance measurement

### Scenario

- Agents: Grok and Codex at least once each when installed; document skips.
- Prompt: medium-length continuous text stream; optional second run that asks
  for a code block.
- Workspace: small fixed fixture directory under
  `codeg-eui/fixtures/perf-workspace` (or equivalent).

### Instrumentation

Rust bridge timestamps:

- `t0` — send accepted
- `t_first_token` — first non-empty live assistant buffer
- `t_end` — turn complete

Process metrics:

- Peak RSS via `/proc/self/status` sampling or wrapper script

WebView baseline: same machine, same agent/prompt/workspace; document the
measurement method even if manual.

### Deliverable

`codeg-eui/README.md` includes build deps, env vars, comparison steps, and a
results table template. One local filled table is enough for spike completion.

## Testing strategy

| Layer | What |
| --- | --- |
| Rust unit/integration | Isolated data_dir init; snapshot merge; bounded event pump; settings JSON round-trip |
| Bridge contract tests | `poll` / `send` without real GUI |
| Manual E2E | Linux real Grok + real Codex streaming |
| Regression | Default `cargo` / desktop builds do not require EUI sources |

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
   package.
2. Hand-written `extern "C"` bridge (not required to adopt `cxx`).
3. EUI-NEO as a **pinned git submodule** under `codeg-eui/third_party/EUI-NEO`.
4. Permission UI stays degraded in M4; no Approve/Deny requirement for spike
   success.

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
