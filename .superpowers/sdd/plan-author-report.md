# Plan Author Report - Completion Protocol V2-Only

Conclusion: done

## Workflow Binding

- workflow_id: `a07e4975-2a54-4672-86a0-93fb94c5714d`
- node_id: `plan-author`
- work_unit_key: `plan|docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md|author|codex|none`
- agent_type: `codex`
- design gate: approved, cycle 1

## Inputs

- Design: `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md`
- Verified design digest: `sha256:61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`
- Author brief: `.superpowers/sdd/plan-author-brief.md`
- Planning workflow: `superpowers:writing-plans`

## Output

- Plan: `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md`
- Plan digest: `sha256:e59e90636265fe6f11c284a1da5e09d5752b04db25c42b142ad3981aaeb15255`
- Plan size: 1,412 lines at revision-2 digest calculation
- Product code implemented: none

## Task Summary

The plan contains 11 ordered tasks. Each task includes exact file ownership,
interfaces, test-first steps, PowerShell verification commands, expected
results, a producer commit, and the required independent review gate before
the next task is admitted.

The sequence is:

1. Fixed v2 identity, shared pair guard, and stable errors.
2. Removed-configuration startup rejection and fixed v2 creation.
3. V2-only settlement tool/transport contract.
4. Mutation, recovery-authorization, final-delivery, and root-prompt fences.
5. V2 task admission and typed terminal fail-closed processing.
6. Legacy restart writer/API removal with historical projection retained.
7. SQLite insert/freeze triggers and migration-aware historical fixtures.
8. Backend rollout/settings/shadow/restart-metric cleanup.
9. Frontend API/control/type/translation cleanup.
10. Pre-final test-only aggregate contract audit.
11. Full desktop/server/MCP/frontend verification, delivery-evidence commit,
    frozen candidate HEAD, and dual final review.

Human UAT appears only under post-delivery residual work.

## Task Routing

Policy: `b2d_task_risk_v1`

- Total tasks: 11
- High risk: 10
- Normal risk: 1
- High route: Codex implementer plus independent Codex and Grok reviewers
- Normal route: Grok implementer plus independent Codex reviewer

The plan includes a complete Task Routing Matrix with index, title,
files/modules, hard-trigger evidence, soft-signal evidence, soft total, final
level/reason, implementer, reviewer set, and policy version for every task.

## Residual Design Minors

- D-CODEX-M3: Task 4 covers corrupt/undecodable headers on publication,
  recovery, and linked-root admission with zero side effects; Tasks 5 and 10
  require dangling/missing workflow headers and all permanent terminal header
  decode/type failures to use `unsupported_completion_protocol`. The durable
  row, wait result, and emitted event must contain the same code; no
  `PendingTerminalRetry`, Card parser, or shadow comparator is allowed for
  those protocol failures. Database infrastructure failures retain their
  existing persistence classification, and SQLite busy/locked retains its
  existing bounded retry rail.
- D-CODEX-M4: Tasks 4 and 5 expand negative tests across
  `resolve_design_self_review`, completion decisions, artifact recovery,
  final-delivery guards, `complete_work`, dispatch/continue/replace, recovery
  authorization, and linked root prompt paths. The shared guard is exercised
  against `(2,v1)`, `(2,v2_shadow)`, and `(1,v2_enforce)` in addition to
  historical `(1,v1)` and `(1,v2_shadow)`.
- D-GROK-M1: Tasks 6, 9, and 10 enforce the parent ruling that
  `creation_mode` is always present and exactly equals the persisted
  `completion_protocol_mode`; no separate column or omitted projection is
  introduced.

## Self-Review Evidence

- Requirements baseline digest rechecked and matched the brief.
- Spec coverage checked across fixed construction, startup, publication,
  settlement, mutation/root/recovery fences, admission, terminal processing,
  historical reads, restart removal, database triggers, settings/metrics,
  frontend behavior, standalone behavior, and post-delivery UAT.
- Placeholder scan found no `TBD`, `TODO`, deferred implementation phrases,
  vague validation/error-handling instructions, or cross-task shorthand.
- Type/signature scan confirmed every named shared interface has a definition
  and consumer, including the migration-aware historical fixture.
- Structural scan found 11 numbered tasks, 11 routing rows, 10 high routing
  blocks, 1 normal routing block, and files/interfaces/commit/review sections
  in every task.
- Markdown hygiene found no trailing whitespace or conflict markers and found
  142 balanced code-fence markers.
- `git diff --cached --check` passed for the plan.
- The ignored plan was added with `git add -f` as required.
- The pre-existing `.superpowers/sdd/progress.md` modification was not edited,
  reverted, or staged by the Plan Author.

## Concerns

None. The plan is ready for task-by-task implementation and review.

## Revision 1 - 2026-08-09

Plan reviewer inputs:

