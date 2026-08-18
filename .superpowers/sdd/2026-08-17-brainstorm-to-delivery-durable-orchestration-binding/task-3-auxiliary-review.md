# Task 3 Auxiliary Review

- **Reviewer:** Grok (independent auxiliary)
- **Task:** Expose complete parent-scoped durable binding snapshots
- **Range:** `344d2ab9..7b941724`
- **Producer commit:** `7b9417243921266e5d346447ccae0d81fc87e5d6`
- **Subject:** `feat(delegation): expose binding snapshots`
- **Inputs:** `task-3-brief.md`, `task-3-report.md`, `review-344d2ab9..7b941724.diff`, Plan Global Constraints
- **Mode:** read-only review; no production edits; full library suite not re-run

## Verdict

**Spec compliance:** Issues found

**Task quality:** Needs fixes

**Ready to merge?** No

| Severity | Count |
| --- | --- |
| Critical | 0 |
| Important | 1 |
| Minor | 2 |

1. Important: `promote_running_detailed` can commit reserving→running and skip the parent revision increment when the unlocked pre-read fails.
2. Minor: no-active-conversation auth is not asserted through `process_orchestration_bindings`.
3. Minor: published query schema does not encode all-or-none `snapshot_id`/`cursor`.

## Spec compliance

| Requirement | Status | Evidence |
| --- | --- | --- |
| Read-only; no workflow-state mutation | Pass | Query module only SELECTs; companion/listener add a read path; retired `WORKFLOW_V2_TOOLS` stay catalog-absent and `unknown tool` (`companion.rs:370-374`, `:5746-5781`). |
| Token-only parent; no client parent/conversation id | Pass | `BrokerOrchestrationBindingsRequest` is token + query fields with `deny_unknown_fields` (`transport.rs:147-181`). Companion never copies a parent field (`companion.rs:747-141`). Auth resolves `current_conversation_id(&entry.parent_connection_id)` (`listener.rs:1640-1658`). |
| Dedicated private auth: token, Root, `coordination_v1`, current parent; no `workflow_v2` inspect/mutate | Pass | Private `orchestration_binding_query_auth_context` (`listener.rs:1640-1658`) does not call `workflow_auth_context` (`:1690-1711`). Success fixture uses `workflow_v2: false` (`:4749-4778`). |
| Auth failures `invalid_token` / `root_only` / `coordination_unavailable` / `no_active_conversation`; no page | Pass | Codes and messages (`listener.rs:534-568`). Process returns only the error envelope (`:1665-1670`). |
| Companion `allows_tool` requires delegation + coordination + Root | Pass | `companion.rs:370-374`; `tools/call` uses the same gate (`:677-678`). |
| Schema: `namespace`, optional `limit`, all-or-none snapshot/cursor, `additionalProperties: false` | Partial | Schema has the four properties and `additionalProperties: false` (`tool_schema.json:165-192`). Pairing is enforced only in `OrchestrationBindingQueryRequest::validate` (`types.rs:2971-2987`), not in the published schema. |
| Exact query errors | Pass | `types.rs:3028-3047` maps Invalid/TooLarge/Failed/SnapshotStale to the four stable codes. |
| Success page only in `structuredContent`; no divergent text copy | Pass | `render_orchestration_binding_page` uses empty `content` on success (`companion.rs:1376-1400`); raw-call test asserts `content == []` (`companion.rs:5707-5844` in the diff hunk). |
| Snapshot cache + 60s TTL + 4096/100/200 bounds | Pass | `orchestration_binding_query.rs:56-64`; limit constants live in `types.rs:60-61` and are re-exported. |
| One process-local RW gate: first page holds read through materialize+insert; writers hold write through commit+increment | Issues found | First-page `page_with_loader` holds `mutation_gate.read()` across loader and cache insert (`orchestration_binding_query.rs:926-1001`). Insert, abandon, admit, pre-admission, and terminal paths take `mutation_guard()` then increment after commit. Promote takes the write guard but increments only if an unlocked `load_by_task_id(...).ok().flatten()` saw `Reserving` (`run_store.rs:3913-3947`). |
| Unguessable stored cursors bound to parent/namespace/snapshot/limit/start; restart is stale | Pass | UUID bytes as base64url, maps not decoded offsets (`orchestration_binding_query.rs:979-983`, `:946-955`). Fresh cache cannot resolve (`:1307-1313`). |
| SQL conflict set, 4097 cap, reject not truncate, `(created_at, task_id)` order | Pass | `materialize_binding_rows` (`:1032-1051`). Union/dedup/foreign-unkeyed/order test (`:1596-1751`). 4096 page / 4097 `TooLarge` (`:1769-1804`). |
| Binding reconstruct: four nulls → `null`; partial → Failed | Pass | `map_binding_row` (`:1054-1079`); partial-row test (`:1807-1840`). |
| Approved fields only; actual agent/profile; complete lineage; redaction | Pass | DTO (`types.rs:2992-3008`); agent/profile/lineage asserts and key scan (`orchestration_binding_query.rs:1686-1750`). |
| Decimal `snapshot_revision`; UTC RFC 3339 timestamps; first `request_cursor: null`; later exact echo; final null/`complete: true`; replay | Pass | `page_from_snapshot` (`:1005-1029`); paging test (`:1209-1287`). |
| Grok `7_680` / `7680` literals unchanged; query added to expected list | Pass | Same test name, `println!`, `<= 7_680`, message text `7680` (`companion.rs:5899-5907`); name inserted after `register_simple_workflow` (`:5914-5916`). Report prints `7677`. |
| Commit ownership | Pass | 8 owned files, 1814/44, subject `feat(delegation): expose binding snapshots`. Report untracked. No `.superpowers/sdd/**` in the commit. |
| Simple remains gate-free; no Agent substitution; no Task 4/5 validator work | Pass | No Simple/validator/Skill edits. Initial Task Agent remains `grok` / `profile_id: null` / generation 1 in the Plan routing block. |
| RED observed for absent tool/broker variant | ⚠️ | Not visible in the diff. Report claims `running 1 test` then `0 passed; 1 failed`. Not independently re-run. |

