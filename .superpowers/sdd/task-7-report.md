# Task 7 Report: SQLite V2-Only Insert And Freeze Triggers

## Status

**IMPLEMENTATION COMPLETE; CODEX RE-REVIEW APPROVED; GROK REVIEW PENDING**

- Work unit: `task|7|implementer|codex|none`
- Scope: Completion Protocol V2-Only plan Task 7 only
- Baseline HEAD: `498e7e052e2b9d163b9cdb9eb86bcb825dc85390`
- Producer commit: `9cfd617f2491138b228fb38e6d80dee51610a1b4`
- Review-fix commit: `8056433ae455065f25d7bc04a28585ff2f4a8081`
- Task 8+: not started

## Implementation

- Added and registered `m20260809_000001_completion_protocol_v2_only`
  immediately after `m20260806_000004_legacy_restart_context`.
- Added `trg_delegation_workflows_v2_only_insert`, which permits only exact
  `(2, v2_enforce)` rows with a null `legacy_source_workflow_id`.
- Added null-safe value-change freeze triggers for the two completion protocol
  fields and for `legacy_source_workflow_id`. Identical assignments remain
  writable, including SeaORM-shaped updates that re-SET unchanged protocol
  values while changing graph metadata.
- Kept migration `up` data-preserving. `down` contains only the three matching
  `DROP TRIGGER IF EXISTS` statements.
- Added `HistoricalWorkflowSeed` and
  `historical_completion_protocol_db`, which migrate through the predecessor,
  seed historical headers and links, and then apply only remaining migrations
  on the same in-memory connection.
- Replaced post-latest v1/inconsistent header mutation in
  `completion_protocol_v2.rs` with predecessor-seeded fixtures. Ordinary fresh
  databases retain the triggers and no fully migrated shared fixture drops or
  disables them.
- Added explicit test-only predecessor/finalization helpers for complex
  historical fixtures. Broker, listener, completion-evidence, and workflow
  store tests now finish their historical or corrupt header setup before the
  normal migrator installs the v2-only triggers.
- Restored undecodable raw-mode coverage (`future_mode` and `corrupt_mode`)
  without weakening or bypassing triggers on a fully migrated connection.
- Did not edit any 2026-08-04 migration and did not rewrite historical rows.

## TDD Evidence

Before registering the migration, the focused migration target failed for the
expected reasons:

- The registration test reported the v2-only migration missing.
- The matrix reported that an insert omitting protocol columns succeeded.

After registering the exact trigger SQL, both migration tests passed. The
historical integration suite then failed because all five legacy fixtures
attempted post-migration protocol updates and received
`completion_protocol_frozen`. After moving those headers to predecessor
seeding, all five passed.

The first independent code review found the same post-migration mutation
pattern in library fixtures and found that typed historical seeds had replaced
undecodable-mode coverage. The named broker regression reproduced with
`completion_protocol_frozen`; the restored raw-mode integration test then
failed to compile until the explicit predecessor/finalization API existed.
After implementing that API and migrating every affected library fixture, the
17 focused library regressions passed and all 14 trigger-related failures
disappeared from the full library failure list.

The migration matrix covers omitted and every non-exact supported protocol
pair, exact-v2 success, non-null legacy-source rejection, historical/current
protocol freezes, all legacy-source value transitions, identical protocol
re-SETs, ordinary non-protocol updates, historical row/link preservation,
conversation deletion with dependent cascades, and rollback scope with an
unrelated sentinel trigger.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_migrations --features test-utils v2_only_trigger`
  - Pass: 2 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical`
  - Pass: 5 passed, 0 failed.
- Full `completion_protocol_migrations` integration target
  - Pass: 12 passed, 0 failed.
- Full `completion_protocol_v2` integration target
  - Pass: 29 passed, 0 failed.
- Focused broker/listener/completion-evidence/store historical and corrupt
  fixture regressions
  - Pass: 17 passed, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass.
- Scoped Rustfmt check over all six review-fix Rust files
  - Pass.
- `git diff --check` and staged diff checks before both producer commits
  - Pass.
- Scope/invariant searches
  - Pass: trigger drops exist only in the new migration `down`; no 2026-08-04
    migration changed; no post-latest v1/inconsistent header update remains in
    `completion_protocol_v2.rs`.

The full library command completed with 4,183 passed, 103 failed, and one
ignored. The 14 Task 7 trigger-fixture failures from the initial 117-failure
run were eliminated; the remaining failures are the existing Task 2/3
`V2CallerEvidenceRejected`/legacy settlement fixture debt already recorded by
prior task reports and are outside Task 7.

Full `cargo fmt --check` also reports existing differences in untouched files;
the scoped check is clean. Cargo continues to emit the existing zero-byte
`codeg-mcp` sidecar warning. Neither concern is part of the producer diff.

## Independent Re-Review

The independent Codex re-review reported zero Critical, Important, or Minor
findings and `Ready to merge: Yes`. It confirmed both prior Important findings
closed, all 14 Task 7 library regressions removed, raw corrupt-mode coverage
restored, and ordinary fresh-database and migration/down invariants preserved.
Independent Grok review remains pending before Task 8.

## Producer Commit

- `9cfd617f2491138b228fb38e6d80dee51610a1b4` -
  `feat(db): enforce completion protocol v2-only triggers`
- `8056433ae455065f25d7bc04a28585ff2f4a8081` -
  `fix(db): seed historical protocol fixtures before freeze`

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added SQLite v2-only insert/freeze triggers and predecessor-stage typed/raw historical fixtures; fixed library fixture regressions while preserving corrupt-protocol classification, cascades, links, and rollback scope.","commits":[{"sha":"9cfd617f2491138b228fb38e6d80dee51610a1b4","subject":"feat(db): enforce completion protocol v2-only triggers"},{"sha":"8056433ae455065f25d7bc04a28585ff2f4a8081","subject":"fix(db): seed historical protocol fixtures before freeze"}],"tests":{"status":"passed","passed":58,"failed":0,"summary":"The 12 migration, 29 completion_protocol_v2, and 17 focused library regressions passed with desktop cargo check, scoped Rustfmt, diff checks, and invariant searches."},"concerns":["The full library run has 103 existing Task 2/3 evidence-fixture failures outside Task 7; all 14 Task 7 trigger-fixture regressions were eliminated.","Full cargo fmt --check finds unrelated existing differences; scoped Rustfmt passes.","The existing zero-byte codeg-mcp sidecar packaging warning remains outside this diff.","Independent Grok review remains pending before Task 8; Codex re-review approved with no findings."],"report_file":".superpowers/sdd/task-7-report.md"}
-->
