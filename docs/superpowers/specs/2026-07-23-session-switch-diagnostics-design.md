# Session Switch Visual Residual Diagnostics Design

Date: 2026-07-23

Status: Design approved in conversation; written-spec review pending

## Summary

Add an opt-in, content-free trace for main-window conversation switches. One
trace correlates the frontend switch request, detached-window focus probe, tab
store commit, React surface commit, detail load, ACP connection initialization,
active-connection projection, and selector readiness. Frontend timestamps are
captured before asynchronous logging and forwarded to the existing Rust
`tracing` hub, where they are available through the Settings log viewer, the
in-memory log API, and rotating files.

This work is diagnostic only. It does not change tab switching, preview
replacement, detail caching, ACP connection ownership, selector fallback, or
rendering behavior. A subsequent fix will be selected only after a captured
trace identifies the interval in which the old UI remains visible.

## Current Evidence and Open Hypotheses

The current code supports multiple independent explanations for the reported
visual residual:

1. `openTab` awaits `focusDetachedConversation` before changing
   `rawTabs`/`activeTabId`. The old surface remains authoritative during that
   wait.
2. A newly created ACP connection can render selector data from the global
   per-agent cache before the connection emits `selectors_ready`. Cached data
   also suppresses the selector loading chips.
3. `activeKey` and session statistics follow an active-tab change from React
   effects, so status-bar projections can trail the tab-store commit.
4. A cold persisted conversation waits for full detail parsing before it can
   safely resume the historical ACP session. Large transcripts can extend that
   interval.

The existing preview path already gives each persisted conversation a distinct
tab ID/React key, and `useConversationDetail` reports cold loading on its first
render. The trace therefore measures existing boundaries instead of adding
another key or loading state.

## Goals

1. Correlate every relevant switch boundary under one trace ID.
2. Measure client-observed elapsed time without including logging transport
   latency in the measurement.
3. Distinguish whole-surface delay from selector-only and status-bar delay.
4. Distinguish cached selector presentation from authoritative live selector
   presentation.
5. Persist traces in the existing diagnostic log system so they can be queried
   automatically after reproduction.
6. Keep the diagnostic bounded, locally controlled, and free of user content.
7. Support desktop Tauri and authenticated server/web transports through the
   existing transport abstraction.

## Non-Goals

- Fixing the visual residual in the same change.
- Adding product UI or a permanent analytics system.
- Uploading logs or telemetry to an external service.
- Recording prompts, responses, titles, paths, model names, configuration
  values, external agent session IDs, errors, or tool payloads.
- Treating `requestAnimationFrame` as an exact browser-paint timestamp.
- Preloading conversations, retaining runtime sessions, or prewarming agents.
- Changing detached-window no-mirror guarantees.

## Alternatives Considered

### Per-Event Persistent Ingestion

Selected. Each stage captures its frontend timestamp synchronously, then sends
a small, fire-and-forget typed event to a narrow backend command. A stalled or
failed trace still leaves its earlier evidence, and the existing tracing hub
provides ordering, persistence, filtering, and retrieval.

The transport calls add a small amount of work. Capturing the timestamp before
the call and limiting a trace to a small fixed stage set prevents that work from
contaminating the measured boundaries materially.

### Frontend Buffer With End-of-Trace Flush

Buffering all stages in memory and uploading one batch would perturb the switch
less. It loses the most useful evidence when a switch stalls, the window closes,
or the trace never reaches its terminal stage. It also requires a separate
flush/timeout protocol.

### WebView Console Only

Console logging is the smallest code change, but it is not part of the existing
log viewer or file output and is difficult to collect automatically from a
running WebView. It remains a best-effort mirror, not the source of truth.

## Enablement

The frontend owns enablement because all measured stages originate there.

```text
enabled = NODE_ENV is exactly "development"
       or localStorage["codeg:debug:session-switch"] is exactly "1"
```

- `pnpm dev` and `pnpm tauri dev` run Next in `development`, so tracing is on.
- Vitest runs with `NODE_ENV=test`, so normal tests do not emit diagnostics.
- Static export/release builds use `production`, so tracing is off unless the
  local user explicitly enables the browser-local switch.
- Access to `window` or `localStorage` is guarded and failures resolve to off.
- The backend ingress remains available in every build so an explicitly enabled
  production frontend and server/web client use the same path. It accepts only
  the fixed schema below.

No database setting, migration, environment variable, or settings-page control
is added.

