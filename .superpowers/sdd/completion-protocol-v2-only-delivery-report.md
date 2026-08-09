# Completion Protocol V2-Only Delivery Report

## Delivery Identity

- Work unit: `task|11|implementer|codex|none`
- Branch: `feat/completion-protocol-v2-only`
- Clean Task 10 baseline: `e38cf90974da66af2c8a2d32f48fa4c329a7451a`
- Task 1 producer: `017954713566ddcbfd274f099055ddce022e2d01`
- Implementation base: `190c1e141de84460c01130b4da42713a14362759`
- Prior rejected Final candidate: `a813a60a955e560cecea14ed5c7b4477eedc211f`
- Reviewer-requested fixture repair: `c4ec7aa3ff5d0a438aad19476c186fdb19d80b25`
- Range before this evidence update: 35 commits, 89 files, 12,021 insertions,
  and 6,604 deletions from the implementation base through the repair commit
- Approved design:
  `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md`
- Verified design SHA-256:
  `61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`
- Plan: `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md`
- Verified plan SHA-256:
  `e59e90636265fe6f11c284a1da5e09d5752b04db25c42b142ad3981aaeb15255`
- Risk policy: `b2d_task_risk_v1`; Task 11 is high risk with independent
  Codex and Grok Final reviewers

The new Final candidate is the commit containing this report. Its hash is
resolved only after the evidence commit and a clean-porcelain check, avoiding a
self-referential commit hash inside the committed report.

## Final Review Repair

Both prior Final reviewers identified the same Important gate failure:
`T11-CODEX-I1` / `T11-GROK-I1`. The required desktop and server library
commands had 103 failing tests because Task 2/3 fixtures still wrote legacy
settlement authority into fixed-v2 workflows. The exact 103-test inventory is
retained in `.superpowers/sdd/task-11-report.md`.

Repair commit `c4ec7aa3` makes the fixtures exercise the fixed-v2 contract:

- Test workflows use real Git workspaces, verified Design/Plan bytes, v2
  admission, terminal materialization, and durable completion evidence.
- A shared test settlement adapter derives the current Plan review round and
  graph revision before calling `settle_workflow_gate_v2_core`.
- Gate-state fixtures update rows initialized by v2 publication instead of
  inserting duplicate primary keys.
- Recovery and project fixtures seed valid predecessor evidence before
  admitting and completing the next v2 work unit.
- Legacy caller findings, summaries, digests, and lineage-reset receipts remain
  non-authoritative; tests now assert fixed-v2 read-only/fail-closed outcomes.
- Invalid per-run completion evidence is omitted from the bounded workflow
  projection, while persistence failures still abort projection.
- The full-migration cascade/uniqueness tests insert current `(2, v2_enforce)`
  workflow headers so they reach the invariants they are intended to test.

## Red-To-Green Evidence

| Stage | Exact surface | Outcome |
| --- | --- | --- |
| Prior frozen baseline | Desktop library | Exit 101: 4,183 passed, 103 failed, 1 ignored. |
| Prior frozen baseline | Server library | Exit 101: 4,075 passed, 103 failed, 1 ignored. |
| First repair pass | Desktop library | Exit 101: 4,283 passed, 3 failed, 1 ignored. The final three tests had stale whole-projection error expectations. |
| Focused final assertions | Workflow store tests | Exit 0: 112 passed, including the duplicate gate-state regression and all three projection cases. |
| Final library | Desktop library | Exit 0: 4,286 passed, 0 failed, 1 ignored. |
| Final server library | Server library plus server bin unit | Exit 0: 4,178 library passed, 1 ignored; 1 server-bin test passed. |

The first full-desktop retry failed during linking because drive `D:` had zero
bytes free; MSVC reported `LNK1318` on multiple generated PDBs. This was not
counted as a passing gate. `cargo clean -p codeg` removed 69.6 GiB of stale
package artifacts. The next full run linked and exposed two stale
`delegation_workflows_migration` inserts; their focused target failed 2/4 before
the fixed-v2 header repair and passed 4/4 afterward. The final exact full
desktop command then exited 0 across every target.

## Required Command Outcomes

