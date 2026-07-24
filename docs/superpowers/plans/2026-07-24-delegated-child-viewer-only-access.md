# Delegated Child Viewer-Only Access Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a delegated child open in its existing main tab with complete persisted and live output, while backend-enforced viewer-only access prevents a replacement ACP or child mutation until the child task is terminal and its immediate parent is idle.

**Architecture:** A single Rust resolver projects delegate access from the child row, immediate parent row, and live parent `SessionState`; every user-originated mutation reuses that resolver at admission. The frontend treats access and ACP ownership as separate concepts: a locked child uses `observe_existing`, aliases its tab to the broker's canonical connection, and never spawns or disconnects that broker connection. Persisted detail, cold snapshots, replay/live events, and a bounded terminal reconciliation converge into one transcript without changing `kind`, `parent_id`, tab identity, or `external_id`.

**Tech Stack:** Rust 2021, SeaORM, Tauri 2 commands, Axum HTTP/WebSocket, React 19, TypeScript strict, Zustand, Vitest/Testing Library, next-intl.

## Global Constraints

- Effective policy is exactly `viewer_only = child task is non-terminal OR immediate parent durable status is in_progress OR immediate parent live turn_in_flight is true`.
- Terminal child task states are exactly `completed`, `failed`, and `canceled`.
- Missing, malformed, undecodable, vanished, or contradictory delegate/parent/live identity state fails closed with reason `state_unknown`.
- Lock-reason display precedence is `task_running`, then `parent_turn_active`, then `state_unknown`.
- A later parent turn re-locks every direct delegate child; an already-admitted child turn is not canceled.
- Permission approve/reject remains enabled in viewer-only mode; prompt, cancel, mode/config mutation, fork, feedback, and question answers remain disabled and backend-rejected.
- Observer discovery uses delays `[0, 300, 700, 1500, 2500]` milliseconds and never falls through to `acpConnect`.
- The existing row remains `kind = delegate` with the same `parent_id`, tab id, conversation id, and `external_id`; no schema migration is added.
- Viewer attach, detach, discovery, detail refresh, and access refresh never call `mark_agent_activity` and never renew a delegation/tool-watchdog lease.
- Desktop and server modes expose the same DTO and policy; server rejection uses HTTP 409 with code `delegate_viewer_only`.
- Preserve all pre-existing worktree changes, including the three design documents, migration files, broker/run-store/conversation-service edits, `.worktree-salvage/`, and `tmp_wt_audit.json`.

## Plan Review Amendments (2026-07-25)

Adjudicated from independent plan reviews (CodeBuddy GLM5.2, CodeBuddy KimiK3, Codex). Design review was skipped per product owner; baseline design remains approved. Implementers must treat the amendments below as part of Global Constraints.

### Critical (must implement)

1. **`acp_connect` identity agreement before preflight/spawn.** Do not guard only `Some(request.conversation_id)`. Resolve an effective conversation target from the request `conversation_id` and/or durable row matched by `(session_id/external_id, agent_type)`. Reject when: the target is a locked delegate; request conversation id disagrees with the durable external-id row; or identity is ambiguous. Add Tauri + HTTP tests for omitted `conversation_id` with a known locked child session id, and for mismatched conversation/session ids, asserting no process spawn.
2. **Unbound connection mutations must not skip admission.** `ensure_connection_delegate_interactive` must not return `Ok(())` merely because `SessionState.conversation_id` is `None`. For `acp_prompt` / `acp_fork` (and any mutation that later adopts a caller `conversation_id`), derive effective target as: request `conversation_id` when `Some`, else state `conversation_id`, else resolve via state `external_id` + `agent_type` against durable storage. Reject absent/contradictory identity for persisted mutations. Test: unlinked connection + explicit locked child `conversation_id` on prompt/fork → `delegate_viewer_only` on both transports.

### Important (must implement)

3. **Parent live-turn resolution is multi-candidate fail-closed.** Do not stop at the first `find_connection_by_conversation_id` hit. Scan every live connection bound to the parent (conversation id and external-id fallback). If any valid candidate has `turn_in_flight`, lock. If candidates disagree on identity (conflicting conversation/external binding), return `state_unknown`. Add an order-independent duplicate-candidate test.
4. **Observer discovery classifies errors.** Retry only transient/retryable discovery failures while `task_running`. Terminal/unrecoverable errors stop discovery immediately and never fall through to `acpConnect`. Test both classes.
5. **Terminal transcript sync only from verified terminal signals.** Four triggers: (1) surface `TurnComplete` / prompting→idle for a known delegate; (2) workspace upsert with terminal `delegation_task_status`; (3) access reason **leaves** `task_running` for any value **other than** `state_unknown` (e.g. `parent_turn_active` or interactive/`null` — these are resolver-verified terminal-task signals); (4) reconnect only for sessions whose detail already shows a terminal task status. **Never** start terminal polling on `task_running → state_unknown` (access lookup outage). Add both the positive access-edge test and the `state_unknown` no-poll regression.
6. **Reconnect refreshes persisted detail for every open delegate, including running.** On transport reconnect: refresh access (Task 3), refresh detail with live-buffer preservation for open delegate sessions (not only terminal), then cold-attach observer (Task 5). Test missed running-child event recovers via detail refresh + cold snapshot.
7. **Typed `delegate_viewer_only` rejection handling is centralized for every interactive command**, not only `handleSend`. Mode/config/cancel/fork/feedback/question answer paths share the same typed-rejection → draft/access refresh behavior (where applicable). Test at least one non-prompt race.
8. **Owner handoff must not strand a terminal child.** After bounded broker-settling polls, if the broker ACP is still alive, re-attach as observer. **Handoff discovery errors must not be treated as disappearance:** classify like observer discovery; on retryable/auth/transient failure, retain observation and retry — only a positive `null` discovery (connection gone) advances to owner spawn. **Re-entry path:** after re-attach, the next owner handoff is driven by (a) `useConnectionLifecycle`'s existing auto-connect effect when `isActive && autoConnectAllowed` re-runs `connConnect` with stored `own_or_observe` intent on focus/param change, and (b) observer alias cleanup / `CONNECTION_REMOVED` when the broker disconnects, which transitions status and allows a subsequent lifecycle connect with the same stored intent. Backgrounded tabs may wait until focused; that is acceptable. Tests must assert re-entry still uses `intent: "own_or_observe"` and completes without manual reconnect after broker disappearance.
9. **Task 2 HTTP admission fixtures must insert/bind the test connection** before expecting 409 on `acp_set_mode` / permission contrast cases.
10. **Task 8 `ws_attach` parent-projection setup must be concrete** (how to obtain parent `SessionState` arc, populate `active_delegations` / `tool_watchdog_projections` / `last_agent_activity_at`, and assert post-drop clocks). Follow existing `ws_attach.rs` harness patterns.

### Minor (fix or document retention)

11. Task 9 i18n JSON example: `delegateAccess` under `Folder.chat`; `delegateViewerOnly` under `Folder.chat.acpConnections.backendErrors` (prose is authoritative over any flat JSON sketch).
12. Task 5 `acpConnect` assertions: exact-match identity args; use `expect.anything()` for saved-pref slots.
13. Task 6 Cline readiness gate: intentionally waits for detail when `hasPersistedConversation && detailLoading && delegatedOpenIntent == null` so unknown-kind Cline children cannot spawn; document as deliberate fail-closed tradeoff vs historical Cline immediate-connect.
14. Feedback HTTP: preserve existing special 4xx arms (`NoActiveTurn`, `FeedbackDisabled`, `InvalidFeedback`) while adding `DelegateViewerOnly` → 409.
15. Task 7: confirm `FETCH_DETAIL_SUCCESS` already accepts `preserveLive`; extend the action/reducer in-task if absent.

### Round-2 adjudication (2026-07-25 re-review)

16. **Define identity helpers in Task 1/2** (must implement):
    - `resolve_conversation_id_from_external(db, external_id, agent_type) -> Result<Option<i32>, AcpError>` — query durable conversation by external_id + agent_type; `Ok(None)` if no row; `Err(DelegateViewerOnly{state_unknown})` if multiple ambiguous rows.
    - `ConnectionManager::find_all_connections_for_conversation_identity(conversation_id, external_id, agent_type) -> Vec<String>` — single-lock scan; include every connection whose conversation_id matches OR (external_id, agent_type) matches with compatible binding.
17. **Admission tests:** both transports for connect (omit conversation_id / mismatch) and unbound prompt **and** fork; assert no spawn where applicable.
18. **Task 1:** add order-independent two-valid-candidate test (both bound to parent; only one `turn_in_flight`; assert lock regardless of insert order). Run the assertion for **both** insertion orders (`in_flight` first and second).
19. **Effective-identity cross-check (Critical):** In `ensure_effective_delegate_interactive` / `ensure_connect_delegate_interactive`, when request conversation id is `Some`, still resolve state/session external identity when present and reject any disagreement among `{request_conversation_id, state.conversation_id, external_derived_id}`. Never prefer the request id alone when another identity source points at a different durable row.
20. **Handoff re-entry must be explicit:** Do not rely only on focus auto-connect (it intentionally ignores status changes). After observer re-attach during handoff, register a one-shot / cancellable `onCanonicalConnectionRemoved` (or equivalent status→disconnected for the observed broker id) that re-invokes `connect(..., intent: "own_or_observe")` while the surface still wants interactive ownership. Test: active tab, broker disappears after final poll → owner connect runs without remount/focus toggle.
21. **`isRetryableObserverDiscoveryError` taxonomy (must define in Task 5):**
    - Retryable: transport timeout, network reset, HTTP 5xx, temporary “not ready”.
    - Non-retryable / terminal (stop discovery or re-attach observe; never spawn): auth/401/403, permanent not-found for the conversation row, malformed payload, explicit protocol permanent errors.
    - Auth is **non-retryable** (stop or re-attach; do not spin).
    Export a pure helper with unit tests for each class.

---

## File Structure

### New files

- `src-tauri/src/models/delegate_access.rs` - wire DTO and stable Rust enums for access mode/reason.
- `src-tauri/src/commands/delegate_access.rs` - shared resolver plus conversation/connection admission helpers and Tauri command.
- `src-tauri/tests/delegate_access_api.rs` - real Axum endpoint and admission-status integration coverage.
- `src/lib/delegate-access.ts` - fail-closed frontend constants and typed rejection recognition.
- `src/hooks/use-delegate-access.ts` - coalesced child/parent event and reconnect refresh owner.
- `src/hooks/use-delegate-access.test.ts` - access hook loading, failure, event, reconnect, and coalescing coverage.
- `src/components/chat/delegate-access-status.tsx` - compact status row for discovery, read-only, parent lock, interactive, and sync failure states.
- `src/components/chat/delegate-access-status.test.tsx` - localized status precedence and accessibility coverage.
- `src/components/chat/conversation-shell.test.tsx` - capability propagation and permission exception coverage.
- `src/lib/transport/web-event-stream.test.ts` - resume-versus-cold WebSocket reattach protocol coverage.

### Existing files with focused changes

- `src-tauri/src/models/mod.rs`, `src-tauri/src/commands/mod.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/web/router.rs`, `src-tauri/src/web/handlers/acp.rs` - register the projection on both transports.
- `src-tauri/src/acp/error.rs`, `src-tauri/src/app_error.rs`, `src-tauri/src/web/handlers/error.rs` - stable rejection payload and HTTP 409 mapping.
- `src-tauri/src/commands/acp.rs`, `src-tauri/src/commands/feedback.rs`, `src-tauri/src/web/handlers/feedback.rs` - user-facing admission guards only.
- `src/lib/types.ts`, `src/lib/api.ts` - TypeScript DTO/error code and API wrapper.
- `src/contexts/acp-connections-context.tsx` - canonical connection plus tab aliases, alias-aware reads/actions/sinks, observer cleanup.
- `src/contexts/acp-connections-context.test.tsx` - alias identity, event fan-out, cleanup, and non-disconnect tests.
- `src/hooks/use-connection.ts`, `src/hooks/use-connection-lifecycle.ts`, `src/hooks/use-connection-lifecycle.test.ts` - explicit connection intent, bounded discovery, and observer-to-owner handoff.
- `src/lib/transport/types.ts`, `src/lib/transport/web-event-stream.ts` - cold reattach option for delegate observers.
- `src/components/conversations/conversation-session-surface.tsx`, `src/components/conversations/conversation-session-surface.test.ts` - child detail fallback, access integration, draft restoration, and capability wiring.
- `src/components/chat/conversation-shell.tsx`, `src/components/chat/chat-input.tsx`, `src/components/chat/message-input.tsx`, `src/components/chat/question-dialog.tsx`, `src/components/chat/ask-question-card.tsx`, `src/hooks/use-session-feedback.ts` - explicit `interactionLocked` capability gate with permission exception.
- `src/components/chat/mode-selector.tsx`, `src/components/chat/session-config-selector.tsx`, `src/components/chat/model-option-picker.tsx` - disable mutation selectors while retaining their visible current values.
- `src/components/chat/message-input.test.tsx`, `src/components/chat/chat-input.test.tsx`, `src/components/chat/question-dialog.test.tsx`, `src/components/chat/ask-question-card.test.tsx`, `src/components/chat/session-config-selector.test.tsx`, `src/hooks/use-session-feedback.test.ts` - focused control-lock regressions.
- `src/stores/conversation-runtime-store.ts`, `src/stores/viewer-detail-sync.test.ts` - delegate terminal convergence and visible sync failure.
- `src/contexts/app-workspace-context.tsx`, `src/contexts/app-workspace-context.test.tsx` - terminal/detail reconciliation nudges for open child sessions and reconnect coverage.
- `src-tauri/tests/ws_attach.rs`, `src-tauri/tests/tool_watchdog_lifecycle.rs`, `src-tauri/src/acp/connection.rs`, `src-tauri/src/acp/delegation/event_emitter.rs`, `src-tauri/src/acp/lifecycle.rs`, `src-tauri/src/acp/delegation/run_store.rs` - health-clock, exact lease renewal, cold snapshot, disconnect settlement, and startup orphan regressions.
- `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json` - all user-visible delegate access and sync states.

## Acceptance Traceability

| Criterion | Implemented and proved by |
| --- | --- |
| 1. A running child shows live messages, tools, and status in its main tab. | Tasks 4-6 canonicalize the broker ACP, cold-attach the observer, fan out live state, and render the child from persisted detail even when the root workspace list excludes it; Task 9 covers the visible waiting/observing states. |
| 2. Observer mode never creates a second ACP. | Task 5's `observe_existing branches before SDK preflight and never spawns`, bounded-discovery, and observer-to-owner handoff tests assert no `acpConnect` until the broker connection is gone. |
| 3. A locked child cannot start or mutate an interactive turn. | Task 2 rejects every user mutation except permission response with `delegate_viewer_only`; Task 6 locks composer, queue, cancel, selectors, fork, feedback, and question controls. |
| 4. A terminal child becomes interactive when its parent is idle. | Task 1's resolver matrix proves the backend transition; Tasks 3, 5, 6, and 9 refresh access, hand ownership back without duplication, enable interaction, and show `interactive`. |
| 5. A later parent turn re-locks every direct child. | Task 1's two-child regression proves the shared policy; Task 3 refreshes on parent changes; Tasks 5-6 prove an open child is re-locked without ownership replacement. |
| 6. An already-admitted child turn finishes under the new lock. | Task 2 keeps guards out of manager/broker/cleanup paths; Task 5's relock regression streams content through `turn_complete` and asserts neither cancel nor disconnect occurs. |
| 7. Terminal transcript reconciliation has no gaps or duplicates. | Task 7 anchors the current user turn, rejects older repeated prompts, waits for the in-flight marker to clear, preserves live buffers until convergence, and tests superseding-fetch and cancellation races. |
| 8. Parent/child, tab, conversation, and external-session identity stay stable. | Task 4 aliases the tab to one canonical ACP; Task 5 resumes with the original session/conversation ids; Task 9 asserts tab id, conversation id, `external_id`, `kind`, and `parent_id` remain unchanged. |
| 9. Watchdogs are viewer-count independent and late viewers recover warnings. | Task 8 compares zero, one, and two viewers without clock/projection changes and cold-attaches the current delegation observation plus grace projection. |
| 10. Lost backend children do not remain durable running orphans. | Task 8 settles bare disconnects through the real broker/store path and verifies restart reconciliation terminalizes both reserving and running child rows. |
| 11. Semantic LLM activity keeps only the correct task/lease healthy. | Task 8 tests text, thinking, plan, tool start, and tool progress against the owning session clock; noise remains inert; exact `parent_tool_use_id -> task_id` correlation renews only the matching parent lease. |

---

### Task 1: Shared Rust access projection and both transport endpoints

**Files:**
- Create: `src-tauri/src/models/delegate_access.rs`
- Create: `src-tauri/src/commands/delegate_access.rs`
- Create: `src-tauri/tests/delegate_access_api.rs`
- Modify: `src-tauri/src/models/mod.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/web/router.rs`

**Interfaces:**
- Consumes: `conversation_service::get_by_id(&DatabaseConnection, i32) -> Result<DbConversationSummary, DbError>`, `ConnectionManager::{find_connection_by_conversation_id,find_connection_by_external_id,get_state}`, and `SessionState::{conversation_id,external_id,agent_type,turn_in_flight}`.
- Produces: `DelegateAccessState`, `DelegateAccessMode::{ViewerOnly,Interactive}`, `DelegateAccessReason::{TaskRunning,ParentTurnActive,StateUnknown}`, `get_delegate_access_core(&AppDatabase, &ConnectionManager, i32) -> DelegateAccessState`, Tauri `get_delegate_access`, and Axum `POST /api/get_delegate_access`.

- [ ] **Step 1: Write resolver matrix and HTTP projection tests**

Add unit tests at the bottom of `src-tauri/src/commands/delegate_access.rs`. Seed rows through production services, mutate only the status fields under test, and bind a synthetic live parent when testing the pre-durable window:

