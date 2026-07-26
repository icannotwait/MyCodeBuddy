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

### Provenance and vendoring

1. Download the published crates.io `kill_tree` 0.2.4 `.crate` archive.
2. Verify its checksum against the lockfile pin (current
   `Cargo.lock` records sha256 `f3879339…` for registry `kill_tree` 0.2.4).
3. Extract source and package metadata into `src-tauri/vendor/kill_tree`
   without unrelated edits (keep `Cargo.toml` / package metadata, `build.rs`,
   `src/`, and package tests; drop cache noise such as `.cargo-ok`).
4. Prefer a two-commit structure when practical: (a) verbatim import of the
   verified 0.2.4 tree, (b) cycle-guard + tests only — so the behavioral diff
   is reviewable.

### Cargo resolution and pinning

- Add `[patch.crates-io] kill_tree = { path = "vendor/kill_tree" }` next to the
  existing `sacp-tokio` path patch so both DrawCode's direct dependency and
  vendored `sacp-tokio`'s `kill_tree = "0.2"` resolve to **one** local package.
- Pin DrawCode's direct dependency to exact `=0.2.4` (with the existing
  `tokio` feature) so a later crates.io 0.2.x cannot supersede the vendored
  path after lockfile regeneration.
- After the patch, regenerate/commit `Cargo.lock` and verify with
  `cargo tree -i kill_tree` that exactly one `kill_tree` package appears and
  its source is the path crate (not a second registry copy).
- `THIRD_PARTY_LICENSES.txt` already lists `cargo:kill_tree@0.2.4`; keep that
  entry unchanged after path-patching (crate identity and version stay 0.2.4).
- The existing `kill_tree=warn` logging clamp remains valid because the crate
  name is unchanged.

### Visited-set algorithm

In `get_process_ids_to_kill`, maintain a `HashSet<ProcessId>` beside the queue.

**Preferred marking: on enqueue (and at the initial seed).** When a PID is
about to be enqueued, insert it into the set first; if it was already present,
skip the enqueue. Seed/mark the target the same way before the loop so it is
expanded exactly once.

On dequeue, preserve existing `Config::include_target` output semantics:

- append the dequeued PID to the kill list only when it is **not** the target,
  or when `include_target` is true;
- always expand its children once (subject to the enqueue-time visited filter).

The HashSet controls first-visit enqueue only; it must not force the target into
the output when `include_target = false` (upstream default is `true`; production
callers use defaults, but the public config must keep working).

This bounds both the result vector **and** the queue to unique PIDs, so
traversal time and memory are `O(unique PIDs + edges examined)` and, under the
one-parent process-snapshot model, proportional to unique PIDs in the snapshot.
First-visit order remains ordinary BFS; reversing the result still kills
descendants before their first-seen ancestors on acyclic trees.

**Cyclic graphs:** reverse-first-discovery order is deterministic but is not a
true "descendants-first" guarantee for every edge inside a cycle (undefined
ancestry). Cycle members are still each killed at most once. This is
best-effort cleanup and strictly better than unbounded growth / OOM.

The guard belongs in the common traversal rather than the Windows snapshot
reader. This protects all platforms from malformed or synthetic cyclic input
and ensures every current `kill_tree` entry point shares the same invariant.

**Residual product risk (explicit):** the guard prevents traversal OOM and
double-kill within one pass. It does **not** fully correct kill-set membership
under Windows PID reuse between snapshot and termination (a reused PID linked
as a child can still be killed on first visit). Job Objects / process-group
ownership remain out of scope.

## Error Handling

Cycle detection is normal control flow and does not create a new public error.
Existing snapshot, process-open, termination, and handle-close errors retain
their current behavior. Duplicate PIDs are ignored after their first visit and
are never killed twice.

## Testing Strategy

