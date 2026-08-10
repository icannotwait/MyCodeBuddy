# Task 5 Re-Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 5 HIGH re-review (Grok), fix round 1
- **reviewed_task_id:** `0a4e6cc1-1fa2-47b6-95b4-6fc5995e29d4`
- **lineage prior:** `7c63eb27` / `20149d71`
- **Producer fix commit:** `0239f462bf33c922cefe4fbe172f881f38479aaa`
- **Platform artifact / HEAD tip:** `ec180b603fdc7d49ca362e4afc1dc752539f1206`
- **Original producer:** `d145b2c2b7a1811d4c11905935227625e0849e44`
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 5
- **Design:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md` — Terminal Fail-Closed Host Surface / Task Admission and MCP Binding
- **Implementer report:** `.superpowers/sdd/task-5-report.md` (includes Fix Round 1 section)
- **Prior Grok report:** this file (overwritten)
- **Prior Codex report:** `.superpowers/sdd/task-5-review-codex-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`approve_with_minors`**

Fix round 1 closes every previously blocking Important finding. Independent re-verification of the three required closures is green. Residual items are non-blocking consistency/cleanup notes and do not reopen the fail-closed host surface.

## Important findings — re-check

| Prior id | Requirement | Status | Evidence |
| --- | --- | --- | --- |
| **T5-GROK-I1 / T5-CODEX-I3** | Post-admit launch binding preserves stable protocol codes on gen-1, continue, and replacement races | **Closed** | `load_workflow_child_mcp_binding` now returns `TaskStoreError`; `RunStore::workflow_child_mcp_binding` no longer stringifies; `WorkflowLaunchLoadError::{WorkflowBinding,CompletionInstruction}` carry `TaskStoreError` and prefer `workflow_admission_code()` over spawn/admission fallbacks. Gen-1 protocol path settles a durable failed row instead of provisional abandon. Host races: `gen1_pre_spawn_protocol_race_preserves_legacy_code…`, `continuation_pre_spawn_protocol_race_preserves_legacy_code…`, `replacement_pre_spawn_protocol_race_preserves_dangling_code…` all pass. |
| **T5-CODEX-I1** | Connection-availability remains `Transient` on terminal protocol lookup | **Closed** | New `is_transient_db_error` classifies `ConnectionAcquire(Timeout\|ConnectionClosed)`, SQLx `PoolTimedOut`/`PoolClosed`, closed-connection internals, and SQLite busy/locked before permanent fallback. `map_db_err` and MCP-binding DB mapping use it. Header `WorkflowStoreError::Persistence` maps to `TaskStoreError::Transient` in terminal/admission mappers. Unit test `terminal_protocol_db_errors_preserve_connection_availability_and_decode_classes` passes. |
| **T5-CODEX-I2** | No stale conversation status after in-txn protocol reject | **Closed** | `settle_task` derives publication status from the persisted winning report: `Failed`/`Canceled` → `ConversationStatus::Cancelled` before `publish_terminal_meta_and_event`. Race test `terminal_protocol_race_publishes_transaction_authoritative_conversation_status` flips the header after pre-read/before CAS and asserts durable row, conversation projection, wait report, `ConversationStatusChanged`, and terminal event all agree on failed/cancelled + `legacy_completion_protocol_read_only`. |

### Closure detail (I1 / I3)

```text
load_workflow_child_mcp_binding -> Result<..., TaskStoreError>
  Transient DB / protocol WorkflowAdmission / Permanent

RunStore::workflow_child_mcp_binding -> same TaskStoreError (no String collapse)

WorkflowLaunchLoadError::durable_error_code(fallback)
  workflow_admission_code() if present  -> legacy|unsupported
  else if transient                     -> persistence_error
  else                                  -> spawn_failed | admission_failed | completion_instruction_binding_failed

gen-1 protocol failure:
  settle_pre_admission_failure_if_owned(Failed, stable code)  // durable row kept
continue protocol failure:
  same settle path with ADMISSION_FAILED_CODE fallback only for non-protocol errors
```

