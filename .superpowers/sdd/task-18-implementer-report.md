# Task 18 Implementer Report

## Result

Task 18 proves one platform-owned completion truth across tool, terminal text,
report fallback, direct user adjudication, and obsolete model-authored Card
inputs. Each capability case keeps exactly one child run, persists validated v2
evidence with a null `card_summary_json`, and projects the same completion Card
through the desktop core, authenticated server route, and `codeg-mcp` surface.

The release fixtures also prove that session-2889-style Card prose causes no
format-repair or Card re-emission run, post-settlement Final drift returns
`final_artifact_drift` through the normal listener read and reopens Final, stale
Final evidence is omitted from the next projection, and legacy restart creates
or reopens one linked empty v2 successor while retaining the original request
context and routing the new Plan Author to Codex.

The High fix closes `T18-CODEX-I1` / `T18-GROK-I1`, `T18-CODEX-I2`, and
`T18-CODEX-I3`: corrective Plan republication preserves its pending or
authorized `ContinueReview` round, roster-only add/remove transitions update
selection without rotating lineage, and Final report/status enrichment now
returns each artifact diagnostic's real contract code.

## Capability Matrix

| Capability / regression | Named fixture | Proven result |
| --- | --- | --- |
| Tool completion | `every_model_capability_reaches_one_platform_completion_truth` / `ToolCompleteWork` | Child-only semantic tool intent becomes validated platform evidence; no model Card is retained. |
| Terminal conclusion | `every_model_capability_reaches_one_platform_completion_truth` / `TerminalConclusionOnly` | One natural-language `Conclusion: done` completes without JSON, HTML, digest, or tool use. |
| Report fallback | `every_model_capability_reaches_one_platform_completion_truth` / `ReportConclusionOnly` | A validated report conclusion becomes the same platform completion projection. |
| Direct adjudication | `every_model_capability_reaches_one_platform_completion_truth` / `AmbiguousThenUserAdjudication` | Ambiguous meaning opens typed attention; an authenticated HTTP snapshot issues the root completion context and `/api/resolve_completion_decision` completes the existing run without child continuation. |
| Obsolete Card tolerance | `every_model_capability_reaches_one_platform_completion_truth` / `ObsoleteCardPlusNaturalConclusion` | Obsolete Card JSON is ignored; the natural conclusion settles one run and `card_summary_json` is cleared. |
| Session 2889 | `session_2889_and_final_drift_have_no_format_repair_escape` | Zero format-repair runs, zero Card re-emission prompts, and one child run. |
| Final drift | `session_2889_and_final_drift_have_no_format_repair_escape` | The listener validates the full required Final-reviewer cohort and frozen v2 evidence, reports `final_artifact_drift`, atomically reopens Final with a new lineage/round, and subsequently projects state without obsolete Final evidence. |
| Legacy restart | `legacy_restart_*`, `legacy_prompt_restart_*`, and `rollout_restart_*` | Source remains immutable; one reciprocal v2 successor is idempotent/retryable, fail-closed without context, and routed to Codex Plan Author. |
| Runtime parity | `completion_projection_is_identical_across_graph_http_and_mcp_surfaces` plus every capability case | Desktop, authenticated HTTP, and MCP expose byte-equivalent completion Cards. |
| Manual root resume | `workflow-overlay.test.tsx` legacy backlink/root-resume fixture plus attention transport parity | Durable root refresh controls resume; no terminal child is reopened. |
| Corrective Plan republication | `task14_fix2_plan_authorizes_corrective_round_before_reviewer_admission` | Desktop and server retain the pending corrective round so Plan Author completion persists its immutable authorization. |
| Reviewer roster transition | `roster_only_republication_selects_only_added_reviewers_and_retires_removed_ones` | Plan and Final additions advance the same lineage and select only added reviewers; removal keeps the round and retires removed selections. |
| Non-drift Final rejection | `final_status_enrichment_preserves_artifact_unavailable_diagnostic` | A dirty Final worktree remains round 1 and reports `completion_artifact_unavailable`, without stale completion text or a false drift reopen. |

## Cross-Feature Evidence

The named targeted binaries and the full Rust suites jointly cover all 31
Design acceptance criteria:

