# Task 4 Report: Mutation, Recovery, Delivery, and Root Fences

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX/GROK REVIEW PENDING**

- Work unit: `task|4|implementer|codex|none`
- Scope: Plan Task 4 only
- Task 5: not started

Task 4 now rejects every covered semantic mutation unless the durable workflow
header is exactly `(2, v2_enforce)`. Historical version-1 workflows return
`legacy_completion_protocol_read_only`; inconsistent and corrupt headers return
`unsupported_completion_protocol`; caller ownership errors remain
`unauthorized` without exposing the target protocol.

## Implementation

- Added typed `load_completion_protocol_header` and
  `load_completion_protocol_for_conversation` loaders.
- Added `UnsupportedCompletionProtocolHeader` and narrowly mapped only
  SeaORM `DbErr::Type` / `DbErr::TryIntoErr` header-decode failures to
  `unsupported_completion_protocol`. Connection, query, execution, and other
  infrastructure failures retain persistence classification and existing
  retry behavior.
- Applied `require_v2_mutation` before and inside publication, v2 settlement,
  state-only revisions, workflow recovery, completion decisions, Design
  self-review, artifact retry/resolution, Final delivery, and `complete_work`.
- Resolved completion authority from durable attention/task bindings rather
  than caller-provided CAS task fields.
- Fenced recovery-authorization preparation for workflow subjects and
  workflow-bound task subjects before authorization/question/attention writes;
  standalone task behavior remains unchanged.
- Replaced linked-root legacy restart admission with one manager-owned,
  read-only protocol preflight before hydration, transcript/status/route
  writes, events, process launch, or prompt enqueue. Foreground, background,
  automation, and chat paths converge on this manager boundary.
- Preserved historical MCP workflow-state projection while preventing the
  Final-delivery mutation that normally accompanies current v2 reads.
- Added stable-code-preserving completion, command, ACP, and listener error
  mappings.
- Added five-pair negative matrices, corrupt-header fixtures, no-side-effect
  snapshots, cross-parent checks, root prompt checks, recovery authorization,
  completion, Final delivery, and replay regressions.

Automation, chat-channel, Tauri, and Axum files did not require separate
production changes: they already delegate to the manager or shared command
cores fenced above.

## TDD Evidence

RED was observed before the corresponding production changes:

- Historical recovery returned generic `workflow_invalid`.
- Historical and inconsistent completion/final-delivery mutations reached
  legacy paths or returned the wrong stable code.
- Linked root prompts returned the retired restart-required error and could
  reach the previous admission path.
- Corrupt header decoding escaped as generic `workflow_invalid`.
- Typed error variants and loaders initially failed to compile.
- Cross-parent historical mutation exposed protocol classification instead of
  returning `unauthorized`.
- Historical MCP state reads were incorrectly rejected instead of projected.

GREEN was then observed for the five-pair mutation matrices, corrupt-header
fences, typed DB classification, recovery authorization, root prompt
foreground/background paths, historical read projection, stable command codes,
and recovery replay/conflict behavior.

## Verification

- `rustfmt --edition 2021 --check` over all ten touched Rust files: pass.
- `git diff --check`: pass before staging.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils`
  - Pass: 30 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils historical_protocol_mutation_matrix`
  - Pass: 2 passed, 0 failed.
- Focused library filters for `recovery_authorization_protocol_fence`,
  `header_db_error_classification`,
  `completion_protocol_mutations_preserve_stable_app_error_codes`,
  `typed_completion_attention_artifact_retry_is_typed_and_records_scope_invalidation`,
  `publish_workflow_reaches_v2_store_guard_without_rollout_selection`,
  `same_direct_parent_reply_replay_is_idempotent_and_conflict_is_already_resolved`,
  and `exact_replay_returns_original_revision_and_different_correlation_conflicts`
  all passed: 7 selected, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`: pass.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings`: pass.
- `git diff --cached --check`: pass before the producer commit.

The broader library run made during implementation reported `4171 passed; 100
failed; 1 ignored`. The failures are existing Task 2/3 fixture debt: v2
workflows sent through the test-only legacy settlement adapter, duplicate
Design gate-state rows now created automatically by fixed-v2 publication, and
their downstream cascades. The known touched test
`typed_completion_attention_design_self_review_is_typed_and_replayable` fails
at that pre-existing duplicate-key fixture before reaching Task 4 behavior.
This task did not broaden into fixture migration work.

Cargo also emitted the existing local packaging warning that the ignored
`codeg-mcp` sidecar is a zero-byte placeholder. It did not affect compilation
or tests and is not part of the producer diff.

## Producer Commit

- `7b826557fe38fca115dfadd65c10b2eb0da54abf` -
  `fix: fence legacy workflow mutations`

## Conclusion

