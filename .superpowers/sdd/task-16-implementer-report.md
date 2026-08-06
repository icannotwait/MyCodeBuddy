# Task 16 Implementer Report

## Result

Implemented bounded, durable completion projections and carried the same
completion truth through Tauri, Axum, WebSocket events, ACP status/history,
`codeg-mcp`, and the desktop/server frontend. Direct adjudication uses the
exact six-field CAS, completion events trigger an authoritative snapshot
reload, and legacy workflows expose linked restart and root-owned resume
controls without continuing or replacing a terminal child.

The delegation settings surface now shows server-owned creation modes,
profile overrides, per-profile rollout windows and decisions, minimum-sample
progress, and bounded shadow/enforce counters. All ten locales have the same
completion key set with localized strings.

## TDD Evidence

Task 16 began with the RED test commit `61c774e1` (`test: cover completion
projection parity`). Those tests established missing backend projection and
transport fields, exact-CAS UI behavior, event replay/refetch behavior,
legacy restart and root-resume controls, rollout visibility, and locale key
parity before the production implementation was added.

During resumed verification, the no-default `codeg-mcp` target produced two
additional RED results:

- Shared restart authorization failed to compile because
  `unauthorized_context_error` was incorrectly gated to `tauri-runtime`.
- After that compile boundary was fixed, the one-write 7,680-byte JSONL test
  reached execution and failed because its synthetic workflow-state fixture
  omitted the required server-owned completion-protocol projection.

The minimal GREEN changes removed the feature gate from the dependency-free
shared error helper and supplied a valid v2-enforce projection to the MCP
fixture. The exact no-default test then passed.

Fresh focused GREEN evidence on the formatted staged tree:

- `cargo test --lib completion_projection::tests -- --list`: 2 tests listed.
- `cargo test --lib completion_projection::tests -- --nocapture`: 2 passed.
- `cargo test --features test-utils --test completion_transport_parity
  projection -- --list`: 1 test listed.
- `cargo test --features test-utils --test completion_transport_parity
  projection -- --nocapture`: 1 passed.
- `cargo test --features test-utils --test completion_transport_parity
  restart -- --list`: 1 test listed.
- `cargo test --features test-utils --test completion_transport_parity
  restart -- --nocapture`: 1 passed.
- `cargo test --lib typed_completion_attention -- --nocapture`: 10 passed.
- Corrupt open-attention fail-closed projection: 1 passed.
- Design self-review durable/state projection: 1 passed.
- Listener, status, and MCP durable projection parity: 1 passed.
- `cargo test --lib get_workflow_state_ -- --list`: 15 tests listed.
- `cargo test --lib get_workflow_state_ -- --nocapture`: 15 passed,
  including the fixed 7,680-byte response budget.
- `cargo test --no-default-features --bin codeg-mcp
  write_response_emits_one_complete_jsonl_write -- --nocapture`: 1 passed.
- Focused Vitest slice: 10 files and 223 tests passed. This included the six
  planned UI/store/locale files plus API transport and conversation-root
  wiring coverage.
- Focused ESLint: 0 errors. It reports one pre-existing unused `_isActive`
  warning in `message-list-view.tsx`; Task 16 did not introduce that binding.
- Targeted Prettier check, `cargo fmt --check`, `git diff --cached --check`,
  and `git diff --check`: passed.

The plan's unqualified `cargo test completion_projection::tests` form was
also attempted. Cargo compiled unrelated integration targets without their
required `test-utils` feature and failed before test discovery. Adding
`--lib` preserved the intended focused unit scope and produced the nonzero
listing and passing result above.

## Review Closure

The first independent review reported one Critical, eight Important, and one
Minor issue. The implementation closes them as follows:

- Completion events are now revision/refetch clocks only; the browser never
  synthesizes validated evidence or clears artifact-recovery authority.
- Projection loading strictly validates typed attention payload, current
  task/run/binding, scope, kind, role, and all six CAS fields.
- Corrupt projection data propagates through graph and workflow state instead
  of being converted to apparent absence.
- Legacy restart uses the Tauri-compatible top-level camelCase argument while
  the Rust request accepts the same shape in Axum.
- Completion, artifact retry, and Design self-review mutation responses reload
  the committed durable projection after the transaction.
- Manual root resume reloads the installed snapshot and stops if automatic
  wake or another durable transition made the fallback obsolete.
- Rollout UI renders independent profile windows, all decisions, creation
  counts, and shadow counters without aggregating unlike windows.
- All non-English completion strings are translated, and unbroken summaries
  and report paths wrap within compact overlays.
- The static MCP workflow-state response retains its fixed 7,680-byte budget.
  Status task IDs remain arbitrary fan-out as documented by the existing
  status contract; Task 16 does not introduce an unauthorized count cap.

A follow-up task review confirmed spec compliance and closure of listener/MCP
corruption parity plus the persisted report-path bound, then identified shared
workflow context still being rebuilt during each resolved-node validation. A
new RED assertion observed one redundant active-manifest load (`(1, 1)` rather
than `(0, 1)` for manifest/requirements preparation). The GREEN change passes
the caller's normalized manifest into the completion batch and preloads the
requirements identity once per workspace. Node-specific gate, scope, and
current-artifact validation remains mandatory for every `evidence_validated`
card; its result is reused for both projection and gate reduction.

Focused RED -> GREEN coverage now exercises resolved graph and workflow-state
paths directly. Test-only counters first failed at `0 != 1`; after wiring the
instrumentation, both tests proved zero standalone projection loads, zero
terminal-row reloads, exactly one durable validation for the projected node,
and a resolved card with `evidence_validated: true`. The fix agent's independent
read-only review reported no Critical, Important, or Minor findings. The final
task re-review then examined the post-preload staged package and returned
`Compliant` for spec compliance and `Approved` for code quality, again with no
findings at any severity.

## Scope And Hygiene

The plan's primary file list omitted supporting DTO/store modules and the
conversation/message bridge required to expose root-resume controls on the
actual session surface. Those Task 16 dependencies are included in the staged
implementation.

Pre-existing changes in `.superpowers/sdd/progress.md`, the Task 13 report,
`src-tauri/src/acp/connection.rs`, and
`src-tauri/src/acp/delegation/launch_snapshot.rs` remain unstaged. Untracked
publication and approved-manifest JSON files also remain unstaged. Plan and
Design documents were not modified.

## Concerns

No full suite, build, or Clippy run was performed because Task 16 explicitly
requires focused tests; Task 18 owns repository-wide verification. Cargo
emitted the existing warning that the packaged `codeg-mcp` sidecar is absent
and a zero-byte build placeholder was used.