| Step | Command | Outcome |
| --- | --- | --- |
| 1 | Banned-symbol `rg` assertion over `src-tauri/src`, `src-tauri/tests`, and `src` | **Pass**, wrapper exit 0; `rg` returned no matches. |
| 2 | `pnpm eslint .` | **Pass**, exit 0; 0 errors and 25 existing warnings. |
| 2 | `pnpm test` | **Pass**, exit 0; 346 files and 5,083 tests passed. Expected mocked-error diagnostics remained non-failing output. |
| 2 | `pnpm build` | **Pass**, exit 0; Next.js 16.1.6 compiled, type-checked, and statically generated 33/33 pages. |
| 3 | `cargo check --manifest-path src-tauri/Cargo.toml` | **Pass**, exit 0. The known missing `codeg-mcp` sidecar warning wrote only an ignored placeholder. |
| 3 | `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils` | **Pass**, exit 0; 4,286 passed, 0 failed, 1 ignored out of 4,287. |
| 3 | `cargo test --manifest-path src-tauri/Cargo.toml --features test-utils` | **Pass**, exit 0; aggregate target summaries contain 4,446 passed, 0 failed, 1 ignored, including all library, binary, integration, and doc-test targets. |
| 3 | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings` | **Pass**, exit 0; no Clippy diagnostics. |
| 4 | `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server` | **Pass**, exit 0. |
| 4 | `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib` | **Pass**, exit 0; 4,178 library tests passed, 1 ignored, and the server-bin unit test passed 1/1. |
| 4 | `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib -- -D warnings` | **Pass**, exit 0; no Clippy diagnostics. |
| 5 | `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp` | **Pass**, exit 0. |
| 5 | `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp -- -D warnings` | **Pass**, exit 0; no Clippy diagnostics. |

## Supplementary Coverage

| Command | Outcome |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils acp::delegation::workflow::store::tests` | **Pass**, 112/112. |
| `cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test delegation_workflows_migration` | **Pass**, 4/4. |
| `cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test completion_protocol_v2 --test completion_protocol_migrations --test completion_transport_parity` | **Pass**, 27/27, 12/12, and 10/10. |

## Scope Review

- The Task 11 repair from prior frozen candidate `a813a60a` to `c4ec7aa3`
  changes nine already-owned files: eight completion-protocol modules and one
  workflow migration test. This evidence update changes only the two requested
  Task 11 reports.
- Large Rust diffs are fixture/helper migrations under tests or `#[cfg(test)]`.
  The only production behavior change is the bounded projection rule described
  above; no new completion-protocol abstraction or selection surface was added.
- `git diff --check` passes for the repair and report worktree. The complete
  implementation range still reports two pre-existing Markdown hard-break
  spaces in immutable reviewer evidence:
  `task-1-review-grok-report.md:81` and
  `task-4-review-grok-report-r2.md:98`. Those reports were not rewritten.
- No file is deleted in the implementation range. No tracked `target`, `.next`,
  `out`, coverage, executable, library, or PDB artifact is present.
- The only database migration change in the complete range remains the new
  forward v2-only trigger migration and its registration. No prior migration or
  entity was rewritten by this repair.
- Historical restart-context and predecessor/successor read paths remain
  present; restart writers, rollout selection, shadow comparison, and settings
  surfaces remain absent.

## Acceptance Checklist

- [x] New workflow creation is fixed to `(2, v2_enforce)` with no caller,
  profile, agent, environment, or rollout selection.
- [x] Desktop/server removed-configuration handling is retained; the server
  binary regression proves exit code 2.
- [x] Shared exact-pair mutation guards cover publication, settlement, Design
  self-review, recovery, root prompts, admission, complete-work, delivery, and
  terminal paths.
- [x] Dangling, missing, inconsistent, and undecodable workflow headers fail
  closed with stable protocol codes and no Card/shadow fallback.
- [x] Historical v1 and `v2_shadow` rows remain read-only and navigable with
  Cards, restart context, and predecessor/successor links preserved.
- [x] New SQLite inserts require `(2, v2_enforce)` and protocol identity fields
  are immutable; rollback affects only the new triggers.
- [x] Legacy restart, rollout, shadow, settings, transport, metric, and UI
  surfaces are absent under the final source assertion.
- [x] Remaining root workflow tools, recovery authorization, v2 intent/evidence/
  attention metrics, and `CompletionRootWakeQueue` remain present.
- [x] `complete_work`, conclusion-line, bounded-report, ambiguity, and typed user
  adjudication semantic channels remain covered.
- [x] Standalone delegation without a workflow binding retains Card display.
- [x] Frontend locale parity, historical read-only UI, v2 decisions, static
  export, and TypeScript checks pass.
- [x] Desktop, server, and MCP check/Clippy gates pass.
- [x] Required desktop library, full desktop, and server library test commands
  exit 0 with no failures.

## Residual Design Minor Results

| Design minor | Delivery result |
| --- | --- |
| `D-CODEX-M3` | **Resolved and verified.** `v2_only_aggregate_acceptance` proves a dangling terminal binding uses `unsupported_completion_protocol` identically in the durable row, wait result, and emitted event, with no Card authority. The focused v2 target passes 27/27. |
| `D-CODEX-M4` | **Resolved and verified.** The passing mutation matrix covers Design self-review, complete-work, final delivery, and inconsistent pairs `(2,v1)`, `(2,v2_shadow)`, and `(1,v2_enforce)` through the shared exact-pair guard. |
| `D-GROK-M1` | **Resolved and verified.** Historical/current projection keeps required `creation_mode` equal to persisted `completion_protocol_mode`; focused aggregate and historical projection tests pass. No new column was introduced. |

