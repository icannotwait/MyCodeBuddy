# Task 1 Report

## Implemented

- Added `ReviewerSlot::{Primary, Auxiliary}`.
- Added canonical Design Fixer and explicit slotted Task reviewer builders/parsers.
- Preserved the legacy five-part Task reviewer builder and parse it as primary.
- Updated admission role/readiness/stamp matches for Design Fixer.
- Added slot-aware observed projection identities and focused round-trip tests.

## TDD Evidence

RED:

```text
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
```

The focused build failed with the expected missing `ReviewerSlot`,
`DesignFixer`, and `TaskReviewerSlotted` symbols. The first cold attempt was
blocked before compilation because the ignored root `out/` directory did not
exist; after creating it, the intended compile failures were observed.

GREEN:

```text
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
14 passed; 0 failed

cargo test --lib --features test-utils observed_projection_slotted_keys -- --nocapture
1 passed; 0 failed
```

Both commands emitted the repository's existing Tauri sidecar and macOS linker
warnings; there were no test failures.

## Files Changed

- `src-tauri/src/acp/delegation/workflow/types.rs`
- `src-tauri/src/acp/delegation/workflow/key.rs`
- `src-tauri/src/acp/delegation/workflow/admission.rs`
- `src-tauri/src/acp/delegation/workflow/project.rs`

Commit: `717795f4 feat(workflow): add slotted reviewer work units`

## Self-review

- Builder and parser use the same existing path, Agent, profile, index, control
  character, and Unicode-scalar bounds.
- Legacy five-part reviewer keys remain byte-for-byte unchanged and map to
  `ReviewerSlot::Primary`.
- Design Fixer remains producer-like in historical admission plumbing and does
  not receive a document-review Gate stamp.
- Primary and auxiliary reviewer node IDs include both slot and key digest, so
  same-Agent/profile reviewers cannot collide.

## Concerns

- The focused Rust commands are green but output is not pristine because the
  worktree has no prepared codeg-mcp sidecar and the macOS linker reports its
  pre-existing compact-unwind size warning.
- Independent Task review still required before Task 1 is accepted.

## Fix Round 1

Addressed both Important findings from the independent Task review:

- Added direct builder/parser cases for path, Agent, profile control
  characters, index, invalid slot, and exact 200/201 Unicode-scalar bounds on
  both new work-unit branches.
- Ran the missing shared-library compile check.

Covering evidence:

```text
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
15 passed; 0 failed

cargo check --lib --features test-utils
Finished dev profile successfully
```

The test command retained the pre-existing sidecar/linker warnings. The compile
check retained only the ignored sidecar warning.

Fix commit: `6973793f test(workflow): cover slotted key validation`