## Trace Identity and Lifetime

Starting `openTab` or `switchTab` creates a random trace ID and records:

- source: `open_preview`, `open_pinned`, or `switch_tab`;
- target numeric conversation ID when one exists;
- target canonical tab ID;
- previous active tab ID when one exists; and
- wall-clock and monotonic start timestamps.

The frontend recorder indexes the active trace by target tab ID and positive
conversation ID so later boundaries can find it without widening public tab or
connection APIs. The newest trace for a target supersedes an older unfinished
one, which receives a terminal `trace_finished` event with outcome
`superseded`.

At most 32 traces are retained in memory. Every trace ends when the selector
state has committed for the active surface, when a terminal focus outcome means
no main-window switch occurs, or after 15 seconds. Timeout cleanup records
`trace_finished{outcome=timeout}` before removing the indexes. Completion and
cleanup are idempotent.

## Event Schema

Every frontend event uses a strongly typed payload with no free-form message or
metadata map:

```text
trace_id             bounded random identifier
stage_seq            per-trace increasing integer
stage                 fixed enum
source                fixed enum
conversation_id       optional positive integer
tab_id                optional bounded canonical tab identifier
previous_tab_id       optional bounded canonical tab identifier
client_timestamp_ms   browser wall-clock milliseconds
elapsed_ms            monotonic milliseconds since trace start
outcome               optional fixed enum
focus_found           optional boolean
detail_cached         optional boolean
detail_loading        optional boolean
connection_status     optional fixed connection-status enum
has_cached_selectors  optional boolean
selectors_ready       optional boolean
mode_count            optional bounded integer
config_option_count   optional bounded integer
```

The backend rejects unknown enum values, non-finite/negative timing values,
non-positive conversation IDs, overlong identifiers, and counts outside a
small configured bound. It never accepts a frontend-provided log level, target,
message, path, error text, or arbitrary JSON fields.

The Rust request type uses `#[serde(deny_unknown_fields)]`; unknown fields are
rejected rather than silently ignored.

## Stage Sequence

Not every path emits every stage. Analysis sorts by `stage_seq` and uses
`client_timestamp_ms`/`elapsed_ms`; backend log sequence is not assumed to match
frontend order because fire-and-forget calls may complete out of order.

```text
switch_requested
focus_probe_started
focus_probe_finished
tab_committed
surface_committed
surface_next_frame
detail_fetch_started
detail_fetch_finished
connection_created
session_started
active_key_synced
selectors_event_received
selectors_render_committed
selectors_next_frame
status_bar_committed
trace_finished
```

Key meanings:

- `tab_committed` is recorded immediately after the Zustand update.
- `surface_committed` is recorded from a layout effect for the active target
  surface. `surface_next_frame` is an rAF approximation of the next visual
  opportunity, not a guaranteed paint timestamp.
- detail stages are recorded in the runtime-store fetch path only when a real
  fetch occurs. `surface_committed{detail_cached=true}` covers cache hits.
- `connection_created`, `session_started`, and `selectors_event_received` are
  recorded at their connection-store boundaries.
- `selectors_render_committed` records counts and the two source flags
  `has_cached_selectors`/`selectors_ready` after React commits the selector
  projection. It never records selected values or option labels.
- `active_key_synced` and `status_bar_committed` expose lag outside the
  conversation surface.

## Frontend Recorder

A focused module owns enablement, clocks, trace indexes, stage sequencing,
bounds, timeout cleanup, and the sink. Call sites only start a trace or record a
fixed stage against a tab/conversation identity.

The recorder captures `Date.now()` and `performance.now()` before any console or
transport operation. It mirrors a compact event to `console.info` and invokes
the backend asynchronously. Sink failure is swallowed after one console warning
per trace; diagnostic failure must never block or alter tab switching.

The module exposes dependency-injected clock and sink construction for unit
tests. Production call sites use one module-level instance. No test-only global
window API is added.

## Backend Ingestion

Add a narrow `record_session_switch_diagnostic` command beside the existing
logging commands, with matching Tauri and Axum wrappers. The shared core:

1. validates the fixed payload;
2. applies a process-global fixed-memory token bucket (64 events/second with a
   burst of 256); over-limit events return `accepted=false` without logging;
3. emits one `tracing::info!` event with target
   `codeg_frontend::session_switch` and structured fields; and
4. returns whether the event was accepted without database or filesystem work
   of its own.