| Design criteria | Evidence surface |
| --- | --- |
| 1-5: semantic outcomes, tool identity/exposure, prompt binding, durable authority | Five-case capability matrix; Broker committed-binding launch/continuation tests; admission instruction/scope fixtures. |
| 6-9: clean producer baseline/commit/no-op, Reviewer revalidation, Final freeze, v2 evidence and projection-only Cards | Admission artifact-contract fixtures; automatic listener Final-drift guard over the full current reviewer cohort; shared completion validator and stale-evidence projection tests. |
| 10-14: durable adjudication, role outcome compatibility, complete-set gates, external gate reduction, authenticated Design self-review | Completion evidence/store unit suites; six-field CAS and authenticated core/HTTP parity fixtures. |
| 15-21: Plan material/lineage/selective rounds, strict improvement, immutable Final contexts, canonical scope and golden identities | Plan reducer, admission scope, artifact, Final-package, and recovery fixtures in the full desktop/server libraries. |
| 22-25: transactional migration/rollback, typed attention lifecycle, durable outbox replay, root re-entry and transport parity | Ten migration fixtures; completion attention/outbox tests; seven transport parity fixtures; focused overlay tests. |
| 26-30: pre-budget recovery fences, exact-current-Plan recovery, session 2889, legacy restart, frozen rollout/rollback thresholds | Completion evidence/store recovery suites and the 22-test protocol-v2 integration binary. |
| 31: shared validation and desktop/server/MCP truth | Shared admission/store/gate/projection/recovery validation exercised by both feature builds; transport parity and MCP checks. |

Specific cross-feature fixtures include:

- Historical v1 shapes, nullable-count semantics, schema/index/FK rollback,
  tool-redelivery uniqueness, restart links, typed attention rows, and rollback:
  `completion_protocol_migrations` (10 tests).
- Stable completion tool binding across launch and continuation, forged/ineligible
  caller rejection, canonical prompt binding, and exact tool ordering: Broker,
  companion, and tool-schema tests in the full Rust suite.
- Clean baseline, workflow-owned commit, no-op policy, Reviewer HEAD/dirt
  revalidation, immutable Final package/context, and Final admission ordering:
  admission and completion-evidence artifact-contract fixtures.
- Canonical Plan material and role scope, selective Plan rounds, stagnation and
  rewrite thresholds, full-group lineage, exact-current-Plan recovery, and
  fail-closed recovery: store/admission/material/golden-vector fixtures.
- Typed six-field attention CAS, caller ownership, stale/conflict replay,
  durable outbox replay, and automatic/manual root wake: completion-evidence,
  transport-parity, and frontend overlay fixtures.

## Verification

Fresh commands were run with `CARGO_BUILD_JOBS=1` for Rust:

- `cargo test --features test-utils --test completion_protocol_migrations -- --list`:
  10 tests listed.
- `cargo test --features test-utils --test completion_protocol_migrations -- --nocapture`:
  10 passed, 0 failed.
- `cargo test --features test-utils --test completion_protocol_v2 -- --list`:
  17 tests listed in the original release run; the consolidated Final-review
  fixer raised that to 20, and the High fix now lists 22.
- `cargo test --features test-utils --test completion_protocol_v2 -- --nocapture`:
  17 passed, 0 failed in the original release run; 20 passed after the first
  fixer pass; the High fix passes 22, 0 failed.
- `cargo test --features test-utils --test completion_transport_parity -- --list`:
  7 tests listed.
- `cargo test --features test-utils --test completion_transport_parity -- --nocapture`:
  7 passed, 0 failed.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  562 passed, 0 failed across 14 suites.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  PASS, 53 checks and 0 failures.
- Focused frontend Vitest command from Task 18: 132 passed in 5 files.
- Focused frontend ESLint command from Task 18: exit 0 with no output.
- `pnpm eslint .`: exit 0 with 0 errors and 25 pre-existing warnings.
- `pnpm test`: complete Vitest run exited 0.
- `pnpm build`: compiled, typechecked, and statically generated 33/33 pages.
- Desktop `cargo check`: passed.
- Desktop `cargo test --features test-utils --lib -- --test-threads=4`: library
  4,276 passed, 0 failed, and 2 ignored in the High fix pass.