### Spec Compliance

- ❌ Issues found: promote revision increment is not guaranteed after a committed reserving→running write (`run_store.rs:3913-3947`).
- ⚠️ Cannot verify from diff: Step 2 RED observation (absent tool / broker variant) and the printed Grok JSONL byte count `7677`.

## Strengths

- Parent identity is token-derived at every layer: companion arguments, broker request, and SQL predicate never accept a client parent or conversation id.
- Auth is a dedicated private path with the required order and stable codes, and it does not reuse or loosen `workflow_auth_context`.
- The conflict-set predicate, 4096/4097 reject-not-truncate rule, opaque cursor cache, expiry/restart staleness, and redaction scan are implemented and tested against real rows.
- Success MCP rendering keeps the page only in `structuredContent`.
- The Grok catalog grew by adding the query and shortening descriptions without changing the `7_680` / `7680` literals.

## Issues

### Critical (Must Fix)

- None.

### Important (Should Fix)

1. **Promote can commit a status change without advancing the parent snapshot revision**
   - File: `src-tauri/src/acp/delegation/run_store.rs:3913-3947`
   - Issue: `promote_running_detailed` captures `(parent_id, was_reserving)` with `load_by_task_id(...).await.ok().flatten()` *before* the write guard. After `promote_running_once` returns `Promoted` or `AlreadyRunning`, increment runs only when that pre-read was `Some((_, true))`. `load_by_task_id` (`:4850-4858`) can fail as `Err` (swallowed by `.ok()`) or return `Ok(None)` when `model_to_persisted_run` rejects the row. `PromoteRunningKind::Promoted { run }` already carries `parent_conversation_id` (`:509-511`) and is ignored.
   - Why it matters: the Task 3 fence exists so a committed insert/status cannot be observed under the prior revision. A successful reserving→running write that skips `record_parent_mutation` leaves the process-local snapshot serving the old `reserving` row for up to 60 seconds. Task 5 is specified to treat these pages as the only durable evidence.
   - Fix: after a `Promoted { run }` outcome, increment `run.parent_conversation_id`. Do not gate the increment on the unlocked pre-read. Optionally increment `AlreadyRunning` the same way; extra increments are fail-closed.