## Reviewer Input Manifest

Both independent Final reviewers receive the new frozen candidate hash, this
report, `.superpowers/sdd/task-11-report.md`, the raw command outcomes, approved
design/plan digests, the complete implementation range, repair commit
`c4ec7aa3`, and both prior Final request-change reports.

| Task | Final producer/fix commit(s) | Implementer evidence | Reviewer evidence |
| --- | --- | --- | --- |
| 1 | `017954713566ddcbfd274f099055ddce022e2d01` | `.superpowers/sdd/task-1-report.md` | `.superpowers/sdd/task-1-review-codex-report.md`; `.superpowers/sdd/task-1-review-grok-report.md` |
| 2 | `d6af10d559216ff8e7ffc900e29a65447187867d`, `74b2e5e9302b840317c7ae4600be65f1059a7405` | `.superpowers/sdd/task-2-report.md` | `.superpowers/sdd/task-2-review-codex-report.md`; `.superpowers/sdd/task-2-review-grok-report.md`; `.superpowers/sdd/task-2-review-codex-report-r2.md`; `.superpowers/sdd/task-2-review-grok-report-r2.md` |
| 3 | `83a3a73a1d85602d377922d3497a047ba041515a`, `87279ef9519b83c72ab3d59e63c02c2b18af4df9` | `.superpowers/sdd/task-3-report.md` | `.superpowers/sdd/task-3-review-codex-report.md`; `.superpowers/sdd/task-3-review-grok-report.md`; `.superpowers/sdd/task-3-review-codex-report-r2.md`; `.superpowers/sdd/task-3-review-grok-report-r2.md` |
| 4 | `7b826557fe38fca115dfadd65c10b2eb0da54abf`, `3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c` | `.superpowers/sdd/task-4-report.md` | `.superpowers/sdd/task-4-review-codex-report.md`; `.superpowers/sdd/task-4-review-grok-report.md`; `.superpowers/sdd/task-4-review-codex-report-r2.md`; `.superpowers/sdd/task-4-review-grok-report-r2.md` |
| 5 | `d145b2c2b7a1811d4c11905935227625e0849e44`, `0239f462bf33c922cefe4fbe172f881f38479aaa` | `.superpowers/sdd/task-5-report.md` | `.superpowers/sdd/task-5-review-codex-report.md`; `.superpowers/sdd/task-5-review-grok-report.md` |
| 6 | `83c27aa13a4e83383b1cfa28d615210e90e44cda` | `.superpowers/sdd/task-6-report.md` | `.superpowers/sdd/task-6-review-codex-report.md`; `.superpowers/sdd/task-6-review-grok-report.md` |
| 7 | `9cfd617f2491138b228fb38e6d80dee51610a1b4`, `8056433ae455065f25d7bc04a28585ff2f4a8081` | `.superpowers/sdd/task-7-report.md` | `.superpowers/sdd/task-7-review-codex-report.md`; `.superpowers/sdd/task-7-review-grok-report.md` |
| 8 | `1f8da1184a59f985ea510576430952be7f997a8f` | `.superpowers/sdd/task-8-report.md` | `.superpowers/sdd/task-8-review-codex-report.md`; `.superpowers/sdd/task-8-review-grok-report.md` |
| 9 | `bd011e818cec86c543744abf07df7f0e8c3ff6f5` | `.superpowers/sdd/task-9-report.md` | `.superpowers/sdd/task-9-review-codex-report.md`; `.superpowers/sdd/task-9-review-grok-report.md` |
| 10 | `f69eafcf0ae8ee6e2b7c9680f1287f05402fd71a` | `.superpowers/sdd/task-10-report.md` | `.superpowers/sdd/task-10-review-codex-report.md` |
| 11 prior Final | `a813a60a955e560cecea14ed5c7b4477eedc211f` | Prior `.superpowers/sdd/task-11-report.md` at rejected freeze | `.superpowers/sdd/task-11-review-codex-report.md`; `.superpowers/sdd/task-11-review-grok-report.md` |
| 11 repair | `c4ec7aa3ff5d0a438aad19476c186fdb19d80b25` | `.superpowers/sdd/task-11-report.md` | Independent dual Final re-review pending for the new clean HEAD |

Final reviewer verdicts are authoritative only in their platform reports/cards.
They are produced after this branch is frozen and are not committed afterward.
