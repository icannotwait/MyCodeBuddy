# Task 14 Implementer Report

## Result

Implemented protocol-v2 Plan round reduction and durable Final findings
packages. Plan settlement now derives ranks, improvement, thresholds, and the
next selected round from current platform evidence. Final non-pass completion
atomically snapshots immutable remediation bytes before Fixer admission.

## TDD Evidence

RED was established before production changes:

- `plan_review::v2_tests` failed to compile because the v2 round state,
  reducer, rank comparison, and platform next-round derivation did not exist.
- The roster-only test failed until retained sibling evidence and stagnation
  were preserved.
- `final_findings::tests` failed to compile before package, finding, context,
  digest, persistence, and corruption interfaces existed.
- The route-identity regression proved that changing durable remediation
  routes did not change the source evaluation identity.
- The persisted-state regression proved that altered evidence task/scope
  columns were accepted beside unchanged Plan state JSON.
- `task14_final_completion_mints_immutable_package_before_fixer_admission`
  resolved completion without creating a package.
- `task14_final_nonpass_without_context_opens_decision_without_package`
  resolved non-pass evidence instead of opening typed attention.
- `task14_final_artifact_recovery_keeps_pre_read_snapshot` opened artifact
  recovery but lost the pre-read Final report snapshot.

Fresh focused GREEN verification:

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
  in both paths. Passing, incomplete, new-lineage, and terminal/delete paths
  retire active stale packages.
- A complete non-pass evaluation without material context opens typed
  `CompletionDecision` attention with
  `completion_remediation_context_required`, leaves no active package, and
  cannot admit a Fixer.
- Final Fixer admission now requires one verified active current package. The
  package digest is copied to `final_findings_identity`, and the canonical
  instruction embeds stored provenance, digest, length, availability, and
  exact base64 bytes.

## Scope And Hygiene

The plan's primary file list did not include the terminal evidence boundary,
but immutable snapshot authority must be captured before that transaction
discards `final_assistant_text` and pre-read reports. Task 14 therefore also
updates `completion_evidence.rs`, `completion_intent.rs`, and `metrics.rs` for
atomic lifecycle handling and the typed reason code. Existing generic
completion projection and graph events already carry the resulting state and
revision, so no change was required in `completion_projection.rs` or
`events.rs`.

Pre-existing changes in `.superpowers/sdd/progress.md`, the Task 13 report,
`connection.rs`, `companion.rs`, `launch_snapshot.rs`, and `workflow/project.rs`
remain unstaged. Untracked `publish*.json` and manifest files also remain
unstaged. Plan and Design documents were not modified.

## Concerns

The older
`all_role_instruction_scope_admission_derives_material_from_durable_sources`
fixture manually marks protocol-v2 upstream runs complete using legacy Card
fields, so a diagnostic run fails the shared v2 validator before reaching its
Final assertions. Task 14 uses dedicated terminal-materialization fixtures and
does not broaden scope to rewrite that pre-existing cross-task harness.