```rust
#[cfg(test)]
mod tests {
    use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::entities::conversation::{
        self, ConversationStatus, DelegationTaskStatus,
    };
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    async fn fixture() -> (AppDatabase, ConnectionManager, i32, i32) {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/delegate-access").await;
        let parent = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tool-1".into(),
                delegation_call_id: "task-1".into(),
            }),
        )
        .await
        .unwrap();
        (db, ConnectionManager::new(), parent.id, child.id)
    }

    async fn set_parent_status(db: &AppDatabase, id: i32, status: ConversationStatus) {
        conversation_service::update_status(&db.conn, id, status)
            .await
            .unwrap();
    }

    async fn set_child_task(
        db: &AppDatabase,
        id: i32,
        status: Option<DelegationTaskStatus>,
    ) {
        let row = conversation::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.delegation_task_status = Set(status);
        active.update(&db.conn).await.unwrap();
    }

    #[tokio::test]
    async fn running_child_wins_reason_precedence() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_parent_status(&db, parent_id, ConversationStatus::InProgress).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await,
            DelegateAccessState::viewer_only(
                DelegateAccessReason::TaskRunning,
                Some(parent_id),
            )
        );
    }

    #[tokio::test]
    async fn terminal_child_unlocks_only_after_parent_is_idle() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.reason,
            Some(DelegateAccessReason::ParentTurnActive)
        );
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.mode,
            DelegateAccessMode::Interactive
        );
    }

    #[tokio::test]
    async fn live_parent_turn_relocks_before_durable_status_changes() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Failed)).await;
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        manager
            .insert_test_connection(
                "parent-live",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state("parent-live").await.unwrap();
        {
            let mut state = state.write().await;
            state.conversation_id = Some(parent_id);
            state.turn_in_flight = true;
        }
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.reason,
            Some(DelegateAccessReason::ParentTurnActive)
        );
    }

    #[tokio::test]
    async fn later_parent_turn_relocks_every_direct_terminal_child() {
        let (db, manager, parent_id, child_id) = fixture().await;
        let folder_id = conversation_service::get_by_id(&db.conn, child_id)
            .await
            .unwrap()
            .folder_id;
        let sibling = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("sibling".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tool-2".into(),
                delegation_call_id: "task-2".into(),
            }),
        )
        .await
        .unwrap();

        for id in [child_id, sibling.id] {
            set_child_task(&db, id, Some(DelegationTaskStatus::Completed)).await;
        }
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        for id in [child_id, sibling.id] {
            assert_eq!(
                get_delegate_access_core(&db, &manager, id).await.mode,
                DelegateAccessMode::Interactive
            );
        }

        set_parent_status(&db, parent_id, ConversationStatus::InProgress).await;
        for id in [child_id, sibling.id] {
            assert_eq!(
                get_delegate_access_core(&db, &manager, id).await.reason,
                Some(DelegateAccessReason::ParentTurnActive)
            );
        }
    }

    #[tokio::test]
    async fn conflicting_live_parent_identity_fails_closed() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        manager
            .insert_test_connection(
                "conflicting-parent",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state("conflicting-parent").await.unwrap();
        {
            let mut state = state.write().await;
            state.conversation_id = Some(parent_id);
            // Intentionally mismatch agent_type or external_id vs parent row
            // so identity validation returns Err → state_unknown.
            state.agent_type = AgentType::Gemini;
        }

        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.reason,
            Some(DelegateAccessReason::StateUnknown)
        );
    }

    #[tokio::test]
    async fn duplicate_valid_parent_candidates_lock_order_independent() {
        async fn run(
            order: &[(&str, bool)],
        ) {
            let (db, manager, parent_id, child_id) = fixture().await;
            set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
            set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
            for (id, in_flight) in order {
                manager
                    .insert_test_connection(*id, AgentType::ClaudeCode, None, EventEmitter::Noop)
                    .await;
                let state = manager.get_state(*id).await.unwrap();
                let mut s = state.write().await;
                s.conversation_id = Some(parent_id);
                s.turn_in_flight = *in_flight;
            }
            assert_eq!(
                get_delegate_access_core(&db, &manager, child_id).await.reason,
                Some(DelegateAccessReason::ParentTurnActive)
            );
        }
        // Both insertion orders: in_flight second, then first.
        run(&[("parent-a", false), ("parent-b", true)]).await;
        run(&[("parent-b", true), ("parent-a", false)]).await;
    }

    #[tokio::test]
    async fn missing_task_and_parent_fail_closed() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, None).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.reason,
            Some(DelegateAccessReason::TaskRunning)
        );

        set_child_task(&db, child_id, Some(DelegationTaskStatus::Canceled)).await;
        conversation::Entity::delete_by_id(parent_id)
            .exec(&db.conn)
            .await
            .unwrap();
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.reason,
            Some(DelegateAccessReason::StateUnknown)
        );
    }
}
```

Create `src-tauri/tests/delegate_access_api.rs` with a real router assertion. The response contract is snake_case and is available through the same authenticated API prefix as other ACP endpoints:

```rust
use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::acp::delegation::spawner::DelegationLink;
use codeg_lib::app_state::AppState;
use codeg_lib::db::service::conversation_service;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::json;

#[tokio::test]
async fn web_endpoint_returns_the_shared_projection() {
    let data = tempfile::tempdir().unwrap();
    let static_dir = tempfile::tempdir().unwrap();
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/delegate-access-api").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        None,
        None,
    )
    .await
    .unwrap();
    let child = conversation_service::create_with_delegation(
        &db.conn,
        folder,
        AgentType::Codex,
        None,
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tool".into(),
            delegation_call_id: "task".into(),
        }),
    )
    .await
    .unwrap();
    let state = Arc::new(AppState::new_for_test(db, data.path().to_path_buf()));
    let router = build_router(
        state,
        "token".into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let server = TestServer::new(router).unwrap();
    let response = server
        .post("/api/get_delegate_access")
        .add_header("authorization", "Bearer token")
        .json(&json!({ "conversationId": child.id }))
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({
        "mode": "viewer_only",
        "reason": "task_running",
        "parent_id": parent.id,
    }));
}
```

- [ ] **Step 2: Run the focused tests and verify they fail for missing symbols/routes**

Run:

```powershell
Set-Location src-tauri
cargo test --features test-utils delegate_access -- --nocapture
```

Expected: compilation fails because `models::delegate_access`, `commands::delegate_access`, `get_delegate_access_core`, and the `/get_delegate_access` route do not exist.

- [ ] **Step 3: Add DTOs, fail-closed resolver, and thin Tauri/Axum wrappers**

Create `src-tauri/src/models/delegate_access.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateAccessMode {
    ViewerOnly,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateAccessReason {
    TaskRunning,
    ParentTurnActive,
    StateUnknown,
}

impl DelegateAccessReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskRunning => "task_running",
            Self::ParentTurnActive => "parent_turn_active",
            Self::StateUnknown => "state_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateAccessState {
    pub mode: DelegateAccessMode,
    pub reason: Option<DelegateAccessReason>,
    pub parent_id: Option<i32>,
}

impl DelegateAccessState {
    pub const fn interactive(parent_id: Option<i32>) -> Self {
        Self { mode: DelegateAccessMode::Interactive, reason: None, parent_id }
    }

    pub const fn viewer_only(
        reason: DelegateAccessReason,
        parent_id: Option<i32>,
    ) -> Self {
        Self { mode: DelegateAccessMode::ViewerOnly, reason: Some(reason), parent_id }
    }
}
```

Create `src-tauri/src/commands/delegate_access.rs` with one policy implementation:

```rust
use crate::acp::manager::ConnectionManager;
use crate::db::entities::conversation::{
    ConversationKind, DelegationTaskStatus,
};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::{
    DelegateAccessMode, DelegateAccessReason, DelegateAccessState,
    DbConversationSummary,
};

fn unknown(parent_id: Option<i32>) -> DelegateAccessState {
    DelegateAccessState::viewer_only(DelegateAccessReason::StateUnknown, parent_id)
}

fn task_is_terminal(status: Option<&DelegationTaskStatus>) -> bool {
    matches!(
        status,
        Some(DelegationTaskStatus::Completed)
            | Some(DelegationTaskStatus::Failed)
            | Some(DelegationTaskStatus::Canceled)
    )
}

/// Required: implement `ConnectionManager::find_all_connections_for_conversation_identity`
/// (single map lock; collect ALL matching ids; never first-hit only). See
/// Round-2 amendment #16. Fail closed on identity conflict; any valid
/// candidate with `turn_in_flight` locks.
async fn live_parent_turn(
    manager: &ConnectionManager,
    parent: &DbConversationSummary,
) -> Result<bool, ()> {
    let candidates = manager
        .find_all_connections_for_conversation_identity(
            parent.id,
            parent.external_id.as_deref(),
            parent.agent_type,
        )
        .await;
    if candidates.is_empty() {
        return Ok(false);
    }
    let mut saw_valid = false;
    let mut any_in_flight = false;
    for connection_id in candidates {
        let Some(state_arc) = manager.get_state(&connection_id).await else {
            return Err(());
        };
        let state = state_arc.read().await;
        if state.agent_type != parent.agent_type {
            return Err(());
        }
        let conv_ok = state.conversation_id == Some(parent.id)
            || (state.conversation_id.is_none()
                && parent.external_id.as_deref().is_some()
                && state.external_id.as_deref() == parent.external_id.as_deref());
        if !conv_ok {
            return Err(());
        }
        if let Some(expected) = parent.external_id.as_deref() {
            if state.external_id.as_deref().is_some()
                && state.external_id.as_deref() != Some(expected)
            {
                return Err(());
            }
        }
        saw_valid = true;
        if state.turn_in_flight {
            any_in_flight = true;
        }
    }
    if !saw_valid {
        return Err(());
    }
    Ok(any_in_flight)
}

pub async fn get_delegate_access_core(
    db: &AppDatabase,
    manager: &ConnectionManager,
    conversation_id: i32,
) -> DelegateAccessState {
    let child = match conversation_service::get_by_id(&db.conn, conversation_id).await {
        Ok(child) => child,
        Err(_) => return unknown(None),
    };
    if child.kind != ConversationKind::Delegate {
        return DelegateAccessState::interactive(None);
    }
    let Some(parent_id) = child.parent_id else {
        return unknown(None);
    };
    let parent = match conversation_service::get_by_id(&db.conn, parent_id).await {
        Ok(parent) => parent,
        Err(_) => return unknown(Some(parent_id)),
    };
    if !task_is_terminal(child.delegation_task_status.as_ref()) {
        return DelegateAccessState::viewer_only(
            DelegateAccessReason::TaskRunning,
            Some(parent_id),
        );
    }
    let durable_active = match parent.status.as_str() {
        "in_progress" => true,
        "pending_review" | "completed" | "cancelled" => false,
        _ => return unknown(Some(parent_id)),
    };
    if durable_active {
        return DelegateAccessState::viewer_only(
            DelegateAccessReason::ParentTurnActive,
            Some(parent_id),
        );
    }
    match live_parent_turn(manager, &parent).await {
        Ok(true) => DelegateAccessState::viewer_only(
            DelegateAccessReason::ParentTurnActive,
            Some(parent_id),
        ),
        Ok(false) => DelegateAccessState::interactive(Some(parent_id)),
        Err(()) => unknown(Some(parent_id)),
    }
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn get_delegate_access(
    conversation_id: i32,
    db: tauri::State<'_, AppDatabase>,
    manager: tauri::State<'_, ConnectionManager>,
) -> Result<DelegateAccessState, crate::acp::error::AcpError> {
    Ok(get_delegate_access_core(&db, &manager, conversation_id).await)
}
```

Export the module/types, register `commands::delegate_access::get_delegate_access` in the Tauri invoke list, add this handler to `src-tauri/src/web/handlers/acp.rs`, and add the router entry:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDelegateAccessParams {
    pub conversation_id: i32,
}

pub async fn get_delegate_access(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<GetDelegateAccessParams>,
) -> Result<Json<crate::models::DelegateAccessState>, AppCommandError> {
    Ok(Json(
        crate::commands::delegate_access::get_delegate_access_core(
            &state.db,
            &state.connection_manager,
            params.conversation_id,
        )
        .await,
    ))
}
```

```rust
.route(
    "/get_delegate_access",
    post(handlers::acp::get_delegate_access),
)
```

- [ ] **Step 4: Run resolver, API, desktop, and server compile checks**

Run:

```powershell
Set-Location src-tauri
cargo test --features test-utils delegate_access -- --nocapture
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: every command exits 0; matrix tests prove later parent re-entry locks a terminal child and the API returns the same shared projection.

- [ ] **Step 5: Commit the projection boundary**

```powershell
git add src-tauri/src/models/delegate_access.rs src-tauri/src/models/mod.rs src-tauri/src/commands/delegate_access.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/web/router.rs src-tauri/tests/delegate_access_api.rs
git commit -m "feat: expose delegated child access projection"
```

---

### Task 2: Typed viewer-only rejection and user-entry admission guards

**Files:**
- Modify: `src-tauri/src/acp/error.rs`
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`
- Modify: `src-tauri/src/commands/delegate_access.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/commands/feedback.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/web/handlers/feedback.rs`
- Modify: `src-tauri/tests/delegate_access_api.rs`

**Interfaces:**
- Consumes: Task 1's `get_delegate_access_core` and `DelegateAccessState`.
- Produces: `AcpError::DelegateViewerOnly { reason }`, `AppErrorCode::DelegateViewerOnly`, `ensure_delegate_interactive`, and `ensure_connection_delegate_interactive`.

- [ ] **Step 1: Add failing typed-error, guard-matrix, HTTP 409, and permission-exception tests**

Extend `src-tauri/src/acp/error.rs` tests:

```rust
#[test]
fn delegate_viewer_only_serializes_reason_and_stable_code() {
    let value = serde_json::to_value(&AcpError::DelegateViewerOnly {
        reason: DelegateAccessReason::ParentTurnActive,
    })
    .unwrap();
    assert_eq!(
        value,
        json!({
            "code": "delegate_viewer_only",
            "message": "Delegated conversation is read-only",
            "detail": "parent_turn_active",
            "i18n_key": "backendErrors.delegateViewerOnly",
            "i18n_params": { "reason": "parent_turn_active" },
        })
    );
}
```

Add guard tests to `commands/delegate_access.rs`:

```rust
#[tokio::test]
async fn connection_guard_rejects_locked_delegate_and_accepts_regular() {
    let (db, manager, _parent_id, child_id) = fixture().await;
    manager
        .insert_test_connection("child-live", AgentType::Codex, None, EventEmitter::Noop)
        .await;
    manager
        .get_state("child-live")
        .await
        .unwrap()
        .write()
        .await
        .conversation_id = Some(child_id);
    assert!(matches!(
        ensure_connection_delegate_interactive(&db, &manager, "child-live").await,
        Err(AcpError::DelegateViewerOnly {
            reason: DelegateAccessReason::TaskRunning,
        })
    ));

    let folder = seed_folder(&db, "/tmp/delegate-access-regular").await;
    let regular = conversation_service::create(
        &db.conn,
        folder,
        AgentType::Codex,
        None,
        None,
    )
    .await
    .unwrap();
    manager
        .get_state("child-live")
        .await
        .unwrap()
        .write()
        .await
        .conversation_id = Some(regular.id);
    ensure_connection_delegate_interactive(&db, &manager, "child-live")
        .await
        .unwrap();
}
```

Extend `src-tauri/tests/delegate_access_api.rs` so a locked prompt/config/cancel/feedback/question request returns 409, while the permission response reaches its pre-existing manager behavior rather than the delegate gate. **Before posting**, insert and bind the connection on `AppState.connection_manager` (test-utils `insert_test_connection`) with `conversation_id = Some(child.id)` so the request fails as `delegate_viewer_only` rather than `connection_not_found`. Also cover on **both** Tauri (unit/command) and HTTP where practical: (a) prompt **and fork** with unbound connection + explicit locked `conversationId`; (b) connect with omitted `conversationId` but `sessionId` matching a locked child external_id (assert no spawn / no connection created); (c) connect with mismatched conversation/session identity (assert no spawn). Use an unknown permission request id and assert the response code is not `delegate_viewer_only`:

```rust
// After seeding parent/child and starting the test server AppState:
state
    .connection_manager
    .insert_test_connection("child-live", AgentType::Codex, None, EventEmitter::Noop)
    .await;
state
    .connection_manager
    .get_state("child-live")
    .await
    .unwrap()
    .write()
    .await
    .conversation_id = Some(child.id);

let guarded = server
    .post("/api/acp_set_mode")
    .add_header("authorization", "Bearer token")
    .json(&json!({ "connectionId": "child-live", "modeId": "plan" }))
    .await;
assert_eq!(guarded.status_code(), 409);
assert_eq!(guarded.json::<serde_json::Value>()["code"], "delegate_viewer_only");

let permission = server
    .post("/api/acp_respond_permission")
    .add_header("authorization", "Bearer token")
    .json(&json!({
        "connectionId": "child-live",
        "requestId": "missing",
        "optionId": "allow"
    }))
    .await;
assert_ne!(
    permission.json::<serde_json::Value>()["code"],
    "delegate_viewer_only"
);
```

- [ ] **Step 2: Run tests and verify viewer-only requests are not yet rejected**

Run:

```powershell
Set-Location src-tauri
cargo test --features test-utils delegate_viewer_only -- --nocapture
cargo test --features test-utils --test delegate_access_api -- --nocapture
```

Expected: compilation fails for the new enum/error/guard symbols, or the HTTP mutation reaches the manager and does not return the required 409 payload.

- [ ] **Step 3: Add the typed error and guard only user-facing wrappers**

Add the `AcpError` variant and structured mapping:

```rust
#[error("delegated conversation is viewer-only: {reason:?}")]
DelegateViewerOnly {
    reason: crate::models::DelegateAccessReason,
},
```

```rust
Self::DelegateViewerOnly { .. } => Some("delegate_viewer_only"),
```

```rust
AcpError::DelegateViewerOnly { reason } => Some(
    AppCommandError::new(
        AppErrorCode::DelegateViewerOnly,
        "Delegated conversation is read-only",
    )
    .with_detail(reason.as_str())
    .with_i18n(
        "backendErrors.delegateViewerOnly",
        BTreeMap::from([("reason".into(), reason.as_str().into())]),
    ),
),
```

Add `DelegateViewerOnly` to `AppErrorCode` and the conflict arm in `status_for_app_error_code`:

```rust
AppErrorCode::AlreadyExists
| AppErrorCode::TurnInProgress
| AppErrorCode::ConversationWaitingForSubagents
| AppErrorCode::DelegateViewerOnly
| AppErrorCode::SessionRouteConflict => StatusCode::CONFLICT,
```

Add both shared guards to `commands/delegate_access.rs`:

```rust
pub async fn ensure_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    conversation_id: i32,
) -> Result<(), crate::acp::error::AcpError> {
    let access = get_delegate_access_core(db, manager, conversation_id).await;
    if access.mode == DelegateAccessMode::Interactive {
        return Ok(());
    }
    Err(crate::acp::error::AcpError::DelegateViewerOnly {
        reason: access.reason.unwrap_or(DelegateAccessReason::StateUnknown),
    })
}

/// Resolve durable conversation id by external session identity.
/// - Ok(None): no matching row
/// - Ok(Some(id)): exactly one row for (external_id, agent_type)
/// - Err(DelegateViewerOnly{state_unknown}): multiple ambiguous rows
async fn resolve_conversation_id_from_external(
    db: &AppDatabase,
    external_id: Option<&str>,
    agent_type: crate::models::AgentType,
) -> Result<Option<i32>, crate::acp::error::AcpError> {
    let Some(external_id) = external_id.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    // Implement via SeaORM filter on conversation.external_id + agent_type
    // (add a focused conversation_service helper if none exists).
    let matches =
        conversation_service::list_ids_by_external_and_agent(&db.conn, external_id, agent_type)
            .await
            .map_err(|_| crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })?;
    match matches.as_slice() {
        [] => Ok(None),
        [id] => Ok(Some(*id)),
        _ => Err(crate::acp::error::AcpError::DelegateViewerOnly {
            reason: DelegateAccessReason::StateUnknown,
        }),
    }
}

/// Prefer this helper when the caller may supply an explicit conversation id
/// that is not yet bound on SessionState (prompt/fork first-link paths).
pub async fn ensure_effective_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    connection_id: &str,
    request_conversation_id: Option<i32>,
) -> Result<(), crate::acp::error::AcpError> {
    let state = manager
        .get_state(connection_id)
        .await
        .ok_or_else(|| crate::acp::error::AcpError::ConnectionNotFound(
            connection_id.to_string(),
        ))?;
    let (state_conv, external_id, agent_type) = {
        let s = state.read().await;
        (s.conversation_id, s.external_id.clone(), s.agent_type)
    };
    // Always resolve external identity when present and cross-check every
    // non-None source. Never prefer request conversation_id alone when state
    // external_id points at a different durable row (Amendment 19).
    let from_external =
        resolve_conversation_id_from_external(db, external_id.as_deref(), agent_type).await?;
    let sources = [
        request_conversation_id,
        state_conv,
        from_external,
    ];
    let mut effective: Option<i32> = None;
    for candidate in sources.into_iter().flatten() {
        match effective {
            None => effective = Some(candidate),
            Some(existing) if existing == candidate => {}
            Some(_) => {
                return Err(crate::acp::error::AcpError::DelegateViewerOnly {
                    reason: DelegateAccessReason::StateUnknown,
                });
            }
        }
    }
    match effective {
        Some(id) => ensure_delegate_interactive(db, manager, id).await,
        None => Ok(()), // brand-new root path: no durable id on request/state/session
    }
}

