# Task 2 Auxiliary Review

- **Reviewer:** Grok (independent auxiliary)
- **Task:** Enforce binding transport and lineage admission
- **Range:** `457f536c..43c63745`
- **Producer commit:** `43c63745d501a2619b151522d550bcdf0450f931`
- **Subject:** `feat(delegation): enforce binding lineage`
- **Inputs:** `task-2-brief.md`, `task-2-report.md`, `review-457f536c..43c63745.diff`, Plan Global Constraints
- **Mode:** read-only review; no production edits; full library suite not re-run

## Verdict

**Spec compliance:** Compliant

**Task quality:** Approved

**Ready to merge?** Yes

| Severity | Count |
| --- | --- |
| Critical | 0 |
| Important | 0 |
| Minor | 0 |

No findings.

## Spec compliance

| Requirement | Status | Evidence |
| --- | --- | --- |
| Optional all-or-none `orchestration_binding` on both request DTOs | Pass | `DelegationRequest` / `ContinueDelegationRequest` fields are `Option` with `serde(default, skip_serializing_if = "Option::is_none")` (`types.rs:278-279`, `:300-301`). Containing MCP schemas keep the property out of `required`. |
| Shared fixture is the only transport grammar table | Pass | Schema and listener tests load unchanged `src-tauri/tests/fixtures/orchestration_binding_v1.json` via `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), ...))`. Commit does not touch the fixture. Corpus remains `{schema_version,cases}`, 24 unique names, 3 valid / 21 invalid. |
| Schema, listener, and semantic validation agree on every case | Pass | Companion schema test and listener test inject each `value` into both tools. Invalid cases, including explicit JSON `null`, fail with exactly `orchestration_binding_invalid`. Omitted property is a separate `None` path. |
| `$defs` binding object is strict | Pass | Both tools use the same object: `additionalProperties: false`, four required fields, `schema_version` const `1`, namespace pattern/`minLength`/`maxLength`, generation `1..=4294967295`, fingerprint length 71 + `sha256:[0-9a-f]{64}`. No `null` type alternative. |
| `parse_orchestration_binding` is absence-only `Ok(None)` | Pass | Property missing → `Ok(None)`; any present value is deserialized and `validate()`d (`listener.rs:2566-2577`). Parse runs after token/parent lookup and before work-unit, continue/first split, recovery-authorization parse, and broker entry. |
| Direct broker entry revalidates | Pass | `start_delegation` and `continue_delegation` call `binding.validate()` as the first statement, before inflight, correlation, depth, child, or resume. |
| Stable error codes stay distinct | Pass | `DelegationError::{OrchestrationBindingInvalid, OrchestrationBindingLineageMismatch}` map in `DelegationOutcome::from_err` to `orchestration_binding_invalid` / `orchestration_binding_lineage_mismatch`. Store mismatch maps through `store_err_to_delegation_error` and `TaskStoreError::wire_code` and is not folded into `not_continuable` or `invalid_replacement`. |
| First dispatch: omit unbound; valid persists before spawn; invalid rejects first | Pass | Invalid namespace is rejected with empty spawn args. Bound insert is present after `start_delegation` and `admit_gen1_reserving` precedes `spawn_with_workflow_binding`. Omitted request persists `None`. |
| Continue inherit / exact / four-field mismatch / unbound conversion | Pass | `orchestration_binding_lineage_continue_inherits_rejects_conversion_and_is_idempotent` covers bound omit, bound exact, unbound omit, schema/namespace/generation/fingerprint changes, and unbound→bound. Mismatch leaves no reserving row. Omitted inherit replay is idempotent. Broker computes `inherited_binding` immediately after target load and fingerprints the effective value. |
| Replacement inherit / exact / unbound conversion; mismatch before child | Pass | Store test persists source binding for omit/exact and rejects conversion. Broker mismatch test keeps spawn count and child count unchanged, then reuses the same recovery authorization on the exact call. |
| Txn backstop before eligibility / authorization / budget | Pass | Continue reloads the source, runs `inherited_binding`, then parent-tool / eligibility / `authorize_recovery_admission_txn` / preflight / insert (`run_store.rs:2988-3139`). Replacement overwrites the insert inside `validate_replacement_insert_txn` before recovery provenance, eligibility, and `preflight_replacement`. |
| Different effective bindings cannot alias one parent tool-use ID | Pass | First-dispatch alias with a changed namespace under the same tool id returns `duplicate_parent_tool` and does not spawn. Bound fingerprints use the effective binding. |
| Unbound seven-string fingerprints unchanged | Pass | Independent focused run of `request_fingerprint_` still expects `55687507…e557f4` and `f9487ae9…04a97f`. Bound Design vector remains `aca47c46…87ff172`. |
| Grok `7_680` / `7680` contract unchanged | Pass | Same test name, `println!`, comparison literal `7_680`, and message text `7680`. Printed size is `7669`. No query tool added. |
| Literal scans match the Task 2 file set | Pass | Request literals remain in the eight owned files plus `types.rs` definitions. `ContinueRunAdmission {` remains in `broker.rs`, `run_store.rs`, and `workflow/completion_evidence.rs`. Legacy literals set the new fields to `None`. |
| Commit ownership and process | Pass | 13 owned files, 1169/95, subject `feat(delegation): enforce binding lineage`. Report is untracked. No `.superpowers/sdd/**` in the commit. Commands use `--no-default-features --features server,test-utils`. |
| No Task 3 query / no Simple Gates / no Agent substitution | Pass | Catalog has no `get_delegation_orchestration_bindings`. `project.rs` / validator / Simple projection untouched. Initial Task Agent remains `grok` / `profile_id: null` / generation 1. |

