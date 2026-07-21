# DrawCode Memory Diagnostics and ACP Teardown Hardening Design

Date: 2026-07-21

Status: Design approved in conversation; written-spec review pending

## Summary

Build an opt-in developer toolkit that can distinguish gradual retained-memory
growth from a sudden oversized allocation, external agent-process pressure,
and system-wide Windows commit exhaustion. At the same time, harden ACP
connection teardown so a connection is not reported as released until its
runtime, agent process tree, manager entry, and lifecycle worker have actually
converged.

The selected architecture combines:

- bounded Rust metrics and lifecycle instrumentation;
- a crash-resilient Rust allocator recorder;
- an idempotent ACP teardown supervisor;
- a lightweight PowerShell system/process-tree sampler;
- a Node CLI that controls collection and generates reports; and
- fully simulated Rust and script-level stress tests.

The toolkit is disabled by default. It adds no product UI, installs no system
software, changes no registry settings, and never invokes a real model during
stress tests.

## Incident Evidence and Limits

The 2026-07-21 crash occurred at approximately 17:41:21 after 7 hours and
7 minutes of runtime. Windows reported virtual-memory exhaustion approximately
four seconds before the crash. Disassembly of the faulting offset reached
`std::alloc::rust_oom` and the fail-fast sequence `mov ecx, 7; int 29h`.

This establishes that an allocation in the DrawCode process failed. It does
not establish any of the following:

- the size of the failed allocation;
- whether memory rose gradually or in one step;
- whether DrawCode, one of its agent descendants, or another system process
  consumed most of the system commit;
- whether the failed request was itself abnormal or was an ordinary request
  made after system commit was already exhausted; or
- which Rust call site requested the allocation.

Post-crash system inspection showed 127.4 GiB of physical RAM, a configured
15 GiB page file, and 11.9 GiB peak page-file usage. Those values are useful
context but are not a time series for the failed run.

The ACP log audit also showed that the observed count of 214 was a cumulative
spawn count, not a live-connection count:

| Connection purpose | Spawned | Confirmed disconnected | Present at log stop |
| --- | ---: | ---: | ---: |
| Delegated SubAgents | 81 | 79 | 2 |
| Auto-title work | 88 | 88 | 0 |
| Internal probe | 1 | 1 | 0 |
| Main conversations | 44 | 33 | 11 |

Peak delegated concurrency was 4. All 79 completed SubAgents had both a
disconnect request and a `Disconnected` confirmation in the inspected log.
Four targeted Rust cleanup tests passed. This is evidence that normal cleanup
exists and worked in those cases. It is not proof that every process, worker,
or allocation was released, and it does not explain the OOM.

The design therefore keeps two primary hypotheses open:

1. memory was retained gradually by a connection, cache, queue, worker, or
   child process; or
2. a bug requested an abnormally large allocation shortly before the crash.

External process pressure and system-wide commit exhaustion remain independent
third and fourth possibilities.

## Goals

1. Separate cumulative spawn counters from current live-resource gauges.
2. Record enough evidence to classify gradual retention, sudden allocation,
   external child pressure, system commit exhaustion, and cleanup failure.
3. Preserve the failed allocation size and a stack fingerprint even when the
   normal JSON writer cannot run during OOM.
4. Make ACP teardown idempotent, observable, bounded, and owned by one
   supervisor path.
5. Verify that completed SubAgents release runtime resources without waiting
   for the idle sweeper.
6. Follow DrawCode crashes and restarts within one diagnostic run.
7. Keep diagnostics bounded in memory, disk use, cardinality, and privacy.
8. Provide deterministic simulated stress tests that never call real models.
9. Keep the implementation reusable for later memory regressions.

## Non-Goals

- A product-facing diagnostics page or settings UI.
- Always-on production telemetry or remote upload.
- Installing ProcDump, WPR extensions, debuggers, profilers, or other tools.
- Modifying Windows registry dump settings or creating scheduled tasks.
- Capturing prompt text, response text, tool payloads, tokens, environment
  variables, or raw ACP traffic.
- Automatically killing a main conversation solely because it has remained in
  `Prompting` for a long time.
- Proving that every native, WebView, GPU, or third-party allocation belongs to
  a Rust call site.
