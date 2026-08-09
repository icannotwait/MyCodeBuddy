# Task 5 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 5 HIGH reviewer (Grok)
- **reviewed_task_id / implementer work unit:** `task|5|implementer|codex|none`
- **Producer commit:** `d145b2c2b7a1811d4c11905935227625e0849e44`
- **Parent / baseline:** `20ddf3e78094d0ea6df9b50b8f2e1d009576a84b` (Task 4 docs after dual approve; prior implementation fix `3f0fb8f4`)
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 5
- **Design:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md` — Task Admission and MCP Binding / Terminal Completion / Terminal Fail-Closed Host Surface
- **Implementer report:** `.superpowers/sdd/task-5-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`request_changes`**

Task 5 lands the core fail-closed rails: exact-pair admission before durable launch side effects, typed `TerminalCompletionProtocol::{Standalone,V2}`, permanent protocol failures outside the `PendingTerminalRetry` / `persistence_error` rail, in-transaction CAS reclassification that clears stale Card/completion authority, immutable exact-v2 MCP binding load, standalone Card preservation, and retained v2 semantic inputs. Focused integration and broker host tests re-verify green.

However, the post-admit launch recheck introduced in Step 4 is incomplete at the host surface. `load_workflow_child_mcp_binding` now returns typed `CompleteWorkError::Protocol` codes, but `RunStore::workflow_child_mcp_binding` collapses that to `String`, and `WorkflowLaunchLoadError::WorkflowBinding` always maps to `spawn_failed` / `admission_failed`. A header flip, dangling header, or non-v2 recheck between admission commit and child spawn therefore loses the stable protocol code. That is an Important fix + regression before Task 6.

## Spec compliance (Task 5 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| Exact-pair admission for first / continue / replacement | Pass | `load_workflow_header` + `require_v2_mutation`; broker `workflow_launch_variants_reject_historical_protocol_before_process_spawn` covers all three launch variants |
| Zero durable side effects on rejected admission | Pass | Integration `workflow_admission_requires_v2` freezes run row + binding; continue/replacement historical tests assert no new parent-tool run and no process spawn/resume |
| Dangling claimed binding → `unsupported_completion_protocol` | Pass (admission + terminal lookup/host) | Continuation dangling abort; terminal lookup matrix; broker `dangling_terminal_header_preserves_code_across_host_surfaces` |
| Immutable exact-v2 MCP binding load | Pass (loader) / **Partial (launch host)** | Loader guards pair and returns `protocol_version=2` only after `require_v2_mutation`; launch host collapses Protocol → `spawn_failed` |
| `TerminalCompletionProtocol::{Standalone,V2}` | Pass | `store.rs` enum; `run_store::load_terminal_completion_protocol` / `RunStore::terminal_completion_protocol` |
| Permanent protocol failures outside persistence retry rail | Pass | Broker `prepare_terminal_for_workflow` → `prepare_typed_terminal_failure`; tests assert no `PendingTerminalRetry` |
| Durable / wait / event code parity | Pass (v1 + dangling host) | Broker typed-failure and dangling tests; lookup matrix covers remaining pairs |
| Transient protocol lookup keeps bounded retry; exhaustion non-semantic | Pass | `terminal_completion_protocol_with_retry`; `fail_terminal_completion_protocol_loads(4)` → `persistence_error`, no Card/retry queue |
| In-txn terminal CAS reclassifies and clears stale authority | Pass | `run_store` settlement sets `protocol_failure`, clears Card/completion/remediation columns, blocks graph completion effect |
| No production Card/shadow fallback for workflow-bound terminals | Pass | Shadow compare/sample removed from prepare path; V2 path uses `prepare_terminal_for_v2` with `card_summary_json = None` |
| Standalone Card display preserved | Pass | `standalone_terminal_preserves_card_summary_and_strips_result_text` |
| Valid v2 semantic inputs preserved | Pass | `completion_v2_semantic_inputs` (tool / conclusion / report / adjudication / obsolete Card+natural) |
| Exact-pair recheck on admitted completion instruction | Pass | `require_v2_mutation` in `load_admitted_completion_instruction`; broker unit recheck for `(2,v2_shadow)` |
| No Task 6 restart-surface deletion | Pass | Producer touches only admission/run_store/store/broker/listener-test/integration; restart tools remain |

### Terminal host surface map (producer)

```text
prepare_terminal_for_workflow(task_id)
  terminal_completion_protocol_with_retry
    Transient + attempts remain -> sleep/retry
    WorkflowAdmission { code } -> prepare_typed_terminal_failure(code)  // no Card/v2 parse
    other after exhaustion     -> prepare_typed_terminal_failure("persistence_error")
    Ok(V2)                     -> prepare_terminal_for_v2 (no Card authority)
    Ok(Standalone)             -> prepare_terminal_with_card_summary

settle CAS (run_store)
  load_terminal_completion_protocol inside txn
    WorkflowAdmission -> force Failed + stable code, clear Card/completion columns,
                         protocol_failure=true, WorkflowTxnSideEffect::None
    Transient/other   -> bubble to settle_with_retry rail
    Ok(V2)+Completed  -> materialize_terminal_completion_txn
    Ok(Standalone)    -> existing on_terminal_settle_txn / Card path
```