done

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Fenced all Task 4 workflow mutation, recovery-authorization, Final-delivery, complete_work, and linked-root prompt paths with the exact protocol-v2 guard; added narrow typed corrupt-header mapping and no-side-effect negative matrices.","commits":[{"sha":"7b826557fe38fca115dfadd65c10b2eb0da54abf","subject":"fix: fence legacy workflow mutations"}],"tests":{"status":"passed","passed":39,"failed":0,"summary":"Thirty integration tests, two completion/complete_work matrices, seven focused regressions, rustfmt, cargo check, strict all-target Clippy, and cached diff checks passed."},"concerns":["A broader library run remains non-green because of 100 pre-existing Task 2/3 fixture failures involving the test-only legacy settlement adapter and duplicate fixed-v2 Design gate-state inserts; scoped Task 4 verification is green.","Independent Codex and Grok review is still pending."],"report_file":".superpowers/sdd/task-4-report.md"}
-->

## Fix Round 1

Both independent reviewers requested changes on producer `7b826557`. Fix round
1 closes all three Important findings in a new producer commit:

- `T4-GROK-I1`: removed the automatic legacy restart from MCP
  `process_recover_workflow`. Recovery now enters the fenced store core,
  returns `legacy_completion_protocol_read_only` for a historical workflow,
  and creates no successor. A listener-level regression covers this production
  boundary.
- `T4-CODEX-I1`: changed
  `load_completion_protocol_for_conversation` to resolve the
  conversation-owned workflow plus every workflow referenced by every durable
  child-run binding across generations. Distinct workflow ids are loaded in
  stable order; any missing or corrupt authoritative header fails closed, and
  rejection precedence is deterministic (`unsupported` before `legacy` before
  allowed). Regressions cover a latest unbound generation masking an older v1
  binding and an owned v2 workflow conflicting with a bound v1 workflow.
- `T4-CODEX-I2`: made the Design self-review preflight transaction load and
  validate the typed protocol header before decoding the full workflow model.
  Direct and nested completion protocol errors preserve their stable,
  non-retryable classification; unrelated completion errors remain persistence
  failures. The corrupt-mode race regression asserts
  `unsupported_completion_protocol` and zero graph, gate, binding, attention,
  or other semantic writes.

### Fix-Round TDD Evidence

RED was observed before each fix:

- MCP recovery returned a successor projection instead of the read-only error.
- The multi-generation loader returned `None` when a latest unbound run masked
  an older binding, and returned `(2, v2_enforce)` when an owned v2 association
  masked a bound v1 association.
- Concurrent corrupt-mode Design preflight returned retryable
  `workflow_persistence_failure`.
- The nested completion-error mapping regression initially failed to compile
  because no structural mapper existed.

GREEN was then observed for the MCP recovery boundary, both multi-association
loader cases, the corrupt-mode Design race, and the structural nested-error
mapping.

### Fix-Round Verification

- Full `completion_protocol_v2` integration target: 32 passed, 0 failed.
- MCP recovery listener regression: 1 passed, 0 failed.
- Multi-association loader regressions: 2 passed, 0 failed.
- Design preflight regressions and nested protocol mapper: passed.
- Historical completion and `complete_work` matrices: 2 passed, 0 failed.
- Recovery-authorization fence: 1 passed, 0 failed.
- Header database-error classification: 1 passed, 0 failed.
- `cargo check`: passed.
- Strict all-target Clippy: passed.
- Rustfmt over the three fix-round Rust files and cached/working diff checks:
  passed.

The optional reviewer minors were deferred. Root companion auto-restart belongs
to the later admission/restart-removal work; automation, chat, Tauri, Axum, and
expanded snapshot coverage were not broadened into this focused Important-fix
round. The existing broader Task 2/3 fixture debt remains unchanged. Task 5 was
not started.

### Fix-Round Producer Commit

- `3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c` -
  `fix: close workflow mutation fence gaps`

Independent Codex and Grok re-review is pending.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done","summary":"Closed the three Task 4 Important review findings: MCP recovery no longer auto-restarts legacy workflows, root protocol lookup scans every durable association deterministically and fails closed, and Design preflight preserves typed corrupt-header classification before full-model decoding or writes.","commits":[{"sha":"3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c","subject":"fix: close workflow mutation fence gaps"}],"tests":{"status":"passed","passed":40,"failed":0,"summary":"The 32-test completion_protocol_v2 target and eight focused listener/store/completion regressions passed, followed by cargo check, strict all-target Clippy, rustfmt, and diff checks."},"concerns":["The broader library suite still has pre-existing Task 2/3 fixture debt outside this fix-round scope.","Independent Codex and Grok re-review is pending.","Optional root companion, cross-entry harness, and expanded snapshot minors are deferred to their owning later work or a dedicated test expansion."],"report_file":".superpowers/sdd/task-4-report.md"}
-->