pub async fn ensure_connection_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    connection_id: &str,
) -> Result<(), crate::acp::error::AcpError> {
    ensure_effective_delegate_interactive(db, manager, connection_id, None).await
}

/// Resolve connect-time conversation target before preflight/spawn.
/// Agreement rules:
/// - If request conversation_id is Some, load that row and (when session_id is
///   also Some) require external_id/agent_type agreement.
/// - If request conversation_id is None but session_id is Some, load the durable
///   row by (external_id=session_id, agent_type) and use that id when found.
/// - If both resolve and disagree → DelegateViewerOnly { state_unknown }.
/// - If the effective row is a locked delegate → ensure_delegate_interactive.
pub async fn ensure_connect_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    agent_type: crate::models::AgentType,
    session_id: Option<&str>,
    conversation_id: Option<i32>,
) -> Result<(), crate::acp::error::AcpError> {
    let from_session = match session_id {
        Some(sid) if !sid.is_empty() => {
            resolve_conversation_id_from_external(db, Some(sid), agent_type).await?
        }
        _ => None,
    };
    let effective = match (conversation_id, from_session) {
        (Some(req), Some(found)) if req != found => {
            return Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            });
        }
        (Some(req), _) => Some(req),
        (None, Some(found)) => Some(found),
        (None, None) => None,
    };
    if let Some(id) = effective {
        ensure_delegate_interactive(db, manager, id).await?;
    }
    Ok(())
}
```

Apply this admission matrix at the outer Tauri command and Axum handler boundaries:

| Entry | Lookup | Guard |
| --- | --- | --- |
| `acp_connect` | `ensure_connect_delegate_interactive` (request conversation_id + session_id/external durable row) | yes, **before** preflight/spawn |
| `acp_prompt` | `ensure_effective_delegate_interactive(..., request.conversation_id)` | yes |
| `acp_set_mode` | `ensure_connection_delegate_interactive` | yes |
| `acp_set_config_option` | `ensure_connection_delegate_interactive` | yes |
| `acp_cancel` | `ensure_connection_delegate_interactive` | yes |
| `acp_fork` | `ensure_effective_delegate_interactive(..., request.conversation_id)` | yes |
| `submit_session_feedback` | `ensure_connection_delegate_interactive` | yes |
| `acp_answer_question` | `ensure_connection_delegate_interactive` | yes |
| `acp_respond_permission` | none | no |
| disconnect/detach | none | no |
| broker startup/continue/cleanup/settle | none | no |

The wrapper pattern for connection mutations is:

```rust
crate::commands::delegate_access::ensure_connection_delegate_interactive(
    &db,
    &manager,
    &connection_id,
)
.await?;
manager.set_mode(&connection_id, mode_id).await
```

Add `db: State<'_, AppDatabase>` to Tauri wrappers that do not already receive it (`acp_set_mode`, `acp_set_config_option`, `acp_answer_question`, `submit_session_feedback`). In Axum handlers use `&state.db` and `&state.connection_manager`. For connect, always run identity agreement before preflight:

```rust
crate::commands::delegate_access::ensure_connect_delegate_interactive(
    &db,
    &manager,
    agent_type,
    session_id.as_deref(),
    conversation_id,
)
.await?;
```

Map Axum `AcpError` values through `app_command_error()` before the generic task failure so the stable 409 survives. **Exception for feedback HTTP:** preserve the existing special arms for `NoActiveTurn`, `FeedbackDisabled`, and `InvalidFeedback`; only add `DelegateViewerOnly` → 409 / `app_command_error` mapping alongside them (do not collapse all feedback errors into a single generic path).

```rust
.map_err(|error| {
    error
        .app_command_error()
        .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
})?;
```

Do not place this guard in `ConnectionManager`, `DelegationBroker`, the spawner, continuation, disconnect, child cleanup, or settlement paths.

- [ ] **Step 4: Run admission and both-target checks**

Run:

```powershell
Set-Location src-tauri
cargo test --features test-utils delegate_viewer_only -- --nocapture
cargo test --features test-utils --test delegate_access_api -- --nocapture
cargo check
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

Expected: locked mutations serialize `delegate_viewer_only`, HTTP returns 409, permission response is not rejected by this policy, and broker/codeg-mcp targets still compile without a user-entry guard in their internal paths.

- [ ] **Step 5: Commit backend enforcement**

```powershell
git add src-tauri/src/acp/error.rs src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs src-tauri/src/commands/delegate_access.rs src-tauri/src/commands/acp.rs src-tauri/src/commands/feedback.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/web/handlers/feedback.rs src-tauri/tests/delegate_access_api.rs
git commit -m "fix: enforce delegated child viewer-only admission"
```

---

### Task 3: Frontend projection API, typed rejection, and access refresh hook

**Files:**
- Create: `src/lib/delegate-access.ts`
- Create: `src/hooks/use-delegate-access.ts`
- Create: `src/hooks/use-delegate-access.test.ts`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`

**Interfaces:**
- Consumes: Task 1's `POST get_delegate_access` and Task 2's `delegate_viewer_only` error payload.
- Produces: `DelegateAccessState`, `getDelegateAccess(conversationId)`, `isDelegateViewerOnlyRejection(error)`, and `useDelegateAccess({ conversationId, enabled })` returning `{ access, loading, refresh }`; failed lookups retry after `300, 700, 1500, 2500` ms and every timer is canceled on success, scope change, disable, or unmount.

- [ ] **Step 1: Write hook tests for fail-closed loading, cancelable retry, coalescing, events, reconnect, stale responses, and rejection recognition**

Create `src/hooks/use-delegate-access.test.ts`:

```tsx
import { act, renderHook, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { useDelegateAccess } from "./use-delegate-access"
import { isDelegateViewerOnlyRejection } from "@/lib/delegate-access"

const h = vi.hoisted(() => ({
  get: vi.fn(),
  handlers: new Map<string, (payload: unknown) => void>(),
  reconnect: null as (() => void) | null,
}))

vi.mock("@/lib/api", () => ({ getDelegateAccess: h.get }))
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(async (name: string, handler: (payload: unknown) => void) => {
    h.handlers.set(name, handler)
    return () => h.handlers.delete(name)
  }),
  onTransportReconnect: (callback: () => void) => {
    h.reconnect = callback
    return () => { h.reconnect = null }
  },
}))

beforeEach(() => {
  h.get.mockReset()
  h.handlers.clear()
  h.reconnect = null
})

afterEach(() => vi.useRealTimers())

describe("useDelegateAccess", () => {
  it("is fail-closed while loading and on lookup failure", async () => {
    let reject!: (error: Error) => void
    h.get.mockReturnValue(new Promise((_, r) => { reject = r }))
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    expect(result.current.access).toEqual({
      mode: "viewer_only",
      reason: "state_unknown",
      parent_id: null,
    })
    act(() => reject(new Error("offline")))
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.access.reason).toBe("state_unknown")
  })

  it("retries a failed lookup with backoff and cancels timers on unmount", async () => {
    vi.useFakeTimers()
    h.get
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({ mode: "interactive", reason: null, parent_id: 3 })
    const { result, unmount } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await act(async () => { await Promise.resolve() })
    expect(result.current.access.reason).toBe("state_unknown")
    expect(h.get).toHaveBeenCalledTimes(1)

    await act(async () => { await vi.advanceTimersByTimeAsync(300) })
    expect(h.get).toHaveBeenCalledTimes(2)
    expect(result.current.access.mode).toBe("interactive")

    h.get.mockRejectedValueOnce(new Error("offline again"))
    await act(async () => { await result.current.refresh() })
    unmount()
    await vi.runAllTimersAsync()
    expect(h.get).toHaveBeenCalledTimes(3)
  })

  it("never applies a stale interactive result after the child id changes", async () => {
    let resolveOld!: (value: unknown) => void
    h.get
      .mockReturnValueOnce(new Promise((resolve) => { resolveOld = resolve }))
      .mockResolvedValueOnce({
        mode: "viewer_only",
        reason: "task_running",
        parent_id: 4,
      })
    const { result, rerender } = renderHook(
      ({ id }) => useDelegateAccess({ conversationId: id, enabled: true }),
      { initialProps: { id: 7 } }
    )
    rerender({ id: 8 })
    await waitFor(() => expect(h.get).toHaveBeenCalledWith(8))
    act(() => resolveOld({ mode: "interactive", reason: null, parent_id: 3 }))
    await waitFor(() => expect(result.current.access.reason).toBe("task_running"))
  })

  it("coalesces refreshes and refreshes for child, parent, and reconnect", async () => {
    h.get.mockResolvedValue({
      mode: "viewer_only",
      reason: "parent_turn_active",
      parent_id: 3,
    })
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))
    const changed = h.handlers.get("conversation://changed")!
    act(() => {
      changed({ kind: "state", patch: { id: 3 } })
      changed({ kind: "state", patch: { id: 7 } })
    })
    await waitFor(() => expect(h.get.mock.calls.length).toBeGreaterThanOrEqual(2))
    act(() => h.reconnect?.())
    await waitFor(() => expect(h.get.mock.calls.length).toBeGreaterThanOrEqual(3))
    await act(async () => {
      await Promise.all([result.current.refresh(), result.current.refresh()])
    })
  })

  it("recognizes the structured backend rejection", () => {
    expect(isDelegateViewerOnlyRejection({
      code: "delegate_viewer_only",
      message: "Delegated conversation is read-only",
      detail: "task_running",
    })).toBe(true)
  })
})
```

- [ ] **Step 2: Run the hook test and verify the new imports fail**

Run:

```powershell
pnpm test -- src/hooks/use-delegate-access.test.ts
```

Expected: FAIL because the delegate access types, API wrapper, rejection helper, and hook do not exist.

- [ ] **Step 3: Add strict types, API wrapper, rejection helper, and coalesced hook**

Add to `src/lib/types.ts`:

```ts
export type DelegateAccessMode = "viewer_only" | "interactive"
export type DelegateAccessReason =
  | "task_running"
  | "parent_turn_active"
  | "state_unknown"

export interface DelegateAccessState {
  mode: DelegateAccessMode
  reason: DelegateAccessReason | null
  parent_id: number | null
}
```

Add `"delegate_viewer_only"` to `AppErrorCode`, and add this API wrapper to `src/lib/api.ts`:

```ts
export function getDelegateAccess(
  conversationId: number
): Promise<DelegateAccessState> {
  return getTransport().call("get_delegate_access", { conversationId })
}
```

Create `src/lib/delegate-access.ts`:

```ts
import { extractAppCommandError } from "@/lib/app-error"
import type { DelegateAccessState } from "@/lib/types"

export const UNKNOWN_DELEGATE_ACCESS: DelegateAccessState = {
  mode: "viewer_only",
  reason: "state_unknown",
  parent_id: null,
}

export const NON_DELEGATE_ACCESS: DelegateAccessState = {
  mode: "interactive",
  reason: null,
  parent_id: null,
}

export function isDelegateViewerOnlyRejection(error: unknown): boolean {
  return extractAppCommandError(error)?.code === "delegate_viewer_only"
}
```

Create `src/hooks/use-delegate-access.ts` with one single-flight request per child scope, one queued rerun, and cancelable lookup-error backoff:

```ts
"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { getDelegateAccess } from "@/lib/api"
import {
  NON_DELEGATE_ACCESS,
  UNKNOWN_DELEGATE_ACCESS,
} from "@/lib/delegate-access"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import {
  CONVERSATION_CHANGED_EVENT,
  type ConversationChange,
  type DelegateAccessState,
} from "@/lib/types"

export interface UseDelegateAccessArgs {
  conversationId: number | null
  enabled: boolean
}

const ACCESS_LOOKUP_RETRY_DELAYS_MS = [300, 700, 1500, 2500] as const

function changedId(change: ConversationChange): number {
  return change.kind === "upsert"
    ? change.summary.id
    : change.kind === "deleted"
      ? change.id
      : change.patch.id
}

export function useDelegateAccess({
  conversationId,
  enabled,
}: UseDelegateAccessArgs) {
  const scope = enabled && conversationId != null
    ? `delegate:${conversationId}`
    : "disabled"
  const [snapshot, setSnapshot] = useState<{
    scope: string
    access: DelegateAccessState
    loading: boolean
  }>(() => ({
    scope,
    access: enabled ? UNKNOWN_DELEGATE_ACCESS : NON_DELEGATE_ACCESS,
    loading: enabled,
  }))
  // Scope mismatch is synchronously fail-closed during render. Waiting for an
  // effect here would leave one frame where child B inherits child A's unlock.
  const access = !enabled
    ? NON_DELEGATE_ACCESS
    : snapshot.scope === scope
      ? snapshot.access
      : UNKNOWN_DELEGATE_ACCESS
  const loading = enabled && (snapshot.scope !== scope || snapshot.loading)
  const accessRef = useRef(access)
  accessRef.current = access
  const requestRefreshRef = useRef<() => Promise<void>>(
    async () => undefined
  )
  const refresh = useCallback(
    (): Promise<void> => requestRefreshRef.current(),
    []
  )

  useEffect(() => {
    setSnapshot({
      scope,
      access: enabled ? UNKNOWN_DELEGATE_ACCESS : NON_DELEGATE_ACCESS,
      loading: enabled,
    })
    let disposed = false
    let dispose: (() => void) | undefined
    let retryTimer: ReturnType<typeof setTimeout> | null = null
    let retryIndex = 0
    let inFlight: Promise<void> | null = null
    let rerun = false

    const cancelRetry = () => {
      if (retryTimer) clearTimeout(retryTimer)
      retryTimer = null
    }
    const run = (resetBackoff: boolean): Promise<void> => {
      if (disposed || !enabled || conversationId == null) {
        return Promise.resolve()
      }
      if (resetBackoff) {
        retryIndex = 0
        cancelRetry()
      }
      if (inFlight) {
        rerun = true
        return inFlight
      }
      setSnapshot((current) =>
        current.scope === scope ? { ...current, loading: true } : current
      )
      const request = getDelegateAccess(conversationId)
        .then((next) => {
          if (disposed) return
          retryIndex = 0
          cancelRetry()
          setSnapshot({ scope, access: next, loading: false })
        })
        .catch(() => {
          if (disposed) return
          setSnapshot((current) => ({
            scope,
            loading: false,
            access: {
              ...UNKNOWN_DELEGATE_ACCESS,
              parent_id:
                current.scope === scope ? current.access.parent_id : null,
            },
          }))
          const delay = ACCESS_LOOKUP_RETRY_DELAYS_MS[retryIndex]
          if (delay !== undefined) {
            retryIndex += 1
            retryTimer = setTimeout(() => {
              retryTimer = null
              void run(false)
            }, delay)
          }
        })
        .finally(() => {
          if (inFlight === request) inFlight = null
          if (disposed) return
          setSnapshot((current) =>
            current.scope === scope ? { ...current, loading: false } : current
          )
          if (rerun) {
            rerun = false
            cancelRetry()
            queueMicrotask(() => void run(true))
          }
        })
      inFlight = request
      return request
    }

    const scopeRefresh = () => run(true)
    requestRefreshRef.current = scopeRefresh
    if (enabled && conversationId != null) void run(true)
    void subscribe<ConversationChange>(CONVERSATION_CHANGED_EVENT, (change) => {
      const id = changedId(change)
      const current = accessRef.current
      if (id === conversationId || id === current.parent_id) void run(true)
    }).then((off) => {
      if (disposed) off()
      else dispose = off
    })
    const offReconnect = onTransportReconnect(() => void run(true))
    return () => {
      disposed = true
      cancelRetry()
      if (requestRefreshRef.current === scopeRefresh) {
        requestRefreshRef.current = async () => undefined
      }
      dispose?.()
      offReconnect?.()
    }
  }, [conversationId, enabled, scope])

  return { access, loading, refresh }
}
```

The scope-local `disposed` flag and `scopeRefresh` identity are both required: a stale promise or timer from conversation A can never apply an interactive result to conversation B or overwrite B's refresh function.

- [ ] **Step 4: Run focused frontend tests and type-aware lint**

Run:

```powershell
pnpm test -- src/hooks/use-delegate-access.test.ts
pnpm eslint src/hooks/use-delegate-access.ts src/hooks/use-delegate-access.test.ts src/lib/delegate-access.ts src/lib/api.ts src/lib/types.ts
```

Expected: both commands exit 0; loading/error stay fail-closed, lookup failures retry on the declared backoff, cleanup cancels retries, stale child results are ignored, and parent/child/reconnect refreshes converge without overlapping requests.

- [ ] **Step 5: Commit the frontend access boundary**

```powershell
git add src/lib/types.ts src/lib/api.ts src/lib/delegate-access.ts src/hooks/use-delegate-access.ts src/hooks/use-delegate-access.test.ts
git commit -m "feat: add delegated child access hook"
```

---

### Task 4: Canonical ACP state with tab observer aliases

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`

**Interfaces:**
- Consumes: the existing ref store keyed by `contextKey`, `ConnectionState.connectionId`, `connectAsViewer`, delegation attach/detach, per-key listeners, live sinks, and user action methods.
- Produces: `observerAliasesRef: Map<string, string>` mapping `tabId -> canonicalContextKey`; the canonical key for an observed backend ACP is exactly its `connectionId`; alias-aware state reads, subscriptions, live sinks, actions, and cleanup; one canonical snapshot/cursor/subscription per backend connection.

- [ ] **Step 1: Add failing canonical-alias lifecycle tests**

Extend `src/contexts/acp-connections-context.test.tsx` with tests that use the existing provider harness:

```tsx
describe("AcpConnectionsProvider canonical observer aliases", () => {
  it("publishes one canonical state to the tab alias", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    expect(h.store!.getConnection(TAB)).toBe(
      h.store!.getConnection("broker-child")
    )
    expect(h.store!.getConnection(TAB)?.contextKey).toBe("broker-child")
    expect(h.attach).toHaveBeenCalledTimes(1)
  })

  it("fans canonical updates to alias listeners and alias live sinks", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    const notify = vi.fn()
    const sink = vi.fn()
    const off = h.store!.subscribeKey(TAB, notify)
    const offSink = h.actions!.registerLiveMessageSink(TAB, sink)
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "broker-child",
      type: "content_delta",
      text: "live child output",
    })

    expect(notify).toHaveBeenCalled()
    expect(sink).toHaveBeenCalledTimes(1)
    expect(sink.mock.calls[0][0].content).toContainEqual({
      type: "text",
      text: "live child output",
    })
    offSink()
    off()
  })

  it("merges delegation metadata into an existing canonical viewer", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    const original = h.store!.getConnection(TAB)

    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })

    const merged = h.store!.getConnection(TAB)
    expect(merged?.liveMessage).toBe(original?.liveMessage)
    expect(merged).toMatchObject({
      isViewer: true,
      isDelegationChild: true,
      parentConnectionId: "parent",
      parentToolUseId: "tool-1",
    })
    expect(h.attach).toHaveBeenCalledTimes(1)
  })

  it("retains observer state after delegation detach and never disconnects it", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
      h.actions!.detachDelegationChild("broker-child")
    })

    expect(h.store!.getConnection(TAB)).toMatchObject({
      connectionId: "broker-child",
      isViewer: true,
      isDelegationChild: false,
      parentConnectionId: null,
      parentToolUseId: null,
    })
    await act(async () => { await h.actions!.disconnect(TAB) })
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })
})
```

Also add focused cases proving that `sendPrompt`, `setMode`, `setConfigOption`, `cancel`, `respondPermission`, `answerQuestion`, and `clearAcpLoadError` resolve an alias to the canonical connection, while `touchActivity(TAB)` and the periodic keepalive never call `acpTouchConnection` for an alias. The permission case must assert `acpRespondPermission("broker-child", requestId, optionId)`.

- [ ] **Step 2: Run the alias tests and confirm the duplicated-state failure**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx
```

