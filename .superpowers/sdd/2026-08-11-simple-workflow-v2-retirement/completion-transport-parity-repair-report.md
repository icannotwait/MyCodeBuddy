# completion_transport_parity Repair Report

Date: 2026-08-12

Branch: `feature/simple-workflow-v2-retirement`

Base: `90185fb289f6f2b6b82cb8ff98b99a86e8e62de3`

## Status

The scoped `completion_transport_parity` repair is green. All three exact
RED cases failed for the expected reasons before the edit, all three exact
GREEN cases passed after the edit, and the full parity target passed 10/10.

The full desktop command is not green because it reaches a later unrelated
failure in `delegation_session_reuse_integration`. The scoped rustfmt check
also reports pre-existing formatting drift in an untouched block of the owned
test file. Neither unrelated issue was changed under this repair's ownership.

## Changed files

- `src-tauri/tests/completion_transport_parity.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/completion-transport-parity-repair-report.md`

No production file changed.

## Repair

1. Removed only the obsolete `successor_conversation_id` banned entry from
   `legacy_restart_surface_is_absent`.
2. Removed only the obsolete `successor_conversation_id` banned entry from
   `v2_only_removed_surface_inventory`.
3. Wrapped only the foreign direct-core
   `resolve_completion_decision_core` future in
   `with_historical_workflow_fixture_mutations`. The foreign HTTP request
   remains outside the fixture scope.

## RED evidence

All Rust test commands were run from `src-tauri` with
`RUST_MIN_STACK=16777216`, one Cargo process at a time.

### `legacy_restart_surface_is_absent`

Command:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_transport_parity legacy_restart_surface_is_absent -- --exact
```

Exit code: `1`

Count: `0 passed; 1 failed; 9 filtered out`

Expected failure: `legacy restart surface successor_conversation_id remains in listener`

### `v2_only_removed_surface_inventory`

Command:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_transport_parity v2_only_removed_surface_inventory -- --exact
```

Exit code: `1`

Count: `0 passed; 1 failed; 9 filtered out`

Expected failure: `removed public symbol successor_conversation_id remains in ...\src\acp\delegation\broker.rs`

### `attention_authenticated_context_owns_durable_root_across_core_and_http`

Command:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_transport_parity attention_authenticated_context_owns_durable_root_across_core_and_http -- --exact
```

Exit code: `1`

Count: `0 passed; 1 failed; 9 filtered out`

Expected failure: `called Option::unwrap() on a None value` at the core/HTTP
detail comparison. The unwrapped foreign direct-core future returned the
workflow-v2 retirement error before reaching the intended authenticated-root
ownership rejection, so its serialized error did not provide the compared
string detail.

## GREEN evidence

The same three exact commands were rerun after the minimal test-only edit.

| Test | Exit code | Count |
| --- | ---: | --- |
| `legacy_restart_surface_is_absent` | `0` | `1 passed; 0 failed; 9 filtered out` |
| `v2_only_removed_surface_inventory` | `0` | `1 passed; 0 failed; 9 filtered out` |
| `attention_authenticated_context_owns_durable_root_across_core_and_http` | `0` | `1 passed; 0 failed; 9 filtered out` |

## Full-target verification

Command:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_transport_parity
```

Exit code: `0`

Count: `10 passed; 0 failed; 0 ignored; 0 filtered out`

## Full-desktop verification

Command:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils
```

Exit code: `1`

The owned parity target passed within this run: `10 passed; 0 failed`.
Before Cargo stopped at the later failing integration target, the observed
test binaries reported a combined `4458 passed; 1 failed; 1 ignored`.
Notable results include:

- Library tests: `4350 passed; 0 failed; 1 ignored`.
- `completion_protocol_v2`: `27 passed; 0 failed`.
- `completion_transport_parity`: `10 passed; 0 failed`.
- `delegation_session_reuse_integration`: `18 passed; 1 failed`.

The unrelated failure was:

`skill_forward_routing_invariants_nine_scenarios`

Failure: `Skill-forward policy lost required marker 'Design, Plan, Task, and Final gates advance from platform outcomes'` at
`tests/delegation_session_reuse_integration.rs:832`.

Diagnostic exact rerun:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test delegation_session_reuse_integration skill_forward_routing_invariants_nine_scenarios -- --exact
```

Exit code: `1`

Count: `0 passed; 1 failed; 18 filtered out`

The test reads `.agents/skills/brainstorm-to-delivery/SKILL.md` and requires
that exact prose marker. Repository search finds the marker only in the test;
the current structured Simple skill expresses routing through its contract
and route table but does not contain that sentence. Neither the failing test
nor the skill is modified by this repair, and both were unchanged in base
commit `90185fb2` relative to its parent.

## Formatting and diff checks

Command:

```powershell
git diff --check -- 'src-tauri/tests/completion_transport_parity.rs'
```

Exit code: `0`

Command:

```powershell
rustfmt --edition 2021 --check 'tests/completion_transport_parity.rs'
```

Exit code: `1`

`rustfmt 1.9.0-stable (2d8144b788 2026-07-07)` proposes only an untouched
rewrite at lines 98-126 around the existing
`with_historical_workflow_fixture_mutations(RunStore::...admit_gen1_reserving(...))`
call. None of the repair hunks is mentioned by rustfmt. Streaming the base
file through the same rustfmt version emits the identical proposal, and the
owned Git diff contains no change in that block. The block was deliberately
left unchanged to avoid unrelated formatting churn.

The test diff is `8 insertions, 8 deletions`. This production-path check
returned no paths:

```powershell
git diff --name-only -- 'src-tauri/src'
```

Git also warned that LF will be replaced by CRLF the next time it touches the
owned test file; `git diff --check` remained clean.

## Self-review

- Confirmed the diff removes exactly the two obsolete successor navigation
  bans and preserves every other inventory source, token, loop, and assertion.
- Confirmed the foreign context and durable-root mismatch remain unchanged.
- Confirmed historical mutation permission encloses only the foreign
  direct-core future.
- Confirmed the foreign HTTP request remains outside the fixture scope and
  continues through the normal authentication/authorization route.
- Confirmed the core-versus-HTTP status/detail assertions are unchanged.
- Confirmed no production path has a diff.
- Confirmed protected `.codex-tmp-*` and `.task-runtimes/` paths were not
  staged, modified, deleted, or cleaned.

No issue was found in the scoped repair diff.

## Remaining risks

1. The full desktop gate is open because of the independently reproducible
   stale skill-policy marker assertion described above. Cargo stopped at that
   target, so later desktop integration targets in command order were not run.
2. The scoped rustfmt gate is open because of pre-existing formatting drift in
   an untouched block of the owned test file. Fixing it would add unrelated
   changes outside the minimal repair contract.
3. The checkout retains unrelated untracked `.codex-tmp-*` files and the
   `.task-runtimes/` directory exactly as found.
