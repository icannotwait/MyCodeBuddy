# Task 1 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 1 HIGH reviewer (Grok)
- **reviewed_task_id / implementer task:** `b954d688-e237-493d-b1e0-31df91323c1b`
- **Producer commit:** `017954713566ddcbfd274f099055ddce022e2d01`
- **Baseline:** `190c1e14` (dispatch ledger HEAD); requested `bb24a884`
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 1
- **Design:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md`
- **Implementer report:** `.superpowers/sdd/task-1-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`approve_with_minors`**

Task 1 delivers the fixed v2 identity, exact pair guard, and stable typed error surfaces required by the plan and design. Focused tests and desktop `cargo check` re-verified clean. One non-blocking minor about the new blanket `From<WorkflowStoreError>` conversion is recorded for later tasks; no Critical or Important defects found.

## Spec compliance (Task 1 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| `CURRENT_COMPLETION_PROTOCOL_VERSION = 2` | Pass | `workflow/types.rs` |
| `current_completion_protocol_mode() -> V2Enforce` | Pass | `workflow/types.rs`; re-exported via `types::*` |
| `require_v2_mutation(version, mode)` exact pair semantics | Pass | `(2, V2Enforce)` ok; all version `1` → `legacy_completion_protocol_read_only`; other pairs → `unsupported_completion_protocol` |
| Protocol errors non-retryable; not collapsed into generic codes | Pass | `is_retryable()` excludes new variants; `code()` returns exact stable strings |
| Typed removed-configuration error for Task 2 preflight | Pass | `CompletionProtocolConfigurationRemoved` with stable `code()` |
| Stable `AppErrorCode` + `AcpError` variants + structural maps | Pass | snake_case codes; `app_command_error()`; `From` for protocol / config errors |
| HTTP: read-only/unsupported/instruction-binding → 409; config-removed → 400 | Pass | `web/handlers/error.rs` + unit test |
| Restart/rollout family left available for later tasks | Pass | restart variants still present; no Task 2–11 work |
| Exhaustive MCP listener map for new `WorkflowStoreError` variants | Pass | `listener.rs` fan-out (required compile fix; not in plan Step 6 file list) |
| Scope limited to Task 1 | Pass | Single producer commit; 8 files; no creation/preflight/settlement work |

### Guard matrix (design Central Mutation Guard / plan Shared Interfaces)

```text
(2, v2_enforce)           -> Ok
(1, v1|v2_shadow|v2_enforce) -> legacy_completion_protocol_read_only
(2, v1|v2_shadow)         -> unsupported_completion_protocol
(0|3, any mode)           -> unsupported_completion_protocol   // extra coverage beyond plan table
```

Non-retryability asserted in unit and integration tests.

### Public error contract

| Stable code | WorkflowStoreError | AcpError | AppErrorCode | HTTP |
| --- | --- | --- | --- | --- |
| `legacy_completion_protocol_read_only` | yes | yes | yes | 409 |
| `unsupported_completion_protocol` | yes | yes | yes | 409 |
| `completion_instruction_binding_failed` | n/a (Task 1 surface only) | yes | yes | 409 |
| `completion_protocol_configuration_removed` | typed struct (not store enum) | yes | yes | 400 |

Codes are assigned structurally, not by message parsing.

## Independent verification

Re-ran on this worktree after inspecting `git show 01795471`:

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils require_v2_mutation` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils stable_completion_protocol` | 2 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils stable_protocol_error_codes` | 1 passed |
| `cargo check --manifest-path src-tauri/Cargo.toml` | pass |

## Strengths

1. TDD shape matches the plan: table-driven guard unit test beside `WorkflowStoreError`, public-code integration test, App/HTTP unit assertions.
2. Pair classification matches design and Shared Interfaces exactly, including `(1, v2_enforce)` as read-only.
3. Stable wire codes and HTTP statuses are explicit and tested.
4. Restart/rollout leftovers intentionally retained for later planned deletion — no premature Task 6/8 scope.
5. Listener exhaustive-match fan-out is correctly limited to the two new store variants.

## Findings

| id | severity | title | evidence | suggested fix |
| --- | --- | --- | --- | --- |
| T1-GROK-M1 | Minor | Blanket `From<WorkflowStoreError> for AcpError` drops stable codes for non-protocol variants | `acp/error.rs`: only read-only and unsupported map to typed `AcpError`; `_ => Protocol(message)` yields `code() == None`. Fine for Task 1 / `require_v2_mutation`, but later plan uses `map_err(AcpError::from)` near admission boundaries — easy to widen incorrectly. | Keep the conversion for the protocol pair only (e.g. dedicated helper), or expand the match as later tasks need typed store→ACP codes. Do not use the blanket `From` for general store errors until exhaustive. |

No Critical findings.  
No Important findings.

## Review card

```json
{
  "kind": "task_review",
  "task": 1,
  "reviewer": "grok",
  "reviewed_task_id": "b954d688-e237-493d-b1e0-31df91323c1b",
  "producer_commit": "017954713566ddcbfd274f099055ddce022e2d01",
  "verdict": "approve_with_minors",
  "critical": [],
  "important": [],
  "minor": [
    {
      "id": "T1-GROK-M1",
      "title": "Blanket From<WorkflowStoreError> for AcpError collapses non-protocol variants to Protocol without stable code",
      "blocking": false
    }
  ],
  "verification": {
    "require_v2_mutation": "pass",
    "stable_completion_protocol": "pass",
    "stable_protocol_error_codes": "pass",
    "cargo_check_desktop": "pass"
  },
  "scope_notes": [
    "listener.rs compile fan-out for new WorkflowStoreError variants is justified and limited",
    "Task 2 creation/preflight and later mutation wiring not in this commit (correct)"
  ]
}
```

## Conclusion

**approve_with_minors** — Task 1 is complete for dual-review gate purposes. The minor does not require a fix commit before Task 2; implementers of later admission mapping should avoid over-using the blanket `From`. Codex dual reviewer concurrence still required per plan routing.