Expected: FAIL because the current viewer lives under `TAB`, a delegation attach with the same `connectionId` can create or preserve a second state, canonical updates do not notify alias listeners, and alias cleanup has no distinct lifecycle.

- [ ] **Step 3: Add canonical key resolution and alias fan-out**

Add these refs/helpers beside `storeRef`:

```ts
// Observer tabs are names only. Canonical ACP state, cursor, subscription and
// reverse routing stay under the backend connection id.
const observerAliasesRef = useRef(new Map<string, string>())

const canonicalKey = useCallback((key: string): string => {
  return observerAliasesRef.current.get(key) ?? key
}, [])

const aliasKeysFor = useCallback((canonical: string): string[] => {
  const aliases: string[] = []
  for (const [alias, target] of observerAliasesRef.current) {
    if (target === canonical) aliases.push(alias)
  }
  return aliases
}, [])

const getConnectionForKey = useCallback((key: string) => {
  return storeRef.current.connections.get(canonicalKey(key))
}, [canonicalKey])
```

Change `ConnectionStoreApi` without changing its public signature:

```ts
getConnection(key: string) {
  const canonical = observerAliasesRef.current.get(key) ?? key
  return storeRef.current.connections.get(canonical)
}
```

Keep listener registration under the requested key, but replace every canonical notification with a fan-out:

```ts
const notifyConnectionKeys = useCallback(
  (canonical: string) => {
    notifyKeyListeners(canonical)
    for (const alias of aliasKeysFor(canonical)) notifyKeyListeners(alias)
  },
  [aliasKeysFor, notifyKeyListeners]
)
```

Apply the same key set to `mirrorLiveMessageOnce`: invoke at most one sink per registered key, canonical first and then each alias. Registration reads the canonical state immediately, but the sink remains stored under the caller's key so two open aliases can mirror into two distinct runtime sessions without copying `ConnectionState`.

Audit every context action. State reads and reducer dispatches for mutation actions use `canonicalKey(contextKey)`. `setActiveKey` keeps the tab id, and `touchActivity` returns immediately when `observerAliasesRef.current.has(contextKey)`; this makes alias focus a UI event, not an ACP keepalive or health event.

- [ ] **Step 4: Canonicalize viewer attach and make delegation metadata mergeable**

Change `connectAsViewer` so it binds `contextKey` as an alias and stores the viewer at `connectionId`:

```ts
const bindObserverAlias = (
  alias: string,
  connectionId: string,
  agentType: AgentType,
  workingDir: string | null,
  conversationId: number | null
) => {
  const previous = observerAliasesRef.current.get(alias)
  if (previous && previous !== connectionId) releaseObserverAlias(alias)
  observerAliasesRef.current.set(alias, connectionId)

  const existing = storeRef.current.connections.get(connectionId)
  if (existing) {
    dispatch({
      type: "OBSERVER_METADATA_MERGED",
      contextKey: connectionId,
      conversationId,
      workingDir,
    })
  } else {
    dispatch({
      type: "CONNECTION_CREATED",
      contextKey: connectionId,
      connectionId,
      agentType,
      workingDir,
      isViewer: true,
      conversationId,
    })
  }
  notifyKeyListeners(alias)
}
```

Call the helper at the start of `connectAsViewer`, then keep activity, attach routing, desktop hydration, and reverse routing on the canonical key:

```ts
bindObserverAlias(
  contextKey,
  connectionId,
  agentType,
  workingDir,
  conversationId
)
lastActivityRef.current.set(connectionId, Date.now())

const stream = getEventStream()
if (stream) {
  if (!attachSubscriptionsRef.current.has(connectionId)) {
    setupAttachSubscription(connectionId, connectionId, undefined)
  }
  return
}
```

In the existing desktop branch, re-check `storeRef.current.connections.get(connectionId)`, dispatch `HYDRATE_FROM_SNAPSHOT` with `contextKey: connectionId`, and set `reverseMapRef.current.set(connectionId, connectionId)`. `OBSERVER_METADATA_MERGED` must preserve the complete existing state and set only `isViewer: true`, a non-null incoming `conversationId`, and the viewer's working directory when the canonical entry did not have one. It must not reset `liveMessage`, cursor, permission, selector, tool, or delegation fields.

Modify `DELEGATION_CHILD_ATTACH`: when the same canonical `connectionId` exists, merge `isDelegationChild: true`, `parentConnectionId`, and `parentToolUseId` instead of returning the old object unchanged. Modify `DELEGATION_CHILD_DETACH` to carry `retainObserver`; when true, clear only those three delegation fields and retain the canonical viewer. `detachDelegationChild` computes `retainObserver` from `aliasKeysFor(connectionId).length > 0` and leaves the existing attach subscription intact in that branch.

Only create an attach subscription or desktop reverse-map entry when the canonical connection does not already have one. A delegation event arriving after viewer discovery therefore enriches the one canonical entry without opening a second stream.

- [ ] **Step 5: Add alias-only cleanup**

At the beginning of `disconnect(contextKey)`, release aliases before looking up direct owners:

```ts
const releaseObserverAlias = (alias: string): string | null => {
  const canonical = observerAliasesRef.current.get(alias)
  if (!canonical) return null
  observerAliasesRef.current.delete(alias)
  liveSinksRef.current.delete(alias)
  notifyKeyListeners(alias)

  const hasOtherAlias = aliasKeysFor(canonical).length > 0
  const conn = storeRef.current.connections.get(canonical)
  if (!hasOtherAlias && conn?.isViewer && !conn.isDelegationChild) {
    teardownAttachSubscription(canonical)
    reverseMapRef.current.delete(conn.connectionId)
    pendingUnmappedEventsRef.current.delete(conn.connectionId)
    lastActivityRef.current.delete(canonical)
    dispatch({ type: "CONNECTION_REMOVED", contextKey: canonical })
  }
  return canonical
}

const disconnect = useCallback(async (contextKey: string) => {
  if (observerAliasesRef.current.has(contextKey)) {
    releaseObserverAlias(contextKey)
    return
  }
  // Existing direct owner/viewer teardown follows.
}, [releaseObserverAlias])
```

Clear aliases in `disconnectAll` and provider teardown after detaching canonical viewer subscriptions. Never call `acpDisconnect` for a canonical entry with `isViewer` or `isDelegationChild`. The open-tab keepalive continues to use direct map lookup, so an active/open alias deliberately produces no `acpTouchConnection` call.

- [ ] **Step 6: Run focused tests and lint**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx
pnpm eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
```

Expected: both commands exit 0; one backend `connectionId` has one state/cursor/subscription, every alias receives the same updates, delegation metadata merges and clears in place, and closing aliases never disconnects the broker ACP.

- [ ] **Step 7: Commit canonical observer aliases**

```powershell
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit -m "fix: share canonical ACP state with observer tabs"
```

---

### Task 5: Explicit observe intent, bounded discovery, cold reconnect, and owner handoff

**Files:**
- Create: `src/lib/transport/web-event-stream.test.ts`
- Modify: `src/lib/transport/types.ts`
- Modify: `src/lib/transport/web-event-stream.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/hooks/use-connection.ts`
- Modify: `src/hooks/use-connection-lifecycle.ts`
- Modify: `src/hooks/use-connection-lifecycle.test.ts`

**Interfaces:**
- Consumes: Task 4's canonical observer aliases, `acpFindConnectionForConversation(conversationId, sessionId, agentType)`, the normal owner `acpConnect` path, and `EventStream.attach`.
- Produces: `type ConnectionIntent = "own_or_observe" | "observe_existing"`; connect requests carrying both `intent` and `retryObserverDiscovery`; observer delays `[0, 300, 700, 1500, 2500]`; `AttachOptions.reconnectMode?: "resume" | "cold"`; a no-duplicate observer-to-owner handoff.

- [ ] **Step 1: Write failing transport reconnect-mode tests**

Create `src/lib/transport/web-event-stream.test.ts` with a small fake host that records frames and exposes its registered ready callback:

```ts
import { beforeEach, describe, expect, it, vi } from "vitest"
import type { LiveSessionSnapshot } from "@/lib/types"
import { WebEventStream, type AttachTransportHost } from "./web-event-stream"

function snapshot(eventSeq: number): LiveSessionSnapshot {
  return {
    connection_id: "conn",
    conversation_id: 42,
    folder_id: 1,
    status: "connected",
    external_id: "sid",
    live_message: null,
    active_tool_calls: [],
    pending_permission: null,
    modes: null,
    current_mode: null,
    config_options: null,
    prompt_capabilities: null,
    usage: null,
    fork_supported: false,
    available_commands: [],
    selectors_ready: true,
    event_seq: eventSeq,
  }
}

function hostFixture() {
  let ready: (() => void) | null = null
  const sendFrame = vi.fn(() => true)
  const host: AttachTransportHost = {
    isWsOpen: () => true,
    sendFrame,
    onWsReady: (callback) => {
      ready = callback
      return () => { ready = null }
    },
  }
  return { host, sendFrame, reconnect: () => ready?.() }
}

const handlers = {
  onSnapshot: vi.fn(),
  onReplay: vi.fn(),
  onEvent: vi.fn(),
  onDetached: vi.fn(),
}

describe("WebEventStream reconnect mode", () => {
  beforeEach(() => vi.clearAllMocks())

  it("resumes an ordinary subscription from its last applied seq", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach("conn", {}, handlers)
    stream.handleServerFrame({
      type: "snapshot",
      subscription_id: sub.subscriptionId,
      connection_id: "conn",
      snapshot: snapshot(11),
      event_seq: 11,
    })
    f.sendFrame.mockClear()
    f.reconnect()
    expect(f.sendFrame).toHaveBeenCalledWith(
      expect.objectContaining({ action: "attach", since_seq: 11 })
    )
  })

  it("cold-reattaches a delegate observer even after applying events", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach(
      "conn",
      { reconnectMode: "cold" },
      handlers
    )
    stream.handleServerFrame({
      type: "event",
      subscription_id: sub.subscriptionId,
      envelope: { seq: 12, connection_id: "conn", type: "turn_complete" },
    })
    f.sendFrame.mockClear()
    f.reconnect()
    expect(f.sendFrame).toHaveBeenCalledWith(
      expect.objectContaining({ action: "attach", since_seq: undefined })
    )
  })
})
```

The assertion is the protocol distinction: default reconnect sends the cursor, while cold reconnect omits it.

- [ ] **Step 2: Implement `reconnectMode` without changing ordinary subscriptions**

Extend `AttachOptions`:

```ts
export interface AttachOptions {
  sinceSeq?: number
  /** `cold` always requests a full snapshot after WS reconnect. */
  reconnectMode?: "resume" | "cold"
}
```

Store the mode in `ActiveSub` with a default of `"resume"`, and choose the wire cursor in `sendAttach`:

```ts
interface ActiveSub {
  connectionId: string
  lastAppliedSeq: number | undefined
  reconnectMode: "resume" | "cold"
  handlers: AttachHandlers
}

