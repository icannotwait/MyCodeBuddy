# Plan Re-review 1

## Verdict

**CHANGES REQUIRED**

Counts: **0 Critical, 2 Important, 0 Minor**.

Git was re-inspected at `f13c0c79` on
`codex/b2d-generic-task-agent-routing`. The complete revised Plan, approved
Design, prior review, revision brief, Plan Author report, current Skill, and
affected source interfaces were re-read. The current static validator still
passes with seven Plan Tasks and seven progress Tasks.

## Prior-finding dispositions

### I-1: ADDRESSED

Tasks 1-3 now activate the applicable soft signals and use scores 3, 5, and 5
in both the routing block and Task prose ([Plan lines 87-254](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
All three routes remain correctly high.

### I-2: NOT ADDRESSED

The revision correctly owns the previously identified
`ReservingRunInsert`, `PersistedRun`, request-fingerprint, delegation-request,
and continuation-admission sites, and it adds full-library/test-target compile
checks. However, Task 1 also adds four fields to the SeaORM
`delegation_task_run::Model` ([Plan lines 451-475](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
The current source has a complete direct `delegation_task_run::Model` literal
in [project.rs](../../../src-tauri/src/acp/delegation/workflow/project.rs#L4785)
that has no struct-update tail and therefore must name the four new fields.
Task 1 neither owns nor stages `project.rs`; that file is deferred to Task 6.
Consequently Task 1's required `cargo test --lib` and `cargo check --tests`
cannot compile at its serial commit boundary. Add this model-literal scan and
compatibility edit to Task 1 ownership, GREEN evidence, and commit list.

### I-3: ADDRESSED

Task 1 now creates the exact shared
`src-tauri/tests/fixtures/orchestration_binding_v1.json` corpus and defines its
positive/negative grammar vectors; Tasks 2 and 4 load that same file for MCP
schema/listener and Node validation ([Plan lines 465-484, 659-671, and
927-941](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

### I-4: ADDRESSED

Task 1 adds the named `durable_binding_lifecycle_identity_` fault matrix over
rollback and successful lifecycle/status paths, byte-comparing actual
Agent/profile and all four binding columns, with a focused GREEN command
([Plan lines 545-556 and 615-617](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

### I-5: ADDRESSED

Task 5 defines the exact nullable `pending_route_change` shape, static/full
validator invariants, one bounded half-applied recovery state, and every
interruption checkpoint; Task 7 encodes the complete ordered mutation and
approval-settlement sequence ([Plan lines 1134-1150, 1194-1196, 1211-1223,
and 1455-1473](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

### I-6: ADDRESSED

Task 5 defines the exact non-authorizing `B2D-DURABLE-005` status-refresh
classification and transition controls, while Task 7 requires state-only
progress mutation, a discarded snapshot, complete requery, and fresh full
admission ([Plan lines 1132, 1213, and
1460](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

### I-7: ADDRESSED

Task 7 now requires a fresh complete snapshot and the applicable admission
mode before every Design/Plan dispatch or continuation and before initial or
continued final review, including the document-to-full switch after routing
synchronization ([Plan lines 1424-1425, 1456-1462, and
1475](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

### M-1: ADDRESSED

Task 4's test and direct GREEN command now derive bindings from the exact
production Plan and assert seven ordered high routes, generation 1 Grok/null,
canonical keys, and deterministic fingerprints; final verification repeats
the command ([Plan lines 1005, 1060-1071, and
1560-1567](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).

## New findings

### Critical

None.

### Important

#### I-8: Task 3 names an authentication helper that is incompatible with the new read-only query

Task 3 requires the query for a root companion with delegation plus
coordination, but its implementation step says to call
`workflow_auth_context`/token lookup ([Plan lines 808-812 and
872-875](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
At HEAD, `workflow_auth_context` explicitly requires `entry.workflow_v2`
([listener.rs](../../../src-tauri/src/acp/delegation/listener.rs#L1599)), while
production feature parsing deliberately ignores `workflow_v2` and leaves it
false ([companion.rs](../../../src-tauri/src/acp/delegation/companion.rs#L272)).
Using the named helper therefore makes the query unavailable in production;
loosening that shared helper risks changing the retired workflow-mutation
paths that also call it. Task 3 must specify a separate read-only auth path:
token lookup, root role, the intended coordination/delegation gate, and current
parent-conversation resolution, with tests proving success while
`workflow_v2` is false and preserving retirement of all workflow-v2 tools.

### Minor

None.

## Confirmed checks

- The Plan has one routing block and seven contiguous Task headings.
- Generation 1 remains exactly Grok/null, and every Task remains high with a
  Codex implementer, Codex primary reviewer, and Grok auxiliary reviewer.
- The published high-route vector independently recomputes to
  `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
- Every Rust compile/test/lint command uses exactly
  `--no-default-features --features server,test-utils`; none enables
  `tauri-runtime`.
- The fixed Grok `7_680`/`7680` budget, parent coordinator-only boundary,
  warning-only projection, and manifest/Gate/Card/platform-completion
  prohibitions remain intact.
