# Task 7 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 7 HIGH reviewer (Grok)
- **reviewed_task_id:** `97446ddb-3202-4e9c-80d0-c7821e999ecb`
- **Producer code commits:** `9cfd617f2491138b228fb38e6d80dee51610a1b4` + `8056433ae455065f25d7bc04a28585ff2f4a8081`
- **HEAD tip:** `6de5905ac95ed5bbe85b233dfeb1b5d329fcbcee`
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 7
- **Implementer report:** `.superpowers/sdd/task-7-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`approve`**

**Ready to merge: Yes**

Task 7 installs exactly three SQLite triggers with plan-exact SQL, keeps `up` data-preserving, limits `down` to the three matching `DROP TRIGGER IF EXISTS` statements, and moves every historical/corrupt protocol fixture onto predecessor-stage seeding. The migration matrix and historical integration suite pass on independent re-run. Library fixture repairs stay inside Task 7’s freeze/seeding contract and do not enter Task 8 rollout/settings/shadow/restart cleanup.

No Critical, Important, or blocking Minor findings.

## Spec compliance (Task 7 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| Register `m20260809_000001_completion_protocol_v2_only` immediately after `m20260806_000004_legacy_restart_context` | Pass | `migration/mod.rs` module + migrator list; registration test asserts `v2_only == predecessor + 1` |
| Insert trigger permits only exact `(2, v2_enforce)` with null `legacy_source_workflow_id` | Pass | Plan-exact `WHEN NEW.version <> 2 OR NEW.mode <> 'v2_enforce' OR NEW.legacy_source IS NOT NULL` → `completion_protocol_v2_only` |
| Insert rejects omitted protocol columns | Pass | Columns are `NOT NULL DEFAULT 1` / `DEFAULT 'v1'`; omitted insert becomes non-exact and aborts with `completion_protocol_v2_only` |
| Insert rejects every non-exact supported pair | Pass | Matrix rejects `(1,v1)`, `(1,v2_shadow)`, `(1,v2_enforce)`, `(2,v1)`, `(2,v2_shadow)` |
| Insert rejects non-null legacy source even for exact v2 | Pass | Matrix case `wf-v2-with-legacy-source` |
| Protocol freeze on value change only (`IS NOT`) | Pass | `trg_delegation_workflows_protocol_frozen` uses null-safe `NEW IS NOT OLD` on version/mode |
| Legacy-source freeze null-safe (`NOT (NEW IS OLD)`) | Pass | Rejects NULL→value, value→NULL, value→different; allows identical re-SET |
| SeaORM-shaped identical protocol re-SET remains writable | Pass | Matrix updates historical + current with graph/`updated_at` + self-assigned protocol columns |
| Ordinary non-protocol updates remain writable | Pass | Matrix bumps `graph_revision`/`updated_at` without protocol columns |
| Historical rows unchanged by `up` | Pass | Full-row snapshot equality before/after `Migrator::up` |
| Historical links survive up/down | Pass | Linked successor `legacy_source_workflow_id` preserved across up and down |
| Parent conversation delete cascades despite triggers | Pass | Deletes workflow + manifest revision + task run; triggers do not intercept `DELETE` |
| `down` drops only the three triggers, no row rewrite | Pass | Sentinel trigger retained; row snapshot unchanged; v1 insert succeeds after down |
| Predecessor historical seeding helper | Pass | `HistoricalWorkflowSeed` + `historical_completion_protocol_db` + explicit before/complete helpers under `cfg(any(test, feature = "test-utils"))` |
| Never insert/update historical protocol after latest on shared migrated DB | Pass | Integration + library fixtures mutate or seed before `complete_historical_*` / helper finalization |
| Undecodable raw-mode coverage retained | Pass | `future_mode` / `corrupt_mode` / version `99` via `PRAGMA ignore_check_constraints` on predecessor connections |
| Do not edit 2026-08-04 migrations / rewrite historical rows | Pass | Code-commit name list has no `m20260804*`; migration `up` is CREATE TRIGGER only |
| No Task 8 scope | Pass | Rollout/settings/shadow/restart metrics symbols remain; no Task 8 file surface cleanup |

### Trigger / fixture map

```text
CREATED (migration up)
  trg_delegation_workflows_v2_only_insert
    abort completion_protocol_v2_only unless exact (2, v2_enforce) + NULL legacy_source
  trg_delegation_workflows_protocol_frozen
    abort completion_protocol_frozen when version/mode value changes
  trg_delegation_workflows_legacy_source_frozen
    abort legacy_source_workflow_frozen when legacy_source value changes

REMOVED ONLY ON down
  DROP TRIGGER IF EXISTS for the three names above

