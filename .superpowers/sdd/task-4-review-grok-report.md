# Task 4 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 4 HIGH reviewer (Grok)
- **reviewed_task_id / implementer task:** `aac0bcf4-09ac-482e-af17-9d2a2f28a960`
- **Producer commit:** `7b826557fe38fca115dfadd65c10b2eb0da54abf`
- **Parent / baseline:** `87279ef9519b83c72ab3d59e63c02c2b18af4df9` (Task 3 design-self-review preflight guard fix)
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 4
- **Design:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md` — Central Mutation Guard / Root Prompt Admission Fence / Recovery Authorization Fence / corrupt-header mapping
- **Implementer report:** `.superpowers/sdd/task-4-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`request_changes`**

Task 4 lands the shared exact-pair fence, typed header loaders, narrow `Type`/`TryIntoErr` corrupt-header mapper, recovery-authorization pre-prepare guard, Final-delivery fail-closed codes, completion/`complete_work` protocol-preserving errors, and manager linked-root admission that replaces auto-restart on the production prompt path. Focused store/core matrices and recovery-authorization tests re-verify green.

However, the production MCP **`recover_workflow` listener still auto-restarts historical v1** via `restart_legacy_if_required` before the newly fenced `recover_workflow_core`. That is a missed production mutation boundary for a surface Task 4 explicitly owns. The negative matrix only exercises the store core, so the hole is not covered. One Important fix + regression is required before Task 5.

## Spec compliance (Task 4 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| `load_completion_protocol_header` / `load_completion_protocol_for_conversation` | Pass | `store.rs` typed `select_only` loaders; conversation resolves parent-kind then child binding |
| Mapper only `DbErr::Type` / `TryIntoErr` → unsupported header | Pass | `map_completion_protocol_header_db_error`; infrastructure → `Persistence` |
| `UnsupportedCompletionProtocolHeader` stable code, non-retryable | Pass | `error.rs` `code()` + `is_retryable()`; unit test |
| Guard publish revision / settle / recover cores | Pass (core) | `require_owned_stored_v2_header` / `require_stored_v2_header` + in-txn rechecks |
| Guard Final delivery (current/task/direct) | Pass | Replaces silent `version != 2 → Ok(None)` with protocol errors |
| Guard completion decision / Design self-review / artifact retry | Pass | `completion_evidence.rs` task/workflow header guards; authority from attention binding |
| Guard `accept_complete_work` | Pass | Header load + `require_v2_mutation` before intent writes; stable `CompleteWorkError::Protocol` |
| Recovery-authorization prepare fence (workflow + bound task) | Pass | Challenge paths load header and guard before prepare/question; standalone preserved |
| Manager linked-root fence before hydration/link/send | Pass | `send_prompt_linked_impl` after effective conversation id, before `admit_external_prompt` / hydrate |
| Automation / chat / Tauri / Axum use manager or shared cores | Pass (no local edits needed) | automation + chat use `send_prompt_linked_background`; Tauri/Axum `acp_prompt` use linked with message id; completion web handlers share command cores |
| Five-pair negative matrix + no side effects | Pass (core surfaces) | Integration + lib matrices for publish/settle/recover-core/Final/completion/`complete_work`/root/recovery-auth |
| Corrupt header fixtures (unknown version + undecodable mode) | Pass | `corrupt_header_nonterminal_fences` on publish, recover-core, linked-root |
| Cross-parent remains `unauthorized` without protocol leak | Pass | `historical_protocol_cross_parent_mutations_remain_unauthorized` |
| Historical MCP state projection without Final mutation | Pass | `process_get_workflow_state` returns state on legacy read-only; only v2 continues to Final guard |
| Production MCP `recover_workflow` fail-closed | **Fail** | `process_recover_workflow` still calls `restart_legacy_if_required` and can create successors |
| No Task 5 admission/terminal rewrite | Pass | Admission terminal protocol enum / broker terminal split not present; continue fence on recovery-challenge only |

### Mutation fence map (producer)

```text
require_v2_mutation allows only (2, v2_enforce)
  version 1           -> legacy_completion_protocol_read_only
  anything else       -> unsupported_completion_protocol

typed header load:
  load_completion_protocol_header(workflow_id)
  load_completion_protocol_for_conversation(conversation_id)
  map_completion_protocol_header_db_error:
    Type | TryIntoErr -> UnsupportedCompletionProtocolHeader
    other             -> Persistence (busy/locked retry via existing Persistence rail)

store mutation surfaces:
  publish (token/parent race + classify + in-txn revision)
  settle_workflow_gate_v2_core (+ derived txn)
  recover_workflow_core (outer + in-txn)
  append_state_only_revision_txn
  guard_final_delivery_* (current/task/direct/txn)

completion surfaces:
  resolve_completion_decision / open+resolve Design self-review
  retry artifact (user txn + once path)
  accept_complete_work_once (header + workflow reload)

listener:
  recovery authorization challenge (workflow + bound task) BEFORE prepare
  get_workflow_state: historical projection without Final mutation
  process_recover_workflow: STILL restart_legacy_if_required first  <-- gap

manager:
  non-delegation linked prompt preflight uses load_completion_protocol_for_conversation
  + require_v2_mutation before admission/hydration/link/send
```

