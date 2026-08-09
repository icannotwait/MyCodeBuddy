# Task 7 Report: SQLite V2-Only Insert And Freeze Triggers

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX/GROK REVIEW PENDING**

- Work unit: `task|7|implementer|codex|none`
- Scope: Completion Protocol V2-Only plan Task 7 only
- Baseline HEAD: `498e7e052e2b9d163b9cdb9eb86bcb825dc85390`
- Producer commit: `9cfd617f2491138b228fb38e6d80dee51610a1b4`
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
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass.
- Scoped Rustfmt check over all five modified Rust files
  - Pass.
- `git diff --check` and staged diff check before the producer commit
  - Pass.
- Scope/invariant searches
  - Pass: trigger drops exist only in the new migration `down`; no 2026-08-04
    migration changed; no post-latest v1/inconsistent header update remains in
    `completion_protocol_v2.rs`.

Cargo emitted the existing warning that the ignored `codeg-mcp` sidecar is a
zero-byte placeholder. It did not affect compilation or tests and is outside
the producer diff.

## Producer Commit

- `9cfd617f2491138b228fb38e6d80dee51610a1b4` -
  `feat(db): enforce completion protocol v2-only triggers`

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added SQLite v2-only insert/freeze triggers plus predecessor-seeded historical fixtures, with coverage for invalid inserts, frozen updates, identical re-SETs, cascade deletes, preserved links, and rollback scope.","commits":[{"sha":"9cfd617f2491138b228fb38e6d80dee51610a1b4","subject":"feat(db): enforce completion protocol v2-only triggers"}],"tests":{"status":"passed","passed":41,"failed":0,"summary":"The 12-test migration and 29-test completion_protocol_v2 integration targets passed, along with desktop cargo check, scoped Rustfmt, diff checks, and invariant searches."},"concerns":["The existing zero-byte codeg-mcp sidecar packaging warning remains outside this diff.","Independent Codex and Grok review is pending before Task 8."],"report_file":".superpowers/sdd/task-7-report.md"}
-->