The installed tracing subscriber fans the event into stderr, the daily JSONL
file, the in-memory ring buffer, and `logs://appended` when a viewer is present.
Normal transport authentication and request-size limits continue to apply.

## Reproduction and Analysis

After implementation:

1. Run focused frontend and Rust tests plus the relevant checks.
2. Start `pnpm tauri dev` with a WebView2 remote-debugging port.
3. Use a one-off Chrome DevTools Protocol script to activate two existing
   development-data conversations through the real sidebar/tab UI.
4. Exercise at least these paths when data permits:
   - sidebar preview A to B;
   - already-open tab A to B;
   - same-agent cold selector initialization; and
   - warm selector initialization.
5. Query recent logs for target `codeg_frontend::session_switch` and group by
   trace ID.
6. Report the dominant interval and whether cached selectors committed before
   authoritative readiness.

If development data or automation cannot reproduce the visible symptom, retain
the instrumentation and report `insufficient reproduction evidence`; do not
select a fix from code inspection alone. A user-driven reproduction can then be
read from the same persisted log target.

## Error Handling and Bounds

- Logging never participates in switch control flow and never gets awaited by
  product actions.
- One warning per trace bounds console failure noise.
- 32 active traces and a 15-second lifetime bound memory and timers.
- The stage enum and per-trace sequence prevent unbounded arbitrary events.
- Identifier and count validation bounds each log record.
- The backend token bucket bounds callers that bypass the frontend recorder.
- The existing tracing hub owns its ring-buffer and disk-rotation bounds.
- An unavailable backend, disabled logging level, or full log sink degrades to
  the console mirror without changing application behavior.

## Testing Strategy

### Frontend Unit Tests

Use a fake clock and sink to verify, in red-green order:

1. development enablement is on, test enablement is off, and the production
   local switch is explicit and failure-safe;
2. stage timestamps are captured before sink execution and elapsed time is
   monotonic;
3. tab and conversation indexes resolve one trace across components;
4. supersession and completion are idempotent;
5. timeout emits one terminal event and clears both indexes;
6. the 32-trace bound evicts/finishes the oldest trace; and
7. sink rejection does not reject or delay the caller.

Extend tab-store tests to prove the focus probe and tab commit emit ordered
stages while the existing no-mirror behavior remains unchanged. Add focused
component/hook tests for the cached-before-ready and ready-rendered selector
states without asserting user configuration values.

### Rust Tests

Test the shared validator/core for:

- accepted valid payloads;
- every enum and numeric bound;
- overlong identifiers;
- NaN/infinite/negative timings;
- non-positive conversation IDs; and
- rejection of arbitrary message/content fields in the request type; and
- token-bucket burst acceptance, refill, and over-limit behavior using a fake
  clock.

Run desktop and server checks because the command is exposed through both
transports.

### Manual/Automated Runtime Verification

Tests prove instrumentation contracts, not the original visual symptom. A real
Tauri/WebView switch and persisted trace are required before making a root-cause
claim.

## Risks and Mitigations

### Instrumentation Perturbs Timing

Capture time before a fire-and-forget call, keep payloads small, and limit the
stage count. Compare client elapsed values rather than backend arrival order.

### Cached Data Is Mistaken for the Root Cause

Record both cache presence and authoritative readiness at React commit. Do not
record or compare selected values.

### Focus Probe Is Mistaken for the Root Cause

Measure request-to-probe, probe duration, and probe-to-tab-commit separately.
A long total switch with a short probe remains unresolved rather than being
attributed to pop-out logic.

### Production Logging Remains Enabled Accidentally

Production defaults off and requires an exact browser-local value. The events
remain content-free even when enabled, and the switch can be removed locally
without a database/config migration.

## Acceptance Criteria

1. One main-window switch produces a bounded trace queryable from the existing
   logging API/file output.
2. A trace identifies the switch source, focus duration, tab/surface commit
   latency, detail duration, connection milestones, and selector cache/ready
   state without logging user content.
3. Development builds enable tracing automatically; tests and production builds
   do not unless explicitly enabled as designed.
4. Diagnostic transport failure does not change switch behavior or surface an
   application error.
5. At least one real Tauri switch is captured and analyzed, or the result is
   explicitly reported as insufficient reproduction evidence.
6. No visual-residual fix is implemented until the trace evidence selects a
   failing interval.
7. Focused tests, lint/type checks, and affected desktop/server Rust checks pass.
