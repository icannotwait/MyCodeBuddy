# Task 15 Implementer Report

## Result

Implemented immutable protocol-v1 restart, frozen completion-protocol rollout
selection, root-only restart transports, and bounded protocol observability. A
legacy root now receives one fresh linked protocol-v2 successor while the
source remains unchanged and becomes read-only by projection.

The consolidated High dual-review fix closes the one Critical and five
Important findings from the Codex and Grok reviews. Root prompts now hit the
same pre-mutation restart fence as workflow tools, every restart transport
honors the current rollout, successors retain bounded original-request and
source launch-profile context, production transitions feed the protocol
metrics, shadow mode compares actual v1 and v2 outcomes, and the Grok catalog
remains below its fixed stdio limit.

## TDD Evidence

RED was established before production changes:

- The legacy restart tests did not compile before the restart transaction,
  failure injector, successor projection, and persisted linkage existed.
- The rollout tests did not compile before the frozen selector, environment
  parser, strict threshold evaluator, and workflow protocol columns were
  integrated at creation.
- The schema/catalog test found no `restart_legacy_workflow` tool or root-only
  companion authorization.
- The registered transport test had no shared authenticated adapter, Tauri
  handler, or Axum route.
- The metrics tests lacked the bounded completion-protocol snapshot and the
  v2 format-repair/CARD-reemit zero guards.

Fresh focused GREEN verification on the final formatted tree:

- `cargo test --features test-utils --test completion_protocol_v2
  legacy_restart -- --nocapture`: 2 passed.
- `cargo test --features test-utils --test completion_protocol_v2 rollout --
  --nocapture`: 3 passed.
- `cargo test --features test-utils --test completion_protocol_v2
  completion_protocol_metrics -- --nocapture`: 1 passed.
- `cargo test --lib completion_protocol_metrics -- --nocapture`: 1 passed.
- `cargo test --features test-utils --test completion_protocol_v2
  restart_tool_schema_is_registered_for_root_only -- --nocapture`: 1 passed.
- `cargo test --features test-utils --test completion_transport_parity restart
  -- --nocapture`: 1 passed.
- The corresponding `--list` filters each discovered a nonzero focused set.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The plan's unqualified filtered Cargo forms were attempted. Cargo compiles
unrelated integration targets and fails before discovery because those targets
import `db::test_helpers`, `AppState::new_for_test`, parser test constructors,
and other helpers gated behind `test-utils`. The focused `--lib` metrics target
and explicit `--features test-utils --test completion_protocol_v2` schema
target above compile and pass.

No full suite, Clippy, frontend test, build, push, or PR was run. Cargo emitted
the existing warning that the packaged `codeg-mcp` sidecar is absent and a
zero-byte build placeholder was used.

## Consolidated Review Fix

### Findings Closed

- `T15-CODEX-C1`: fenced `ConnectionManager::send_prompt_linked_impl` before
  prompt admission, hydration, events, transcript/status mutation, or agent
  send. Enforce creates or reuses the v2 successor and returns its ID through
  the typed restart response; the legacy source never receives the prompt.
- `T15-CODEX-I1`: current server-owned rollout now gates MCP, Tauri, HTTP, and
  automatic restart. Stored protocol-v1 `v2_shadow` workflows restart when the
  current selection becomes `v2_enforce`; new successors are rejected in
  current `v1`/`v2_shadow`, while an existing successor remains reusable.
- `T15-CODEX-I2` / `T15-GROK-I2`: the first accepted root request is captured
  once as bounded text, stable identity, and digest. The atomic restart copies
  that context and derives the active author agent/profile instead of using
  `codex|none`. Coverage uses Grok with `review-canary` and no auto-title row.
- `T15-CODEX-I3`: lifecycle, adjudication age/latency, outbox, Plan reducer and
  classification, Final context/package, continuation, and sibling-rerun
  recorders now run at production transitions. Shadow samples compare the
  authoritative v1 Card outcome to the v2 resolver and retain latest-100
  agent/profile windows. Guarded v2 Card-only repair attempts are rejected and
  counted.
