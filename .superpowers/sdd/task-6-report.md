# Task 6 Report: continue_delegation + replacement inputs

## Status

DONE

## Commits

- Base: `f21091457acc79c1e3af1effb5500985dbfb786d`
- `e0f8c64b256e5cab5eb17053d26729ef6207564c` feat(mcp): continue_delegation and replacement lineage
- `e2db84ca5da410ea2a9d33d93ac03c31e370fc5b` fix(delegation): continue admission drain, cancel gates, and contract gaps
- `2b988325403620eff3d7cf619aaf878ce2fd138f` fix(delegation): close continue cancel gap and replacement contract holes

## Summary

Task 6 implements `continue_delegation` as an asynchronous MCP operation that
reserves a new run on the existing child conversation, registers a
pre-bootstrap parent-cancel handoff immediately, resumes only the recorded
external session, sends the follow-up prompt, and promotes the run to
`running` only after prompt admission. It also adds durable replacement
lineage inputs and server-side validation.

## Files

- `src-tauri/src/acp/delegation/tool_schema.json`
- `src-tauri/src/acp/delegation/types.rs`
- `src-tauri/src/acp/delegation/companion.rs`
- `src-tauri/src/acp/delegation/listener.rs`
- `src-tauri/src/acp/delegation/run_store.rs`
- `src-tauri/src/acp/delegation/broker.rs`
- `src-tauri/src/acp/delegation/spawner.rs`
- `src-tauri/src/acp/delegation/store.rs`
- `src-tauri/src/acp/manager.rs`
- `src-tauri/src/acp/connection.rs`
- `src-tauri/src/acp/lifecycle.rs`
- `.superpowers/sdd/task-6-report.md`

`transport.rs`, `meta_writer.rs`, and `bin/codeg_mcp.rs` use the existing
generic delegation transport, meta persistence, and companion dispatch paths;
the new tool is exposed through the shared schema/companion layer without a
separate binary protocol branch.

## Verification

| Command | Result |
| --- | --- |
| `cargo test --lib --features test-utils continue_parent_cancel_after_reserve_before_config_never_spawns` | 1 passed |
| `cargo test --lib --features test-utils replacement_missing` | 2 passed |
| `cargo test --lib --features test-utils replacement_` | 15 passed |
| `cargo test --lib --features test-utils acp::delegation` | 620 passed |
| `cargo test --lib --features test-utils acp::connection` | 167 passed |
| `cargo check --lib --features test-utils` | passed |
| `cargo clippy --lib --features test-utils -- -D warnings` | passed |
| `cargo check --no-default-features --bin codeg-mcp` | passed |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | passed |
| `git diff --check` | passed before source commit |

## Self Review

| Brief interface | Result |
| --- | --- |
| Continue MCP wiring | Done. Schema, companion tools/list and tools/call tagging, listener dispatch, lifecycle recognition without `agent_type`, broker dispatch, resume-capable spawner, manager, and `codeg-mcp` companion path agree on `continue_delegation`. |
| Async acknowledgement and typed precedence | Done. A continuation returns a task acknowledgement after successful prompt admission; the ordering is not_found, fingerprint duplicate handling, not_supported, busy, stale, not_continuable, budget, unresumable, then replacement validation. |
| Continuability decision table | Done. `ContinueEligibility` and `decide_continue_eligibility` cover completed/failed, reserving host restart inheritance, unexpected and unknown-origin cancellation, policy rejection, replacement class, superseded/deleted children, and agent-type mismatch. |
| Duplicate parent tool semantics | Done. Matching durable fingerprints return the same run before busy/stale, including reserving and terminal rows; mismatched or legacy-missing fingerprints reject. Fingerprints never derive from `task_preview`. |
| Replacement inputs | Done. `delegate_to_agent` accepts paired `replaces_task_id` and `replacement_reason`, plus `work_unit_key`; parsing rejects incomplete or unsupported pairs. |
| Work-unit bypass closure | Done. A same-key generation-1 re-dispatch with an established `reached_running_at` lineage and no replacement linkage returns `invalid_replacement`; never-running priors do not establish lineage. |
| Replacement seven checks | Done. The transaction validates ownership with cross-parent redaction, role/profile, normalized workspace, terminal/latest source, durable reason, budget room, and lineage inheritance before reserving. |
| Counter charging and retry behavior | Done. Gen-1 re-dispatch ignores never-running priors, and replacement/unexpected counters increment only in `promote_running` with `reached_running_at`; failed reservations remain uncharged. |
| Parent card correlation and missing metadata | Done. Continue uses the explicit parent tool id even without `agent_type`; missing `_meta.tool_use_id` fails closed with `missing_parent_tool_use_id`, including concurrent-card coverage. |
| Resume-only continuation safety | Done. The path is `admit_continue_reserving` then `begin_run_admission` then `spawn_resume_existing` with a preallocated connection id. It does not use `session/new`, checks cancellation after awaited boundaries, and settles/unregisters pre-spawn failures. |
| Parent result hygiene | Done. Card-summary comments are stripped from parent MCP results and stored only as validated durable/event data. |

## Concerns

- `cargo fmt --check` reports pre-existing formatting drift in unrelated Rust
  files. It was not run as a formatter to avoid unrelated churn; the Task 6
  diff passes `git diff --check`.
- Cargo reports existing non-fatal environment warnings about the missing
  packaged `codeg-mcp` sidecar placeholder and a future-incompatible third
  party procedural macro. The `codeg-mcp` check and strict clippy commands pass.

## Out of Scope

- Task 7 frontend work
- Task 8 skill markdown
- Task 9 live-agent end-to-end fixtures