### Admission / MCP binding map (producer)

```text
load_workflow_header(parent)
  no workflow row + claimed run binding -> unsupported_completion_protocol
  header pair via load_completion_protocol_header + require_v2_mutation
  only then load full workflow model

admit_workflow_run_txn / ensure_workflow_child_conversation_independent
  share load_workflow_header (first, continue, replacement)

load_workflow_child_mcp_binding
  no binding -> Ok(None) standalone
  missing header -> CompleteWorkError::Protocol(unsupported_completion_protocol)
  non-v2 pair -> Protocol(legacy|unsupported)
  exact v2 -> WorkflowChildMcpBinding { protocol_version: 2, .. }

RunStore::workflow_child_mcp_binding
  map_err(|e| e.to_string())  // LOSES Protocol code  <-- gap

WorkflowLaunchLoadError::WorkflowBinding
  durable_error_code -> always spawn_failed / admission_failed fallback  <-- gap
```

## Independent verification

Re-ran on this worktree at producer `d145b2c2` (branch HEAD):

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils workflow_admission_requires_v2 -- --exact` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils terminal_protocol_failure_is_typed -- --exact` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils completion_v2_semantic_inputs -- --exact` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils terminal_protocol_failure_is_typed_outside` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils dangling_terminal_header` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils terminal_protocol_transient_exhaustion` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_launch_variants_reject` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils dangling_workflow_admission` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils admitted_completion_instruction_rechecks` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils standalone_terminal_preserves_card` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils complete_work_launch_carries` | pass |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils complete_work_continuation_carries` | pass |

Static audit:

| Check | Result |
| --- | --- |
| Permanent protocol → no Card/v2 parse | Pass (`prepare_typed_terminal_failure`) |
| Permanent protocol → no `PendingTerminalRetry` on successful settle | Pass (host tests) |
| In-txn protocol reject clears stale Card/completion authority | Pass |
| Production shadow sample recording on prepare path | Absent (correct) |
| `completion_evidence.rs` production edits | None (acceptable; materialization still gated by V2+Completed) |
| Launch binding Protocol code preservation | **Fail** (`String` collapse + `spawn_failed` fallback) |
| Scope beyond Task 5 (Task 6 restart deletion) | Absent (correct) |

## Strengths

1. Typed terminal classification matches the design host-surface table: Standalone vs exact V2 vs typed permanent protocol vs transient retry/exhaustion.
2. Broker splits before Card/v2 parsing, so historical or unsupported workflow terminals cannot invent Card authority or shadow samples.
3. Settlement CAS re-validates protocol inside the transaction and atomically clears stale Card/completion/remediation columns when authority rejects the attempt.
4. Admission shares one header loader across first/continue/replacement and detects dangling claimed bindings without a workflow row.
5. MCP binding loader and admitted-instruction loader both re-apply `require_v2_mutation`, so `(2,v2_shadow)` cannot keep the canonical v2 contract.
6. Strong host tests for row/wait/event parity on v1 and dangling terminals, transient exhaustion, standalone Card, and historical launch-variant rejection.
7. Scope stays inside Task 5 files; restart APIs remain for Task 6.

## Findings