TEST HELPERS (test-utils / tests only)
  BeforeCompletionProtocolV2Only migrator slice
  historical_completion_protocol_db_before_v2_only
  complete_historical_completion_protocol_migrations
  HistoricalWorkflowSeed + historical_completion_protocol_db

FIXTURE MIGRATION ORDER
  predecessor migrator → seed/mutate historical or corrupt headers
  → install remaining migrations (installs freeze/insert triggers)
  → exercise production read/reject paths on fully migrated connection
```

## Independent verification

Re-ran on this worktree at HEAD `6de5905a` (producer `9cfd617f` + fix `8056433a` + docs tips):

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_migrations --features test-utils v2_only_trigger` | **pass** (2) |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical` | **pass** (5) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_launch_variants_reject_historical_protocol` | **pass** (1) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils design_self_review_preflight` | **pass** (2) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils historical_protocol_mutation_matrix_completion_evidence` | **pass** (1) |

Static audit:

| Check | Result |
| --- | --- |
| Trigger SQL matches plan byte-for-byte intent | **match** (exact WHEN/RAISE markers) |
| `DROP TRIGGER` only in new migration `down` | **pass** (only three statements there) |
| 2026-08-04 migrations in producer/fix commits | **absent** |
| Production/shared DB never drops or disables v2-only triggers | **pass** (`ignore_check` / predecessor helpers are test-only; `fresh_in_memory_db` installs full migrator with triggers) |
| Task 8 rollout/settings/restart metrics symbols | **still present** (correctly deferred) |
| Producer file set vs Task 7 intent | **in scope**; broker/listener/store/completion_evidence edits are freeze-aware fixture repairs required by Step 4, not Task 8 cleanup |

## Strengths

1. Trigger SQL is plan-exact, minimal, and uses null-safe freeze predicates (`IS NOT` / `NOT (... IS ...)`), which is the right SQLite shape for SeaORM full-row updates.
2. Migration matrix is unusually complete: omitted insert, all non-exact supported pairs, legacy-source insert reject, historical/current freezes, identical re-SETs, non-protocol writes, cascade delete, and down-scope with a sentinel trigger.
3. Predecessor seeding is factored once in `test_helpers` and reused by integration + library fixtures, preserving corrupt raw-mode coverage without weakening fully migrated connections.
4. Review-fix commit correctly closed the library post-migration mutation holes without rewriting historical production rows or touching rollout surfaces.
5. `up` is purely additive (triggers only); historical headers and links are snapshot-proven unchanged.

## Findings

| id | severity | title | blocking |
| --- | --- | --- | --- |
| — | — | No Critical, Important, or Minor findings | — |

### Notes (non-findings)

- Insert-trigger comparisons use `<>` rather than null-safe `IS NOT`. This matches the plan SQL and is safe because `completion_protocol_version` / `mode` are `NOT NULL` with defaults `(1, v1)`; omitted columns therefore fail the exact-pair check rather than silently bypassing the `WHEN` clause.
- Plan **Files:** list names only migration/tests helpers, while Step 4 necessarily required library fixture updates in broker/listener/store/completion_evidence. Those extra paths are justified freeze/seeding repairs, not scope creep into Task 8.
- Implementer report’s remaining full-library failures (Task 2/3 evidence-fixture debt) are outside Task 7 and were not re-litigated here.

## Scope notes

- Code commits `9cfd617f` + `8056433a` implement Task 7 triggers + predecessor fixtures only.
- Tips after code (`4e71473b`, `6de5905a`) are SDD docs only.
- Task 8 surfaces (`CompletionProtocolRolloutConfig`, settings APIs, shadow/restart metrics) remain intentionally present.
- No production code was changed by this review.

## Conclusion

**approve** — Task 7 migration enforcement and historical fixture migration are correct, matrix-covered, independently verified, and ready for Task 8.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve","summary":"Grok HIGH review: Task 7 triggers exact, freeze/insert matrix pass, predecessor historical seeding correct, down-only drops, no Task 8 scope. Ready to merge.","commits":[{"sha":"9cfd617f2491138b228fb38e6d80dee51610a1b4","subject":"feat(db): enforce completion protocol v2-only triggers"},{"sha":"8056433ae455065f25d7bc04a28585ff2f4a8081","subject":"fix(db): seed historical protocol fixtures before freeze"}],"tests":{"status":"passed","passed":11,"failed":0,"summary":"Independent re-run: 2 migration trigger tests, 5 historical integration tests, 4 focused library fixture tests — all passed."},"concerns":[],"report_file":".superpowers/sdd/task-7-review-grok-report.md","reviewed_task_id":"97446ddb-3202-4e9c-80d0-c7821e999ecb","findings":{"critical":0,"important":0,"minor":0},"ready_to_merge":true}
-->