- `T15-GROK-I1`: retained the root restart tool while shortening redundant
  schema prose. The frozen Grok name list includes all six workflow tools and
  the serialized `tools/list` line measures `7557 / 7680` bytes.

### Fix TDD Evidence

RED was observed before each production correction:

- The observability regression failed compilation because the required live
  recorder methods and snapshot fields did not exist.
- The Card-only guard regression failed because no v2 boundary rejected and
  counted the attempted repair.
- The Grok catalog regression first failed on the missing restart-tool name;
  after moving the byte assertion ahead of that list check it exposed the
  catalog measurement used to reclaim budget.
- The transport and stored-shadow restart regressions failed with `original
  request context is unavailable` until their source requests were durably
  captured.
- The existing metrics unit regression failed until attempted v2 invariant
  violations were reported rather than remaining vacuous zeroes.

Fresh focused GREEN verification on the final fix tree:

- `completion_protocol_v2 legacy_restart`: 3 passed.
- Plain-text root prompt restart fence: 1 passed.
- `completion_protocol_v2 rollout`: 5 passed.
- `completion_protocol_v2 completion_protocol_metrics`: 3 passed.
- Card-only repair boundary: 1 passed.
- Root-only restart schema: 1 passed.
- `completion_transport_parity restart`: 1 passed.
- Library completion-protocol metrics: 1 passed.
- Grok fixed stdio catalog: 1 passed, `7557 / 7680` bytes.
- This is 17 focused test executions (16 distinct tests), all passing.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

## Implementation

- Added an atomic, idempotent legacy restart transaction. It validates a live
  v1 root, reuses the unique successor on replay, or creates a fresh root
  conversation, minimal v2 skeleton, manifest revision, and Plan Author
  binding. Any creation failure rolls back every successor row and remains
  retryable.
- The successor imports no task run, completion evidence, attention,
  settlement, gate state, cycle, lineage, review round, or legacy task ID.
  Reciprocal source/successor links, the stable read-only reason, Design open
  gate, and `automatic_root_wake: false` are projected through workflow graph
  and root state responses.
- Added server-owned rollout configuration with a validated default plus exact
  agent/profile overrides. Selection is persisted once per workflow. `v1` and
  `v2_shadow` persist protocol version 1; `v2_enforce` persists version 2.
  Invalid desktop/server configuration fails startup.
- Workflow publication records the selected creation mode and metrics. Shadow
  mode evaluates bounded completion intent/classification on copies and writes
  metrics only. Enforce mode restarts linked legacy workflows before publish,
  settle, recovery, or root delegation can mutate the source.
- Registered `restart_legacy_workflow` in the closed MCP schema and root-only
  companion catalog, the Tauri handler, and authenticated Axum router. Every
  adapter applies root ownership and calls the same restart core operation;
  child and foreign-owner calls are rejected.
- Extended fixed enum/counter metrics for completion authority, evidence,
  attention, artifacts, Plan/Final reduction, outbox, restart, creation mode,
  shadow comparison, and continuation. Derived rollout decisions require 100
  samples and stop only above 1% role mismatch or above 5% needs-decision.
  Metrics retain no prose, paths, report bytes, user text, or profile config.

## Scope And Hygiene

The plan's primary list omitted DTO/wire types and the two startup/state bridge
files required to carry the same frozen configuration and restart request
through desktop, server, and companion runtimes. Task 15 therefore also updates
`workflow/dto.rs`, delegation `transport.rs` and `types.rs`,
`server_bin/main.rs`, and `web/mod.rs`.

Pre-existing changes in `.superpowers/sdd/progress.md`, the Task 13 report,
`connection.rs`, `launch_snapshot.rs`, and formatting-only hunks in
`companion.rs` and `workflow/project.rs` remain unstaged. Untracked
`publish*.json` and manifest JSON files also remain unstaged. Plan and Design
documents were not modified.

## Concerns

The repository's default filtered Cargo invocation still compiles integration
tests without their required `test-utils` feature. Task 15 uses explicit
focused targets and does not broaden scope to alter the shared test harness.