private sendAttach(subscriptionId: string): void {
  const sub = this.subs.get(subscriptionId)
  if (!sub) return
  this.host.sendFrame({
    action: "attach",
    subscription_id: subscriptionId,
    connection_id: sub.connectionId,
    since_seq:
      sub.reconnectMode === "cold" ? undefined : sub.lastAppliedSeq,
  })
}
```

`setupAttachSubscription` accepts the reconnect mode and threads it through initial attach plus `lagged`/`server_shutdown` reattach. Extend `connectAsViewer` with the same final parameter so the Task 4 alias implementation and every Task 5 call share one signature. Apply these exact edits to the callback while retaining its existing desktop snapshot/hydration and detach-race branches:

```diff
 const connectAsViewer = useCallback(
   async (
     contextKey: string,
     connectionId: string,
     agentType: AgentType,
     workingDir: string | null,
-    conversationId: number | null
+    conversationId: number | null,
+    reconnectMode: "resume" | "cold" = "resume"
   ) => {
     lastActivityRef.current.set(connectionId, Date.now())

     const stream = getEventStream()
     if (stream) {
+      if (
+        reconnectMode === "cold" &&
+        attachSubscriptionsRef.current.has(connectionId)
+      ) {
+        teardownAttachSubscription(connectionId)
+      }
+      if (!attachSubscriptionsRef.current.has(connectionId)) {
+        setupAttachSubscription(
+          connectionId,
+          connectionId,
+          undefined,
+          reconnectMode
+        )
+      }
       return
     }
```

Existing owner and ordinary cross-client viewer calls pass or default to `"resume"`; only `observe_existing` passes `"cold"`. If a parent-side `DELEGATION_CHILD_ATTACH` already opened the canonical subscription, the cold observer path detaches that subscription before replacing it, preserving one subscription and one `ConnectionState` at every point.

- [ ] **Step 3: Add failing connection-intent tests**

Expose the existing cancel mock through the hoisted context harness so relock tests can prove that ownership retention does not cancel an admitted child turn:

```ts
// In the hoisted h object and @/lib/api mock respectively:
acpCancel: vi.fn(),
acpCancel: h.acpCancel,

// In beforeEach:
h.acpCancel.mockReset()
h.acpCancel.mockResolvedValue(undefined)
```

Extend the context test suite with these cases:

```tsx
it("observe_existing branches before SDK preflight and never spawns", async () => {
  h.acpFindConnectionForConversation.mockResolvedValue(null)
  await mountProvider()
  await act(async () => {
    await h.actions!.connect(
      TAB, "claude_code", "/tmp/x", "sid", 42, null, null,
      "observe_existing", false
    )
  })
  expect(h.acpGetAgentStatus).not.toHaveBeenCalled()
  expect(h.acpConnect).not.toHaveBeenCalled()
  expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
})

it("discovers a child that appears inside the bounded spawn window", async () => {
  vi.useFakeTimers()
  h.acpFindConnectionForConversation
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce({ connection_id: "broker-child", event_seq: 0 })
  await mountProvider()
  const pending = act(async () => {
    await h.actions!.connect(
      TAB, "claude_code", "/tmp/x", "sid", 42, null, null,
      "observe_existing", true
    )
  })
  await vi.advanceTimersByTimeAsync(300)
  await pending
  expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
  expect(h.acpConnect).not.toHaveBeenCalled()
})

it("keeps an admitted owner turn streaming when a later parent turn relocks it", async () => {
  h.acpFindConnectionForConversation.mockResolvedValue(null)
  await mountProvider()
  await act(async () => {
    await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
  })
  const handlers = latestAttachHandlers()
  emitAcpEvent(handlers, {
    seq: 1,
    connection_id: "spawned-conn",
    type: "status_changed",
    status: "prompting",
  })
  h.acpDisconnect.mockClear()
  h.acpCancel.mockClear()
  h.acpGetAgentStatus.mockClear()
  await act(async () => {
    await h.actions!.connect(
      TAB, "claude_code", "/tmp/x", "sid", 42, null, null,
      "observe_existing", false
    )
  })
  expect(h.store!.getConnection(TAB)?.isViewer).toBe(false)
  expect(h.acpDisconnect).not.toHaveBeenCalled()
  expect(h.acpCancel).not.toHaveBeenCalled()
  expect(h.acpGetAgentStatus).not.toHaveBeenCalled()

  emitAcpEvent(handlers, {
    seq: 2,
    connection_id: "spawned-conn",
    type: "content_delta",
    text: "reply after parent relock",
  })
  emitAcpEvent(handlers, {
    seq: 3,
    connection_id: "spawned-conn",
    type: "usage_update",
    used: 1,
    size: 100,
  })
  expect(h.store!.getConnection(TAB)?.liveMessage?.content).toEqual([
    { type: "text", text: "reply after parent relock" },
  ])

  emitAcpEvent(handlers, {
    seq: 4,
    connection_id: "spawned-conn",
    type: "turn_complete",
    session_id: "sid",
    stop_reason: "end_turn",
    mark_awaiting_reply: false,
  })
  expect(h.store!.getConnection(TAB)?.status).toBe("connected")
  expect(h.acpDisconnect).not.toHaveBeenCalled()
  expect(h.acpCancel).not.toHaveBeenCalled()
})
```

Add timer tests for all five discovery attempts, disconnect cancellation, request supersession when intent changes, and `retryObserverDiscovery: false` doing exactly one lookup. Add a handoff regression: start as an alias to `broker-child`; change to `own_or_observe`; return that same connection from discovery twice and then `null`; assert the alias subscription detaches before polling, `acpConnect` runs only after `null`, and the old connection is never passed to `acpDisconnect`. The owner resume must receive the original external session id and conversation id:

```ts
expect(h.acpConnect).toHaveBeenCalledWith(
  "claude_code",
  "/tmp/x",
  "sid",
  undefined,
  {},
  42,
  null,
  null
)
```

Add the reverse-order canonical-subscription regression as well: call `attachDelegationChild` first so `broker-child` already has a resume subscription, then connect `TAB` with `observe_existing`. Assert the old subscription's `detach` is called before the replacement `attach`, the replacement options are `{ sinceSeq: undefined, reconnectMode: "cold" }`, the canonical state object is preserved, and `acpConnect` remains uncalled.

In `src/hooks/use-connection-lifecycle.test.ts`, assert that intent/retry changes retrigger connect, unmount calls `disconnect` to cancel observer polling, and `handleReconnect` preserves the current intent.

- [ ] **Step 4: Add intent to every connect request boundary**

Export the type from `acp-connections-context.tsx`:

```ts
export type ConnectionIntent = "own_or_observe" | "observe_existing"
```

Append two parameters to `AcpActionsValue.connect`, `UseConnectionReturn.connect`, and their wrappers:

```ts
intent: ConnectionIntent = "own_or_observe",
retryObserverDiscovery = false
```

Store both in `ConnectRequest` and compare both in `sameConnectRequest`:

```ts
type ConnectRequest = {
  agentType: AgentType
  workingDir?: string
  sessionId?: string
  conversationId?: number
  delegationRouteOverride?: DelegationRoutePolicy | null
  ownerOperationId?: string | null
  intent: ConnectionIntent
  retryObserverDiscovery: boolean
}
```

Thread them through the pending-request microtask. Without this, a queued `observe_existing -> own_or_observe` transition is incorrectly deduplicated and can leave the child permanently locked to its old observer lifecycle.

Add `connectionIntent` and `retryObserverDiscovery` to `UseConnectionLifecycleOptions`, refs, the auto-connect effect dependencies, focus reconnect, explicit reconnect, and every `connConnect` call. Defaults preserve current callers.

- [ ] **Step 5: Branch observer discovery before preflight and make its delay cancelable**

Add the fixed delays and a cancelable delay map:

```ts
const OBSERVER_DISCOVERY_DELAYS_MS = [0, 300, 700, 1500, 2500] as const
const observerDelayCancelsRef = useRef(new Map<string, () => void>())

const waitObserverDelay = (key: string, delayMs: number) =>
  new Promise<boolean>((resolve) => {
    if (delayMs === 0) return resolve(true)
    const timer = setTimeout(() => {
      observerDelayCancelsRef.current.delete(key)
      resolve(true)
    }, delayMs)
    observerDelayCancelsRef.current.set(key, () => {
      clearTimeout(timer)
      observerDelayCancelsRef.current.delete(key)
      resolve(false)
    })
  })
```

When a same-key connect is superseded or `disconnect` runs, invoke and remove that cancel function. Then branch at the top of `connect` after the concurrent-request guard and before `acpGetAgentStatus`:

```ts
if (intent === "observe_existing") {
  const direct = storeRef.current.connections.get(contextKey)
  if (direct && !observerAliasesRef.current.has(contextKey)) {
    // Re-locking a locally owned connection changes access, not ownership.
    return
  }
  if (conversationId == null || conversationId <= 0) return

  const delays = retryObserverDiscovery
    ? OBSERVER_DISCOVERY_DELAYS_MS
    : OBSERVER_DISCOVERY_DELAYS_MS.slice(0, 1)
  for (const delay of delays) {
    if (!(await waitObserverDelay(contextKey, delay))) return
    if (abandonedKeysRef.current.has(contextKey)) return
    const queued = pendingConnectRequestsRef.current.get(contextKey)
    if (queued && !sameConnectRequest(queued, request)) return

    let discovered: ConversationConnectionInfo | null = null
    try {
      discovered = await acpFindConnectionForConversation(
        conversationId,
        sessionId,
        agentType
      )
    } catch (error) {
      console.warn("[acp-context] observer discovery failed", error)
      // Classify: transport/timeout/5xx → retryable (continue).
      // Auth, not-found permanent, malformed, explicit unrecoverable → stop.
      // Never fall through to acpConnect from this branch.
      if (!isRetryableObserverDiscoveryError(error)) {
        return
      }
      continue
    }
    if (abandonedKeysRef.current.has(contextKey)) return
    const queuedAfterLookup = pendingConnectRequestsRef.current.get(contextKey)
    if (queuedAfterLookup && !sameConnectRequest(queuedAfterLookup, request)) {
      return
    }
    if (!discovered) continue
    await connectAsViewer(
      contextKey,
      discovered.connection_id,
      agentType,
      workingDir ?? null,
      conversationId,
      "cold"
    )
    return
  }
  return
}
```

`retryObserverDiscovery` is true only for access reason `task_running`. A terminal child locked solely by `parent_turn_active` therefore performs one lookup and never starts a reconnect loop. A failed/null observer lookup never reaches `acpConnect` under any branch.

Define and unit-test the pure helper (Amendment 21):

```ts
/** Auth/401/403, permanent not-found, malformed, permanent protocol → false.
 *  Timeout/network/5xx/temporary not-ready → true. */
export function isRetryableObserverDiscoveryError(error: unknown): boolean
```

Add tests: retryable (timeout, 5xx), auth (non-retryable), permanent not-found (non-retryable). Same helper is used by observe_existing and handoff discovery.

- [ ] **Step 6: Implement observer-to-owner handoff without duplication**

At the start of `own_or_observe`, detect an alias. Release it first and retain the old canonical `connectionId` in a local variable. Poll discovery on the same fixed delays:

```ts
const releasedObserverId = observerAliasesRef.current.get(contextKey) ?? null
if (releasedObserverId) releaseObserverAlias(contextKey)

if (releasedObserverId && conversationId != null && conversationId > 0) {
  let oldStillAlive = true
  for (const delay of OBSERVER_DISCOVERY_DELAYS_MS) {
    if (!(await waitObserverDelay(contextKey, delay))) return
    if (abandonedKeysRef.current.has(contextKey)) return
    const queuedBeforeLookup = pendingConnectRequestsRef.current.get(contextKey)
    if (queuedBeforeLookup && !sameConnectRequest(queuedBeforeLookup, request)) {
      return
    }
    let found: ConversationConnectionInfo | null = null
    try {
      found = await acpFindConnectionForConversation(
        conversationId,
        sessionId,
        agentType
      )
    } catch (error) {
      // Same classification as observe_existing: do NOT treat errors as
      // confirmed disappearance (that would spawn a second ACP while the
      // broker may still be alive).
      console.warn("[acp-context] handoff discovery failed", error)
      if (!isRetryableObserverDiscoveryError(error)) {
        await connectAsViewer(
          contextKey,
          releasedObserverId,
          agentType,
          workingDir ?? null,
          conversationId,
          "resume"
        )
        return
      }
      continue
    }
    if (abandonedKeysRef.current.has(contextKey)) return
    const queuedAfterLookup = pendingConnectRequestsRef.current.get(contextKey)
    if (queuedAfterLookup && !sameConnectRequest(queuedAfterLookup, request)) {
      return
    }
    if (!found) {
      oldStillAlive = false
      break
    }
    if (found.connection_id !== releasedObserverId) {
      await connectAsViewer(
        contextKey,
        found.connection_id,
        agentType,
        workingDir ?? null,
        conversationId,
        "resume"
      )
      return
    }
  }
  if (oldStillAlive) {
    // Do not strand the tab with a hard throw that requires manual reconnect.
    await connectAsViewer(
      contextKey,
      releasedObserverId,
      agentType,
      workingDir ?? null,
      conversationId,
      "resume"
    )
    // Explicit re-entry (Amendment 20): focus auto-connect ignores status
    // changes, so register a one-shot listener for broker removal on this
    // canonical id. When it fires, re-invoke connect with stored
    // intent "own_or_observe" if the surface still wants interactive ownership.
    // Cancel the listener on unmount, intent change, or successful owner connect.
    scheduleOwnOrObserveOnBrokerRemoved(contextKey, releasedObserverId, request)
    return
  }
}
```

After the old broker connection disappears, continue through the existing SDK preflight and resume/create path using the same `sessionId`/`external_id`. If a different live owner appears, attach to it normally. This ordering guarantees there is no interval with the broker ACP and a replacement owner ACP for the same child.

**Required test:** active tab remains focused; handoff polls exhaust with broker still alive → re-attach observer → simulate broker `CONNECTION_REMOVED` → assert a subsequent `connect` with `intent: "own_or_observe"` runs and completes owner path without remount or focus toggle.

- [ ] **Step 7: Run transport, context, lifecycle, and lint checks**

Run:

```powershell
pnpm test -- src/lib/transport/web-event-stream.test.ts src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts
pnpm eslint src/lib/transport/types.ts src/lib/transport/web-event-stream.ts src/lib/transport/web-event-stream.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection.ts src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts
```

Expected: both commands exit 0; observer discovery never preflights or spawns, spawn-window polling is bounded and cancelable, terminal parent-only lock does one lookup, relock retains owners, handoff waits for the broker ACP to vanish, and delegate observers cold-snapshot after reconnect.

- [ ] **Step 8: Commit explicit observer connection lifecycle**

```powershell
git add src/lib/transport/types.ts src/lib/transport/web-event-stream.ts src/lib/transport/web-event-stream.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection.ts src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts
git commit -m "feat: add observer-only ACP connection intent"
```

---

### Task 6: Main-tab access wiring and explicit interaction capability lock

**Files:**
- Create: `src/components/chat/conversation-shell.test.tsx`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-session-surface.test.ts`
- Modify: `src/hooks/use-connection-lifecycle.ts`
- Modify: `src/hooks/use-connection-lifecycle.test.ts`
- Modify: `src/components/chat/conversation-shell.tsx`
- Modify: `src/components/chat/chat-input.tsx`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/chat/message-input.test.tsx`
- Modify: `src/components/chat/chat-input.test.tsx`
- Modify: `src/components/chat/question-dialog.tsx`
- Modify: `src/components/chat/question-dialog.test.tsx`
- Modify: `src/components/chat/ask-question-card.tsx`
- Modify: `src/components/chat/ask-question-card.test.tsx`
- Modify: `src/hooks/use-session-feedback.ts`
- Modify: `src/hooks/use-session-feedback.test.ts`
- Modify: `src/components/chat/mode-selector.tsx`
- Modify: `src/components/chat/session-config-selector.tsx`
- Modify: `src/components/chat/session-config-selector.test.tsx`
- Modify: `src/components/chat/model-option-picker.tsx`

**Interfaces:**
- Consumes: Task 3's fail-closed `useDelegateAccess`, Task 5's `ConnectionIntent`, the child `DbConversationDetail`, current queue/runtime actions, and Task 2's typed `delegate_viewer_only` rejection.
- Produces: an independent `interactionLocked: boolean` capability propagated through the main conversation surface; permission responses remain outside that lock; `onDelegateViewerOnly` restores a raced draft and refreshes access.

- [ ] **Step 1: Add failing surface and shell capability tests**

Extend `conversation-session-surface.test.ts` with pure policy coverage and the existing surface harness:

```ts
it("uses child detail when the root workspace store excludes the row", () => {
  expect(resolveSurfacePersistedSummary(null, childDetail.summary)).toBe(
    childDetail.summary
  )
})

it("maps fail-closed delegate access to observer connection policy", () => {
  expect(resolveDelegateConnectionPolicy({
    isDelegate: true,
    access: {
      mode: "viewer_only",
      reason: "task_running",
      parent_id: 10,
    },
  })).toEqual({
    interactionLocked: true,
    intent: "observe_existing",
    retryObserverDiscovery: true,
  })
})

it("terminal child plus idle parent restores normal connection policy", () => {
  expect(resolveDelegateConnectionPolicy({
    isDelegate: true,
    access: { mode: "interactive", reason: null, parent_id: 10 },
  })).toEqual({
    interactionLocked: false,
    intent: "own_or_observe",
    retryObserverDiscovery: false,
  })
})
```

Create `conversation-shell.test.tsx`. Mock `ChatInput`, `QuestionDialog`, and `AskQuestionCard` to capture props, but render the real `PermissionDialog`. Assert that `interactionLocked` reaches all mutation controls, question answers are disabled, and clicking a permission approve/reject option still calls `onRespondPermission` with the exact request/option ids.

Add a surface integration case where the child is absent from `useAppWorkspaceStore.conversations`, its detail says `kind: "delegate"`, and access is `task_running`: assert detail content renders, lifecycle receives `observe_existing`, and no owner reconnect affordance is exposed.

- [ ] **Step 2: Run surface/shell tests and verify the current root-only policy fails**

Run:

```powershell
pnpm test -- src/components/conversations/conversation-session-surface.test.ts src/components/chat/conversation-shell.test.tsx
```

Expected: FAIL because root summaries exclude child rows, no access hook drives connection intent, and controls have no capability separate from `isViewer`/connection status.

- [ ] **Step 3: Resolve delegate identity, summary fallback, and connection policy in the surface**

Export these pure helpers beside the existing reconnect-policy helpers:

```ts
export function resolveSurfacePersistedSummary(
  root: DbConversationSummary | null,
  detail: DbConversationSummary | null
): DbConversationSummary | null {
  return root ?? detail
}

export function resolveDelegateConnectionPolicy(args: {
  isDelegate: boolean
  access: DelegateAccessState
}): {
  interactionLocked: boolean
  intent: ConnectionIntent
  retryObserverDiscovery: boolean
} {
  const interactionLocked =
    args.isDelegate && args.access.mode === "viewer_only"
  return {
    interactionLocked,
    intent: interactionLocked ? "observe_existing" : "own_or_observe",
    retryObserverDiscovery:
      interactionLocked && args.access.reason === "task_running",
  }
}
```

After `useConversationDetail`, derive and wire access:

```ts
const isDelegateConversation =
  delegatedOpenIntent != null || detail?.summary.kind === "delegate"
const {
  access: delegateAccess,
  loading: delegateAccessLoading,
  refresh: refreshDelegateAccess,
} = useDelegateAccess({
  conversationId: dbConversationId,
  enabled: isDelegateConversation,
})
const delegatePolicy = resolveDelegateConnectionPolicy({
  isDelegate: isDelegateConversation,
  access: delegateAccess,
})
const interactionLocked = delegatePolicy.interactionLocked
const summaryForSessionPolicy = resolveSurfacePersistedSummary(
  persistedSummary,
  detail?.summary ?? null
)
```

Pass `summaryForSessionPolicy` to `resolveSessionAutoConnectAllowed`. Add `hasPersistedConversation && detailLoading && delegatedOpenIntent == null` to the readiness gate so an historical Cline child cannot spawn during the one render before its `kind` is known. **Retention note (plan review Minor):** this deliberately trades the historical Cline “connect immediately without waiting for detail” optimization for fail-closed delegate identity on all persisted Cline rows; implementers must not narrow the gate away without re-opening that race. Once a delegate is known, Task 3's scope-keyed hook returns `state_unknown` synchronously until its lookup resolves, so there is no one-frame owner-connect gap.

Pass these to `useConnectionLifecycle`:

```ts
connectionIntent: delegatePolicy.intent,
retryObserverDiscovery: delegatePolicy.retryObserverDiscovery,
```

A later parent upsert changes `delegateAccess` and therefore re-runs lifecycle connect. Task 5 retains a direct owner on relock; only interaction changes.

- [ ] **Step 4: Pause every prompt path and queue flush while locked**

Keep a synchronous ref for zero-delay queue timers:

```ts
const interactionLockedRef = useRef(interactionLocked)
interactionLockedRef.current = interactionLocked
```

Add `interactionLocked` to the auto-flush effect guard and its timer recheck before `mqDequeue`. Add it as the first guard in `handleSend`, before queueing, optimistic turn construction, draft clearing, or DB creation:

```ts
if (interactionLocked) return
```

Add the same first guard to `handleForkSend`, `handleModeChange`, the config wrapper, cancel wrapper, free-text answer, structured answer, and feedback resend. Do not clear or enqueue the current composer draft when the lock appears. Queue items stay visible but cannot auto-flush until access is interactive again.

- [ ] **Step 5: Recognize a raced backend rejection on every interactive path and restore draft/access**

Do not wire typed rejection handling only into `handleSend`. Centralize:

```ts
function handleDelegateViewerOnlyRejection(options?: {
  optimisticTurnId?: string
  fromQueueFlush?: boolean
  draft?: string
  selectedModeIdArg?: string | null
}): void {
  if (options?.optimisticTurnId) {
    removeOptimisticTurn(effectiveConversationId, options.optimisticTurnId)
  }
  setSyncState(effectiveConversationId, "idle")
  if (options?.fromQueueFlush && options.draft != null) {
    mqRequeueFront(options.draft, options.selectedModeIdArg ?? null)
  } else if (options?.draft != null) {
    promptDraftRestoreRevisionRef.current += 1
    setPromptDraftRestore({
      revision: promptDraftRestoreRevisionRef.current,
      draft: options.draft,
    })
  }
  void refreshDelegateAccess()
}
```

Extend lifecycle `handleSend` options with `onDelegateViewerOnly?: () => void` and also apply `isDelegateViewerOnlyRejection` in wrappers for cancel, mode, config, fork, feedback, and question answers (before generic error logging). Each path calls the shared helper (prompt restores draft; non-prompt paths still refresh access).

```ts
if (isDelegateViewerOnlyRejection(e)) {
  opts?.onDelegateViewerOnly?.()
  return
}
```

Add lifecycle and surface regressions for prompt rejection with `{ code: "delegate_viewer_only", detail: "parent_turn_active" }` (optimistic turn cleared, draft restored / queue requeued, access refresh). Add at least one non-prompt race (e.g. mode change or cancel) asserting access refresh and no generic error toast that claims a hard disconnect.

- [ ] **Step 6: Propagate `interactionLocked` through composer and command controls**

Add `interactionLocked?: boolean` (default `false`) to `ConversationShell`, `ChatInput`, and `MessageInput`. `ConversationShell` passes it to `ChatInput`, `QuestionDialog`, and `AskQuestionCard`; it deliberately does not pass it to `PermissionDialog`.

In `ChatInput`:

```ts
const sendDisabled =
  interactionLocked ||
  isWaitingForSubagents ||
  (!allowOfflineCompose && ((!isConnected && !isPrompting) || selectorsLoading))
const showCancel = !interactionLocked &&
  (isPrompting || isWaitingForSubagents)
```

In `MessageInput`, guard before `buildDraft()` so the existing `disabled && isPrompting` enqueue exception cannot bypass the access lock:

```ts
const handleSend = useCallback(() => {
  if (interactionLocked) return
  if (disabled && !isPrompting && !isEditingQueueItem) return
  const draft = buildDraft()
  // Existing send/enqueue flow.
}, [interactionLocked, disabled, isPrompting, isEditingQueueItem, buildDraft])
```

Guard `handleForkSendClick` the same way. Pass `disabled={interactionLocked}` to `InlineModeSelector`, `InlineSessionConfigSelector`, and `ModelOptionPicker`; each selector disables its trigger while continuing to render the current value. Close an open dropdown/popover when `disabled` becomes true so a stale menu item cannot fire after relock. Disable the feedback menu item when locked.

Add a `message-input.test.tsx` regression with `disabled`, `isPrompting`, `interactionLocked`, and `onEnqueue`: enter a draft and submit; assert neither `onSend` nor `onEnqueue` fires and the editor retains the text. Add selector tests proving the current label remains visible and `onSelect` cannot fire.

- [ ] **Step 7: Lock question answers and feedback while preserving read-only state**

Add `interactionLocked?: boolean` to both question components. `QuestionDialog` disables its textarea/send button and makes `handleSubmit` a no-op. `AskQuestionCard` computes:

```ts
const locked = submitting || readOnly || interactionLocked
```

Keep tabs navigable, but disable option, free-text, skip, next, and submit mutations. This is distinct from `readOnly`: a pending question remains visibly pending and can become interactive again after unlock.

Add `interactionLocked?: boolean` to `UseSessionFeedbackArgs`. Preserve snapshot/live note hydration, but close the dialog on relock, make `openDialog` and `submit` no-ops, and include `!interactionLocked` in `canSubmit`. Tests must prove notes remain visible while submission and the turn-end resend fallback are both disabled.

- [ ] **Step 8: Verify the complete capability matrix**

Run:

```powershell
pnpm test -- src/components/conversations/conversation-session-surface.test.ts src/hooks/use-connection-lifecycle.test.ts src/components/chat/conversation-shell.test.tsx src/components/chat/message-input.test.tsx src/components/chat/chat-input.test.tsx src/components/chat/question-dialog.test.tsx src/components/chat/ask-question-card.test.tsx src/hooks/use-session-feedback.test.ts src/components/chat/session-config-selector.test.tsx
pnpm eslint src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts src/components/chat/conversation-shell.tsx src/components/chat/conversation-shell.test.tsx src/components/chat/chat-input.tsx src/components/chat/message-input.tsx src/components/chat/message-input.test.tsx src/components/chat/question-dialog.tsx src/components/chat/ask-question-card.tsx src/hooks/use-session-feedback.ts src/components/chat/mode-selector.tsx src/components/chat/session-config-selector.tsx src/components/chat/model-option-picker.tsx
```

Expected: both commands exit 0. Prompt, enqueue, queue flush, cancel, mode/config, fork, feedback, and both question-answer paths are locked; drafts and visible values remain; permission approve/reject still calls the backend action.

- [ ] **Step 9: Commit the interaction boundary**

```powershell
git add src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts src/components/chat/conversation-shell.tsx src/components/chat/conversation-shell.test.tsx src/components/chat/chat-input.tsx src/components/chat/message-input.tsx src/components/chat/message-input.test.tsx src/components/chat/chat-input.test.tsx src/components/chat/question-dialog.tsx src/components/chat/question-dialog.test.tsx src/components/chat/ask-question-card.tsx src/components/chat/ask-question-card.test.tsx src/hooks/use-session-feedback.ts src/hooks/use-session-feedback.test.ts src/components/chat/mode-selector.tsx src/components/chat/session-config-selector.tsx src/components/chat/session-config-selector.test.tsx src/components/chat/model-option-picker.tsx
git commit -m "feat: lock delegated child interaction in main tabs"
```

---

### Task 7: Bounded terminal transcript convergence without content loss

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`
- Modify: `src/stores/viewer-detail-sync.test.ts`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-session-surface.test.ts`
- Modify: `src/contexts/app-workspace-context.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`

**Interfaces:**
- Consumes: `getFolderConversation`, the existing `userTurnContentKey`, Task 3's `DelegateAccessReason`, Task 5's cold reconnect, Task 6's delegate identity, and the runtime store's `FETCH_DETAIL_SUCCESS` live-buffer preservation behavior.
- Produces: `syncDelegateTerminalDetail(conversationId: number): void`, `ConversationRuntimeSession.delegateSyncError: string | null`, one cancelable poll per runtime conversation, and four explicit triggers: child `turn_complete`, a terminal child summary, access leaving `task_running`, and transport reconnect.

- [ ] **Step 1: Add failing convergence, repeated-prompt, preservation, and cancellation tests**

Extend the shared session fixture in `viewer-detail-sync.test.ts` with:

```ts
import type {
  DbConversationDetail,
  DbConversationSummary,
  MessageTurn,
} from "@/lib/types"
```

```ts
delegateSyncError: null,
```

Add a dedicated helper and suite. The repeated `"继续"` case is mandatory: the old identical prompt/reply must not satisfy the new turn's anchor.

```ts
function syncDelegate(): void {
  useConversationRuntimeStore
    .getState()
    .actions.syncDelegateTerminalDetail(CID)
}

describe("syncDelegateTerminalDetail", () => {
  it("bypasses the pure-viewer guard and preserves live content until persistence converges", async () => {
    vi.useFakeTimers()
    seed({
      detail: {
        ...detail([userTurn("parser-u", "inspect")], 10, "parser-u"),
        summary: {
          ...detail([]).summary,
          kind: "delegate",
          parent_id: 1,
          delegation_task_status: "completed",
        },
      },
      localTurns: [
        userTurn("wire-u", "inspect"),
        assistantTurn("wire-a", "complete live answer"),
      ],
      liveOwnsActiveTurn: true,
      lastTurnOwned: false,
    })
    mockGet
      .mockResolvedValueOnce({
        ...detail([userTurn("parser-u", "inspect")], 10, null),
        summary: session()!.detail!.summary,
      })
      .mockResolvedValueOnce({
        ...detail(
          [
            userTurn("parser-u", "inspect"),
            assistantTurn("parser-a", "complete live answer"),
          ],
          42,
          null
        ),
        summary: session()!.detail!.summary,
      })

    syncDelegate()
    await vi.advanceTimersByTimeAsync(0)
    expect(session()?.localTurns).toHaveLength(2)
    expect(session()?.delegateSyncError).toBeNull()

    await vi.advanceTimersByTimeAsync(300)
    expect(session()?.detail?.turns.map((turn) => turn.role)).toEqual([
      "user",
      "assistant",
    ])
    expect(session()?.localTurns).toEqual([])
    expect(session()?.optimisticTurns).toEqual([])
    expect(session()?.liveMessage).toBeNull()
  })

  it("does not match a repeated prompt before the captured baseline", async () => {
    vi.useFakeTimers()
    const delegateSummary: DbConversationSummary = {
      ...detail([]).summary,
      kind: "delegate",
      parent_id: 1,
      delegation_task_status: "completed",
    }
    seed({
      detail: {
        ...detail(
          [userTurn("old-u", "继续"), assistantTurn("old-a", "old answer")],
          20
        ),
        summary: delegateSummary,
      },
      localTurns: [
        userTurn("wire-new-u", "继续"),
        assistantTurn("wire-new-a", "new answer"),
      ],
      liveOwnsActiveTurn: true,
    })
    mockGet
      .mockResolvedValueOnce({
        ...detail(
          [userTurn("old-u", "继续"), assistantTurn("old-a", "old answer")],
          20
        ),
        summary: delegateSummary,
      })
      .mockResolvedValueOnce({
        ...detail(
          [
            userTurn("old-u", "继续"),
            assistantTurn("old-a", "old answer"),
            userTurn("parser-new-u", "继续"),
            assistantTurn("parser-new-a", "new answer"),
          ],
          50
        ),
        summary: delegateSummary,
      })

    syncDelegate()
    await vi.advanceTimersByTimeAsync(0)
    expect(session()?.localTurns).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(300)
    expect(session()?.localTurns).toEqual([])
    expect(session()?.detail?.turns.at(-1)?.id).toBe("parser-new-a")
  })

  it("waits for in_flight_user_turn_id to clear even when an assistant tail exists", async () => {
    vi.useFakeTimers()
    const delegateSummary: DbConversationSummary = {
      ...detail([]).summary,
      kind: "delegate",
      parent_id: 1,
      delegation_task_status: "completed",
    }
    const settledTurns = [
      userTurn("u-current", "work"),
      assistantTurn("a-current", "final"),
    ]
    seed({
      detail: {
        ...detail([userTurn("u-current", "work")], 10, "u-current"),
        summary: delegateSummary,
      },
      localTurns: settledTurns,
      liveOwnsActiveTurn: true,
    })
    mockGet
      .mockResolvedValueOnce({
        ...detail(settledTurns, 30, "u-current"),
        summary: delegateSummary,
      })
      .mockResolvedValueOnce({
        ...detail(settledTurns, 40, null),
        summary: delegateSummary,
      })

    syncDelegate()
    await vi.advanceTimersByTimeAsync(0)
    expect(session()?.localTurns).toEqual(settledTurns)
    await vi.advanceTimersByTimeAsync(300)
    expect(session()?.localTurns).toEqual([])
  })

  it("keeps visible content and exposes an error after the bounded window", async () => {
    vi.useFakeTimers()
    const delegateSummary: DbConversationSummary = {
      ...detail([]).summary,
      kind: "delegate",
      parent_id: 1,
      delegation_task_status: "failed",
    }
    const liveTurns = [
      userTurn("wire-u", "work"),
      assistantTurn("wire-a", "last visible output"),
    ]
    seed({
      detail: {
        ...detail([], 0, null),
        summary: delegateSummary,
      },
      localTurns: liveTurns,
      liveOwnsActiveTurn: true,
    })
    mockGet.mockResolvedValue({
      ...detail([], 0, null),
      summary: delegateSummary,
    })

    syncDelegate()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(mockGet).toHaveBeenCalledTimes(5)
    expect(session()?.localTurns).toEqual(liveTurns)
    expect(session()?.delegateSyncError).toBeTruthy()
  })

  it("does not report failure after a newer detail fetch commits convergence", async () => {
    vi.useFakeTimers()
    const delegateSummary: DbConversationSummary = {
      ...detail([]).summary,
      kind: "delegate",
      parent_id: 1,
      delegation_task_status: "completed",
    }
    const pendingDetail = {
      ...detail([userTurn("parser-u", "work")], 10, null),
      summary: delegateSummary,
    }
    const convergedDetail = {
      ...detail(
        [
          userTurn("parser-u", "work"),
          assistantTurn("parser-a", "final"),
        ],
        20,
        null
      ),
      summary: delegateSummary,
    }
    let resolveLastPoll!: (detail: DbConversationDetail) => void
    const lastPoll = new Promise<DbConversationDetail>((resolve) => {
      resolveLastPoll = resolve
    })
    seed({
      detail: {
        ...detail([userTurn("parser-u", "work")], 5, "parser-u"),
        summary: delegateSummary,
      },
      localTurns: [
        userTurn("wire-u", "work"),
        assistantTurn("wire-a", "final"),
      ],
      liveOwnsActiveTurn: true,
    })
    mockGet
      .mockResolvedValueOnce(pendingDetail)
      .mockResolvedValueOnce(pendingDetail)
      .mockResolvedValueOnce(pendingDetail)
      .mockResolvedValueOnce(pendingDetail)
      .mockImplementationOnce(() => lastPoll)
      .mockResolvedValueOnce(convergedDetail)

    syncDelegate()
    await vi.advanceTimersByTimeAsync(5_000)
    expect(mockGet).toHaveBeenCalledTimes(5)

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session()?.detail?.turns.at(-1)?.id).toBe("parser-a")

    resolveLastPoll(pendingDetail)
    await Promise.resolve()
    await Promise.resolve()
    expect(session()?.delegateSyncError).toBeNull()
  })
})
```

Add three cancellation cases alongside the existing viewer-sync cancellation tests: `actions.removeConversation(CID)`, `actions.reset()`, and exported `resetConversationRuntimeStore()` after attempt zero must prevent every later attempt and must not create a replacement runtime session when a previously-started promise resolves.

- [ ] **Step 2: Run the store tests and verify the dedicated action is absent**

Run:

```powershell
pnpm test -- src/stores/viewer-detail-sync.test.ts
```

Expected: FAIL because `delegateSyncError` and `syncDelegateTerminalDetail` do not exist and the current `syncViewerDetail` refuses a live-owned delegate session.

- [ ] **Step 3: Add the anchor projection and dedicated cancelable poll**

Add the session field and initialize it in `createEmptySession`:

```ts
/** Terminal persistence failed to replace the last visible delegate reply. */
delegateSyncError: string | null
```

```ts
delegateSyncError: null,
```

Add an internal action so failures update only this field. Clear the field on every successful `FETCH_DETAIL_SUCCESS`, including manual reload, and set it from the failure action without changing any transcript buffers:

```ts
| {
    type: "SET_DELEGATE_SYNC_ERROR"
    conversationId: number
    error: string | null
  }
```

```ts
case "SET_DELEGATE_SYNC_ERROR":
  return updateSessionInState(state, action.conversationId, (current) => ({
    ...current,
    delegateSyncError: action.error,
  }))
```

Define the anchor and convergence helpers beside `userTurnContentKey`. Prefer the visible stable id; otherwise accept the parser's explicit `in_flight_user_turn_id`, or a matching trailing user that has no following assistant. If none identifies the current prompt, `minPersistedIndex` is the persisted count at capture, so content fallback cannot match an older identical prompt/reply.

```ts
interface DelegateTerminalSyncAnchor {
  userId: string
  persistedUserId: string | null
  contentKey: string
  minPersistedIndex: number
}

function captureDelegateTerminalSyncAnchor(
  state: ConversationRuntimeState,
  conversationId: number
): DelegateTerminalSyncAnchor | null {
  const session = state.byConversationId.get(conversationId)
  if (!session) return null
  const timeline = computeTimeline(state, conversationId)
  let visibleUser: MessageTurn | null = null
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    if (timeline[index].turn.role === "user") {
      visibleUser = timeline[index].turn
      break
    }
  }
  if (!visibleUser) return null

  const persisted = session.detail?.turns ?? []
  const contentKey = userTurnContentKey(visibleUser)
  let persistedIndex = persisted.findIndex(
    (turn) => turn.role === "user" && turn.id === visibleUser.id
  )
  if (persistedIndex < 0 && session.detail?.in_flight_user_turn_id != null) {
    const inFlightIndex = persisted.findIndex(
      (turn) =>
        turn.role === "user" &&
        turn.id === session.detail?.in_flight_user_turn_id
    )
    if (
      inFlightIndex >= 0 &&
      userTurnContentKey(persisted[inFlightIndex]) === contentKey
    ) {
      persistedIndex = inFlightIndex
    }
  }
  if (persistedIndex < 0) {
    let trailingUserIndex = -1
    for (let index = persisted.length - 1; index >= 0; index -= 1) {
      if (persisted[index].role === "user") {
        trailingUserIndex = index
        break
      }
    }
    const hasAssistantAfter = persisted
      .slice(trailingUserIndex + 1)
      .some((turn) => turn.role === "assistant")
    if (
      trailingUserIndex >= 0 &&
      !hasAssistantAfter &&
      userTurnContentKey(persisted[trailingUserIndex]) === contentKey
    ) {
      persistedIndex = trailingUserIndex
    }
  }
  return {
    userId: visibleUser.id,
    persistedUserId:
      persistedIndex >= 0 ? persisted[persistedIndex].id : null,
    contentKey,
    minPersistedIndex:
      persistedIndex >= 0 ? persistedIndex : persisted.length,
  }
}