- Treating allocator RSS or process Commit as an exact measure of logical Rust
  heap ownership.
- Making an OS-dependent memory-slope threshold a hard CI gate.
- Changing the user-visible semantics that allow a completed SubAgent result
  to remain available to its parent within the existing cache budget.

## Constraints and Defaults

- Rust diagnostics activate only when `CODEG_MEMORY_DIAGNOSTICS=1` is present
  before process startup.
- The default Rust snapshot interval is 30 seconds.
- The default Windows process/system sampling interval is 5 seconds.
- The default large-allocation threshold is 64 MiB.
- The default graceful ACP teardown timeout is 10 seconds.
- A teardown pending for more than 30 seconds is anomalous.
- A `Prompting` connection with no protocol activity for 15 minutes is
  suspected stuck, but is not automatically terminated on that basis alone.
- One diagnostic run has a default 512 MiB disk budget.
- The allocator emergency file is fixed at 4 MiB.
- The Rust diagnostic subsystem has an 8 MiB incremental-memory budget target.
- Windows is the first fully supported external-sampler and stack-capture
  platform. Rust lifecycle metrics and file formats remain portable.

All thresholds are recorded in the run manifest and may be overridden by the
developer CLI. There is no persistent product setting or database migration.

## Alternatives Considered

### External Sampler Only

A PowerShell sampler can observe Windows commit, DrawCode memory, and child
processes without changing application code. It is low risk and useful for
system attribution, but it cannot distinguish cumulative connection counts
from live internal objects, identify a stuck teardown phase, or preserve a
failed allocation that occurs between samples.

### Rust Instrumentation Only

Rust-only metrics can explain connection, broker, cache, queue, worker, and
allocator behavior. They cannot attribute memory to WebView/native components
or agent descendants reliably, and they lose system-wide commit context.

### Hybrid Toolkit

The selected design combines both sources and correlates them by run ID, PID,
process start time, UTC time, and monotonic elapsed time. It costs more
implementation work, but it is the only approach that covers both slow and
instantaneous failures while preserving system attribution.

## Architecture

```text
DrawCode process
  Rust memory diagnostics
    - internal gauges and counters
    - lifecycle events
    - allocator emergency ring
  ACP teardown supervisor
    - runtime ownership
    - process-tree ownership
    - finalization and retry

PowerShell sampler
  - Windows system commit
  - DrawCode and descendant process metrics
  - crash/restart boundaries

Node CLI
  - watch/launch orchestration
  - manifest and run ownership
  - decoding and correlation
  - classification and summary

Rust/Node test harness
  - fake ACP agents
  - bounded fake allocations
  - deterministic report fixtures
```

Each component has one primary responsibility:

- `memory_diagnostics` records sanitized, bounded facts from inside Rust.
- `ConnectionTeardownSupervisor` is the only owner allowed to finalize a
  connection and remove it from the registry.
- the PowerShell sampler observes the operating system without mutating it.
- the Node CLI owns the developer workflow and offline interpretation.
- simulated agents exercise behavior without network or model dependencies.

## Run and Resource Identity

Every diagnostic session has a random `run_id`. Every DrawCode process is
identified by `(pid, process_start_time)`, not PID alone. Every connection gets
a random run-local `connection_instance_id` that is independent of a persisted
conversation ID, title, path, or provider session ID.

Metrics use two explicit naming classes:

- `*_total` is monotonic for one process and never represents current use;
- `*_current` is a point-in-time live-resource gauge.

For example, a valid final state after a 1000-child churn test is:

```text
connections_spawned_total = 1000
connections_live_current = 0
teardown_pending_current = 0
child_processes_current = 0
lifecycle_workers_current = 0
```

Reports never display a `*_total` value under a heading such as "active" or
"live".

## ACP Teardown Ownership

### Current Gaps

Normal cleanup currently exists in `acp/connection.rs`, `acp/lifecycle.rs`,
`acp/delegation/broker.rs`, and the vendored `sacp-tokio` `ChildGuard`.
However, the current public manager disconnect paths remove or drain
`AgentConnection` entries before sending `ConnectionControl::Disconnect`.
Consequently, manager-map absence can precede event-loop and agent-process
exit.