| id | severity | title | evidence | suggested fix |
| --- | --- | --- | --- | --- |
| T5-GROK-I1 | **Important** | Post-admit launch binding recheck collapses typed protocol failures to `spawn_failed` / `admission_failed` | `load_workflow_child_mcp_binding` returns `CompleteWorkError::Protocol { code: legacy\|unsupported, .. }`, but `RunStore::workflow_child_mcp_binding` does `.map_err(|error| error.to_string())`, and `WorkflowLaunchLoadError::durable_error_code` returns the binding fallback for every `WorkflowBinding` variant. Gen-1 uses `"spawn_failed"`; continue uses `"admission_failed"`. A header flip, dangling header, or non-v2 recheck between admission commit and spawn therefore loses the stable protocol code even though Step 4 upgraded the loader. Existing inject tests assert `spawn_failed` / `admission_failed`, so the hole is currently locked in. | Preserve structured protocol codes through the launch binding load (do not stringify `CompleteWorkError`). Map `Protocol` codes into `WorkflowLaunchLoadError` / durable settlement codes. Add a gen-1 and continue regression that admits exact v2, mutates the header or deletes the workflow before spawn (gate/checkpoint), and asserts the parent/durable code is `legacy_completion_protocol_read_only` or `unsupported_completion_protocol` with no process spawn. |
| T5-GROK-M1 | Minor | Host settle parity matrix is only fully exercised for v1 + dangling, not every permanent pair | Integration `terminal_protocol_failure_is_typed` covers the lookup API for all pairs; broker host parity tests cover historical v1 and dangling. Shared `prepare_typed_terminal_failure(code)` makes additional pairs low risk. | Extend the host settle test (or parameterize the existing one) over `(2,v1)`, `(2,v2_shadow)`, and corrupt-mode using the same durable/wait/event/no-retry assertions. |
| T5-GROK-M2 | Minor | No explicit “transient protocol lookup then successful v2 settle” host test | Exhaustion is covered with `fail_terminal_completion_protocol_loads(4)`. The v1 typed-failure test injects one transient then lands a protocol reject, proving retry, but not “retry then Ok(V2) semantic success”. | Add `fail_terminal_completion_protocol_loads(1)` on a still-v2 workflow and assert completed v2 settlement without Card authority. |
| T5-GROK-M3 | Minor | `load_admitted_completion_instruction` still loads the full workflow model instead of the typed header loader | Missing workflow maps to `completion_instruction_binding_failed`; corrupt-mode decode becomes `Permanent` via `map_db` rather than `unsupported_completion_protocol`. Launch usually fails earlier in binding load, so impact is limited. | Use `load_completion_protocol_header` + `require_v2_mutation` and map missing/corrupt header to `unsupported_completion_protocol` for consistency with terminal/MCP binding. |
| T5-GROK-M4 | Minor | Dangling/replacement admission coverage is continue-only | `dangling_workflow_admission_aborts_continuation_before_process_spawn` is solid; replacement uses the same header loader but has no dangling fixture. | Add a replacement dangling/header-missing abort with the same zero-spawn assertion. |
| T5-GROK-M5 | Minor | Test-only shadow comparator remains as a public broker helper | `compare_completion_shadow_outcome` is retained; production prepare path no longer records shadow samples. Acceptable for historical tests; final deletion can wait for later cleanup/Task 6+ if desired. | Keep for now, or gate/private the helper once historical tests no longer need it. |

No Critical findings.

## Scope notes

- Plan listed `completion_evidence.rs` among Task 5 files. Leaving it unchanged is acceptable: V2 materialization remains gated by `TerminalCompletionProtocol::V2` + completed status, and protocol failures take `WorkflowTxnSideEffect::None`.
- Listener production code is effectively unchanged except tests; binding admission boundary is enforced via the shared loader. Explicit restart tools remain until Task 6 (plan-correct).
- Budget rails remain preflight-only until promote; rejected admissions roll back reserving inserts inside the same transaction, matching the “no side effects” requirement even though insert still appears textually before `admit_workflow_run_txn`.
- Task 6 restart deletion / historical projection cleanup is out of scope and was not started.

## Review card

```json
{
  "kind": "task_review",
  "task": 5,
  "reviewer": "grok",
  "reviewed_task_id": "task|5|implementer|codex|none",
  "producer_commit": "d145b2c2b7a1811d4c11905935227625e0849e44",
  "verdict": "request_changes",
  "critical": [],
  "important": [
    {
      "id": "T5-GROK-I1",
      "title": "Post-admit launch binding recheck collapses typed protocol failures to spawn_failed/admission_failed",
      "blocking": true
    }
  ],
  "minor": [
    {
      "id": "T5-GROK-M1",
      "title": "Host settle parity matrix incomplete for every permanent protocol pair",
      "blocking": false
    },
    {
      "id": "T5-GROK-M2",
      "title": "Missing transient-then-successful-v2 terminal settle coverage",
      "blocking": false
    },
    {
      "id": "T5-GROK-M3",
      "title": "Admitted instruction loader does not use typed header loader for missing/corrupt headers",
      "blocking": false
    },
    {
      "id": "T5-GROK-M4",
      "title": "Dangling admission host coverage is continue-only",
      "blocking": false
    },
    {
      "id": "T5-GROK-M5",
      "title": "Test-only shadow comparator helper remains public on broker",
      "blocking": false
    }
  ],
  "verification": {
    "workflow_admission_requires_v2": "pass",
    "terminal_protocol_failure_is_typed_lookup": "pass",
    "completion_v2_semantic_inputs": "pass",
    "terminal_protocol_failure_host_no_retry": "pass",
    "dangling_terminal_host_parity": "pass",
    "terminal_protocol_transient_exhaustion": "pass",
    "historical_launch_variants": "pass",
    "dangling_continue_admission": "pass",
    "exact_pair_instruction_recheck": "pass",
    "standalone_card_preserved": "pass",
    "v2_launch_and_continue_binding": "pass",
    "launch_binding_protocol_code_preserved": "fail"
  }
}
```

## Conclusion

**request_changes**

Implementer should fix **T5-GROK-I1** test-first: preserve stable protocol codes through the post-admit MCP binding recheck on gen-1 and continue launch, with regressions that mutate or delete the header before spawn. Minor items may land in the same fix commit or follow-up; they are not independently blocking. After the Important fix, re-run the focused Task 5 admission/terminal suite and request dual re-review before Task 6.