- Desktop `cargo clippy --all-targets --features test-utils -- -D warnings`:
  passed.
- Server `cargo check --no-default-features --features server --bin codeg-server`:
  passed.
- Server `cargo test --no-default-features --features server --bin codeg-server --lib -- --test-threads=4`:
  library 4,199 passed, 0 failed, and 2 ignored; server bin had 0 tests and
  passed.
- Server `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`:
  passed.
- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`: passed.
- Task-owned Rust files passed `rustfmt --edition 2021 --check`; scoped
  `git diff --check` and `git diff --cached --check` passed.

The first full desktop run exposed an obsolete hand-built Final fixture: it no
longer satisfied the complete v2 Final cohort and frozen-evidence contract.
The fixture was converted into a valid miniature v2 workflow with Plan
Author/Reviewer materialization, Plan settlement, Final review, and outbox/event
assertions. Its exact test passed, then the unchanged full desktop and server
suites passed with the counts above.

The desktop build emitted the existing warning that the packaged `codeg-mcp`
sidecar placeholder is empty; the separately required companion check and
warning-denying Clippy command both passed.

## Consolidated Final-Review Fixer Pass

The routed Final review exposed five release gaps. The following new tests were
observed RED before production changes:

- `fresh_publication_initializes_gate_state_only_for_v2_enforce`: expected the
  three Design/Plan/Final gate states, found zero.
- `final_projection_requires_every_required_reviewer_outcome`: mixed
  `request_changes + approve` projected Final as passing.
- `session_2889_and_final_drift_have_no_format_repair_escape`: reversed durable
  Final selection order failed cohort validation; after reaching listener
  status enrichment, drift surfaced as `completion_scope_changed`.
- `reopened_final_projection_omits_every_stale_reviewer_completion`: graph
  projection retained or attempted to revalidate stale Final completions.
- `final_drift_report_enrichment_reopens_and_omits_stale_completion`: report
  enrichment surfaced `completion_scope_changed` instead of reopening Final.

The fixer now initializes gate state in the publication transaction only for a
frozen protocol-2 `v2_enforce` workflow. Initial external Design/Plan and
synthetic Final cohorts are canonical sets at round 1 with server-derived
lineages. Same-material publication preserves corrective rounds; changed Plan
material rotates Plan and Final while leaving unchanged Design state intact.

Both terminal report and status enrichment invoke a crate-private task-keyed
Final guard before completion projection. Only a current required Final task
with a complete passing cohort can trigger the atomic freeze; non-Final,
non-required, stale, and incomplete tasks do not reopen the gate. Cohort
comparison is order-insensitive. A rejected or reopened delivery is returned
with the resolver's diagnostic code and stale text/completion removed. Reopened
commit drift remains `final_artifact_drift`; dirty or unavailable artifacts
remain `completion_artifact_unavailable`.

Graph projection excludes stale Final bindings before evidence validation and
evaluates every eligible required Final Reviewer as one cohort while retaining
each Reviewer's individual status. The drift fixture publishes directly as
`v2_enforce`, uses publication-created gate state, and reverses the persisted
Final selected-set order to prove set semantics.

Fresh post-fix verification with `CARGO_BUILD_JOBS=1`:

- All five named RED cases passed individually.
- Full `completion_protocol_v2`: 20 passed, 0 failed.
- Full `completion_transport_parity`: 7 passed, 0 failed.
- Existing store Final-drift unit: 1 passed, 0 failed.
- Existing same-lineage projection-round unit: 1 passed, 0 failed.
- New full-cohort Final projection unit: 1 passed, 0 failed.
- `cargo clippy --all-targets --features test-utils -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

The first attempted exact store filter selected zero tests and is not counted
as evidence; its fully qualified exact name was listed and rerun with one test.

## High Fix Pass: I1-I3

The dual review found three Important gaps. The existing I1 regression was
reproduced RED on both desktop and server before production edits:

- `task14_fix2_plan_authorizes_corrective_round_before_reviewer_admission`:
  0 passed, 1 failed because the corrective round lost its immutable
  authorization.
- `roster_only_republication_selects_only_added_reviewers_and_retires_removed_ones`:
  0 passed, 1 failed because roster addition left the gate at round 1.
