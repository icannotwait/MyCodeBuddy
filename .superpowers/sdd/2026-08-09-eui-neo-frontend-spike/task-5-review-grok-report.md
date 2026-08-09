# Task 5 Independent Review (Grok, high route, second reviewer)

- **reviewed_task_id:** `ca07b7cb-bc13-437d-afaa-3060e6f50523`
- **artifact_digest (producer HEAD):** `624fa8c37c82233a07eaa25cfc166992ee8c9c96`
- **BASE:** `29904a3a8fe6a741372809dfccb08f7a2e194e9f`
- **worktree:** `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike`
- **Design digest:** `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` (matches on-disk design file; no redesign observed)
- **Method:** Read-only review of brief, implementer report, review package, global constraints, and the BASE..HEAD sources (`eui_facade`, `codeg-eui-core` model/runtime/abi/commands, `session_contract`, bridge header). No git mutation. Full cargo suites not demanded (parent host policy).

### Spec Compliance

| Requirement | Result | Notes |
| --- | --- | --- |
| Canonicalize + verify directory before `open_folder_core`; invalid/non-dir must not create rows | **Met** | `set_eui_workspace` canonicalizes, rejects non-dir/non-UTF-8, then opens. Facade test asserts empty folder list on failure. Session contract asserts terminal error for file path. |
| Only Grok/Codex session **create** | **Met** | `ensure_supported` / `parse_supported_agent` on create and settings; create tests reject ClaudeCode. |
| Only Grok/Codex sessions in list/select surface | **Partial** | Workspace projects all `ConversationKind::Regular` rows (activity order via `list_by_folder` default). Live select reuses any connection by conversation id without `ensure_supported`. EUI-isolated data root makes foreign agents unlikely, but the code path does not enforce the agent boundary. |
| Create via `create_project_conversation_core` | **Met** | Direct core call with `(folder_id, agent, None, None)`. |
| History via `get_folder_conversation_with_live_core` + 100-user-turn window | **Met** | `HistoryLoadOpts { user_turn_limit: Some(100), before_turn_id: None }`. Outside-folder conversations rejected. |
| Create: `verify_agent_installed` → `build_acp_launch_inputs` with `AcpRouteRequest::root(Some(conversation_id), None)` → user launch context → `spawn_agent` owner `"eui"`, no delegation override | **Met** | Production ops implement exact order and args (`None` parent/operation, empty preferred config, user launch context from DB). Recording test locks verify/build/spawn order and owner. |
| Select: reuse live by conversation id or resume via external_id | **Partial** | Code structure matches the brief. Reuse depends on `find_connection_by_conversation_id`, which only matches connections with `state.conversation_id` set. Production spawn does not bind the conversation id; binding occurs on first `send_prompt_linked_*`. Pre-send reselect can miss the live connection and spawn a second agent (resume path). No select reuse/resume unit test. |
| Send: one text block, UUID client message id, `send_prompt_linked_with_message_id` with selected folder/conversation | **Met** | Facade + manager test prove single `PromptInputBlock::Text`, UUID message id, and conversation bind on send. |
| Async CoreOps workers; create/select completion JSON includes conversationId + connectionId | **Met** | Workers execute set/create/select/send; `EuiSessionSelection` is camelCase JSON with both ids; header documents async completions. |
| `selection_epoch` advances on accepted workspace/create/select; stale completions exactly-once without overwriting active projection | **Met** | Epoch advanced inside `SharedModel::reserve` under the same lock as ledger reservation; projection cleared immediately; `terminalize_with_update` applies model updates only when epochs match; ledger stales mismatched non-cancelled completions; gated `SlowCreateOps` worker test proves one stale create and empty connection/transcript afterward. |
| `t0_ns` recorded after successful send enqueue | **Met** | `RuntimeOwner::enqueue` calls `record_send_accepted(native_timestamp_ns())` after permit send for `SendUserMessage`. Selection-changing reserve clears timing fields. |
| Positive conversation IDs under Task 3 UI-thread/lifecycle ABI precedence | **Met** | `codeg_eui_select_session` uses `ensure_running` (UI thread then lifecycle) before null-pointer and `conversation_id <= 0` → `CODEG_EUI_ERR_INVALID_STATE`; does not accept. Contract test covers pre-accept reject. |
| No AppState/DB/parser exposure in DTOs; no Axum/Tauri handlers; no second config schema | **Met** | Narrow workspace/session DTOs + existing MessageTurn JSON; facade over existing cores; settings path unchanged. |
| Parent policy: skip full cargo / no dependency-complete shared-codeg gate | **Honored** | Missing full `cargo test --lib` treated as residual only. Focused probes + contracts-only CTest accepted as evidence under host constraint. |

`runtime.rs` is outside the brief’s enumerated file list but is a justified Task 5 dependency: Task 4 left enqueue stubs; session workers and epoch-guarded model application must live there. No unrelated runtime redesign observed.

