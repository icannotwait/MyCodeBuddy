# Task 13 Implementer Report

## Result

Implemented protocol-v2 Design and execution-gate reduction from validated,
platform-owned completion evidence. Added the reduced settlement boundary,
typed platform-only Design self-review, exact-current Plan recovery predicate,
and explicit v1 compatibility branches.

## TDD Evidence

Initial RED was established before production implementation:

- `completion_outcome_gates` failed to compile because `reduce_design_gate`
  and the v2 execution-outcome path did not exist.
- `design_self_review_decision` failed to compile because the typed
  Design-root decision validator and superseded error did not exist.
- The first reduced-request replay test failed at
  `assert!(replay.idempotent_replay)` because identical current evidence
  incorrectly minted another settlement cycle.

Independent review then identified additional regressions. Focused RED tests
proved that stale v2 generations and stale Final tips passed, the platform
Design root leaked into manifest node bindings, graph CAS did not rotate, and
the public workflow-v2 tool still required legacy evidence. The final review
loop also proved that unknown settlement fields were silently discarded while
the public schema remained open, and that a matching v2 Final fixer/reviewer
pair could pass while current Task branch-tip evidence was pending.

Focused GREEN verification:

- `cargo test --lib completion_outcome_gates -- --nocapture`: 5 passed.
- `cargo test --lib design_self_review_decision -- --nocapture`: 2 passed.
- `cargo test --lib external_design_gate -- --nocapture`: 1 passed.
- `cargo test --lib completion_v2_shared_validator -- --nocapture`: 8 passed.
- `cargo test --lib acp::delegation::workflow::gates::tests:: -- --nocapture`:
  35 passed.
- `cargo test --lib workflow_manifest_v2_ -- --nocapture`: 8 passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The plan's exact command without `--lib`,
`cargo test completion_outcome_gates -- --list`, was also attempted. Cargo
compiled unrelated integration targets and failed in `tests/api_integration.rs`
because `db::test_helpers`, title test hooks, and test-only `AppState`
constructors are gated behind `test-utils`. This pre-existing harness issue was
not changed; the focused library filters above compile and pass.

No full suite, Clippy, frontend suite, build, push, or PR was run. Cargo emitted
the existing warning that the packaged `codeg-mcp` sidecar is absent and a
zero-byte build placeholder was used.

## Implementation

- The workflow-v2 tool schema and wire request now expose only workflow/gate
  identity, graph CAS, optional round CAS, optional expected outcome, bounded
  summary, and recovery authorization. The schema is closed, and the companion
  rejects unknown caller fields while retaining the known v1/v2 union until
  the listener selects by persisted protocol. V2 rejects legacy
  manifest/cycle/outcome/evidence fields, while v1 retains its legacy decoder
  and store path.
- V2 settlement revalidates graph/manifest CAS, live Design bytes, active gate
  state, complete evidence identity, optional round/outcome expectations,
  replay payload, and cycle allocation in the final transaction. Exact current
  evidence replays without creating another cycle; conflicts fail closed.
- External Design reduction waits for every current reviewer and applies fixed
  precedence: `request_changes`, then `block`, otherwise approved. Legacy
  finding-count columns stay null and malformed Card data has no authority.
- Empty Design `self_review` resolves current bytes, creates or reuses only the
  dedicated platform Design-root binding, and opens typed
  `design_self_review_decision` attention. Readiness creation or lineage
  rotation advances graph CAS. Platform IDs have no delegated run or child and
  are rejected by delegation, continue, and join paths.
- A forward SQLite migration removes the obsolete foreign key from the
  dedicated Design-root table to ordinary manifest node bindings while
  retaining gate-state ownership and unique task/run CAS IDs.
- Task and Final execution gates require resolved role-legal v2 outcomes,
  exact producer task and generation, matching artifact identity, and Final
  branch-tip equality. A pending Task branch tip fails closed for both Final
  first-pass and fixer paths; `NoTasks` remains distinct. V1 keeps Card
  status/verdict and informational generation behavior.
- Recovery accepts only the latest exact-current v2 Plan approval with matching
  artifact/Author identity, every required Reviewer at rank 0, and no open
  completion-family attention. Legacy counts, Cards, `summary_validated`, and
  fingerprints are excluded.

## Scope And Hygiene

Task 13 reuses the earlier attention, workflow type, and DTO schemas, so no
change was required in `attention.rs`, `workflow/types.rs`, or
`workflow/state_dto.rs`. Interface corrections required focused changes to the
companion parser, transport, tool schema, listener, and one forward migration.

Pre-existing changes in `.superpowers/sdd/progress.md`, `connection.rs`,
`launch_snapshot.rs`, and formatting-only hunks in `companion.rs`,
`listener.rs`, and `project.rs` remain user-owned and unstaged. Untracked
`publish*.json` and manifest JSON files also remain unstaged. Plan and Design
documents were not modified.

## Concerns

The repository's default filtered Cargo invocation currently compiles
integration tests without their required `test-utils` feature. Task 13 uses
the equivalent `--lib` filters to remain focused and avoid changing unrelated
test infrastructure.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Task 13: v2 Design/execution gates reduce from validated platform evidence; Design self-review CAS; null legacy counts; Plan recovery exact; v1 preserved.","commits":[{"sha":"6b9064af2c5fd21582d002bda362c57191edb8b4","subject":"feat: reduce completion gates from platform evidence"}],"tests":{"status":"pass","passed":1,"failed":0,"summary":"focused Task 13 filters passed; see report"},"concerns":[],"report_file":".superpowers/sdd/task-13-implementer-report.md"}
-->

## Fix Review Loop: T13-CODEX-I1

The independent Codex review found that the closed public MCP schema exposed
only the reduced v2 settlement shape even though durable protocol-v1 workflows
still require `manifest_revision`, `gate_cycle`, `outcome`, and `evidence`.

Focused RED evidence:

- `cargo test --lib workflow_manifest_v2_schema_is_compact_and_constructible
  -- --nocapture` failed with `settlement schema omits manifest_revision`.

The fix keeps `additionalProperties: false` on the shared settlement object,
restores the legacy properties, and exposes two explicit schema arms: a
complete v1 legacy arm and a v2 arm with no legacy property names. The listener
continues selecting the decoder exclusively from durable workflow protocol
state. The compact composition avoids duplicating common fields and preserves
the fixed MCP stdio catalog budget.

Focused GREEN verification:

- `cargo test --lib workflow_manifest_v1_ -- --nocapture`: 1 passed.
- `cargo test --lib workflow_manifest_v2_ -- --nocapture`: 8 passed.
- `cargo test --lib
  grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget
  -- --nocapture`: 1 passed; catalog size 7,602 bytes (limit 7,680).
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

No full suite, Clippy, build, frontend test, push, or PR was run.