function delegateTerminalDetailConverged(
  detail: DbConversationDetail,
  anchor: DelegateTerminalSyncAnchor | null
): boolean {
  if (!anchor || detail.in_flight_user_turn_id != null) return false
  const start = Math.min(anchor.minPersistedIndex, detail.turns.length)
  let userIndex = detail.turns.findIndex(
    (turn, index) =>
      index >= start &&
      turn.role === "user" &&
      (turn.id === anchor.userId || turn.id === anchor.persistedUserId)
  )
  if (userIndex < 0) {
    userIndex = detail.turns.findIndex(
      (turn, index) =>
        index >= start &&
        turn.role === "user" &&
        userTurnContentKey(turn) === anchor.contentKey
    )
  }
  return (
    userIndex >= 0 &&
    detail.turns
      .slice(userIndex + 1)
      .some((turn) => turn.role === "assistant")
  )
}
```

Use the same `[0, 300, 700, 1500, 2500]` delays as viewer sync. Add a separate `delegateTerminalSyncCancels` map so viewer and delegate policies cannot cancel each other's ownership accidentally. Export the new action in `RuntimeActions`:

```ts
syncDelegateTerminalDetail: (conversationId: number) => void
```

Implement it with these exact invariants:

```ts
const DELEGATE_TERMINAL_SYNC_FAILED =
  "Delegated transcript did not converge before the retry window ended"
const delegateTerminalSyncCancels = new Map<number, () => void>()

const syncDelegateTerminalDetail = (
  nudgedConversationId: number
): void => {
  const conversationId = resolveViewerRuntimeId(
    get().byConversationId,
    nudgedConversationId
  )
  if (conversationId == null) return
  const initial = get().byConversationId.get(conversationId)
  if (initial?.detail?.summary.kind !== "delegate") return

  cancelViewerDetailSync(conversationId)
  delegateTerminalSyncCancels.get(conversationId)?.()
  dispatch({
    type: "SET_DELEGATE_SYNC_ERROR",
    conversationId,
    error: null,
  })

  const anchor = captureDelegateTerminalSyncAnchor(get(), conversationId)
  let cancelled = false
  let timer: ReturnType<typeof setTimeout> | null = null
  const cancel = (): void => {
    cancelled = true
    if (timer) clearTimeout(timer)
    if (delegateTerminalSyncCancels.get(conversationId) === cancel) {
      delegateTerminalSyncCancels.delete(conversationId)
    }
  }
  delegateTerminalSyncCancels.set(conversationId, cancel)

  const committedDetailHasConverged = (): boolean => {
    const committed = get().byConversationId.get(conversationId)?.detail
    return (
      committed != null &&
      delegateTerminalDetailConverged(committed, anchor)
    )
  }

  const fail = (message = DELEGATE_TERMINAL_SYNC_FAILED): void => {
    if (cancelled) return
    dispatch({
      type: "SET_DELEGATE_SYNC_ERROR",
      conversationId,
      error: message,
    })
    cancel()
  }

  const attempt = (index: number): void => {
    if (cancelled) return
    const current = get().byConversationId.get(conversationId)
    if (!current || current.detail?.summary.kind !== "delegate") {
      cancel()
      return
    }
    const fetchId = current.dbConversationId ?? conversationId
    const generation = bumpFetchGeneration(conversationId)
    getFolderConversation(fetchId)
      .then((detail) => {
        if (cancelled) return
        const currentAfterRead = get().byConversationId.get(conversationId)
        if (!currentAfterRead) {
          cancel()
          return
        }
        const latest = isLatestGeneration(conversationId, generation)
        const converged = delegateTerminalDetailConverged(detail, anchor)
        if (latest) {
          dispatch({
            type: "FETCH_DETAIL_SUCCESS",
            conversationId,
            detail,
            preserveLive: !converged,
          })
        }
        if ((latest && converged) || committedDetailHasConverged()) {
          cancel()
          return
        }
        if (index + 1 >= VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
          fail()
          return
        }
        timer = setTimeout(
          () => attempt(index + 1),
          VIEWER_DETAIL_SYNC_DELAYS_MS[index + 1]
        )
      })
      .catch((error: unknown) => {
        if (cancelled) return
        if (committedDetailHasConverged()) {
          cancel()
          return
        }
        if (index + 1 >= VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
          fail(toErrorMessage(error))
          return
        }
        timer = setTimeout(
          () => attempt(index + 1),
          VIEWER_DETAIL_SYNC_DELAYS_MS[index + 1]
        )
      })
  }

  attempt(0)
}
```

Register the action in the stable action bundle. Keep the reducer's `REMOVE_CONVERSATION` case pure; cancellation belongs in the public action that owns the module-level poll maps. Add this shared reset helper after both maps have been declared:

```ts
function cancelAllDetailSyncs(): void {
  for (const cancel of viewerDetailSyncCancels.values()) cancel()
  viewerDetailSyncCancels.clear()
  for (const cancel of delegateTerminalSyncCancels.values()) cancel()
  delegateTerminalSyncCancels.clear()
}
```

Update the two stable actions without changing their existing dispatch or live-transcript cleanup:

```ts
removeConversation: (conversationId) => {
  bumpFetchGeneration(conversationId)
  cancelViewerDetailSync(conversationId)
  delegateTerminalSyncCancels.get(conversationId)?.()
  dispatch({ type: "REMOVE_CONVERSATION", conversationId })
  liveTranscriptStore.remove(conversationId)
},
reset: () => {
  cancelAllDetailSyncs()
  dispatch({ type: "RESET" })
  liveTranscriptStore.reset()
},
```

In exported `resetConversationRuntimeStore`, replace its viewer-only cancel loop with `cancelAllDetailSyncs()` before clearing Zustand state. Thus both reset entry points cancel and clear both maps. The generation check remains a commit gate: a concurrent panel fetch may supersede a read, but an uncommitted read may not retire live buffers. Before the final poll reports failure, `committedDetailHasConverged` checks the currently committed session detail so a successful superseding fetch cannot be followed by a false sync error.

- [ ] **Step 4: Add failing trigger tests for completion, durable terminal state, access transition, and reconnect**

In `conversation-session-surface.test.ts`, extend the existing lifecycle/access harness with a spy for `syncDelegateTerminalDetail`. Add these cases:

```ts
it("starts delegate convergence on the child prompting-to-connected edge", () => {
  h.renderDelegate({ accessReason: "task_running", connStatus: "prompting" })
  h.rerenderDelegate({ accessReason: "task_running", connStatus: "connected" })
  expect(h.syncDelegateTerminalDetail).toHaveBeenCalledWith(h.runtimeId)
})