### Closure detail (I2)

```text
prepare may still snapshot producer conversation_status (e.g. PendingReview)
settle CAS may force Failed + Cancelled on protocol recheck
publish path now:
  report.status Failed|Canceled -> ConversationStatus::Cancelled
  else keep producer snapshot
```

## Previously requested matrix / minor gaps

| Prior id | Status | Notes |
| --- | --- | --- |
| T5-CODEX-M1 / T5-GROK-M1 host + launch matrices | **Closed** | `workflow_launch_protocol_pair_matrix_rejects_first_continue_and_replacement` (5 pairs × 3 variants); `terminal_protocol_permanent_host_matrix_preserves_all_surface_parity` (5 pairs + unknown version + corrupt mode) |
| T5-GROK-M2 transient-then-v2 success | **Closed** | `terminal_protocol_transient_then_v2_success_settles_without_card_authority` |
| T5-GROK-M4 dangling replacement admission | **Closed** | `dangling_workflow_admission_aborts_replacement_before_process_spawn` (+ race variant) |
| T5-GROK-M3 admitted instruction typed header | **Open (Minor)** | Still loads full workflow model; missing workflow → `completion_instruction_binding_failed` rather than typed header path. Launch usually fails earlier on binding load. |
| T5-GROK-M5 public shadow comparator helper | **Open (Minor)** | `compare_completion_shadow_outcome` remains; production prepare path still does not record shadow samples. |

## Spec compliance residual check

| Requirement | Status |
| --- | --- |
| Exact-v2 admission; zero durable launch side effects on reject | Pass |
| Immutable exact-v2 MCP binding + stable protocol codes on pre-spawn recheck | Pass |
| Typed terminal host surface; permanent protocol outside retry rail | Pass |
| Durable / wait / event / conversation-status parity on protocol reject | Pass |
| Transient connection availability on protocol lookup | Pass |
| Standalone Card + v2 semantic inputs | Pass (prior suite retained; integration target green) |
| No Task 6 restart deletion | Pass |

## Independent verification

Re-ran at HEAD `ec180b60` (producer fix `0239f462` + docs tip):

| Command / filter | Result |
| --- | --- |
| `terminal_protocol_db_errors_preserve_connection_availability_and_decode_classes` | pass |
| `terminal_protocol_race_publishes_transaction_authoritative_conversation_status` | pass |
| `pre_spawn_protocol_race` (gen1 + continue + replacement) | 3 pass |
| `terminal_protocol_permanent_host_matrix_preserves_all_surface_parity` | pass |
| `terminal_protocol_transient_then_v2_success_settles_without_card_authority` | pass |
| `workflow_launch_protocol_pair_matrix_rejects_first_continue_and_replacement` | pass |
| `workflow_launch_variants_reject_historical_protocol_before_process_spawn` | pass |
| `dangling_workflow_admission` (continue + replacement) | 2 pass |
| `complete_work_binding_load_failure` (generic inject still `spawn_failed`) | 2 pass |
| `complete_work_launch_carries_committed_v2_workflow_binding` | pass |
| `terminal_protocol_failure_is_typed_outside_persistence_retry_rail…` | pass |
| `cargo test --test completion_protocol_v2 --features test-utils` | **34 passed, 0 failed** |

## Strengths

1. Structured launch errors finally match the typed loader contract instead of string fallbacks.
2. Protocol pre-spawn races keep durable failed rows with stable codes and zero process spawn/resume growth.
3. Transaction-authoritative conversation status closes the exact check/use window the in-txn reclassification introduced.
4. Connection-availability classification is typed before SeaORM stringification, with a focused permanent-query counterexample.
5. Host and launch matrices now cover the five rejected pairs rather than classifier-only samples.

## Remaining findings

