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
- Added bounded report pre-reading for Markdown links and touched reports with
  canonical workspace containment and size/count limits.
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

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Atomic v2 terminal materialization: evidence/attention, bindings, graph, outbox, artifact recovery replay, platform Card authority, projection metrics.","commits":[{"sha":"7032316d60a5b6dbee3186d5d0c5347a5ed8bc96","subject":"feat: materialize trusted completion evidence"}],"tests":{"status":"pass","passed":12,"failed":0,"summary":"focused completion_evidence suite: 12 passed"},"concerns":[],"report_file":".superpowers/sdd/task-10-implementer-report.md"}
-->

## Consolidated Review Fix

- `T10-CODEX-I1`: replaced raw `](` scanning with `pulldown-cmark` link
  discovery and excluded fenced code, HTML comments, block quotes, tables,
  lists, nested/example content, and indented code. Removed plain `Report:`
  paths from report-channel authority; platform-touched Markdown remains the
  second bounded source.
- `T10-CODEX-I2`: budgeted serialized `completion_decision` payloads against
  the 16 KiB attention cap. Diagnostics are shed first and intermediate
  candidates only if necessary, preserving the reason plus first and last
  candidate endpoints. Maximum parser-bound ambiguity now commits terminal
  state and exactly one durable attention row.
- `T10-CODEX-I3`: an inconclusive protocol pre-read now preserves both the
  validated v1 Card preparation and the v2 terminal input. The RunStore
  transaction selects authority; a v2 materialization result clears the
  preflight Card before terminal meta/event publication.

### Fix TDD Evidence

Focused RED failures before each correction:

- `report_candidates_ignore_links_in_non_top_level_markdown_contexts` opened
  `reports/review.md` from a fenced Markdown example.
- `v2_pre_read_rejects_plain_and_excluded_report_references` promoted plain
  `Report: reports/review.md` to report authority.
- `maximum_bounded_ambiguity_commits_terminal_and_one_attention` returned
  `completion_attention_invalid: attention payload exceeds 16 KiB`, rolling
  the terminal transaction back.
- `protocol_preflight_error_preserves_v1_card_during_terminal_settlement`
  completed the v1 run with `card_summary_json = None` under the original
  preflight-error branch. The fixture independently validates its Card.

Focused GREEN verification:

- `cargo test --lib completion_evidence::tests -- --nocapture`: 8 passed.
- `cargo test --lib card_summary::tests -- --nocapture`: 22 passed.
- Broker I1 and I3 regressions plus the existing v2 Card-authority guard:
  3 passed.
- Total focused verification: 33 passed, 0 failed.
- `git diff --check`: passed.

No full suite or full Clippy command was run, per the fix brief. Cargo emitted
only the existing zero-byte `codeg-mcp` sidecar development warning.

### Minor Finding Assessment

`T10-CODEX-M1` was not wired in this package. The artifact retry API currently
has no production caller and receives only `AppDatabase` plus the retry CAS,
while `DelegationMetrics` is broker-owned. Wiring it here would require either
a process-global counter or an otherwise unnecessary change to the approved
retry API. The bounded dimension should be recorded when the production retry
entry point is introduced with broker metric ownership.