it("starts convergence when access leaves task_running", () => {
  h.renderDelegate({ accessReason: "task_running", connStatus: "connected" })
  h.rerenderDelegate({
    accessReason: "parent_turn_active",
    connStatus: "connected",
  })
  expect(h.syncDelegateTerminalDetail).toHaveBeenCalledTimes(1)
  h.rerenderDelegate({ accessReason: null, connStatus: "connected" })
  expect(h.syncDelegateTerminalDetail).toHaveBeenCalledTimes(1)
})
```

In `app-workspace-context.test.tsx`, spy on the stable runtime action and prove that a terminal delegate upsert triggers it even though the root workspace store intentionally rejects the child row. Also prove that reconnect nudges every open terminal delegate, but neither an open root nor a still-running delegate:

```tsx
it("nudges terminal delegate detail even though child upserts stay out of the root list", async () => {
  const syncDelegate = vi
    .spyOn(
      useConversationRuntimeStore.getState().actions,
      "syncDelegateTerminalDetail"
    )
    .mockImplementation(() => {})
  await mountProvider()
  emit({
    kind: "upsert",
    summary: makeSummary({
      id: 42,
      kind: "delegate",
      parent_id: 1,
      delegation_task_status: "completed",
    }),
  })
  expect(syncDelegate).toHaveBeenCalledWith(42)
  expect(screen.getByTestId("ids").textContent).toBe("")
  syncDelegate.mockRestore()
})
```

Seed partial runtime sessions (casts are acceptable in this event-routing test) for a completed child, failed child, running child, and root; invoke `h.reconnect`; assert only the completed and failed ids are passed to `syncDelegateTerminalDetail`. Separately assert the running open child still receives a preserved-live detail refresh (not terminal sync) before observer cold-attach.

- [ ] **Step 5: Wire all four triggers without creating a second polling owner**

Destructure `syncDelegateTerminalDetail` from `useConversationRuntimeActions`. In the existing prompting-to-idle effect, start convergence immediately after the one-call-stack live promotion, but only for a known delegate:

```ts
completeLiveTranscriptTurn(effectiveConversationId)
if (isDelegateConversation) {
  syncDelegateTerminalDetail(effectiveConversationId)
}
```

**Four terminal-sync triggers** (must match Amendment 5 and the Interfaces block):

1. Surface prompting→idle / `TurnComplete` for a known delegate (above).
2. Workspace upsert with a verified terminal `delegation_task_status` (below).
3. Access reason leaves `task_running` for any reason **other than** `state_unknown` (recover missed TurnComplete when resolver already shows terminal task). Implement:

```ts
const previousDelegateReasonRef = useRef(delegateAccess.reason)
useEffect(() => {
  const previous = previousDelegateReasonRef.current
  previousDelegateReasonRef.current = delegateAccess.reason
  if (
    isDelegateConversation &&
    previous === "task_running" &&
    delegateAccess.reason !== "task_running" &&
    delegateAccess.reason !== "state_unknown"
  ) {
    syncDelegateTerminalDetail(effectiveConversationId)
  }
}, [
  delegateAccess.reason,
  effectiveConversationId,
  isDelegateConversation,
  syncDelegateTerminalDetail,
])
```

4. Transport reconnect for sessions whose **already-loaded detail** shows a terminal task status (below) — plus separate nonterminal detail refresh (next paragraph).

**Never** start terminal polling on `task_running → state_unknown` (access-lookup outage). The Step 4 test `starts convergence when access leaves task_running` (`task_running → parent_turn_active`) remains required; add the no-poll regression for `state_unknown`.

In `app-workspace-context.tsx`, route terminal delegate summaries through the dedicated action. The child task field, not generic conversation status, owns this decision:

```ts
const TERMINAL_DELEGATE_TASK_STATES = new Set([
  "completed",
  "failed",
  "canceled",
])

function syncTerminalDelegateSummary(summary: DbConversationSummary): void {
  if (
    summary.kind === "delegate" &&
    summary.delegation_task_status != null &&
    TERMINAL_DELEGATE_TASK_STATES.has(summary.delegation_task_status)
  ) {
    useConversationRuntimeStore
      .getState()
      .actions.syncDelegateTerminalDetail(summary.id)
  }
}
```

Call it on `change.kind === "upsert"` after applying the child projection. On transport reconnect:

1. Task 3 refreshes access for open delegates.
2. For **every** open delegate session (running or terminal), re-fetch persisted detail with live-buffer preservation (`preserveLive: true` / existing `FETCH_DETAIL_SUCCESS` flag — confirm the field exists; extend the action/reducer in this task if absent). This closes missed-event windows for still-running children; do not limit reconnect detail refresh to terminal children only.
3. Additionally call `syncDelegateTerminalDetail` only for sessions whose loaded detail summary passes the terminal predicate above.
4. Task 5 independently cold-attaches the ACP observer.

Do not call `syncViewerDetail` for the same terminal child event; roots keep the existing viewer-sync path. Add regressions: (a) `task_running → state_unknown` does not invoke `syncDelegateTerminalDetail`; (b) reconnect on a running open child refreshes detail and preserves live buffers before cold attach.

- [ ] **Step 6: Run focused convergence and routing checks**

Run:

```powershell
pnpm test -- src/stores/viewer-detail-sync.test.ts src/components/conversations/conversation-session-surface.test.ts src/contexts/app-workspace-context.test.tsx
pnpm eslint src/stores/conversation-runtime-store.ts src/stores/viewer-detail-sync.test.ts src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx
```

Expected: both commands exit 0. Polling stops only after the captured current user turn and a following assistant are persisted with no in-flight marker; partial/failing reads retain live/local/optimistic content; repeated prompts cannot match older history; remove/reset cancel every timer; and all four triggers restart rather than stack the per-conversation poll.

- [ ] **Step 7: Commit terminal convergence**

```powershell
git add src/stores/conversation-runtime-store.ts src/stores/viewer-detail-sync.test.ts src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx
git commit -m "fix: converge delegated child terminal transcripts"
```

---

### Task 8: Semantic-activity watchdog, viewer independence, and orphan recovery regressions

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/delegation/event_emitter.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/tests/tool_watchdog_lifecycle.rs`
- Modify: `src-tauri/tests/ws_attach.rs`

**Interfaces:**
- Consumes: `advances_agent_activity`, `SessionState::mark_agent_activity`, `tool_watchdog_on_verified_child_activity`, `ToolExecutionLeaseRegistry`, `forward_disconnect_to_broker`, and `RunStore::reconcile_non_terminal`.
- Produces: regression-locked activity semantics only: text, thinking, plan, tool start, and tool progress update the owning session; parent text/thinking renew only the active turn's untracked fallback; exact child activity renews only its proven parent lease; viewers never renew either clock; disconnect/startup recovery cannot leave a logical or durable running orphan.

- [ ] **Step 1: Add failing inbound semantic-activity and noise tests**

Keep `advances_agent_activity` as the single classifier, but extract the repeated state write into a small helper that can be tested without an emitter or frontend subscriber:

```rust
async fn mark_agent_activity_for_update(
    state: &Arc<RwLock<SessionState>>,
    update: &SessionUpdate,
    at: chrono::DateTime<chrono::Utc>,
) -> bool {
    if !advances_agent_activity(update) {
        return false;
    }
    state.write().await.mark_agent_activity(at);
    true
}
```

Add a controlled-clock test beside `agent_activity_classifier_excludes_ui_and_status_noise`:

```rust
use crate::acp::delegation::supervisor::derive_observation;
use crate::acp::delegation::types::TaskObservation;

#[tokio::test]
async fn semantic_updates_advance_session_activity_without_frontend_delivery() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "activity-test".into(),
        AgentType::ClaudeCode,
        None,
        "test-window".into(),
        None,
    )));
    let base = chrono::DateTime::parse_from_rfc3339("2026-07-25T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    state.write().await.last_agent_activity_at = base;

    let semantic = [
        agent_text_update("token"),
        agent_thought_update("reasoning"),
        plan_update(),
        tool_start_update("tool-1"),
        tool_progress_update("tool-1"),
    ];
    for (index, update) in semantic.iter().enumerate() {
        let at = base + chrono::Duration::seconds(index as i64 + 1);
        assert!(mark_agent_activity_for_update(&state, update, at).await);
        assert_eq!(state.read().await.last_agent_activity_at, at);
    }

    let last = state.read().await.last_agent_activity_at;
    assert_eq!(
        derive_observation(
            last + chrono::Duration::seconds(299),
            last,
            false,
            300,
        )
        .observation,
        TaskObservation::Active,
    );
    let noise = [
        available_commands_update(),
        usage_update(),
        user_message_update("keepalive"),
    ];
    for update in &noise {
        assert!(!mark_agent_activity_for_update(
            &state,
            update,
            last + chrono::Duration::hours(1),
        )
        .await);
        assert_eq!(state.read().await.last_agent_activity_at, last);
    }
    assert_eq!(
        derive_observation(
            last + chrono::Duration::seconds(300),
            state.read().await.last_agent_activity_at,
            false,
            300,
        )
        .observation,
        TaskObservation::Stalled,
    );
}
```

This test deliberately has no `EventEmitter`, WebSocket, attach subscription, or viewer. It proves that health is updated before and independently from delivery, that a semantic reply inside the 300-second window remains `Active`, and that excluded noise still cannot defer the exact timeout. `BrokerObservationSource` already reads this exact child `SessionState.last_agent_activity_at`, so the same assertion covers a delegated subagent rather than a frontend proxy clock.

- [ ] **Step 2: Route every normalized inbound update through the tested helper**

Replace each duplicated classifier/write pair in the idle loop, prompt loop, and pre-finalize drain:

```rust
mark_agent_activity_for_update(&st, &notif.update, chrono::Utc::now()).await;
```

Keep private extension content on its existing direct `mark_agent_activity` path because it has already been normalized into semantic content but is not a `SessionUpdate`. Do not move the mark after `emit_conversation_update`: filters, a missing viewer, or a failing frontend emitter must not suppress the health update.

Run the focused unit test:

```powershell
Set-Location src-tauri
cargo test --features test-utils semantic_updates_advance_session_activity_without_frontend_delivery -- --nocapture
```

Expected: PASS. The helper returns true for exactly five semantic categories and false for user echo, usage, and command metadata.

- [ ] **Step 3: Add exact parent-fallback and child-lease lifecycle tests**

Extend `tool_watchdog_lifecycle.rs` with two controlled-clock cases. The first proves parent output never keeps a tracked tool alive:

```rust
#[tokio::test]
async fn parent_agent_output_renews_only_the_active_turn_fallback() {
    let reg = Arc::new(ToolExecutionLeaseRegistry::new(
        ToolWatchdogSettings::default(),
    ));
    let attr = codeg_lib::acp::tool_watchdog::LeaseAttribution::new(reg.clone());
    let turn = sample_turn(1);
    let t0 = clock_base();
    attr.start_turn(turn.clone(), t0).await;

    attr.record_agent_activity(&turn, "reply chunk", t0.advanced(590))
        .await;
    assert!(reg.scan(t0.advanced(600)).await.is_empty());

    let tracked = attr
        .register_or_touch_tool(&turn, "tracked", ToolCategory::Other, t0)
        .await
        .expect("tracked lease");
    attr.record_agent_activity(&turn, "more reply", t0.advanced(590))
        .await;
    let warned = reg.scan(t0.advanced(600)).await;
    assert!(warned.iter().any(|action| matches!(
        action,
        RegistryAction::PublishWarning { stamp, .. }
            if stamp.lease_id == tracked.lease_id
    )));
}
```

The second drives both matching and sibling delegation leases through warning/grace. Only exact child activity recovers to running and returns `Cleared`:

```rust
#[tokio::test]
async fn exact_child_activity_clears_grace_without_renewing_a_sibling() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let parent = register_tool(
        &reg,
        &turn,
        "parent-wait",
        ToolCategory::Delegation,
        t0,
    )
    .await;
    let sibling = register_tool(
        &reg,
        &turn,
        "sibling-wait",
        ToolCategory::Delegation,
        t0,
    )
    .await;

    let warnings = reg.scan(t0.advanced(600)).await;
    for action in warnings {
        let RegistryAction::PublishWarning { stamp, .. } = action else {
            panic!("expected warning");
        };
        reg.warning_published(&stamp.lease_id, stamp.version, t0.advanced(600))
            .await
            .expect("enter grace");
    }

    let renewed = reg
        .record_tool_progress_at(
            progress_key(&turn, "parent-wait"),
            SemanticProgress::DelegationActivity { at_mono_ms: 601_000 },
            t0.advanced(601),
        )
        .await
        .expect("exact parent lease renewed");
    let cleared = renewed.cleared.expect("grace progress emits cleared");
    assert_eq!(cleared.phase, ToolWatchdogPhase::Cleared);
    assert_eq!(cleared.lease_id, parent.lease_id);

    let due = reg.scan(t0.advanced(1200)).await;
    assert!(due.iter().any(|action| matches!(
        action,
        RegistryAction::ClaimCancel { claim, .. }
            if claim.stamp.lease_id == sibling.lease_id
    )));
    assert!(!due.iter().any(|action| matches!(
        action,
        RegistryAction::ClaimCancel { claim, .. }
            if claim.stamp.lease_id == parent.lease_id
    )));
}
```

- [ ] **Step 4: Test the durable `parent_tool_use_id -> task_id` binding at the production emitter**

In `event_emitter.rs` tests, add concrete state/watchdog imports (the production module already imports `DelegationRuntimeStats`, but the test must not depend on that private import being glob-reexported), then exercise the private production helper with one exact and one contradictory task id:

```rust
use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;
use crate::acp::session_state::{ActiveDelegationState, SessionState};
use crate::acp::tool_watchdog::{ToolCategory, WatchdogInstant};
use tokio::sync::RwLock;

#[tokio::test]
async fn verified_child_activity_requires_the_exact_durable_task_binding() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "parent-conn".into(),
        AgentType::ClaudeCode,
        None,
        "test-window".into(),
        None,
    )));
    let (attribution, turn) = {
        let mut state = state.write().await;
        state.external_id = Some("parent-session".into());
        state.active_turn_generation = Some(1);
        let turn = state
            .tool_watchdog_turn_stamp()
            .expect("active turn stamp");
        (state.lease_attribution(), turn)
    };
    let started_at = WatchdogInstant::now();
    attribution.start_turn(turn.clone(), started_at).await;
    let parent = attribution
        .register_or_touch_tool(
            &turn,
            "parent-tool",
            ToolCategory::Delegation,
            started_at,
        )
        .await
        .expect("parent lease")
        .stamp;
    let sibling = attribution
        .register_or_touch_tool(
            &turn,
            "sibling-tool",
            ToolCategory::Delegation,
            started_at,
        )
        .await
        .expect("sibling lease")
        .stamp;
    let now = Utc::now();
    state.write().await.active_delegations.insert(
        "parent-tool".into(),
        ActiveDelegationState {
            parent_tool_use_id: "parent-tool".into(),
            child_connection_id: "child-conn".into(),
            child_conversation_id: 42,
            agent_type: AgentType::Codex,
            task_preview: "work".into(),
            task_id: "task-exact".into(),
            started_at: now,
            runtime_stats: DelegationRuntimeStats::empty(now),
            attention_request: None,
            observation: None,
            last_agent_activity_at: None,
            stalled_since: None,
        },
    );
    let registry = attribution.registry().clone();

    tool_watchdog_on_verified_child_activity(
        &state,
        &EventEmitter::Noop,
        "parent-tool",
        "task-other",
        now,
    )
    .await;
    assert_eq!(
        registry.lease_stamp(&parent.lease_id).await.unwrap().version,
        parent.version,
    );

    tool_watchdog_on_verified_child_activity(
        &state,
        &EventEmitter::Noop,
        "parent-tool",
        "task-exact",
        now + chrono::Duration::seconds(1),
    )
    .await;
    assert!(
        registry.lease_stamp(&parent.lease_id).await.unwrap().version
            > parent.version
    );
    assert_eq!(
        registry.lease_stamp(&sibling.lease_id).await.unwrap().version,
        sibling.version,
    );
}
```

This test protects the exact durable task binding that registry-only tests cannot see and derives the turn identity through `SessionState::tool_watchdog_turn_stamp()`.

- [ ] **Step 5: Prove cold snapshots retain warnings and viewer count does not touch health**

Extend `ws_attach.rs`. Concrete setup (mirror existing harness helpers in that file for inserting a parent connection into `ConnectionManager` / AppState before sockets open):

```rust
// 1) Create parent connection the same way other ws_attach tests do
//    (insert_test_connection / harness connect id). Capture its state arc:
let state_arc = state
    .connection_manager
    .get_state("parent-live")
    .await
    .expect("parent connection");

// 2) Populate authoritative parent SessionState projections under write lock.
//    Prefer production mutators when available (emit DelegationStarted /
//    DelegationObservationChanged / ToolWatchdogChanged through the real
//    event path). If the harness only supports direct state writes:
{
    let mut s = state_arc.write().await;
    s.last_agent_activity_at = chrono::Utc::now();
    // insert active_delegations entry task_id="task-live", observation Active
    // insert tool_watchdog_projections["lease-live"] grace phase version 2
    // with tool_title: ToolCategory::Delegation and no provider tool id
}
let activity_before_viewers = state_arc.read().await.last_agent_activity_at;
let projections_before_viewers = state_arc
    .read()
    .await
    .to_snapshot()
    .tool_watchdog_projections
    .clone();

// 3) Cold-attach two independent WebSocket subscriptions to the parent.
```

Assert both snapshots contain the same parent card and actionable watchdog projection:

```rust
assert_eq!(first["active_delegations"][0]["task_id"], "task-live");
assert_eq!(
    first["active_delegations"][0]["observation"],
    "active"
);
assert_eq!(
    first["tool_watchdog_projections"]["lease-live"]["phase"],
    "grace"
);
assert_eq!(
    second["tool_watchdog_projections"]["lease-live"]["version"],
    2
);
```

After both attaches and after dropping both sockets, assert clocks/projections unchanged:

```rust
assert_eq!(
    state_arc.read().await.last_agent_activity_at,
    activity_before_viewers,
);
assert_eq!(
    state_arc.read().await.to_snapshot().tool_watchdog_projections,
    projections_before_viewers,
);
```

Viewer discovery/attach/detach must not call registry progress APIs, so zero, one, and two viewers produce identical health state.

- [ ] **Step 6: Lock runtime disconnect and startup orphan settlement into tests**

In `lifecycle.rs`, extend the test-module imports explicitly:

```rust
use crate::acp::delegation::store::{
    mock::MockTaskStore,
    DelegationTaskStore,
};
use crate::acp::delegation::types::TaskStatus;
```

Add a store-backed variant of `stage_pending_delegation` using `MockTaskStore::accept_any_running(child_conv_id)` and `.with_task_store(store.clone() as Arc<dyn DelegationTaskStore>)`; return the `Arc<MockTaskStore>` beside the broker and driver. Send a bare terminal `Disconnected` through `lifecycle_subscriber_task`; after the driver resolves, inspect the first settled task id:

```rust
assert_eq!(store.settle_call_count().await, 1);
let calls = store.settle_calls().await;
let persisted = store.persisted(&calls[0].0).await;
assert_eq!(persisted.status, TaskStatus::Canceled);
assert_eq!(persisted.error_code.as_deref(), Some("canceled"));
```

The existing non-terminal `Error` test must continue to assert zero settlements until a true terminal event.

In `run_store.rs`, extend `reconcile_status_and_audit_split_reserving_vs_running` beyond the run rows. Query both child conversation rows after `reconcile_non_terminal`:

```rust
let reserving_child = conversation::Entity::find_by_id(child_a)
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
assert_eq!(
    reserving_child.delegation_task_status,
    Some(DelegationTaskStatus::Failed)
);
assert_eq!(reserving_child.status, ConversationStatus::Cancelled);

let running_child = conversation::Entity::find_by_id(child_b)
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
assert_eq!(
    running_child.delegation_task_status,
    Some(DelegationTaskStatus::Canceled)
);
assert_eq!(running_child.status, ConversationStatus::Cancelled);
```