## Independent verification

Re-ran on this worktree at producer `7b826557` (branch HEAD):

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol` | 2 passed (`historical_protocol_mutation_matrix`, `historical_protocol_cross_parent_mutations_remain_unauthorized`) |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils corrupt_header` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils root_prompt_protocol_fence` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils legacy_restart_upgrade` | 2 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils historical_protocol_mutation_matrix` | 2 passed (completion evidence + complete_work) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils protocol_fence` | 1 passed (`recovery_authorization_protocol_fence`) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils header_db_error_classification` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils completion_protocol_mutations_preserve_stable` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils publish_workflow_reaches_v2` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils exact_replay_returns_original` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils same_direct_parent_reply_replay_is_idempotent` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils typed_completion_attention_artifact_retry` | 1 passed |

Static audit:

| Check | Result |
| --- | --- |
| Typed header mapper arms | Only `Type` / `TryIntoErr` → unsupported header |
| Manager root fence placement | Inside prompt lock, after effective conversation id, before admit/hydrate/link |
| Automation / chat production prompt path | `send_prompt_linked_background` only |
| Tauri / Axum prompt entry | `send_prompt_linked_with_message_id` |
| Completion Axum handlers | Shared `*_core` after auth; inherit store fences + command code mapping |
| MCP `process_recover_workflow` | Still `restart_legacy_if_required` before core |
| Scope beyond Task 4 (admission terminal enum / broker terminal split) | Absent (correct) |

## Strengths

1. Shared helpers (`require_stored_v2_header` / `require_owned_stored_v2_header`) consistently place ownership before protocol classification where cross-parent leakage matters, matching the cross-parent unauthorized tests.
2. Corrupt-header path is correctly narrow: typed column decode only; busy/pool/query/exec remain persistence; unit classification matches the plan snippet.
3. Final delivery no longer silently no-ops non-v2 headers; historical MCP state still projects without Final side effects.
4. Recovery-authorization prepare is fail-closed for workflow and workflow-bound tasks, with standalone behavior preserved and zero authorization/question growth on reject.
5. Manager linked-root fence replaces auto-restart on the real UI/automation/chat path; foreground and background linked APIs share one implementation.
6. Protocol codes are preserved structurally through `CompletionMutationError` / `CompleteWorkError` / AppCommandError mapping rather than collapsing to generic invalid-input.
7. Design-self-review resolve now binds authority from the durable attention task rather than trusting caller CAS task fields alone.
8. Negative matrices cover the five rejected pairs with before/after snapshots on the store and completion surfaces that were fenced.

## Findings

| id | severity | title | evidence | suggested fix |
| --- | --- | --- | --- | --- |
| T4-GROK-I1 | **Important** | MCP `recover_workflow` still auto-restarts historical v1 before the fenced core | `listener.rs` `process_recover_workflow` calls `restart_legacy_if_required` first; under enforce rollout with no successor, `restart_legacy_workflow_if_enforced` creates a successor and returns a restart projection instead of `legacy_completion_protocol_read_only`. Integration matrix only calls `recover_workflow_core`, so the production boundary is untested. This is a Task 4 recover mutation surface, not merely Task 6 catalog deletion. | Remove the pre-recover auto-restart branch (or make it fail closed with the stable protocol codes and zero successor growth). Add a listener/MCP-level regression that seeds historical pairs, invokes `process_recover_workflow`, asserts exact codes, and freezes authorization/successor/context snapshots. |
| T4-GROK-M1 | Minor | Root companion `process()` still auto-restarts historical parents | `listener.rs` Root+`workflow_v2` path still calls `restart_legacy_if_required` before work-unit handling. Task 5/6 own admission and full restart deletion, but this remains an automatic mutation side path. | Prefer removing in the Task 4 fix commit if cheap; otherwise delete with Task 6 and add an explicit “no auto-restart on root process” assertion then. |
| T4-GROK-M2 | Minor | Negative snapshot omits several plan-listed counters | Plan Step 1 lists gate_state, child-spawn, prompt queue, transcript, route; `mutation_snapshot` covers workflow/conversation/revisions/settlements/attentions/bindings/intents/authorizations/successors. | Expand the fixture when touching the recover listener regression. |
| T4-GROK-M3 | Minor | Residual unlinked manager prompt APIs lack the root fence | `send_prompt` / `send_prompt_background` do not call the protocol preflight. Production Tauri/Axum/chat/automation use linked entry points, so risk is low. | Optional defense-in-depth: apply the same conversation-linked header check when state already has a conversation id. |
| T4-GROK-M4 | Minor | Broader lib suite still non-green from pre-existing Task 2/3 fixture debt | Implementer notes ~100 failures (legacy settlement adapter + duplicate Design gate inserts). Scoped Task 4 filters pass; known fixture `typed_completion_attention_design_self_review_is_typed_and_replayable` remains broken before Task 4 logic. | Later fixture migration; not a Task 4 producer regression. |