### Strengths

- Clear separation: DB/folder/conversation/history/ACP orchestration stays in `eui_facade`; ABI admission, epoch ledger, and async workers stay in `codeg-eui-core`.
- Workspace validation is fail-closed before persistence (`canonicalize` + `is_dir` before `open_folder_core`), with explicit invalid-path tests.
- Injected `EuiSessionOps` is the right test seam for deterministic create/send orchestration without real agent processes.
- Epoch/stale handling is carefully co-located: selection-changing accept advances epoch, clears active projection, invalidates worker selection context via `begin_selection`, and still drains exactly one terminal completion.
- Positive conversation-id rejection preserves Task 3 admission order (thread/lifecycle before payload validity).
- Scope hygiene is good: no second config schema, no Axum/Tauri session handlers, design digest unchanged, single session-loop commit at the stated digest.

### Issues (Critical / Important / Minor)

#### Critical

None.

#### Important

1. **Live select reuse is incomplete before first linked send (double-spawn risk).**  
   `ProductionEuiSessionOps::spawn_agent` receives `conversation_id` but ignores it (`_conversation_id`). The manager’s `find_connection_by_conversation_id` only matches connections whose `SessionState.conversation_id` is already `Some`. That field is normally bound on first `send_prompt_linked_with_message_id`, not at spawn. Therefore:
   - create session → live connection exists and is returned in completion JSON, but is **not** discoverable by conversation id;
   - select of that conversation before any send falls through to resume/spawn with external id and can start a **second** `"eui"` agent.  
   Task 5 explicitly requires select to reuse a live connection by conversation id. Either bind the conversation (and folder) on the connection at successful create/select spawn, or teach live lookup to recognize the just-created connection without a second spawn. Add a focused test covering create → select-same-id without send.

2. **Select path agent boundary is weaker than create.**  
   `set_eui_workspace` lists every regular conversation (not Grok/Codex-only). `select_eui_session_with_ops` skips `ensure_supported` when `find_connection` hits. Create is correctly gated. Task scope says “only Grok/Codex sessions.” Under a pure EUI data root this may be latent; it is still a real admission gap if non-supported regular rows or foreign live connections exist. Filter the projected session list and always `ensure_supported` (and prefer owner `"eui"`) before reuse or resume.

#### Minor

1. **In-flight send can still deliver after a newer selection accept.**  
   `begin_selection` clears context for *future* readers; a worker that already cloned `EuiSessionSelection` can still call `send_prompt_linked_*` on the old connection. Completions correctly become `Stale` and do not overwrite projection (matches design completion rules), but the side effect is not cancelled. Document as residual or re-check epoch/context immediately before send if later tasks need stricter cancellation.

2. **Test coverage gaps relative to the brief’s orchestration matrix.**  
   Facade tests cover workspace/create/send and empty history; there is no deterministic select reuse/resume test via `EuiSessionOps`. `session_contract` covers workspace success/error and invalid conversation id only—not create/select/send ABI JSON. Acceptable under host OOM policy as residual, not a substitute for the missing select seam test once hosts allow it.

3. **History window value is hard-coded but not asserted.**  
   Implementation uses `user_turn_limit: Some(100)`; the history test only asserts empty transcript JSON shape for a new conversation.

4. **Orphan conversation row if spawn fails after `create_project_conversation_core`.**  
   Common pattern; not unique to EUI, but create is not transactional with ACP spawn.

5. **Host residual (authorized).**  
   Dependency-complete shared-codeg link/`session_contract` execution and full `cargo test --lib --features test-utils` were skipped per parent policy. Re-run on a larger host before final product-loop acceptance. **Not** Critical/Important under this review policy.

### Assessment (Task quality: Needs fixes)

Task 5 delivers the bulk of the M3 workspace/session/history/send loop with solid epoch/stale semantics, correct create launch ownership, send block/UUID/link binding, and clean DTO/ABI boundaries. The remaining Important defects center on **select live-reuse correctness** (conversation binding before first send) and a **weaker Grok/Codex select/list boundary** than create. Those are high-route lifecycle issues and should be fixed before treating Task 5 as complete.

**Task quality: Needs fixes**

VERDICT: request_changes

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","critical":0,"important":2,"minor":5,"summary":"Task 5 select live-reuse incomplete before first send (double-spawn) and Grok/Codex list/select boundary weaker than create.","reviewed_task_id":"ca07b7cb-bc13-437d-afaa-3060e6f50523","artifact_digest":"624fa8c37c82233a07eaa25cfc166992ee8c9c96","concerns":["Bind conversation_id on connection at create/select so live reuse works before first send; add create→select test.","Filter list and select to Regular+Grok/Codex before live lookup or ACP.","In-flight send after selection change may still deliver on old connection (completion correctly stale).","Focused select reuse/resume and history-window/t0 ABI coverage gaps.","Host residual: full cargo skipped by parent."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-review-grok-report.md"}
-->