| id | severity | title | blocking |
| --- | --- | --- | --- |
| T5-GROK-M3 | Minor | `load_admitted_completion_instruction` still uses full-model load; missing workflow maps to `completion_instruction_binding_failed` instead of the typed header loader | no |
| T5-GROK-M5 | Minor | Test-only `compare_completion_shadow_outcome` remains public on the broker | no |

No Critical or Important findings remain open.

## Scope notes

- Fix commit touches `broker.rs`, `run_store.rs`, `store.rs`, `admission.rs`, and a small `listener.rs` test update only. No Task 6 restart/API/UI removal.
- Generic injected binding-load failures still map to `spawn_failed` / `admission_failed` by design; only protocol-coded `WorkflowAdmission` errors are required to preserve stable codes.
- Mapping all `WorkflowStoreError::Persistence` to `TaskStoreError::Transient` is consistent with the existing retryable Persistence contract; permanent header decode still becomes `WorkflowAdmission` / `unsupported_completion_protocol`.

## Review card

```json
{
  "kind": "task_review",
  "task": 5,
  "reviewer": "grok",
  "reviewed_task_id": "0a4e6cc1-1fa2-47b6-95b4-6fc5995e29d4",
  "lineage_prior": ["7c63eb27", "20149d71"],
  "producer_commit": "0239f462bf33c922cefe4fbe172f881f38479aaa",
  "artifact_digest": "ec180b603fdc7d49ca362e4afc1dc752539f1206",
  "verdict": "approve_with_minors",
  "critical": [],
  "important": [],
  "minor": [
    {
      "id": "T5-GROK-M3",
      "title": "Admitted instruction loader still uses full workflow model instead of typed header loader",
      "blocking": false
    },
    {
      "id": "T5-GROK-M5",
      "title": "Test-only shadow comparator helper remains public on broker",
      "blocking": false
    }
  ],
  "closed_important": [
    "T5-GROK-I1",
    "T5-CODEX-I1",
    "T5-CODEX-I2",
    "T5-CODEX-I3"
  ],
  "verification": {
    "connection_availability_transient": "pass",
    "transaction_authoritative_conversation_status": "pass",
    "gen1_pre_spawn_protocol_race": "pass",
    "continue_pre_spawn_protocol_race": "pass",
    "replacement_pre_spawn_protocol_race": "pass",
    "permanent_host_matrix": "pass",
    "launch_pair_matrix": "pass",
    "transient_then_v2_success": "pass",
    "completion_protocol_v2_integration": "pass"
  }
}
```

## Conclusion

**approve_with_minors**

T5-GROK-I1 / T5-CODEX-I1 / T5-CODEX-I2 / T5-CODEX-I3 are closed with green host races and mapper coverage. Task 5 may proceed to dual-approve settlement and Task 6. Residual minors may be deferred; they do not reopen the terminal fail-closed or launch protocol contracts.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"done","reviewed_task_id":"0a4e6cc1-1fa2-47b6-95b4-6fc5995e29d4","lineage_prior":["7c63eb27","20149d71"],"producer_commit":"0239f462bf33c922cefe4fbe172f881f38479aaa","artifact_digest":"ec180b603fdc7d49ca362e4afc1dc752539f1206","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"Fix round 1 closes all Important Task 5 findings: pre-spawn MCP binding races preserve stable protocol codes on gen-1/continue/replacement, connection-availability stays Transient, and in-txn protocol rejection publishes transaction-authoritative conversation status. Residual minors only: admitted-instruction typed header consistency and a public test-only shadow helper.","report_file":".superpowers/sdd/task-5-review-grok-report.md","closed_important":["T5-GROK-I1","T5-CODEX-I1","T5-CODEX-I2","T5-CODEX-I3"],"tests":{"status":"passed","passed":34,"failed":0,"summary":"Independent re-run: connection-availability mapper, conversation-status race, three pre-spawn protocol races, permanent host matrix, launch pair matrix, transient-then-v2 success, dangling admission, and full completion_protocol_v2 (34/0) all passed."}}
-->