- `final_status_enrichment_preserves_artifact_unavailable_diagnostic`:
  0 passed, 1 failed because dirty-worktree rejection was mislabeled
  `final_artifact_drift`.

`initialize_v2_gate_states_txn` now receives the prior normalized manifest.
For same-lineage roster changes, it compares prior and current required sets:
additions advance the round and select only added reviewers; removals retain
the round and intersect the current selection. A material lineage mismatch
preserves only a legitimate pending `ContinueReview` round, proven by its
latest persisted Plan state and, once selected, its matching immutable
authorization. Ordinary material changes still reset the lineage and delete
stale authorization. Listener enrichment derives `error_code` directly from
the returned artifact diagnostic.

Fresh High-fix verification used `CARGO_BUILD_JOBS=1` from `src-tauri`:

- Exact I1 desktop: 1 passed, 0 failed; 4,277 filtered out.
- Exact I1 server: 1 passed, 0 failed; 4,200 filtered out.
- New I2 and I3 exact filters: 1 passed each, 0 failed.
- `completion_protocol_migrations`: 10 passed, 0 failed.
- `completion_protocol_v2`: 22 passed, 0 failed.
- `completion_transport_parity`: 7 passed, 0 failed.
- Full desktop lib: 4,276 passed, 0 failed, 2 ignored.
- Full server lib: 4,199 passed, 0 failed, 2 ignored; server bin passed.
- Desktop, server, and `codeg-mcp` checks: passed.
- Desktop, server, and `codeg-mcp` Clippy with `-D warnings`: passed.
- Task-owned Rust `rustfmt --check`, scoped `git diff --check`, and cached
  diff check: passed.
- Frontend: N/A for this pass because no frontend file changed; the prior Task
  18 full frontend lint/test/build evidence remains recorded above.

The packaged desktop build repeated the known empty `codeg-mcp` sidecar
placeholder warning. The featureless companion check and warning-denying
Clippy both passed, so this is not a fix-pass failure.

## Scope And Hygiene

The initial verified implementation commit contains 27 Task 18 files:
capability and runtime integration fixtures, production corrections exposed by
those fixtures, server-only test gating, and lint/build corrections required by
the full release gate. The seven-file enforcement follow-up connects the Final
guard to listener state reads, validates all current Final reviewer bindings,
rotates drift recovery atomically, filters stale projected evidence, and
converts the legacy unit fixture to valid v2 evidence. The guard remains
crate-private, so Task 18 adds no new production API.

The following pre-existing or unrelated files remain unstaged and unchanged by
Task 18: `.superpowers/sdd/progress.md`, the Task 13 implementer report,
`src-tauri/src/acp/connection.rs`,
`src-tauri/src/acp/delegation/launch_snapshot.rs`,
`full-approved-manifest.json`, `git-credential-codeg.bat`, and every
`publish*.json` file.

The recovery logs `task-18-cargo-lib.stdout.log` and
`task-18-cargo-lib.stderr.log` remain untracked/ignored evidence only and are
not committed.

The consolidated fixer commit owns only `listener.rs`, workflow `mod.rs`,
`project.rs`, `store.rs`, `completion_protocol_v2.rs`, and this report. All
pre-existing controller/user dirt listed above remains unstaged. Because a
commit cannot embed its own SHA, the parent must add the resulting fixer SHA to
the final completion Card's commit list.

The High fix commit owns only `listener.rs`, workflow `store.rs`, and
`completion_protocol_v2.rs`; this report is committed separately. No Plan,
Design, progress, Task 13 report, generated publication JSON, credential
helper, connection, or launch-snapshot change is staged.

## Commits

- `ca19622b test: prove platform completion evidence end to end`
- `61c8d238 docs: add task 18 implementer report`
- `3e8455fa fix: enforce final delivery evidence guard`
- `e23dd582 docs: finalize task 18 verification report`
- `59f5bcd9 fix: close task 18 final review gaps`
- `57bfd24cb81b735deb79c66167a6df09519bc946 fix: preserve task 18 gate lifecycle semantics`

## Review Handoff

The prior Codex and Grok request-changes findings are closed by the High fix
and fresh matrix above. Formal Task 18 approval remains a parent-workflow gate:
both independent reviewers must re-review the new unchanged tip against the
approved Design and all 31 criteria.
