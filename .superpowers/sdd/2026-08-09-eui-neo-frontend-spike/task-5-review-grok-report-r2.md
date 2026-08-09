# Task 5 Fix Re-Review (Grok, high route, r2)

- **reviewed_task_id:** `07f57b49-9563-463e-a93d-0818c3afe49a`
- **artifact_digest (producer HEAD):** `1b4712060387299c21c6780ccdf3a346fed63864`
- **Fix commit:** `7cb516b83793f57bf7bd1b4a3f2645493d05b0df` (`fix(eui): bind session context and eligibility for task 5`)
- **Docs package commit:** `1b4712060387299c21c6780ccdf3a346fed63864`
- **Prior feature digest:** `624fa8c37c82233a07eaa25cfc166992ee8c9c96`
- **Method:** Read-only re-verification of prior Grok Important findings against fix package, updated report, and live sources at HEAD. Independent of Codex. Full cargo suites not demanded (parent host policy).

## Prior findings disposition

### Important 1 — Live select reuse incomplete before first send (double-spawn)

**Status: ADDRESSED**

Evidence:

- `create_eui_session_with_ops` and the select resume/spawn branch both call `ops.bind_connection(...)` immediately after successful `spawn_agent`.
- Production `bind_eui_connection` emits gated `AcpEvent::ConversationLinked { conversation_id, folder_id, ... }` via `emit_with_state_gated` when `conversation_id` is still unbound.
- `SessionState::apply_event` for `ConversationLinked` sets `self.conversation_id` and `self.folder_id`, which is what `find_connection_by_conversation_id` matches.
- Idempotent same-folder/conversation rebind returns `Ok`; conflict with a different bound conversation returns `ConnectionBinding`.
- Focused tests:
  - `create_then_select_before_send_reuses_the_spawned_connection` — create then select only records `find_connection`, same `connection_id`, no second spawn.
  - `connection_binding_makes_a_spawn_discoverable_before_send` — real manager state becomes findable by conversation id after bind, before any send.
  - Create orchestration order now includes `bind_connection` after `spawn_agent`.

### Important 2 — Select/list agent boundary weaker than create

**Status: ADDRESSED**

Evidence:

- Central helper `is_eui_session_eligible`: `ConversationKind::Regular` **and** `AgentType::Codex | Grok`.
- `set_eui_workspace` filters with `is_eui_session_eligible` (not Regular-only).
- `load_eui_session` loads the row first, rejects outside-folder and ineligible rows **before** `get_folder_conversation_with_live_core` and **before** `find_connection` / ACP.
- Tests:
  - `workspace_list_contains_only_supported_regular_sessions` excludes Claude regular and Grok chat rows.
  - `selection_rejects_ineligible_rows_before_connection_lookup` — Claude live map entry and non-regular Codex never reach `find_connection` (`ops.calls()` empty).

Note: live reuse path still does not re-call `ensure_supported` after a hit, but eligibility already ran in `load_eui_session`, so unsupported rows cannot reach live lookup.

### Critical — dual-review C1 admission binding (immutable workspace/selection at accept)

**Status: ADDRESSED** (no prior Grok Critical; re-verified as requested)

Evidence:

- `RuntimeCommand` carries `context: CommandContext`.
- Under the runtime admission lock, `enqueue` calls `capture_context(selection_epoch, op)` **before** `reserve` / `begin_selection`, so create/select/send workers receive an owned workspace or selection snapshot from admission time.
- `execute_command` fails closed on `CommandContext::Unavailable` and requires `Workspace`/`Selection` for create/select/send.
- Workers no longer re-read mutable `AppCommandContext` for create/select/send side effects; only completion-time projection write is epoch-gated.
- Runtime tests cover capture of immutable queued context and `admitted_send_keeps_original_ids_and_terminalizes_stale_once` (stale send keeps admitted IDs, one Stale completion, no projection overwrite).

### Minor 1 — In-flight send side effect after selection change

**Status: PARTIAL (intentional residual)**

Admitted send still dispatches to the **captured** selection IDs even after a later selection change; completion is correctly `Stale` and does not mutate the active projection. New regression documents this contract rather than cancelling ACP delivery. Acceptable under design completion rules; not re-raised as Important.

### Minor 2 — Select reuse/resume + ABI coverage gaps

**Status: PARTIAL**

Facade now has create→select reuse and binding tests. Runtime has admitted-send stale test. `session_contract` still does not exercise create/select/send end-to-end ABI JSON (host/OOM residual). Residual only.

### Minor 3 — History 100-turn window not asserted

**Status: NOT ADDRESSED**

Implementation still hard-codes `user_turn_limit: Some(100)`; tests still only assert empty transcript shape. Residual minor.

### Minor 4 — Orphan conversation if spawn fails after row create

**Status: NOT ADDRESSED**

Unchanged create-then-spawn non-transactional pattern. Residual minor. Bind-after-spawn can similarly leave a live unbound connection if bind fails after spawn (same class of residual).

### Minor 5 — Host residual (full cargo / shared-codeg)

**Status: NOT ADDRESSED (authorized)**

Full cargo and dependency-complete shared-codeg remain parent-skipped / OOM-limited. Not Critical/Important under policy.

## New issues

### Critical

None.

### Important

None.

### Minor (new or restated residuals)

1. Admitted in-flight send may still deliver to the pre-change connection (documented; completion Stale).
2. History window value remains unasserted.
3. Non-transactional create/spawn/bind failure paths can leave orphan rows or connections.
4. Full cargo / dependency-complete verification still host-limited.

No new Critical or Important defects observed in the fix.

## Assessment

The fix commit correctly closes both prior Grok Important findings and the C1 immutable admission-context gap:

| Finding | Disposition |
| --- | --- |
| I1 live reuse / pre-send bind | **ADDRESSED** |
| I2 Grok/Codex list-select boundary | **ADDRESSED** |
| C1 admission snapshot binding | **ADDRESSED** |
| Prior minors | residual / intentional |

Task 5 is acceptable for high-route settlement with residual host and non-transactional side-effect notes only.

**Task quality: Approved (with residual minors)**

VERDICT: approve_with_minors

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve_with_minors","verdict":"approve_with_minors","critical":0,"important":0,"minor":4,"summary":"Prior Important live-reuse and Grok/Codex eligibility findings are fixed; C1 immutable admission context is in place; only residual minors remain.","reviewed_task_id":"07f57b49-9563-463e-a93d-0818c3afe49a","artifact_digest":"1b4712060387299c21c6780ccdf3a346fed63864","concerns":["Admitted in-flight send may still deliver after selection change (completion correctly Stale).","History 100-turn window still unasserted in tests.","Create/spawn/bind failure can leave orphan rows or connections.","Full cargo and dependency-complete shared-codeg verification remain host-limited by parent policy."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-review-grok-report-r2.md"}
-->