No Critical findings.

## Scope notes

- Plan file list named `automation/engine.rs`, `chat_channel/session_commands.rs`, and `web/handlers/workflow_completion.rs`. Leaving them unchanged is acceptable: they already delegate to the manager fence or shared command cores. Direct automation/chat entry tests are not present, but linked-background manager coverage is the shared path.
- Explicit `restart_legacy_workflow` tools/routes remain until Task 6 (plan-correct). The Important finding is specifically the **automatic** recover diversion, which prevents Task 4’s recover fail-closed contract from holding at the MCP boundary.
- Task 5 admission/terminal typed failure work is not present (correct).
- Task 3 minor about `prepare_v2_design_self_review` using bare `version != 2` is already closed on this baseline (`require_v2_mutation` inside preflight).

## Review card

```json
{
  "kind": "task_review",
  "task": 4,
  "reviewer": "grok",
  "reviewed_task_id": "aac0bcf4-09ac-482e-af17-9d2a2f28a960",
  "producer_commit": "7b826557fe38fca115dfadd65c10b2eb0da54abf",
  "verdict": "request_changes",
  "critical": [],
  "important": [
    {
      "id": "T4-GROK-I1",
      "title": "MCP process_recover_workflow still auto-restarts historical v1 before fenced recover_workflow_core",
      "blocking": true
    }
  ],
  "minor": [
    {
      "id": "T4-GROK-M1",
      "title": "Root companion process() still auto-restarts historical parents",
      "blocking": false
    },
    {
      "id": "T4-GROK-M2",
      "title": "mutation_snapshot omits some plan-listed side-effect counters",
      "blocking": false
    },
    {
      "id": "T4-GROK-M3",
      "title": "Unlinked send_prompt APIs do not apply root protocol fence",
      "blocking": false
    },
    {
      "id": "T4-GROK-M4",
      "title": "Pre-existing broader lib fixture debt remains outside Task 4 scope",
      "blocking": false
    }
  ],
  "verification": {
    "historical_protocol_mutation_matrix_integration": "pass",
    "historical_protocol_cross_parent": "pass",
    "corrupt_header_nonterminal_fences": "pass",
    "root_prompt_protocol_fence": "pass",
    "recovery_authorization_protocol_fence": "pass",
    "header_db_error_classification": "pass",
    "completion_and_complete_work_matrices": "pass",
    "stable_app_error_code_mapping": "pass",
    "mcp_recover_workflow_fail_closed": "fail"
  },
  "scope_notes": [
    "automation/chat/web prompt and completion entry points inherit manager/shared cores without local product edits",
    "Explicit restart tool deletion remains Task 6; automatic recover diversion is still a Task 4 boundary miss",
    "Important finding blocks Task 5 until a fix commit + listener-level regression"
  ]
}
```

## Conclusion

**request_changes** — Core Task 4 fences (typed header load, exact-pair mutation guard, recovery-authorization prepare, Final delivery, completion/`complete_work`, manager linked-root admission, corrupt-header mapping, and the five-pair core matrices) are solid and independently re-verified. Production MCP **`recover_workflow` still auto-restarts historical v1** before the fenced store path, so the Task 4 recover fail-closed contract is incomplete. Fix T4-GROK-I1 test-first, produce a new reviewed commit, then re-run dual review before Task 5.

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"aac0bcf4-09ac-482e-af17-9d2a2f28a960","producer_commit":"7b826557fe38fca115dfadd65c10b2eb0da54abf","verdict":"request_changes","critical":0,"important":1,"minor":4,"summary":"Task 4 core fences and matrices are strong, but MCP process_recover_workflow still auto-restarts historical v1 before the fenced recover core; one blocking Important fix plus listener regression required before Task 5.","report_file":".superpowers/sdd/task-4-review-grok-report.md"}
-->
