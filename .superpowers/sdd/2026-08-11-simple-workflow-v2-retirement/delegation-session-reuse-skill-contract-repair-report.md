# Delegation session reuse Skill contract repair report

## Status

The stale prose assertions in
`skill_forward_routing_invariants_nine_scenarios` now consume the embedded
`codeg-b2d-skill-contract-v1` JSON contract. The exact test and all 19 tests in
`delegation_session_reuse_integration` pass on the final file. The latest full
desktop run is not green because an unrelated Windows update-install unit test
hit a transient `PermissionDenied`; that unit passed in the preceding full run
and again when rerun alone.

## Changed files

- `src-tauri/tests/delegation_session_reuse_integration.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/delegation-session-reuse-skill-contract-repair-report.md`

No Skill, validator, production file, Plan, or progress-ledger file was
modified. The pre-existing `.codex-tmp-*` and `.task-runtimes/` paths were
preserved.

## Root cause and RED evidence

The brief supplied the already-reproduced exact RED evidence, which was reused
instead of rerunning it before the edit:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test delegation_session_reuse_integration skill_forward_routing_invariants_nine_scenarios -- --exact
```

- Exit: `1`
- Count: `0 passed; 1 failed; 18 filtered out`
- First failure: missing
  `Design, Plan, Task, and Final gates advance from platform outcomes`

Before editing, the live Skill was independently checked with:

```powershell
$skill = Get-Content -Raw '.agents\skills\brainstorm-to-delivery\SKILL.md'
$markers = @('Design, Plan, Task, and Final gates advance from platform outcomes', 'agent_type: "grok"', 'Admitted key, role, agent, and profile remain frozen', 'Final whole-branch review remains a new Codex child')
foreach ($marker in $markers) { Write-Output ("PRESENT={0} MARKER={1}" -f $skill.Contains($marker), $marker) }
Write-Output ("ABSENT_COUNT={0}" -f @($markers | Where-Object { -not $skill.Contains($_) }).Count)
```

All four markers reported `PRESENT=False`; `ABSENT_COUNT=4`. The current Skill
contains one `codeg-b2d-skill-contract-v1` comment with the semantic values the
brief requires. This confirms that the failure was assertion drift after the
intentional Task 8 structured-contract rewrite, not a missing routing policy.

The first post-edit exact run correctly got past the new structured assertions,
then exposed one remaining whole-document prose assertion:

- Exit: `1`
- Count: `0 passed; 1 failed; 18 filtered out`
- Failure:
  `Skill Forward policy lost the documented outcome for design_plan_rereview_continue_same_reviewer: platform-selected reviewer nodes and lineage`

A direct normalized scan showed all nine `policy_outcome` prose literals were
absent (`ABSENT_COUNT=9`). Those documentation-only fields and their
whole-document `contains` assertion were removed as required by the brief's
ban on source-text assertions outside the structured comment. The nine routing
fixtures and all behavioral assertions were retained.

## Implementation

The test now:

1. Reads `SKILL.md` from the existing path.
2. Requires exactly one `<!-- codeg-b2d-skill-contract-v1` marker.
3. Requires the matching `-->` terminator and parses the trimmed body with the
   existing `serde_json` dependency.
4. Emits path-specific failures for a missing marker, duplicate marker,
   missing terminator, or invalid JSON.
5. Semantically asserts:
   - the exact seven-entry `phase_order`;
   - `delegate_to_agent`, `continue_delegation`, and
     `get_delegation_status` interfaces;
   - serial execution with a Grok implementer and independent Codex reviewer;
   - two unexpected continuations, one logical replacement, and
     pre-admission-only replacement retry;
   - required independent Codex final review.

All nine scenario names, routes, expected actions, work-unit identity checks,
agent rules, recovery bounds, fingerprints, and final-review separation checks
remain intact. No helper or dependency was added to production code.

## Verification

Every Rust test command used `RUST_MIN_STACK=16777216`, and Cargo processes ran
one at a time.

### Final exact test

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test delegation_session_reuse_integration skill_forward_routing_invariants_nine_scenarios -- --exact
```

- Exit: `0`
- Count: `1 passed; 0 failed; 18 filtered out`

### Integration target

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test delegation_session_reuse_integration
```

- Exit: `0`
- Count: `19 passed; 0 failed`

### Full desktop suite

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils
```

First run:

- Exit: `0`
- Duration: `521.2s`
- Main library: `4350 passed; 0 failed; 1 ignored`
- Remaining binary/integration harnesses: `160 passed; 0 failed`
- Aggregate: `4510 passed; 0 failed; 1 ignored`

Final-tree rerun:

- Exit: `1`
- Duration: `343.5s`
- Main library: `4349 passed; 1 failed; 1 ignored`
- Failure:
  `update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps`
- Error: Windows `PermissionDenied`, `Access is denied (os error 5)`, at
  `src/update/install.rs:1056` while unwrapping the copy-swap result.
- The command stopped at the failed library harness, so later integration
  harnesses did not run in this rerun.

The unrelated failure was diagnosed without changing production code. It uses
a temporary directory and passed in the immediately preceding full run. Its
isolated rerun was:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --lib update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps -- --exact
```

- Exit: `0`
- Count: `1 passed; 0 failed; 4350 filtered out`

The latest full desktop gate is therefore reported as non-green despite the
earlier complete green run and the isolated recovery.

### Formatting and diff checks

```powershell
rustfmt --edition 2021 --check 'tests\delegation_session_reuse_integration.rs'
```

- Initial exit: `1`, one formatter-only wrap in the new JSON parse expression.
- Final exit after applying that exact wrap: `0`.

```powershell
git diff --check -- 'src-tauri/tests/delegation_session_reuse_integration.rs'
```

- Exit: `0`.

After the formatter-only wrap, the exact test (`1/1`) and integration target
(`19/19`) were rerun successfully before the final desktop rerun above.

## Self-review

- Scope: only the owned integration test and this report changed.
- Contract extraction: byte slicing starts after the ASCII marker, terminates
  at the first following comment end, trims the body, and rejects duplicates.
- Assertion quality: routing preconditions are semantic JSON comparisons; no
  `skill.contains`, normalization helper, or `policy_outcome` prose check
  remains.
- Regression preservation: all nine scenarios, expected action matrix,
  work-unit keys, agent constraints, recovery caps, and fingerprint checks are
  unchanged.
- Mutation check: removing/duplicating/unterminating the marker, invalidating
  JSON, reordering a phase, changing an interface, changing task roles/order,
  widening recovery limits, or weakening final review makes the repaired test
  fail.
- Diff hygiene: no unrelated tracked or protected untracked path was touched.

No repair-specific correctness concern remains. The only outstanding concern
is the unrelated intermittent Windows access-denied failure documented above;
it prevents claiming the latest full desktop gate is green.
