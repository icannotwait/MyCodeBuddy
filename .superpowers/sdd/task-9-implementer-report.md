# Task 9 Implementer Report

## Identity

- Work unit: `task|9|implementer|codex|none`
- Task: Canonicalize Evidence Scope and Bind All-Role Instructions
- Required commit subject: `feat: canonicalize completion evidence scope`
- Fix-loop commit subject: `fix: reject floats before scope canonicalization`
- Task 8 baseline: `494f33a2`
- Preserved newer unrelated HEAD commit: `896b5a59 docs: design parent orchestration liveness guard`
- Plan digest: `sha256:965289a2cf8727725a55e3d896406d446f90203059ea5ca6f89979acb8302820`
- Design digest: `sha256:8e9f2555366aed2fcb0afdf801d9a1ddd13cbf4e562d9309957e98e51a855914`

## Implementation

- Added bounded canonical JSON hashing with sorted keys, explicit nulls, NFC strings,
  normalized paths, duplicate-key rejection, domain separation, and fixed lowercase
  SHA-256 tokens.
- Added a recursive pre-serialization `Serialize` validation pass that rejects every
  `f32` and `f64` value before `serde_json` can coerce NaN or either infinity to
  explicit JSON null. This follows Task 9 Step 4's all-floats rejection policy, so
  finite floats are rejected as well as non-finite floats.
- Added 19 fixed golden vectors covering requirements, task/Plan/Final identities,
  typed role scopes, and production instruction bindings.
- Added v2 completion evidence, evidence scope, role review scope, instruction,
  requirements identity, admission context, and validation types.
- Built all delegated role scopes from durable workflow state. Plan roles consume one
  bounded workspace-contained Plan read and Task 8 material parsing/selectors.
- Persisted exact instruction bytes and semantic scope identities during real
  `RunStore` admission, then reloaded and appended those exact bytes before dispatch.
- Added the reusable Design Root canonical scope builder while leaving Task 13
  readiness/persistence ownership unchanged.
- Enforced current Approved Plan authority, exact Plan settlement lineage/round/scope,
  current Final lineage, current-lineage Fixer selection, and clean current-HEAD
  fallback for first review of a new Final lineage.
- Preserved typed Plan/instruction binding failures through gen-1 and continuation
  Broker settlement.
- Added the shared strict v2 completion evidence validator with unknown-field,
  role/outcome, identity, artifact, and current-scope checks.

## TDD Evidence

RED then GREEN cycles were observed for:

- Missing fixed instruction vectors and typed role scope vectors.
- Missing Approved Plan authority and Final lineage binding.
- Duplicate canonical keys and missing `covered_plan_digest` authority.
- Missing Design Root scope construction.
- Real admission and exact instruction append for all seven delegated roles.
- Legacy artifact fixtures that omitted complete v2 Plan authority.
- Final Reviewer selecting a Fixer from a stale Final lineage.
- Final Reviewer fallback incorrectly using the older Task artifact instead of clean
  current `HEAD` after a prior-lineage Fixer commit.
- Missing Final gate state using the wrong stable construction-failure code.
- Broker prompt binding failures losing Task 9 Plan/instruction error codes.
- The canonicalizer accepting NaN and both infinities after `serde_json` erased their
  float type by serializing them as explicit null. The fix-loop RED run showed all
  three new tests fail: the primitive `f32`/`f64` cases accepted NaN and both
  infinities, and nested non-finite values serialized as JSON null. The GREEN run
  rejects finite, NaN, positive-infinity, and negative-infinity cases for both float
  widths before JSON serialization, including recursively nested values.

The final focused source-state commands listed tests before execution and passed:

- `cargo test --lib --features test-utils evidence_scope::tests -- --list`: 19 listed.
- `cargo test --lib --features test-utils evidence_scope::tests -- --nocapture`:
  19 passed, 0 failed.
- `cargo test --lib --features test-utils all_role_instruction_scope_admission -- --list`:
  2 listed.
- `cargo test --lib --features test-utils all_role_instruction_scope_admission -- --nocapture`:
  2 passed, 0 failed.
- `cargo test --lib --features test-utils gen1_and_continuation_instruction_failures_preserve_admission_codes -- --list`:
  1 listed.
- `cargo test --lib --features test-utils gen1_and_continuation_instruction_failures_preserve_admission_codes -- --nocapture`:
  1 passed, 0 failed.
- `cargo test --lib --features test-utils completion_artifact_contract -- --list`:
  8 listed.
- `cargo test --lib --features test-utils completion_artifact_contract -- --nocapture`:
  8 passed, 0 failed.

Focused total: 30 passed, 0 failed.

## Additional Verification

- `cargo check --features test-utils`: passed.
- `cargo check --no-default-features --features server --bin codeg-server`: passed.
- `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`:
  passed.
- `cargo clippy --all-targets --features test-utils -- -D warnings -A clippy::too-many-arguments`:
  passed after checking all targets.
- Targeted `rustfmt --check --edition 2021` for all Task 9 Rust files: passed.
- Authorized-file `git diff --check`: passed.
- Golden fixture parses as `CompletionScopeVectorsV1` with 19 vectors.
- Plan and Design SHA-256 hashes match the approved fixed digests; neither file has a diff.

The Plan's literal `cargo test <filter>` command without `test-utils` was also attempted.
It is blocked before Task 9 tests by the unrelated integration test
`tests/tool_watchdog_lifecycle.rs`, which imports the feature-gated
`db::test_helpers`. The documented `--lib --features test-utils` form above provides
the focused Task 9 evidence without compiling unrelated integration targets.

The exact desktop clippy command without an allow remains blocked only by the
pre-existing `clippy::too_many_arguments` warning on
`src/commands/conversations.rs::get_folder_conversation`. No Task 9-owned clippy
warnings remain. Cargo also prints the existing missing `codeg-mcp` sidecar warning;
this does not affect checks or tests.

## Review Resolution

The read-only Task 9 reviewer raised no Critical findings. All Important findings were
resolved, including Approved Plan settlement authority, single contained Plan reads,
Design Root construction, real all-role admission, duplicate-key rejection, stale
Final Fixer lineage filtering, clean current-HEAD Final fallback, fail-closed Final
gate identity, typed Broker error propagation, and pre-serialization rejection of
finite and non-finite `f32`/`f64` values. Final Fixer remediation-context bytes remain
intentionally deferred to Task 14 by the approved Plan.

## Scope And Exclusions

Only Task 9-owned production/test files and this report are intended for staging.
The following existing workspace changes are explicitly excluded:

- `.superpowers/sdd/progress.md`
- `src-tauri/src/acp/delegation/workflow/project.rs`
- `src-tauri/src/acp/connection.rs`
- `src-tauri/src/acp/delegation/companion.rs`
- `src-tauri/src/acp/delegation/launch_snapshot.rs`
- `full-approved-manifest.json`
- `publish*.json`

No push, merge, PR, Plan edit, or Design edit was performed.