The normal `disconnect()` API sends a best-effort control message and returns
without awaiting task or process-tree completion. A connection task abort can
also skip asynchronous cleanup placed after `run_connection().await`.
Completed result text remains parent-scoped by design, with a default 512 MiB
per-parent cap and a 256 KiB per-result cap. The idle sweeper reclaims only
idle `Connected` connections, so an abandoned `Prompting` connection can
remain indefinitely.

### Unified API

Every close source uses one idempotent operation:

```text
request_teardown(connection_id, reason) -> TeardownTicket
```

Close sources include explicit disconnect, SubAgent completion, cancellation,
parent closure, owner-window cleanup, auto-title completion, probe completion,
idle sweeping, failed bootstrap, and application shutdown.

The first request starts teardown. Later requests attach to the same ticket
and may wait for the same result. A connection marked `Disconnecting` rejects
new prompts and mutable commands but remains observable until finalization.

Ticket acceptance and resource release are separate operations:

```text
request_teardown(...) -> accepted ticket
await_released(ticket, deadline) -> Released | TeardownFailed | timeout
```

User-facing close commands may return after ticket acceptance to preserve UI
latency. Auto-title, probe, application shutdown, tests, and any caller that
must prove reclamation use `await_released`. In both cases the supervisor, not
the caller future, owns completion after acceptance.

Direct `connections.remove`, `connections.drain`, or equivalent removal is
forbidden outside the registry finalizer. A small registry abstraction and a
structural test enforce that invariant instead of relying only on convention.
The existing `ConnectionCleanupGuard` is changed from direct map removal to a
runtime-exit signal consumed by the supervisor, so panic safety does not bypass
the finalizer ordering.

### State Machine

```text
Active
  -> DisconnectRequested
  -> GracefulWaiting
  -> ForceAborting        (only on timeout/channel failure/runtime failure)
  -> Cleaning
  -> Released

Any force or cleanup failure
  -> TeardownFailed
  -> shared reaper retry
  -> Cleaning
  -> Released
```

The lifecycle records these ordered milestones when they occur:

```text
spawned
disconnect_requested
loop_exited
process_tree_exited
worker_removed
map_removed
```

Actual timestamps are retained even if a bug violates the order. The reporter
flags missing, duplicated, or reordered milestones rather than normalizing
them into a false success.

### Supervisor Behavior

1. Mark the registry entry `Disconnecting` and publish
   `disconnect_requested`.
2. Send `ConnectionControl::Disconnect` without holding the registry lock.
3. Wait up to the 10-second graceful timeout for runtime completion.
4. If the channel is full/closed, the runtime panics, or the timeout expires,
   abort the runtime task.
5. Dropping the vendored process guard invokes the existing tree-kill
   backstop. Extend the vendored boundary to expose the root PID, start
   identity, kill outcome, and exit completion needed for verification.
6. Confirm that the captured root and observed descendant identities have
   exited within a 5-second force-verification window. "Kill requested" is
   not equivalent to "process tree exited."
7. Run an independent finalizer that is not part of the abortable runtime
   future. It cancels continuation workers, revokes delegation leases/tokens,
   resolves broker state, cancels parent questions, releases terminal runtime
   resources, and emits a terminal lifecycle event if the runtime did not. The
   finalizer has one 10-second total budget; a timed-out action is recorded and
   cannot prevent later actions from being attempted.
8. Attempt every cleanup action even when an earlier action fails; collect all
   errors in the teardown outcome.
9. Wait for lifecycle-worker terminal acknowledgement through a direct
   supervisor acknowledgement path, then record `worker_removed`. This must
   not depend only on a broadcast event that can lag or be dropped.
10. Remove the manager entry only after runtime/process verification, required
    finalization, and lifecycle-worker acknowledgement.

If force termination, process verification, finalization, or worker
acknowledgement fails, retain a compact
`TeardownFailed` record with process identity, current stage, retry count, and
last error code. A single shared reaper retries these records. The design does
not leave one immortal retry task per failed connection and does not discard
the PID merely to make the active map look clean. The reaper wakes every
30 seconds and also on a new failed entry or observed process-exit signal.

### Connection-Type Policy