## Strengths

- Transport is fail-closed on three layers that share one corpus: published MCP schema, raw listener parse, and `OrchestrationBindingV1::validate()`. Explicit JSON `null` is invalid; omission stays compatible.
- Lineage is resolved once as `inherited_binding` and then enforced again under the writer transaction before continuability, recovery authorization, budget preflight, and insert. Replacement also compares before provisional child creation.
- Side-effect tests use the existing spawn/resume counters, child-conversation count, reserving-row absence, idempotent replay, and authorization reuse rather than new mock seams.
- Compatibility work is mechanical and complete: every current request/admission literal owner compiles with explicit `None`, and unbound fingerprint bytes are unchanged.
- Schema growth stayed inside the frozen Grok JSONL budget without weakening the `7_680`/`7680` literals.

## Issues

### Critical (Must Fix)

- None.

### Important (Should Fix)

- None.

### Minor (Nice to Have)

- None.

## Independent verification

Re-ran only the focused filters from `src-tauri/` with `--no-default-features --features server,test-utils`. Did not re-run the implementer's full `--lib` suite or the two `cargo check` commands.

| Command | Result |
| --- | --- |
| `cargo test ... --lib orchestration_binding_transport_ -- --nocapture` | 2 passed, 0 failed, 4629 filtered out |
| `cargo test ... --lib orchestration_binding_lineage_ -- --nocapture` | 4 passed, 0 failed, 4627 filtered out |
| `cargo test ... --lib request_fingerprint_ -- --nocapture` | 2 passed, 0 failed, 4629 filtered out |
| `cargo test ... --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture` | 1 passed, 0 failed; printed `Grok tools/list JSONL bytes: 7669` |

`git diff --check 457f536c..43c63745` is clean. HEAD is `43c63745`. The same macOS `ld` `__eh_frame` compact-unwind warning reported by the implementer appeared while linking the large lib-test binary; tests still linked and passed.

Word-boundary request-literal files:

```text
src-tauri/src/acp/connection.rs
src-tauri/src/acp/delegation/broker.rs
src-tauri/src/acp/delegation/listener.rs
src-tauri/src/acp/delegation/run_store.rs
src-tauri/src/acp/delegation/workflow/recovery_tests.rs
src-tauri/src/acp/lifecycle.rs
src-tauri/tests/completion_protocol_v2.rs
src-tauri/tests/delegation_session_reuse_integration.rs
```

`ContinueRunAdmission {` files:

```text
src-tauri/src/acp/delegation/broker.rs
src-tauri/src/acp/delegation/run_store.rs
src-tauri/src/acp/delegation/workflow/completion_evidence.rs
```

## Notes for later Tasks (not Task 2 defects)

- Continue mismatch before resume is proven by broker `inherited_binding` immediately after target load plus the run-store matrix; there is no separate broker resume-counter test. Resume remains after `admit_continue_reserving_authorized`.
- The Grok catalog now has 11 bytes of headroom (`7669/7680`). Task 3 must add the snapshot tool without changing the `7_680`/`7680` literals.
- `DelegationRequest` serde `default` would treat a JSON `null` field as omitted if a future deserializer bypassed `parse_orchestration_binding`. The MCP path does not do that.

## Assessment

**Task quality:** Approved

**Reasoning:** Task 2 publishes the optional binding on both tools, rejects malformed input with `orchestration_binding_invalid`, inherits or rejects lineage before child/resume/authorization/budget work, and leaves unbound fingerprints and the Grok 7680-byte contract unchanged. Focused tests and independent scans confirm the producer report. No Critical, Important, or Minor defects.

```json
{
  "kind": "task_review",
  "task": 2,
  "slot": "auxiliary",
  "reviewer": "grok",
  "producer_commit": "43c63745d501a2619b151522d550bcdf0450f931",
  "range": "457f536c..43c63745",
  "spec_compliance": "compliant",
  "task_quality": "approved",
  "critical": 0,
  "important": 0,
  "minor": 0,
  "findings": [],
  "verification": {
    "orchestration_binding_transport_": "2 passed",
    "orchestration_binding_lineage_": "4 passed",
    "request_fingerprint_": "2 passed",
    "grok_tools_list_jsonl_bytes": 7669,
    "unbound_delegate_digest": "55687507f1ed929a92190fb1e1039e422dd219d2238a4b1e10a6968c32e557f4",
    "unbound_continue_digest": "f9487ae94c8b94155514942226be54829c3f5043fdf587d3c33886b01f04a97f",
    "bound_digest": "aca47c464009a8f26bd36e0611b17f62cb7ed7942a387e38e878cf87087ff172"
  }
}
```