Implementation follows red-green TDD **inside the vendored crate**
(`src-tauri/vendor/kill_tree`, in-crate tests — `get_process_ids_to_kill` is
`pub(crate)`, not reachable from the `codeg` package's `tests/`):

1. **Ephemeral local red proof only (do not merge a hang/OOM test):** optionally
   demonstrate that the unpatched traversal is unbounded on a two-node cycle
   via a disposable, memory-limited subprocess that is hard-killed on timeout.
   Do **not** commit a permanent `cargo test` that hangs or can OOM if the
   guard is later reverted. Prefer static review of the unpatched algorithm
   plus green tests after the guard lands.
2. Add the visited guard and a permanent two-node cycle test: returns each PID
   exactly once and terminates promptly.
3. Add a converging-path (diamond) test: duplicate reachability yields one kill
   entry per PID without changing first-visit BFS order.
4. Keep the existing ordinary-tree ordering test green; reverse-kill contract
   remains descendants-before-ancestors for acyclic trees.
5. **Required** permanent cases: self-loop and `include_target = false`
   (target marked/seeded once, **omitted** from kill order when
   `include_target` is false; children expanded once; re-visits skip).

### Required commands (vendored crate is not a workspace member)

`src-tauri` is not a Cargo workspace that members-include path deps under
`vendor/`. Root `cargo test` / `cargo clippy` compile the patched crate but
**do not** execute its unit tests or subject them to `-D warnings` unless
invoked against the vendor manifest.

**Required focused gates** exercise only the cycle-guard surface. Do **not**
require full upstream `cargo test --all-features` (locale-sensitive English
Win32 asserts; integration tests spawn real `node` processes) or
`clippy --all-targets` (nightly-only `benches/bench.rs` with
`#![feature(test)]`). After the change, always run:

```text
# Focused kill_tree algorithm unit tests (lib only, name filter)
cargo test --manifest-path vendor/kill_tree/Cargo.toml --lib --all-features get_process_ids_to_kill

# Focused kill_tree Clippy (lib + tests; not benches)
cargo clippy --manifest-path vendor/kill_tree/Cargo.toml --lib --tests --all-features -- -D warnings

# Repository gates (desktop / server / codeg-mcp) from src-tauri/
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

No frontend checks are required because this change has no frontend surface.

## Risks and Mitigations

- Vendoring adds source files to the repository, but the behavioral diff stays
  limited to cycle detection and tests (aided by checksum-verified import +
  optional two-commit structure).
- A local fork can drift from upstream. Pin exact `=0.2.4`, preserve metadata,
  and keep the change isolated so a future fixed release can replace it.
- Process ordering could regress. Existing ordering tests plus explicit
  first-visit assertions protect the current breadth-first/reverse-kill
  contract on acyclic trees.
- PID-reuse kill-set incorrectness remains possible; this change only bounds
  traversal and deduplicates kills within one snapshot pass.

## Non-Goals

- Replacing `kill_tree` with a new process-management subsystem.
- Changing process launch ownership or adopting Windows Job Objects.
- Submitting or merging an upstream pull request as part of this change.
- Recovering the exact historical PID cycle from the minidump.
- Fully correcting kill-set membership under cross-process PID reuse.

## Acceptance Criteria

1. Cyclic process graphs terminate promptly; result and queue membership are
   bounded by unique PIDs (enqueue-time visitation).
2. Each reachable PID appears at most once in the kill order.
3. Acyclic traversal order remains unchanged (first-visit BFS; reverse kill
   still descendants-before-ancestors on trees).
4. All current DrawCode and vendored `sacp-tokio` users resolve to exactly one
   path-sourced `kill_tree` 0.2.4 package (`cargo tree -i kill_tree`).
5. Direct dependency is pinned to `=0.2.4`; `Cargo.lock` updated accordingly.
6. Focused vendored gates pass:
   `cargo test --manifest-path vendor/kill_tree/Cargo.toml --lib --all-features get_process_ids_to_kill`
   and
   `cargo clippy --manifest-path vendor/kill_tree/Cargo.toml --lib --tests --all-features -- -D warnings`;
   required desktop, server, and MCP Rust checks pass.
7. No permanent hang/OOM regression test is committed; unpatched cycle BFS is
   not executed as a red test.