- A completed or cancelled SubAgent is torn down immediately after terminal
  result persistence. It does not wait for the idle sweep.
- Closing a parent cascades teardown to every still-owned child and drops that
  parent's completed-result cache.
- Auto-title and internal-probe callers await `Released`; failure to release
  makes the operation fail internally.
- Closing a tab does not kill a main conversation that is `Prompting`, waiting
  for permission, or has authoritative background work.
- Once an unobserved main conversation settles to idle `Connected`, its normal
  idle timer applies.
- Suspected stuck `Prompting` is diagnostic evidence only. Explicit user
  cancellation, parent ownership loss, application shutdown, or another
  authoritative terminal cause may still request teardown.
- Application shutdown requests all teardowns concurrently, waits one common
  grace window, then force-aborts and verifies remaining process trees.

### Runtime Resources Versus Result Cache

A completed result cache has a different lifetime from the ACP runtime. It
must not keep a connection classified as live, and it must not be described as
a connection leak.

```text
runtime:
  spawned -> ... -> worker_removed

result cache:
  result_cached -> read | evicted | parent_closed -> caches_dropped
```

The broker reports running, settling, and completed counts separately, along
with completed entry count, retained result bytes, configured cap, eviction
count, and parent age. Parent closure must drive retained result bytes for that
parent to zero.

## Internal Telemetry Schema

All records share:

```text
schema_version
run_id
pid
process_start_time_utc
timestamp_utc
monotonic_ms
sequence
record_type
```

### Connection Snapshot

Each live connection exposes sanitized dimensions:

- anonymous `connection_instance_id`;
- origin: root or Codeg child;
- purpose: main, delegated SubAgent, auto-title, probe, or other fixed enum;
- status: connecting, connected, prompting, disconnecting, error;
- age and time since last protocol activity;
- time in `Prompting`;
- whether a close was requested;
- teardown stage and stage age; and
- child root PID when available.

Aggregates group live counts by origin, purpose, and status. High-cardinality
values are records, not metric labels.

### Required Counters and Gauges

- spawned, disconnect-requested, released, force-aborted, and failed teardown
  totals;
- live connections and pending teardowns by fixed category;
- oldest pending teardown and oldest inactive `Prompting` age;
- broker running, settling, and completed counts;
- completed-cache entries, retained bytes, cap, and evictions;
- continuation and persistence-retry worker counts;
- lifecycle worker count;
- internal event-bus lane length/capacity and drop totals;
- lifecycle worker queue length/capacity and drop totals; and
- Rust requested-heap current, peak, allocation, reallocation, and deallocation
  totals when allocator diagnostics are enabled.

### Lifecycle Events

Lifecycle events contain only enum reason codes, anonymous identities,
timestamps, stage values, numeric sizes, and process identities. They never
contain prompt text, result text, token values, environment entries, command
lines, conversation titles, workspace paths, or raw protocol payloads.

## Allocator Emergency Recorder

### Global Allocator Wrapper

The crate installs a thin wrapper over the existing system allocator.
Enablement is latched on the allocator's first entry using an allocation-free
environment lookup and cannot change during the process lifetime. This avoids
subtracting allocations that predate a later enable transition. When
diagnostics are disabled, subsequent calls take a single fast disabled branch
and perform no per-allocation atomic accounting or file work.

When enabled before runtime startup, successful operations update atomic
logical requested-byte counters:

- `alloc` and `alloc_zeroed` add the requested layout size after success;
- `dealloc` subtracts the original layout size;
- successful `realloc` applies the old-to-new delta; and
- failed operations do not alter live bytes.

These counters measure Rust allocator request sizes, not allocator metadata,
fragmentation, native allocations, RSS, or Windows Commit. Accounting begins
at the first allocator entry. Before the emergency ring reaches `Ready`, large
or failed slot capture increments a fixed pre-initialization loss counter; it
must not attempt lazy file creation from inside the allocator.

### Large and Failed Allocation Slots

The per-process `allocator-<pid>.bin` file is created, sized to 4 MiB, memory
mapped, and pre-touched during diagnostics startup. It is a fixed ring. Full
rings overwrite the oldest complete slot and never grow.

A slot is written for:

