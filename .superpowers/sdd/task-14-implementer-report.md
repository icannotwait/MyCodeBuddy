# Task 14 Implementer Report

## Result

Implemented protocol-v2 Plan round reduction and durable Final findings
packages. Plan settlement now derives ranks, improvement, thresholds, and the
next selected round from current platform evidence. Final non-pass completion
atomically snapshots immutable remediation bytes before Fixer admission.

The consolidated High dual-review fix closes all six deduplicated Important
findings from Codex and Grok. Localized Plan corrections now consume the
trusted material classifier, holistic rewrites mint a fresh full-cohort
lineage, and Final package identity, immutable snapshots, and lifecycle
transitions are derived from complete platform evidence.

## TDD Evidence

RED was established before production changes:

- `plan_review::v2_tests` failed to compile because the v2 round state,
  reducer, rank comparison, and platform next-round derivation did not exist.
- The roster-only test failed until retained sibling evidence and stagnation
  were preserved.
- `final_findings::tests` failed to compile before package, finding, context,
  digest, persistence, and corruption interfaces existed.
- The route-identity regression proved that changing durable remediation
  routes does not change the source evaluation identity while it does change
  the package digest.
- The persisted-state regression proved that altered evidence task/scope
  columns are rejected beside unchanged Plan state JSON.
- `task14_final_completion_mints_immutable_package_before_fixer_admission`
  resolved completion without creating a package.
- `task14_final_nonpass_without_context_opens_decision_without_package`
  resolved non-pass evidence instead of opening typed attention.
- `task14_final_artifact_recovery_keeps_pre_read_snapshot` opened artifact
  recovery but lost the pre-read Final report snapshot.

Consolidated review-fix RED evidence:

- `cargo test --lib
  plan_round_v2_hits_rewrite_then_user_decision_after_two_stagnant_rounds --
  --nocapture` accepted a same-lineage round after a holistic rewrite was
  required.
- `cargo test --lib task14_fix_ -- --nocapture` initially failed both lifecycle
  regressions: incomplete Final evaluation resolved an Active package, and
  explicit terminal cleanup left an Active package when no attention row
  existed.
- `cargo test --lib
  final_source_evaluation_identity_excludes_durable_routes -- --nocapture`
  initially changed the source key when only durable routing changed.

Fresh focused GREEN verification:

- `cargo test --lib task14_fix_ -- --nocapture`: 4 passed, including localized
  Plan classifier selection, incomplete/terminal package lifecycle, and an
  earlier Final Reviewer's immutable terminal snapshot.
- `cargo test --lib
  final_source_evaluation_identity_excludes_durable_routes -- --nocapture`: 1
  passed.
- `cargo test --lib plan_review::v2_tests -- --nocapture`: 4 passed.
- `cargo test --lib final_findings::tests -- --nocapture`: 6 passed.
- `cargo test --lib task14_final_ -- --nocapture`: 3 passed.
- `cargo test --lib task14_v2_plan_state_replay_rejects_evidence_column_drift
  -- --nocapture`: 1 passed.
- `git diff --check`: passed.

No full suite, Clippy, frontend test, build, push, or PR was run. Task 18/Final
owns broad verification. Cargo emitted the existing warning that the packaged
`codeg-mcp` sidecar is absent and a zero-byte build placeholder was used.

## Implementation

- Added `PlanReviewRoundStateV2` and platform-derived Reviewer outcomes with
  fixed ranks: pass `0`, request changes `1`, block `2`.
- Strict improvement rejects new blockers and rank increases and requires at
  least one prior blocker to improve. Initial/new-lineage, corrective,
  roster-only, and holistic rewrite transitions are validated separately.
- Two stagnant corrective rounds require a holistic rewrite. A single rewrite
  resets stagnation; two more stagnant rounds require user decision. Roster
  changes preserve sibling evidence and do not advance stagnation.
