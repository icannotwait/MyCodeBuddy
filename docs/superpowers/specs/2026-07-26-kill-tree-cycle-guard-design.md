# Kill Tree Cycle Guard Design

## Goal

Prevent DrawCode from exhausting memory when process-tree cleanup observes a
cyclic Windows parent-process graph.

## Root Cause

DrawCode uses `kill_tree` 0.2.4 for ACP child cleanup, terminal cleanup, and
OfficeCLI cleanup. Its shared breadth-first traversal records every dequeued
PID but does not track which PIDs were already visited. Windows process IDs can
be reused after a parent exits, so a process snapshot can contain a multi-node
parent cycle even though the original creation graph was acyclic.

The captured crash followed this path:

```text
sacp_tokio::ChildGuard::drop
  -> kill_tree::blocking::kill_tree
  -> kill_tree::common::kill_tree_internal
  -> RawVec<u32>::grow_one
  -> rust_oom
  -> abort
```

The traversal had accumulated 34,359,738,368 PIDs (128 GiB) and aborted when
the next vector growth requested 256 GiB.

## Approved Scope

Apply one repository-owned fix to the `kill_tree` dependency so every existing
caller receives cycle protection:

- vendored `sacp-tokio` ACP child cleanup;
- ACP terminal cleanup;
- OfficeCLI timeout and failure cleanup;
- any future call through the same dependency.

Do not change public process-management APIs, child launch behavior, kill
signals, logging contracts, or frontend behavior.

## Options Considered

### Vendor and Patch `kill_tree` (Selected)

Vendor the exact 0.2.4 crate under `src-tauri/vendor/kill_tree`, add a Cargo
`[patch.crates-io]` entry, and fix the shared traversal. This keeps all callers
on one implementation, provides a direct unit-test boundary, and makes builds
reproducible without waiting for an upstream release.

### Add a DrawCode Process-Tree Implementation

Replace each call with a new application helper. This avoids vendoring but
duplicates platform-specific behavior and requires edits across DrawCode and
vendored `sacp-tokio`.

### Use `taskkill /T` on Windows

Delegate Windows cleanup to an external command. This is smaller in source
volume but changes error, timing, and deployment behavior and does not provide
one cross-platform contract.

## Design

Copy the published `kill_tree` 0.2.4 source and metadata without unrelated
changes. Point Cargo's crates.io patch table at that local crate, causing both
DrawCode's direct dependency and `sacp-tokio`'s dependency to resolve to the
same patched package. Cargo must compile only the local replacement, not a
second copy of the registry crate.

In `get_process_ids_to_kill`, maintain a `HashSet<ProcessId>` beside the queue.
After dequeuing a PID, insert it into the set. If it was already present, skip
both output insertion and child expansion. First visits retain the existing
breadth-first order, so reversing the result still kills descendants before
their first-seen ancestors. The result size becomes bounded by the number of
unique PIDs in the snapshot.

The guard belongs in the common traversal rather than the Windows snapshot
reader. This protects all platforms from malformed or synthetic cyclic input
and ensures every current `kill_tree` entry point shares the same invariant.

## Error Handling

Cycle detection is normal control flow and does not create a new public error.
Existing snapshot, process-open, termination, and handle-close errors retain
their current behavior. Duplicate PIDs are ignored after their first visit and
are never killed twice.

## Testing Strategy

Implementation follows red-green TDD:

1. Add a two-node cycle test and verify the unpatched traversal does not
   terminate within a bounded external test timeout.
2. Add the visited guard and verify the cycle returns each PID exactly once.
3. Add a converging-path test to prove duplicate reachability also yields one
   kill entry per PID without changing first-visit order.
4. Keep the existing ordinary-tree ordering test green.

After focused tests, run the repository's Rust checks for desktop, server, and
`codeg-mcp`, including Clippy with warnings denied. No frontend checks are
required because this change has no frontend surface.

## Risks and Mitigations

- Vendoring adds source files to the repository, but the behavioral diff stays
  limited to cycle detection and tests.
- A local fork can drift from upstream. Pin it to 0.2.4, preserve its metadata,
  and keep the change isolated so a future fixed release can replace it.
- Process ordering could regress. Existing ordering tests plus explicit
  first-visit assertions protect the current breadth-first/reverse-kill
  contract.

## Non-Goals

- Replacing `kill_tree` with a new process-management subsystem.
- Changing process launch ownership or adopting Windows Job Objects.
- Submitting or merging an upstream pull request as part of this change.
- Recovering the exact historical PID cycle from the minidump.

## Acceptance Criteria

1. Cyclic process graphs terminate in time proportional to unique PIDs.
2. Each reachable PID appears at most once in the kill order.
3. Acyclic traversal order remains unchanged.
4. All current DrawCode and vendored `sacp-tokio` users resolve to the patched
   local `kill_tree` crate.
5. Focused tests and required desktop, server, and MCP Rust checks pass.