- every successful allocation or reallocation at or above 64 MiB; and
- every allocation or reallocation that returns null, regardless of size.

Each fixed slot contains:

- format version, sequence, committed marker, length, and checksum;
- UTC and monotonic timestamps;
- operation kind;
- requested size, alignment, and old size for reallocation;
- thread ID;
- logical Rust heap current and peak byte counters; and
- up to 32 raw stack instruction addresses.

The write path uses no Rust heap allocation, formatted logging, mutex, or
normal JSON writer. A thread-local recursion guard prevents the recorder from
recursing if a platform primitive unexpectedly allocates. The committed marker
is written last so post-crash decoding ignores torn slots.

On Windows, stack addresses are captured with a no-heap platform primitive.
The offline report records the executable/module build identity and resolves
addresses against matching symbols when available. Missing PDBs reduce call
site detail but do not invalidate allocation size or timing evidence. Other
platforms may emit zero stack frames until an equally safe implementation is
provided.

The emergency record is best effort under catastrophic system failure. The
preallocated and pre-touched ring substantially improves survival, but the
report must not claim that absence of a slot proves absence of a sudden
allocation.

## External Windows Sampler

The PowerShell sampler streams one sample at a time to disk and does not retain
the run history in PowerShell objects.

### System Fields

- physical memory total and available;
- system committed bytes and commit limit;
- commit headroom;
- page-file current and peak usage when exposed by Windows; and
- sampler timestamp, duration, and gap status.

### Process Fields

For DrawCode and every observed descendant:

- PID, parent PID, and process start time;
- sanitized executable role/name;
- Working Set;
- Private Bytes;
- Commit Size or the closest explicitly named Windows counter;
- handle count;
- thread count;
- cumulative CPU time; and
- whether the process is the DrawCode root or a descendant.

The sampler does not collect command lines or environment blocks. PID reuse is
handled through start time. A process disappearing between enumeration and
counter read produces a typed gap, not a fabricated zero.

The default interval is 5 seconds. The watcher remains alive if DrawCode
crashes, emits a process-boundary event, and waits for the next matching start
within the same run. It performs no installation, registry write, service
creation, or scheduled-task creation.

## Developer CLI

Add one package entry backed by a Node script:

```powershell
pnpm memory:diag watch
pnpm memory:diag watch --launch <DrawCode startup command>
pnpm memory:diag report latest
pnpm memory:diag stress --scenario teardown
```

### Watch

- `watch` runs in the foreground and ends cleanly on `Ctrl+C`.
- `--launch` creates a run ID and launches the supplied command with
  `CODEG_MEMORY_DIAGNOSTICS=1`, `CODEG_MEMORY_DIAGNOSTICS_RUN_ID`, and
  `CODEG_MEMORY_DIAGNOSTICS_DIR`.
- Without `--launch`, the watcher can observe an existing or later DrawCode
  process externally. It clearly reports that an already-running process
  cannot be retrofitted with Rust allocator/internal diagnostics.
- The watcher survives DrawCode exit so a developer can reproduce restart
  behavior in one run.
- A lock contains watcher PID and watcher process start time. It prevents two
  writers from owning one run without killing an unrelated PID-reuse victim.
- `Ctrl+C` closes writers, records the stop reason, and generates a report.

### Report

`report` is offline and cross-platform. It validates file versions, decodes
allocator slots, merges per-PID lifecycle parts, correlates all sources, prints
a concise console summary, and writes `summary.json`.

Findings are not necessarily mutually exclusive. The report chooses a primary
classification from the strongest evidence and lists secondary findings.
Every finding contains:

```text
classification
confidence: high | medium | low
evidence[]
missing_evidence[]
time_range
affected_processes[]
```

Default findings are:

- `gradual_retention`: a sustained positive DrawCode/tree Commit or logical
  Rust-heap trend across at least ten valid samples and five minutes, with a
  total rise of at least 512 MiB and 10 percent of the starting value, while
  the largest positive sample interval explains less than 50 percent of the
  total rise;
- `sudden_allocation`: a successful or failed allocation request of at least
  64 MiB, a failed request consuming at least 20 percent of commit headroom in
  the preceding valid system sample, or a process Commit jump of at least
  512 MiB and 20 percent within one sample interval;
