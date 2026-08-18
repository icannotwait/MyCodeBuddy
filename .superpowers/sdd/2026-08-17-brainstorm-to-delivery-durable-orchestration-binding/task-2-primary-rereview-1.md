# Task 2 Primary Scoped Re-review 1

## Disposition

**ADDRESSED**

New findings: **0 Critical, 0 Important**.

## Open Finding

The missing broker-level continuation mismatch side-effect proof is addressed
by commit `344d2ab99fabbf0c7e62d14bfb852f8272ce0f9c`.

The new focused broker test covers both sides of the conversion fence:

- a bound source with a valid but different namespace;
- an unbound source with a supplied valid binding.

For each case it asserts the exact
`orchestration_binding_lineage_mismatch` code, unchanged resume and spawn
counters, and no durable row for the rejected parent tool-use ID. It then
reuses the same approved continuation authorization in an exact bound retry or
an omitted-binding unbound retry, verifies exactly one resume, verifies reuse
of the source child, and checks that the resulting run persists the source
binding and the reused authorization.

This exercises the broker boundary that the original run-store-only coverage
could not observe. The producer also recorded a mutation RED in which disabling
the equality fence made this exact test fail after the mismatched call reached
resume; the mutation is absent from the committed fix range.

## New Findings

### Critical

None.

### Important

None.

## Scope Review

The fix range `43c63745..344d2ab9` changes only
`src-tauri/src/acp/delegation/broker.rs`, adding the focused test and a test
helper for constructing an approved continuation recovery authorization. Both
additions remain inside the existing test module. No production behavior,
public schema, persistence path, or unrelated file changed.

## Verification

Fresh reviewer verification with
`--no-default-features --features server,test-utils`:

- Exact new broker test: **1 passed, 0 failed**.
- `orchestration_binding_lineage_` filter: **5 passed, 0 failed**.
- `git diff --check 43c63745..344d2ab9`: **passed**.

The focused test links emitted the existing macOS `__eh_frame`
compact-unwind warning; test execution was unaffected. The scoped re-review did
not rerun the full library suite.