- `.superpowers/sdd/plan-review-codex-report.md`: `request_changes`
- `.superpowers/sdd/plan-review-grok-report.md`: `request_changes`
- Previous plan digest:
  `sha256:6da9e6f8a7a79ecffd9dac1d980cbcdb5353bb76f0afb99d3324d6a8a77e7b0e`
- Revised plan digest:
  `sha256:ccb27572d611155b31be009729058f185fcfefe48be7958de2689c5b0cb8334c`

### Important Finding Disposition

- P-CODEX-I1: Task 10 is now strictly test-only, with only
  `completion_protocol_v2.rs` and `completion_transport_parity.rs` in scope.
  Its command rejects any non-test diff. A production gap stops Task 10 and
  reopens the owning high-risk Task under the original Codex plus independent
  Codex/Grok route before Task 10 is freshly admitted.
- P-CODEX-I2: Task 11 now writes and commits delivery evidence before Final
  admission. It resolves a clean candidate hash after that commit, requires
  both Final reviewers to approve the same hash, and forbids branch commits or
  tracked edits after dual approval. Reviewer outcomes remain authoritative in
  external platform reports/cards.
- P-CODEX-I3: Task 4 now defines typed header loaders and permanent decode
  mapping. Unknown version and undecodable mode fixtures exercise publication
  revision, direct recovery mutation, and linked-root admission, expecting
  `unsupported_completion_protocol` and an unchanged full side-effect
  snapshot.
- P-GROK-I1: Task 7's protocol freeze trigger now raises only when
  `NEW.completion_protocol_version IS NOT OLD.completion_protocol_version` or
  `NEW.completion_protocol_mode IS NOT OLD.completion_protocol_mode`. Positive
  tests re-SET identical protocol values while updating `graph_revision` and
  `updated_at` on both historical and current rows.

### Minor Finding Disposition

- P-CODEX-M1: Task 6 stages every declared file explicitly; no delegation
  directory-wide add remains.
- P-CODEX-M2 / P-GROK-M1: Task 7 verifies explicit historical parent
  conversation deletion and normal foreign-key cascade cleanup after trigger
  installation.
- P-GROK-M2: Task 6 positively asserts the retained five-tool root
  `workflow_v2` catalog plus root recovery authorization. Task 8 positively
  drains a v2 attention event through `CompletionRootWakeQueue` and requires
  exactly one acknowledged root wake.
- P-GROK-M3: `src-tauri/src/acp/manager.rs` was removed from Task 2 ownership;
  it remains owned by Tasks 4, 6, and 8 where concrete manager changes exist.

### Revision Verification

- Approved Design digest remains
  `sha256:61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`.
- Task Routing Matrix remains 11 tasks: 10 high, 1 normal. The sole normal Task
  is now mechanically restricted to test files.
- Reviewer-specific audits passed for all four Important findings and all
  inexpensive Minor findings.
- Placeholder, task-structure, type/signature, Markdown hygiene, and staged
  diff checks passed after revision.
- No product code was implemented or modified.

Conclusion: done

## Revision 2 - 2026-08-09

Plan reviewer input:

- `.superpowers/sdd/plan-review-codex-report-cycle2.md`:
  `request_changes`
- Grok cycle-1 review remains approved; no new Grok finding was introduced.
- Previous plan digest:
  `sha256:ccb27572d611155b31be009729058f185fcfefe48be7958de2689c5b0cb8334c`
- Revised plan digest:
  `sha256:e59e90636265fe6f11c284a1da5e09d5752b04db25c42b142ad3981aaeb15255`

### Important Finding Disposition

- P2-CODEX-I1: Task 4 now scopes header error mapping to the typed header-column
  decode boundary and matches the original `DbErr` before string conversion.
  Header `DbErr::Type` and `DbErr::TryIntoErr` conversion failures map to
  `UnsupportedCompletionProtocolHeader`; `ConnectionAcquire`, `Conn`,
  `Query`, `Exec`, and all other database/infrastructure variants remain
  persistence errors. Busy/locked retains its existing retry classification.
- The focused `header_db_error_classification` test covers undecodable enum
  data, SQLite busy/locked, pool timeout/acquisition, closed connection, and a
  non-type query failure. It requires the unsupported code and zero side
  effects only for actual header conversion failure, while verifying the
  existing persistence and retry behavior for infrastructure failures.

### Minor Finding Disposition

- P2-CODEX-M1: Task 10 now combines `git diff --name-only HEAD` with
  `git ls-files --others --exclude-standard`, so its allowlist inspects staged,
  unstaged, and untracked non-ignored paths. Task 11 now requires default
  `git status --porcelain` to be completely empty before freezing the Final
  candidate, including untracked files.

### Revision Verification

- Approved Design digest remains
  `sha256:61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`.
- Task Routing Matrix remains 11 tasks: 10 high and 1 normal. Task 10 remains
  test-only and mechanically rejects production or undeclared paths.
- Placeholder, task/routing structure, typed error-boundary, database failure
  matrix, worktree-cleanliness, Markdown hygiene, and diff checks passed.
- No product code was implemented or modified.

Conclusion: done