- `external_process_pressure`: descendants account for at least 60 percent of
  the DrawCode tree's positive Commit growth while the root accounts for no
  more than 40 percent;
- `system_commit_exhaustion`: commit headroom reaches the smaller of 2 GiB or
  5 percent of the commit limit and the DrawCode tree accounts for less than
  50 percent of the system committed-byte increase in the pressure window;
- `cleanup_leak`: teardown remains pending for more than 30 seconds, milestone
  ordering fails, or a manager/worker/process/cache resource remains after its
  authoritative release point; and
- `insufficient_evidence`: no supported finding reaches its evidence floor or
  the required interval is missing.

Every failed allocator request is reported as an `allocation_failure` evidence
event, including small requests. A small failed request under exhausted system
commit does not by itself imply `sudden_allocation`.

Thresholds are versioned heuristics, not proof. A directly recorded oversized
failed request and teardown-milestone evidence can produce high confidence.
Timing correlation is normally medium confidence, and slope-only inference is
low confidence unless supported by internal live-byte/object growth.

Additional anomaly codes include:

- `prompting_suspected_stuck` after 15 minutes without protocol activity;
- `orphan_process` when manager/runtime ownership is gone before process exit;
- `stale_runtime_state` when the process is gone but manager or worker state
  remains;
- `telemetry_loss` whenever a bounded queue or sampler loses records; and
- `cache_pressure` when retained bytes reach 80 percent of the configured cap
  or at least three evictions occur within five minutes.

The console and JSON summaries include peak commit, peak process, fastest
growth window, live connection peak, cumulative spawn count, unfinished
teardowns, broker/cache peaks, event drops, and the largest allocator events.

### Stress

`stress` runs only repository-controlled fake agents and test processes. It
never discovers credentials, sends prompts to a provider, or invokes a real
model binary.

Supported initial scenarios are:

- `teardown`: high connection churn plus normal, duplicate, and timed-out
  closes;
- `cache`: bounded completed-result insertion, eviction, and parent drop;
- `burst`: a small controlled allocation with a lowered diagnostic threshold,
  plus synthetic failed-slot fixtures;
- `restart`: fake DrawCode root exit/restart and descendant attribution; and
- `mixed`: combined lifecycle and sampling activity.

## Diagnostic Files

The stable run output is:

```text
~/.codeg/diagnostics/memory/<run-id>/
  manifest.json
  system.csv
  processes.csv
  lifecycle.jsonl
  internal-<pid>.jsonl
  allocator-<pid>.bin
  summary.json
```

Per-PID and rotated runtime parts may live under a run-local `.parts/`
directory. The reporter is the only component that merges those parts into
the stable `lifecycle.jsonl`; multiple DrawCode processes never append to one
shared JSONL file concurrently.

File ownership is:

| File | Writer | Purpose |
| --- | --- | --- |
| `manifest.json` | Node CLI | Versions, build IDs, thresholds, run boundaries |
| `system.csv` | PowerShell | Windows physical/commit/page-file samples |
| `processes.csv` | PowerShell | Root and descendant process samples |
| `internal-<pid>.jsonl` | Rust | Periodic counters, gauges, and writer health |
| `.parts/lifecycle-*` | Rust | Per-process sanitized lifecycle events |
| `allocator-<pid>.bin` | Rust allocator | Fixed emergency allocation ring |
| `lifecycle.jsonl` | Node report | Validated, merged lifecycle stream |
| `summary.json` | Node report | Findings, confidence, evidence, and gaps |

`manifest.json` contains diagnostic format/configuration, application version,
build identity, process identities, and start/stop reasons. It does not contain
the environment, command line, workspace path, prompt, result, or credentials.

## Safety, Backpressure, and Failure Handling

### Memory and Queue Bounds

- Internal producers use separate bounded critical and ordinary lanes for
  fixed-shape sanitized records.
- Producers never wait for disk I/O.
- On pressure, periodic snapshots are dropped before teardown/anomaly records.
  If the critical lane also fills, the event is dropped and counted rather
  than blocking application work.
