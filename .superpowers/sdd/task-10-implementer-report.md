# Task 10 Implementer Report

- Work unit: `task|10|implementer|codex|none`
- Base: `1351179cf8ad02b1105f579bc2bd3cf49293d913`
- Plan digest: `sha256:965289a2cf8727725a55e3d896406d446f90203059ea5ca6f89979acb8302820`
- Design digest: `sha256:8e9f2555366aed2fcb0afdf801d9a1ddd13cbf4e562d9309957e98e51a855914`

## Implementation

- Added atomic protocol-v2 terminal materialization for tool, assistant-text,
  and report intent channels. The terminal run state, validated evidence or
  typed attention, binding projections, graph revision, and durable outbox
  event commit in one `RunStore` transaction.
- Added typed completion-decision and artifact-recovery attention opening for
  completed workflow-bound runs, including the six-field CAS envelope.
- Added durable `ArtifactRecoveryPayloadV1` replay. Retry preserves the
  normalized intent, accepts expected artifact recovery, supersedes unrelated
  scope changes, resolves attention atomically, and is idempotent after an
  `artifact_resolved` replay.
- Replaced all three legacy Card-summary preparation call sites with a frozen
  protocol branch. `v2_enforce` ignores and clears model Cards, including
  failed terminals; v1 remains authoritative; `v2_shadow` runs the bounded
  pure resolver for enum-only metrics without v2 persistence or gate effects.
- Added bounded report pre-reading for Markdown links, touched reports, and
  plain `Report: path.md` references with canonical workspace containment and
  size/count limits.
- Added a platform-only terminal completion projection and bounded completion
  metrics for resolution source/role, tool supersession, decision reason,
  artifact phase/reason, and scope invalidation dimension.

## TDD Evidence

RED was observed before implementation for the missing materializer/retry APIs,
the broker v2 preparation API, projection and metric APIs, and shadow observer.
Additional regression RED cases failed with these exact pre-fix symptoms:

- report recovery source was `AssistantConclusion` instead of `Report`;
- replay after `artifact_resolved` returned `completion_attention_invalid`;
- plain `Report: reports/review.md` produced zero pre-read candidates;
- failed v2 settlement retained `card_summary_json`.

All corresponding tests passed after the implementation changes.

## Verification

- `cargo test --lib completion_evidence::tests -- --list`: 7 tests listed.
- `cargo test --lib completion_evidence::tests -- --nocapture`: 7 passed, 0 failed.
- Projection, bounded metrics, shadow observation, plain-report pre-read, and
  v2 Card-authority guards: 5 passed, 0 failed.
- `cargo check`: passed.
- `cargo clippy --lib -- -D warnings -A clippy::too_many_arguments`: passed.
- `git diff --check`: passed.

The unmodified strict clippy command also reports the pre-existing
`clippy::too_many_arguments` finding at
`src-tauri/src/commands/conversations.rs:1867`; Task 10 introduced no finding
after exempting only that existing lint. Cargo commands also emit the existing
development warning that the `codeg-mcp` sidecar placeholder is zero bytes.

## Scope

Only the Task 10 files and this report are intended for the Task 10 commit.
Existing changes in `progress.md`, `connection.rs`, `companion.rs`,
`launch_snapshot.rs`, `workflow/project.rs`, and `publish*.json` remain
unstaged. Plan and Design files were not edited and retain their approved
digests.