- The next Plan round is platform-incremented and selects all current non-pass
  Reviewers plus explicitly intersecting passing Reviewers. Settlement stores
  lineage, round, required/selected nodes, evidence task/scope identities,
  Plan/Author identity, improvement, thresholds, localized digest, and state
  JSON. Legacy findings and counts remain outside v2 authority.
- Added canonical `FinalFindingsPackageV1` items with platform finding IDs,
  Reviewer evidence identity/outcome, target work units, and remediation route
  IDs. Items, targets, and routes are canonicalized before hashing.
- Report and terminal contexts are stored as bounded base64 snapshots with
  exact byte length and SHA-256. Package decoding, admission, and terminal
  settlement verify base64, length, digest, package digest, and source task
  identity without rereading mutable paths.
- Final Reviewer completion derives the complete current evaluation before
  artifact resolution. A non-pass package is committed atomically with either
  resolved evidence or artifact-recovery attention, preserving pre-read bytes
  in both paths. Incomplete evaluation is a package no-op; passing evaluation,
  complete supersession, and explicit terminal/delete paths retire Active
  packages.
- A complete non-pass evaluation without material context opens typed
  `CompletionDecision` attention with
  `completion_remediation_context_required`, leaves no active package, and
  cannot admit a Fixer.
- Final Fixer admission now requires one verified active current package. The
  package digest is copied to `final_findings_identity`, and the canonical
  instruction embeds stored provenance, digest, length, availability, and
  exact base64 bytes.

## Consolidated Review Fixes

- `T14-CODEX-I1` / `T14-GROK-I1`: settlement captures bounded prior/current
  Plan snapshots, calls `classify_plan_change`, uses
  `select_corrective_reviewers`, persists the exact authorized
  `PlanLocalizedChangeV2`, and freshness validation consumes that proof.
- `T14-CODEX-I2` / `T14-GROK-I1`: `HolisticRewriteRequired` deterministically
  opens round 1 on a new lineage with the full cohort. The reducer accepts that
  transition only after the rewrite action and preserves the one-rewrite
  history used by later stagnation thresholds.
- `T14-CODEX-I3`: `source_evaluation_key` now hashes active Final requirements,
  evaluator graph revision, and every ordered Reviewer node/task/scope/outcome
  tuple. Finding targets and routes remain package-digest material only.
- `T14-CODEX-I4`: every Final Reviewer terminal transaction stores its bounded
  report/terminal context snapshot on the evidence task run. Later package
  assembly loads and verifies those immutable bytes instead of rereading prior
  workspace files.
- `T14-CODEX-I5`: explicit workflow terminal/delete cleanup resolves all Active
  Final packages atomically, including the no-open-attention case, with one
  graph-revision bump.
- `T14-GROK-I2`: incomplete multi-Reviewer Final evaluation no longer mutates
  package lifecycle state.
- `T14-CODEX-M1`: this report now records the corrected source-key and replay
  drift assertions.

## Scope And Hygiene

The plan's primary file list did not include the terminal evidence boundary,
but immutable snapshot authority must be captured before that transaction
discards `final_assistant_text` and pre-read reports. Task 14 therefore also
updates the terminal evidence boundary, run store, artifact resolver, task-run
entity, and migration set for atomic lifecycle handling and immutable
per-Reviewer context storage. Existing generic completion projection and graph
events already carry the resulting state and revision, so no change was
required in `completion_projection.rs` or `events.rs`.

Pre-existing changes in `.superpowers/sdd/progress.md`, the Task 13 report,
`connection.rs`, `companion.rs`, `launch_snapshot.rs`, and `workflow/project.rs`
remain unstaged except for the single required task-run fixture field in
`workflow/project.rs`. Untracked `publish*.json` and manifest files also remain
unstaged. Plan and Design documents were not modified.

## Concerns

The older
`all_role_instruction_scope_admission_derives_material_from_durable_sources`
fixture manually marks protocol-v2 upstream runs complete using legacy Card
fields, so a diagnostic run fails the shared v2 validator before reaching its
Final assertions. Task 14 uses dedicated terminal-materialization fixtures and
does not broaden scope to rewrite that pre-existing cross-task harness.