- Drop counts are atomic and appear in later successful snapshots.
- Allocator emergency events bypass the normal queue into the fixed ring.
- Dynamic user content is not accepted as a telemetry label or payload.
- The enabled subsystem targets no more than 8 MiB incremental resident
  diagnostic state, excluding normal OS file cache behavior.

### Disk Bound

One run defaults to 512 MiB. CSV and JSONL files rotate into fixed-size parts.
At the cap, the collector removes the oldest ordinary sample parts from the
current run while retaining the manifest, allocator rings, anomalies, and the
newest time window. The manifest/report records `data_rotated=true` and the
lost time range.

Historical runs are never removed implicitly. Any future cleanup command must
be explicit and limited to this diagnostics directory.

### Writer Failure

An unwritable directory, disk-full response, serialization error, sampler
exception, or writer panic changes diagnostics to `Degraded`; it does not
terminate DrawCode or alter ACP behavior. Rust prints at most one short stderr
warning so an error loop cannot create unbounded logs. The external watcher
records a gap and retries.

Every logical record carries UTC and monotonic time. The reporter ignores a
partial trailing CSV/JSONL record. Allocator slots use a committed marker and
checksum. Missing or corrupt evidence lowers confidence and is never converted
to a zero-valued sample.

## Testing Strategy

### Rust Unit Tests

- counter versus gauge behavior and grouping;
- lifecycle milestone validation;
- teardown idempotence and concurrent callers;
- full/closed control channel behavior;
- graceful timeout and force-abort transition;
- runtime panic followed by independent finalization;
- cleanup continuation after an individual finalizer failure;
- `TeardownFailed` shared-reaper retry and eventual release;
- lifecycle-worker terminal acknowledgement;
- broker running/settling/completed and byte-accounting accuracy;
- parent cache drop and configured-cap eviction;
- allocator slot encoding, wraparound, committed marker, checksum, and decode;
- allocator accounting for successful/failed realloc semantics; and
- diagnostic queue priority/drop accounting and disabled fast path.

Tests use paused Tokio time where possible. They do not wait for production
timeouts.

### Simulated Integration Tests

A fake ACP executable supports deterministic modes:

- clean disconnect;
- ignore disconnect until killed;
- panic/exit during a turn;
- remain `Prompting`;
- spawn a harmless descendant;
- complete a child result; and
- allocate a bounded test block.

The main churn case creates 1000 simulated SubAgents with peak concurrency 4.
After terminal settlement and teardown it asserts:

```text
connections_live_current == baseline
teardown_pending_current == 0
child_processes_current == 0
lifecycle_workers_current == baseline
broker_running_current == 0
broker_settling_current == 0
```

It separately verifies that cumulative spawned remains 1000, completed cache
stays within its cap while the parent lives, and parent closure returns that
parent's retained cache bytes to zero.

### Node and PowerShell Tests

- watcher process start, crash, PID reuse defense, and restart segmentation;
- process-tree aggregation and disappearing-process gaps;
- all classification fixtures and confidence rules;
- partial JSONL/CSV, corrupt/torn allocator slots, and unknown schema versions;
- rotation and explicit missing-time reporting;
- disk/writer failure degradation;
- stable console/JSON distinction between cumulative and live values; and
- an output-schema allowlist that rejects prohibited content fields.

PowerShell integration tests run only on Windows and use built-in facilities.
Node report tests remain cross-platform.

### CI Gates

CI gates only deterministic invariants:

- every close entry point routes through the teardown supervisor;
- only the finalizer can remove a connection registry entry;
- all simulated runtime resources return to baseline within virtual/test
  deadlines;
- process and worker ownership is not forgotten on forced failure;
- cache, queue, allocator ring, and disk-segment bounds hold;
- report fixtures parse and classify consistently; and
- the privacy schema allowlist passes.

RSS, Private Bytes, and Commit slope are reported by local stress runs but are
not hard CI thresholds. Allocator retention, OS scheduling, and parallel test
noise make such thresholds flaky. A local stress command still exits nonzero
for object/process/worker leaks, cap violations, corrupt required records, or
failed teardown invariants.

Repository verification includes the relevant frontend Node tests and the
desktop, server, and `codeg-mcp` Rust check/test/clippy commands documented in
`AGENTS.md`. Windows-only process-tree and raw-stack tests run on Windows.