This is the restart backstop: after successful reconciliation there are zero non-terminal runs and no child conversation still projects `running`.

- [ ] **Step 7: Run all focused backend regressions and clippy**

Run:

```powershell
Set-Location src-tauri
cargo test --features test-utils semantic_updates_advance_session_activity_without_frontend_delivery -- --nocapture
cargo test --features test-utils active_snapshot_re_emits_when_last_agent_activity_at_changes -- --nocapture
cargo test --features test-utils exact_child_activity -- --nocapture
cargo test --features test-utils verified_child_activity -- --nocapture
cargo test --features test-utils dispatcher_disconnected -- --nocapture
cargo test --features test-utils reconcile_status_and_audit_split -- --nocapture
cargo test --features test-utils --test tool_watchdog_lifecycle -- --nocapture
cargo test --features test-utils --test ws_attach -- --nocapture
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: every command exits 0. Recent semantic output keeps the soft observation active with no viewer; an updated activity timestamp re-emits even while the observation remains `Active`; parent output touches only the untracked fallback; exact child output renews only the bound parent lease and clears warning/grace; snapshots preserve current parent warning state; attach count does not affect clocks; disconnect/restart produce durable terminal outcomes.

- [ ] **Step 8: Commit watchdog and recovery regressions**

```powershell
git add src-tauri/src/acp/connection.rs src-tauri/src/acp/delegation/event_emitter.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/tests/tool_watchdog_lifecycle.rs src-tauri/tests/ws_attach.rs
git commit -m "test: lock delegated activity and recovery semantics"
```

---

### Task 9: Delegate access status, complete locale copy, and final verification

**Files:**
- Create: `src/components/chat/delegate-access-status.tsx`
- Create: `src/components/chat/delegate-access-status.test.tsx`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-session-surface.test.ts`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`
- Test: `src/i18n/messages.test.ts`

**Interfaces:**
- Consumes: Task 3's `DelegateAccessState`, Task 4's alias-resolved canonical `connectionId`, Task 6's `interactionLocked`, Task 7's `delegateSyncError`, and next-intl's `Folder.chat` namespace.
- Produces: `DelegateAccessStatus`, `resolveDelegateAccessStatus`, the six visible states `waiting | observing | parent_turn_active | state_unknown | interactive | sync_failed`, and complete localized copy including `backendErrors.delegateViewerOnly`.

- [ ] **Step 1: Add failing status precedence and accessibility tests**

Create `delegate-access-status.test.tsx` with the real English messages provider. Test the pure resolver as a table so contradictory inputs cannot accidentally expose interactive copy:

```tsx
import type { ComponentProps } from "react"
import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import {
  DelegateAccessStatus,
  resolveDelegateAccessStatus,
} from "./delegate-access-status"

const taskRunning = {
  mode: "viewer_only" as const,
  reason: "task_running" as const,
  parent_id: 1,
}

describe("resolveDelegateAccessStatus", () => {
  it.each([
    [
      { access: taskRunning, loading: false, connectionId: null, syncError: null },
      "waiting",
    ],
    [
      {
        access: taskRunning,
        loading: false,
        connectionId: "broker-child",
        syncError: null,
      },
      "observing",
    ],
    [
      {
        access: {
          mode: "viewer_only" as const,
          reason: "parent_turn_active" as const,
          parent_id: 1,
        },
        loading: false,
        connectionId: "owner-child",
        syncError: null,
      },
      "parent_turn_active",
    ],
    [
      {
        access: {
          mode: "viewer_only" as const,
          reason: "state_unknown" as const,
          parent_id: 1,
        },
        loading: false,
        connectionId: null,
        syncError: null,
      },
      "state_unknown",
    ],
    [
      {
        access: { mode: "interactive" as const, reason: null, parent_id: 1 },
        loading: false,
        connectionId: null,
        syncError: null,
      },
      "interactive",
    ],
    [
      {
        access: { mode: "interactive" as const, reason: null, parent_id: 1 },
        loading: false,
        connectionId: null,
        syncError: "flush failed",
      },
      "sync_failed",
    ],
  ])("resolves %j to %s", (args, expected) => {
    expect(resolveDelegateAccessStatus(args)).toBe(expected)
  })
})

function renderStatus(
  props: ComponentProps<typeof DelegateAccessStatus>
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegateAccessStatus {...props} />
    </NextIntlClientProvider>
  )
}

it("announces waiting and observing without calling the child disconnected", () => {
  const view = renderStatus({
    access: taskRunning,
    loading: false,
    connectionId: null,
    syncError: null,
  })
  expect(screen.getByRole("status")).toHaveTextContent(
    "Waiting for the delegated agent"
  )
  expect(screen.queryByText(/disconnected/i)).not.toBeInTheDocument()

  view.rerender(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegateAccessStatus
        access={taskRunning}
        loading={false}
        connectionId="broker-child"
        syncError={null}
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByRole("status")).toHaveTextContent(
    "Observing delegated task"
  )
})

it("gives sync failure alert precedence and retains the diagnostic as a title", () => {
  renderStatus({
    access: taskRunning,
    loading: false,
    connectionId: "broker-child",
    syncError: "transcript flush timed out",
  })
  const alert = screen.getByRole("alert")
  expect(alert).toHaveTextContent("Could not synchronize the final response")
  expect(alert).toHaveAttribute("title", "transcript flush timed out")
})
```

- [ ] **Step 2: Run the status test and verify the component is absent**

Run:

```powershell
pnpm test -- src/components/chat/delegate-access-status.test.tsx
```

Expected: FAIL because the component and status resolver do not exist.

- [ ] **Step 3: Implement one unframed, fixed-height status row**

Create the component with familiar lucide icons and no card wrapper:

```tsx
"use client"

import {
  CircleCheck,
  Eye,
  LoaderCircle,
  LockKeyhole,
  ShieldAlert,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react"
import { useTranslations } from "next-intl"
import type { DelegateAccessState } from "@/lib/types"
import { cn } from "@/lib/utils"

export type DelegateAccessStatusKind =
  | "waiting"
  | "observing"
  | "parent_turn_active"
  | "state_unknown"
  | "interactive"
  | "sync_failed"

export interface DelegateAccessStatusProps {
  access: DelegateAccessState
  loading: boolean
  connectionId: string | null
  syncError: string | null
}

export function resolveDelegateAccessStatus({
  access,
  loading,
  connectionId,
  syncError,
}: DelegateAccessStatusProps): DelegateAccessStatusKind {
  if (syncError) return "sync_failed"
  if (loading || access.reason === "state_unknown") return "state_unknown"
  if (access.mode === "interactive" && access.reason === null) {
    return "interactive"
  }
  if (access.mode !== "viewer_only") return "state_unknown"
  if (access.reason === "task_running") {
    return connectionId ? "observing" : "waiting"
  }
  if (access.reason === "parent_turn_active") return "parent_turn_active"
  return "state_unknown"
}

type DelegateAccessMessageKey =
  | "waiting"
  | "observing"
  | "parentTurnActive"
  | "stateUnknown"
  | "interactive"
  | "syncFailed"

const PRESENTATION: Record<
  DelegateAccessStatusKind,
  {
    icon: LucideIcon
    messageKey: DelegateAccessMessageKey
    tone: string
    spin?: boolean
  }
> = {
  waiting: {
    icon: LoaderCircle,
    messageKey: "waiting",
    tone: "text-muted-foreground",
    spin: true,
  },
  observing: {
    icon: Eye,
    messageKey: "observing",
    tone: "text-muted-foreground",
  },
  parent_turn_active: {
    icon: LockKeyhole,
    messageKey: "parentTurnActive",
    tone: "text-amber-700 dark:text-amber-300",
  },
  state_unknown: {
    icon: ShieldAlert,
    messageKey: "stateUnknown",
    tone: "text-amber-700 dark:text-amber-300",
  },
  interactive: {
    icon: CircleCheck,
    messageKey: "interactive",
    tone: "text-emerald-700 dark:text-emerald-300",
  },
  sync_failed: {
    icon: TriangleAlert,
    messageKey: "syncFailed",
    tone: "text-destructive",
  },
}

export function DelegateAccessStatus(props: DelegateAccessStatusProps) {
  const t = useTranslations("Folder.chat.delegateAccess")
  const kind = resolveDelegateAccessStatus(props)
  const { icon: Icon, messageKey, tone, spin } = PRESENTATION[kind]

  return (
    <div
      role={kind === "sync_failed" ? "alert" : "status"}
      aria-live="polite"
      data-state={kind}
      title={props.syncError ?? undefined}
      className={cn(
        "flex h-8 w-full items-center gap-2 border-b bg-muted/30 px-4 text-xs",
        tone
      )}
    >
      <Icon
        aria-hidden="true"
        className={cn("h-3.5 w-3.5 shrink-0", spin && "animate-spin")}
      />
      <span className="min-w-0 truncate">{t(messageKey)}</span>
    </div>
  )
}
```

Keep the message key inside the static presentation table so next-intl sees a finite key union. The row remains exactly `h-8`, so transitions between loader/check/lock icons cannot resize the conversation surface. It is an informational band, not a nested card or a replacement connection control.

- [ ] **Step 4: Wire the status to canonical connection and runtime sync state**

Add `delegateSyncError` to the existing shallow runtime selector in `conversation-session-surface.tsx`:

```ts
return {
  externalId: session?.externalId ?? null,
  syncState: session?.syncState ?? "idle",
  delegateSyncError: session?.delegateSyncError ?? null,
}
```

Render the row in the existing `topBanner` fragment after the watchdog/background indicators, only for a known delegate:

```tsx
{isDelegateConversation ? (
  <DelegateAccessStatus
    access={delegateAccess}
    loading={delegateAccessLoading}
    connectionId={conn.connectionId ?? null}
    syncError={delegateSyncError}
  />
) : null}
```

When a locked delegate has no canonical connection yet, suppress a stale generic ACP owner error from the shell and expose the `waiting` row instead:

```ts
const shellConnectionError =
  isDelegateConversation && interactionLocked && conn.connectionId == null
    ? null
    : conn.error
```

Pass `error={shellConnectionError}`. Keep the composer disabled through Task 6's `interactionLocked`; do not synthesize `connected`, show reconnect, or call any connection mutation from the status component.

Add a surface regression for each transition:

```ts
it("shows waiting, observing, parent lock, then interactive without changing tab identity", () => {
  const identityBefore = {
    tabId: h.tabId,
    conversationId: h.conversationId,
    externalId: h.runtimeSession().externalId,
    kind: h.detailSummary().kind,
    parentId: h.detailSummary().parent_id,
  }
  const connectCallsBeforeLock = h.acpConnect.mock.calls.length
  h.renderDelegate({ reason: "task_running", connectionId: null })
  expect(h.delegateStatus()).toBe("waiting")
  h.rerenderDelegate({ reason: "task_running", connectionId: "broker-child" })
  expect(h.delegateStatus()).toBe("observing")
  h.rerenderDelegate({ reason: "parent_turn_active", connectionId: "broker-child" })
  expect(h.delegateStatus()).toBe("parent_turn_active")
  expect(h.acpConnect).toHaveBeenCalledTimes(connectCallsBeforeLock)

  h.rerenderDelegate({ reason: null, mode: "interactive", connectionId: null })
  expect(h.delegateStatus()).toBe("interactive")
  expect({
    tabId: h.tabId,
    conversationId: h.conversationId,
    externalId: h.runtimeSession().externalId,
    kind: h.detailSummary().kind,
    parentId: h.detailSummary().parent_id,
  }).toEqual(identityBefore)
})
```

Back `runtimeSession()` and `detailSummary()` with the existing store/detail selectors; do not mirror identity into test-only state. Also assert a non-delegate root renders no delegate status row.

- [ ] **Step 5: Add the exact keys and translations to all ten locales**

Add `delegateAccess` as a sibling of `acpConnections` under `Folder.chat`. Add `delegateViewerOnly` inside each locale's existing `Folder.chat.acpConnections.backendErrors` (so `i18n_key` remains `backendErrors.delegateViewerOnly`). The JSON sketch below is **flat for readability only** — do not place `delegateViewerOnly` as a sibling of `delegateAccess` under `Folder.chat`. Use these exact values:

```json
{
  "en": {
    "delegateAccess": {
      "waiting": "Waiting for the delegated agent...",
      "observing": "Observing delegated task (read-only)",
      "parentTurnActive": "Read-only while the parent conversation is running",
      "stateUnknown": "Access state unavailable; read-only",
      "interactive": "Ready for a new turn",
      "syncFailed": "Could not synchronize the final response. Showing the latest available content."
    },
    "delegateViewerOnly": "This delegated conversation is read-only right now."
  },
  "zh-CN": {
    "delegateAccess": {
      "waiting": "正在等待子智能体连接...",
      "observing": "正在以只读方式查看子任务",
      "parentTurnActive": "父会话正在运行，当前为只读",
      "stateUnknown": "无法确认访问状态，当前为只读",
      "interactive": "可以开始新一轮对话",
      "syncFailed": "无法同步最终回复，已保留当前可见内容。"
    },
    "delegateViewerOnly": "当前子会话为只读。"
  },
  "zh-TW": {
    "delegateAccess": {
      "waiting": "正在等待子代理連線...",
      "observing": "正在以唯讀方式檢視子任務",
      "parentTurnActive": "父會話正在執行，目前為唯讀",
      "stateUnknown": "無法確認存取狀態，目前為唯讀",
      "interactive": "可以開始新一輪對話",
      "syncFailed": "無法同步最終回覆，已保留目前可見內容。"
    },
    "delegateViewerOnly": "目前子會話為唯讀。"
  },
  "ja": {
    "delegateAccess": {
      "waiting": "サブエージェントの接続を待っています...",
      "observing": "委任タスクを読み取り専用で表示中",
      "parentTurnActive": "親会話の実行中は読み取り専用です",
      "stateUnknown": "アクセス状態を確認できないため、読み取り専用です",
      "interactive": "新しいターンを開始できます",
      "syncFailed": "最終応答を同期できませんでした。取得済みの最新内容を表示しています。"
    },
    "delegateViewerOnly": "この委任会話は現在読み取り専用です。"
  },
  "ko": {
    "delegateAccess": {
      "waiting": "하위 에이전트 연결을 기다리는 중...",
      "observing": "위임 작업을 읽기 전용으로 보는 중",
      "parentTurnActive": "상위 대화가 실행 중인 동안 읽기 전용입니다",
      "stateUnknown": "접근 상태를 확인할 수 없어 읽기 전용입니다",
      "interactive": "새 턴을 시작할 수 있습니다",
      "syncFailed": "최종 응답을 동기화하지 못했습니다. 사용 가능한 최신 내용을 표시합니다."
    },
    "delegateViewerOnly": "이 위임 대화는 현재 읽기 전용입니다."
  },
  "de": {
    "delegateAccess": {
      "waiting": "Warten auf die Verbindung des Subagenten...",
      "observing": "Delegierte Aufgabe wird schreibgeschützt angezeigt",
      "parentTurnActive": "Schreibgeschützt, solange die übergeordnete Unterhaltung läuft",
      "stateUnknown": "Zugriffsstatus nicht verfügbar; schreibgeschützt",
      "interactive": "Bereit für eine neue Runde",
      "syncFailed": "Die endgültige Antwort konnte nicht synchronisiert werden. Der neueste verfügbare Inhalt wird angezeigt."
    },
    "delegateViewerOnly": "Diese delegierte Unterhaltung ist derzeit schreibgeschützt."
  },
  "es": {
    "delegateAccess": {
      "waiting": "Esperando la conexión del subagente...",
      "observing": "Viendo la tarea delegada en modo de solo lectura",
      "parentTurnActive": "Solo lectura mientras se ejecuta la conversación principal",
      "stateUnknown": "Estado de acceso no disponible; modo de solo lectura",
      "interactive": "Listo para un nuevo turno",
      "syncFailed": "No se pudo sincronizar la respuesta final. Se muestra el contenido más reciente disponible."
    },
    "delegateViewerOnly": "Esta conversación delegada es de solo lectura en este momento."
  },
  "fr": {
    "delegateAccess": {
      "waiting": "En attente de la connexion du sous-agent...",
      "observing": "Tâche déléguée affichée en lecture seule",
      "parentTurnActive": "Lecture seule pendant l'exécution de la conversation parente",
      "stateUnknown": "État d'accès indisponible ; lecture seule",
      "interactive": "Prêt pour un nouveau tour",
      "syncFailed": "Impossible de synchroniser la réponse finale. Le contenu disponible le plus récent est affiché."
    },
    "delegateViewerOnly": "Cette conversation déléguée est actuellement en lecture seule."
  },
  "pt": {
    "delegateAccess": {
      "waiting": "Aguardando a conexão do subagente...",
      "observing": "Visualizando a tarefa delegada no modo somente leitura",
      "parentTurnActive": "Somente leitura enquanto a conversa principal está em execução",
      "stateUnknown": "Estado de acesso indisponível; modo somente leitura",
      "interactive": "Pronto para um novo turno",
      "syncFailed": "Não foi possível sincronizar a resposta final. O conteúdo mais recente disponível está sendo exibido."
    },
    "delegateViewerOnly": "Esta conversa delegada está no modo somente leitura no momento."
  },
  "ar": {
    "delegateAccess": {
      "waiting": "في انتظار اتصال الوكيل الفرعي...",
      "observing": "عرض المهمة المفوضة بوضع القراءة فقط",
      "parentTurnActive": "للقراءة فقط أثناء تشغيل المحادثة الأصلية",
      "stateUnknown": "حالة الوصول غير متاحة؛ الوضع للقراءة فقط",
      "interactive": "جاهز لبدء جولة جديدة",
      "syncFailed": "تعذرت مزامنة الرد النهائي. يتم عرض أحدث محتوى متاح."
    },
    "delegateViewerOnly": "هذه المحادثة المفوضة للقراءة فقط حاليا."
  }
}
```

The outer locale labels above are an insertion guide, not a new object to paste into a locale. Insert each `delegateAccess` object and `delegateViewerOnly` string into that locale's existing tree. Do not expose the backend's raw reason code to users; the access status row supplies the reason-specific copy.

- [ ] **Step 6: Run component, locale parity, focused frontend, and formatting checks**

Run:

```powershell
pnpm test -- src/components/chat/delegate-access-status.test.tsx src/components/conversations/conversation-session-surface.test.ts src/i18n/messages.test.ts
pnpm eslint src/components/chat/delegate-access-status.tsx src/components/chat/delegate-access-status.test.tsx src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/i18n/messages.test.ts
```

Expected: both commands exit 0. Every locale has all seven new keys; waiting observer discovery is never presented as a disconnected owner; sync failure has accessible alert precedence; regular conversations render no delegate row.

- [ ] **Step 7: Run the complete repository verification matrix**

From the repository root:

```powershell
pnpm test
pnpm eslint .
pnpm build
```

Then run every affected Rust target:

```powershell
Set-Location src-tauri
cargo test --features test-utils
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings
cargo test --no-default-features --bin codeg-server --lib
cargo check --no-default-features --bin codeg-server
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command exits 0. No snapshot update is expected; if `cargo test` reports an intentional parser snapshot delta, inspect it with `cargo insta review` before accepting it.

- [ ] **Step 8: Commit status UI and locale coverage**

```powershell
git add src/components/chat/delegate-access-status.tsx src/components/chat/delegate-access-status.test.tsx src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/en.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json
git commit -m "feat: show delegated child access state"
```
