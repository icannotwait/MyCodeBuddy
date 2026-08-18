# Task 3 Report: Generic Simple Task Route Reconciliation

## Status

DONE_WITH_CONCERNS

Commit: `d63a2951 feat(workflow): reconcile Simple task routes`

## Implementation

- Added the private `SimpleExpectedRoute` projection model.
- Added deterministic `derive_simple_expected_route` validation for:
  - routing schema and risk policy;
  - unique/matching Task index and referenced Task Agent generation;
  - effective generation selection;
  - exact ordered normal/high reviewer slot counts;
  - canonical normal and high route shapes;
  - recorded Agent/profile agreement with the referenced generation;
  - canonical work-unit key construction through `build_work_unit_key`.
- Added bounded, deduplicated `reconcile_simple_progress_route` warnings for all
  eight required route/progress disagreement cases.
- Added exact `run_matches_work_unit_key` matching for durable runs. Generic
  role and Agent equality are not used as route identity.
- Integrated warning-only reconciliation into Simple projection. Invalid
  routing adds `simple_plan_routing_invalid` and leaves the existing aggregate
  Task projection in place; no Gate or admission error is introduced.
- Detects durable/progress runs outside the expected route and detects a
  non-null child conversation reused by two distinct expected work-unit keys.

## Route Fixtures

- Normal Grok:
  - `task|1|implementer|grok|none`
  - `task|1|reviewer|primary|codex|none`
- Normal custom Agent/profile:
  - `task|2|implementer|custom:goose|fast`
  - `task|2|reviewer|primary|codex|none`
- High Grok:
  - `task|3|implementer|codex|none`
  - `task|3|reviewer|primary|codex|none`
  - `task|3|reviewer|auxiliary|grok|none`
- High Task Agent Codex preserves three distinct role/slot keys:
  - `task|4|implementer|codex|none`
  - `task|4|reviewer|primary|codex|none`
  - `task|4|reviewer|auxiliary|codex|none`

## Reconciliation Warnings

Covered and deduplicated:

- `simple_progress_risk_level_mismatch`
- `simple_progress_task_agent_generation_mismatch`
- `simple_progress_implementer_key_mismatch`
- `simple_progress_primary_reviewer_key_mismatch`
- `simple_progress_auxiliary_reviewer_key_mismatch`
- `simple_progress_expected_route_missing`
- `simple_progress_run_outside_expected_route`
- `simple_progress_route_child_not_independent`

## TDD Evidence

### RED

Initial brief command, before the later server-only binding instruction:

```text
cargo test --lib --features test-utils simple_projection_route -- --nocapture
```

Result: failed to compile with the expected missing
`SimpleExpectedRoute`, `derive_simple_expected_route`,
`reconcile_simple_progress_route`, and `run_matches_work_unit_key` symbols.
This established the initial RED state before production implementation.

After self-review identified that ordered reviewer slots must also be rejected,
the server-only focused test was run before the order validation was added:

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_route_rejects_ambiguous_or_untrusted_shapes -- --nocapture
```

Result: failed 0 passed / 1 failed because reversed high reviewer slots were
incorrectly accepted. The order validation was then implemented.

### GREEN

Final permitted focused commands:

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_route -- --nocapture
```

Result: PASS, 3 passed / 0 failed / 4599 filtered out.

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_warns -- --nocapture
```

Result: PASS, 3 passed / 0 failed / 4599 filtered out. This includes the
pre-existing legacy stale-progress warning projection test.

Formatting and diff checks:

```text
cargo fmt --check -- src/acp/delegation/workflow/project.rs
git diff --check
git diff --cached --check
```

Result: PASS with no output.

## Binding Test Constraint And Interrupted Runs

The binding user instruction changed Rust verification to server-only:

```text
cargo test --no-default-features --features server,test-utils ...
```

No default-feature Rust command was started after that instruction. One
already-running default-feature warning filter was stopped immediately with
exit 130; it had reported two passing tests before interruption and is not
counted as GREEN evidence.

The first server-only attempt ended with `No space left on device` while
writing Cargo's incremental query cache. `cargo clean` removed generated Cargo
build output, after which both required server-only filters passed. No source
or user data was removed.

## Files Changed

Committed:

- `src-tauri/src/acp/delegation/workflow/project.rs`

Uncommitted by requirement:

- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-report.md`

## Self-Review

- Compared every required route and warning code against the Task 3 brief.
- Confirmed high Codex identities remain distinct through complete keys.
- Confirmed warning insertion uses the existing 64-code bounded/deduplicated
  projection helper.
- Confirmed invalid routing stays non-blocking and retains legacy aggregate
  projection behavior.
- Confirmed no Plan/progress mutation, Agent fallback, Gate creation, database
  schema change, or admission authority was added.
- Mutation check: changing risk branching, selected Agent/profile, reviewer
  slot/order/count, any expected progress field, complete-key equality, or
  child independence makes a focused test fail.

## Concerns

- The macOS linker emits its pre-existing `__eh_frame section too large`
  warning for the Rust library test binary. Both permitted focused commands
  still completed successfully with zero failed tests.
- Default `tauri-runtime` GREEN verification is intentionally not claimed due
  to the binding server-only instruction.

## Fix Round 1

### Status

IMPLEMENTED_UNCOMMITTED

### Fixes

- Route derivation now validates the complete recorded `b2d_task_risk_v1`
  assessment before selecting normal/high route shapes. It validates known and
  unique hard/soft evidence, canonical soft weights, non-empty bounded
  evidence and reason text, recorded score arithmetic, hard-trigger
  precedence, and the 0..=2 normal / >=3 high boundary.
- Simple projection now groups durable expected-route runs by child
  conversation ID and complete work-unit key. Reuse across two expected keys
  emits the bounded/deduplicated
  `simple_progress_route_child_not_independent` warning even when progress is
  absent.
- Warning-only projection, legacy aggregate fallback, and the absence of
  admission or Gate authority are unchanged.

### RED Evidence

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_route_validates_risk_policy -- --nocapture
```

Result: expected failure, 0 passed / 1 failed, because a normal route with the
`concurrency_lifecycle` hard trigger was accepted.

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_warns_when_durable_route_keys_share_a_child -- --nocapture
```

After correcting the fixture to respect the database's child/generation
uniqueness constraint, result: expected failure, 0 passed / 1 failed, because
the projected Task contained only `simple_progress_expected_route_missing`
and omitted the durable child-independence warning.

### GREEN Evidence

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_route -- --nocapture
```

Result: PASS, 5 passed / 0 failed / 4600 filtered out. This covers score
boundaries, all six hard-trigger names, inconsistent totals/levels, duplicate
soft evidence, canonical route fixtures, and complete-key durable matching.

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_warns -- --nocapture
```

Result: PASS, 4 passed / 0 failed / 4601 filtered out. This includes the
database-backed missing-progress durable child-reuse case and the legacy stale
progress warning projection test.

```text
cargo fmt --check -- src/acp/delegation/workflow/project.rs
git diff --check
```

Result: PASS with no output.

### Commit

Commit creation was attempted with the focused message
`fix(workflow): validate Simple route projection`, but the managed sandbox
denied creation of the linked-worktree Git `index.lock` outside the writable
worktree. No commit was created and no files remain staged.

### Concerns

- The macOS linker still emits the pre-existing `__eh_frame section too large`
  warning; both permitted focused suites completed with zero failures.
- Default `tauri-runtime` verification was not started or claimed.