## Proposed Code Boundaries

The implementation plan may refine filenames, but ownership should remain:

- `src-tauri/src/memory_diagnostics/`
  - enablement and run coordination;
  - fixed telemetry schema;
  - bounded writer;
  - allocator wrapper/ring;
  - snapshots and health.
- `src-tauri/src/acp/manager.rs` and a focused teardown/registry module
  - unified teardown API and registry ownership.
- `src-tauri/src/acp/connection.rs`
  - runtime exit handoff and sanitized lifecycle events.
- `src-tauri/src/acp/lifecycle.rs`
  - worker gauges and terminal acknowledgement.
- `src-tauri/src/acp/delegation/broker.rs`
  - running/settling/completed/cache byte snapshots.
- `src-tauri/vendor/sacp-tokio/src/acp_agent.rs`
  - process identity and exit/kill receipt at the existing process owner.
- `scripts/memory-diagnostics.mjs`
  - CLI, report, correlation, and classification.
- `scripts/memory-diagnostics.ps1`
  - Windows sampler.
- focused Rust and Node tests plus fake-process fixtures.

No frontend component, HTTP/Tauri command, SeaORM entity, or database migration
is required.

## Rollout and Compatibility

1. Land schemas, decoder fixtures, and disabled-path tests first.
2. Introduce the connection registry/teardown supervisor behind existing
   public manager method signatures where compatibility permits.
3. Route all close paths through the supervisor and enforce structural tests.
4. Add internal lifecycle/broker/worker metrics.
5. Add the allocator ring with failure-injection tests before enabling stack
   capture.
6. Add the PowerShell watcher and Node report workflow.
7. Run deterministic churn tests, then a long local simulated stress run.

Diagnostics remain off unless explicitly activated. Existing product APIs and
frontend behavior should remain unchanged except that disconnect completion is
now truthful and cleanup is bounded. Long `Prompting` main conversations are
not automatically killed by the diagnostic feature.

## Risks and Mitigations

### Allocator Instrumentation Recursion or Overhead

Use a disabled fast path, fixed records, atomic counters only while enabled,
and a recursion guard. Test the wrapper independently before stack capture is
enabled. Treat the 8 MiB budget and local overhead measurements as release
criteria.

### Process-Tree Verification Races

Use PID plus start time, capture process ownership at spawn, and retain a
failed teardown record until exit is verified. Never equate manager removal or
kill request with process exit.

### Missing Symbols

Keep raw addresses and module build identity. Symbolization is best effort;
allocation size/timing and OS process evidence remain useful without PDBs.

### Diagnostic Data Loss During OOM

Preallocate and pre-touch the allocator ring, make normal files append-only,
flush bounded batches, validate committed slots, and report missing evidence
explicitly. Do not claim perfect crash durability.

### False Classification

Emit evidence and confidence, permit multiple findings, version all heuristics,
and use `insufficient_evidence` instead of forcing a root cause.

### Teardown Behavior Regression

Preserve main-conversation background semantics, separate runtime release from
result-cache retention, use fake-agent integration tests for every close
source, and verify desktop/server/`codeg-mcp` builds.

## Acceptance Criteria

The work is complete when:

1. a developer can start one opt-in watcher, reproduce across DrawCode
   restarts, stop it, and receive a validated report;
2. reports distinguish cumulative spawns from current live resources;
3. after the emergency ring reaches `Ready`, a failed or large Rust allocation
   leaves a decodable size/timing record and raw stack when the platform
   supports it;
4. the report can attribute tree growth to the DrawCode root, a descendant, or
   wider system pressure without claiming unsupported certainty;
5. all ACP close paths share one idempotent teardown supervisor;
6. manager-map absence means the runtime and tracked process tree have exited,
   required finalization has run, and the lifecycle worker has acknowledged
   removal;
7. 1000 simulated SubAgents at peak concurrency 4 return connection, process,
   teardown, broker-running, and worker gauges to baseline;
8. parent closure drops its completed-result cache while normal result lookup
   remains available before parent closure;
9. diagnostics stay within declared queue, allocator, memory, and disk bounds;
10. disabled diagnostics create no files and do not change product-visible
    behavior; and
11. all required repository checks pass without invoking a real model.