### Minor (Nice to Have)

1. **No-active-conversation is not proven through the process path**
   - File: `src-tauri/src/acp/delegation/listener.rs:4813-4818`
   - Issue: invalid token, child, and `coordination_v1: false` all call `process_orchestration_bindings` and assert `runs` is absent. The no-parent case only checks `orchestration_binding_query_auth_context`.
   - Why it matters: the brief asked for no query or partial page on that failure too. The production function returns before `get_orchestration_binding_page`, so this is coverage, not a behavior hole.

2. **Published schema treats `snapshot_id` and `cursor` as independently optional**
   - File: `src-tauri/src/acp/delegation/tool_schema.json:182-191`
   - Issue: there is no `dependentRequired` / `allOf` pairing. Rust `validate()` rejects unpaired values (`types.rs:2971-2987`) and companion dispatch fails closed with `-32602`.
   - Why it matters: hosts that only read `tools/list` can emit a half-specified continuation that the schema appears to allow.

## Independent verification

Did not re-run the implementer's `orchestration_binding_query_` filter, Grok budget test, companion suite, or `cargo check`. Judged the promote fence from the committed source.

Named risks checked outside the changed hunks:

| Risk | Check | Result |
| --- | --- | --- |
| Unfenced run-store status writers | `reconcile_non_terminal` calls `settle_terminal`; `settle_legacy_conversation_terminal` updates conversations only; `bind_child_connection_while_reserving` / `write_runtime_stats` do not change run status | No additional production status path |
| Out-of-module `delegation_task_run` inserts | `conversation_service.rs`, `manager.rs`, `simple_workflow.rs`, `broker.rs` hits are test fixtures | Not a live fence bypass |
| Child/catalog bypass | `allows_tool` is applied to `tools/call`; `WORKFLOW_V2_TOOLS` includes `publish_workflow_manifest`, `settle_workflow_gate`, `recover_workflow` | Call path stays unavailable |
| Shared cache identity | `RunStore` is not `Clone`; one `OrchestrationBindingSnapshotCache` per store | Restarted store is stale as specified |

`git diff --check 344d2ab9..7b941724` is clean. HEAD is `7b941724`.

## Notes for later Tasks (not additional Task 3 defects)

- Snapshot entries are process-local, 60-second, and uncapped in count. The brief does not require a live-entry cap; expired rows are purged opportunistically.
- The Grok catalog has 3 bytes of headroom at the reported 7677/7680 size. That budget is a later-Task hazard, not a Task 3 defect.
- Admitted continue/replacement inserts are write-guarded in `admit_*_authorized` but are not separately enumerated by the mutation-invalidation test. The missing increment is on promote, not those insert paths.

## Assessment

**Task quality:** Needs fixes

**Reasoning:** The read-only token-scoped page, conflict set, cursor/revision contract, retirement matrix, and Grok budget literals are in place, but the required mutation fence is incomplete on promote: a committed reserving→running write can keep the prior snapshot live. That is not merge-ready for a high Task whose pages become Task 5 admission evidence.

```json
{
  "kind": "task_review",
  "task": 3,
  "slot": "auxiliary",
  "reviewer": "grok",
  "producer_commit": "7b9417243921266e5d346447ccae0d81fc87e5d6",
  "range": "344d2ab9..7b941724",
  "spec_compliance": "issues_found",
  "task_quality": "needs_fixes",
  "critical": 0,
  "important": 1,
  "minor": 2,
  "findings": [
    {
      "severity": "important",
      "summary": "promote_running_detailed can commit reserving→running and skip the parent revision increment when the unlocked pre-read fails"
    },
    {
      "severity": "minor",
      "summary": "no-active-conversation auth is not asserted through process_orchestration_bindings"
    },
    {
      "severity": "minor",
      "summary": "published query schema does not encode all-or-none snapshot_id/cursor"
    }
  ],
  "verification": {
    "diff_check": "clean",
    "suite_rerun": false,
    "named_risk_promote_fence": "increment gated on load_by_task_id().ok().flatten() before write guard"
  }
}
```
