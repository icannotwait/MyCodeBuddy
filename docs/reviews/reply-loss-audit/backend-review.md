# F11 backend review

Date: 2026-09-24. Baseline: `093f5a4a`.

**Verdict: request changes.** One Important finding; no Critical findings. The production-entry fix addresses the normal F11 case, but its “bind only if still unbound” guarantee is not atomic. Spec compliance is incomplete at that ownership boundary. Quality is otherwise appropriately scoped: one shared helper, event-owned identity, and real SQLite tests through the production handler.

## Scope and evidence

- Reviewed `git diff HEAD -- src-tauri/src/acp/lifecycle.rs`, F11 in `docs/reviews/2026-09-24-reply-loss-audit.md`, and `backend-fix-results.md`.
- Read unchanged DB-binding, prompt-linking, completion capture and lifecycle-worker code only to establish the changed code's ownership and concurrency behavior. No review of store, provider or views.
- No Rust edits, cargo commands, builds, test execution or subagents. This requested report is the only authored file for this task.
- This is a source-proven interleaving, not a claim that a concurrent Rust regression was executed. The backend worker owns compilation and runtime validation. Its results file still says GREEN/legacy compatibility validation is in progress at the review cutoff.
- Reviewed `lifecycle.rs` SHA-256: `B4E5B91157CF117C97470E8274387744FE0E8449036EA41E226B11E5FB27A409`.

## Important B1: the completion's null check can be invalidated before the binding transaction

**Changed location:** `src-tauri/src/acp/lifecycle.rs:713` — the `get_by_id`/`external_id.is_none()` check, followed by the awaited `bind_live_external_id` call at line 716. This is reached by the newly added production call at line 250.

**Supporting locations:**

- `src-tauri/src/acp/lifecycle.rs:696`: the bind-only helper invokes the general `bind_external_id` and discards its returned preserved-row ID.
- `src-tauri/src/db/service/conversation_service.rs:1001`: that general binding transaction deliberately supports rebinding; after acquiring the writer lock it reads the current value and preserves a different previous session on a sibling row. It does not enforce the caller's earlier `external_id IS NULL` observation.
- `src-tauri/src/acp/manager.rs:5527`: `send_prompt_linked` uses a **per-connection** prompt lock.
- `src-tauri/src/acp/manager.rs:5733`: first-link/adopt-row processing calls `bind_external_id` directly, before announcing the link. The surrounding code explicitly supports a fresh connection adopting an existing conversation row after a reconnect/session-new fallback.
- `src-tauri/src/acp/lifecycle.rs:2901`: lifecycle FIFO serialization is per connection; different connections execute concurrently.

### Concrete reachable interleaving

Use one ordinary conversation row C and two distinct, otherwise unowned session IDs A and B. A built-in agent has no continuation ancestors, so the generic binder will treat A and B as different histories.

1. C is still unbound after A's earlier SessionStarted/ConversationLinked binding failed or was unavailable. This is the exact F11 prerequisite. A completes and emits an internal envelope whose sidecar captures C and whose event session ID is A.
2. A's lifecycle worker enters the new helper. `get_by_id(C)` returns `external_id = NULL`. That read ends outside the later binding transaction. The worker has not retained a DB writer lock or a conversation-wide ownership lock.
3. A fresh connection for B adopts C through `send_prompt_linked`, for example after disconnect/reconnect with a new session. Its first-link path at `manager.rs:5733` commits `C.external_id = B`. It can announce the link/start the next turn. A's pending completion worker is independent of this connection's prompt lock; removing A from the manager also does not invalidate its captured completion sidecar.
4. A's worker resumes and calls `bind_live_external_id(C, A)`. Its transaction acquires the writer lock **after B committed**, reads `Some(B)`, and correctly follows the generic binder's *rebind* branch: C is repointed to A and B is preserved on a new row D. Neither the unique index nor the transaction rejects this, because A has no other holder and a rebind is an allowed operation for this API.
5. B's live connection and frontend still identify their conversation as C, but C now resolves to A's old transcript. D is not announced by the completion helper: `_preserved` is discarded. The subsequent status CAS operates on C and does not repair ownership or announce D.

This does **not** rely on two consecutive events for the same connection overtaking its FIFO. The competing write is the direct manager first-link write on another connection. The gap is before the binding transaction starts, so SQLite writer serialization does not protect the earlier null check. Even a one-connection SQL pool can execute the competing transaction between those two operations.

There is also an independent writer outside lifecycle/prompt locks: detail recovery calls `renormalize_external_id_alias(..., expected_old = None, ...)` in `commands/conversations.rs:1985`. If that fills an unbound row with a parser-normalized ID between the same read and transaction, a completion carrying another spelling can manufacture a history split and undo normalization. The reconnect interleaving above is sufficient for the finding; this second caller shows why a lifecycle-only lock would not fix the underlying condition.

### Impact and baseline distinction

The active conversation can resolve to the old session; the newer session's history moves to an unannounced sibling. This is wrong-session ownership and reply misfiling, not deletion of the transcript file. The generic preservation transaction prevents physical history destruction but does not preserve the active row's association.

The non-atomic sequence existed in the **legacy** completion branch before this patch. F11 now makes it reachable from the **production** internal completion entrypoint. This is therefore a defect introduced to the production binding path by the proposed fix, not an unrelated request to redesign the generic binder.

### Minimal change

Make this fallback an atomic conditional bind: update the live row only while `external_id IS NULL` and `deleted_at IS NULL`, preserving the existing uniqueness/error behavior. If another binding won, do nothing. The existing `renormalize_external_id_alias` implementation at `conversation_service.rs:1379` demonstrates—and already exposes with `expected_old = None`—the necessary expected-null CAS shape. Reuse that narrow operation if its contract is appropriate, or use an equivalently narrow DB operation.

Do not change the semantics of ordinary `bind_external_id`: SessionStarted and first-link paths intentionally use its history-preserving rebind behavior. The completion fallback has a smaller permission: fill an empty binding. Moving a plain null check into a transaction that releases its lock before the write would not solve the race.

### Required regression

Deterministically pause a production `handle_internal_event` completion **after it observes C as unbound but before its write**, commit B's binding through the real DB binding API, then resume A. Assert:

- C remains bound to B.
- No sibling row is created by A's completion fallback.
- B's live `external_id` and conversation ID remain consistent with C.
- The binding step introduces no extra or missing preserved-row upsert; normal completion status/event behavior retains its intended contract.

The current `internal_completion_preserves_existing_external_id` test binds the competing ID **before the helper starts**. That passes the initial read guard and cannot expose this interleaving. A test must straddle the decision/write boundary, not simply add another already-bound fixture.

## Ownership and CAS review

| Case | Assessment |
| --- | --- |
| Sidecar present; live state changes or disappears | The new binding correctly uses the captured conversation ID and event session ID. It does not substitute the newer live session. The null-check race B1 still applies to the DB row. |
| Sidecar missing; live ID differs from event ID | The wrapper declines the binding. This is a correct fail-closed choice for the newly added write. |
| Sidecar missing; live ID absent/empty or equal | The wrapper uses the live conversation ID and the event ID. It drops the state read guard before DB work; it is not an atomic claim over the row. |
| Event session ID empty | The shared helper returns without binding, even if live state has a nonempty newer ID. This avoids misattributing old completion content. |
| Row already bound before the initial read | Preserved, whether it holds the same or a different session. Covered by the new tests; insufficient for B1. |
| Binding fails | Logs and continues to status handling, matching the old best-effort behavior. Binding errors are not independently retried by the outer handler because this helper returns `()`. This behavior is explicit, not proof that every transient bind failure is repaired. |
| Successful uncontended first bind | No preservation split occurs. Binding emits no extra upsert; existing status CAS remains the completion state writer. |
| Duplicate/terminal status behavior | The status helper is unchanged; status updates remain conditional on InProgress, with post-commit event publication. The new wrapper does not replace that CAS. |

**Important scope distinction:** the no-sidecar session mismatch guard applies only to the new binding step. The wrapper still calls `handle_turn_complete_internal`; its existing no-sidecar branch at `lifecycle.rs:285` independently obtains the current live conversation ID for status handling and has no event-session comparison. Thus the new negative binding test does not prove that a mismatched sidecar-free completion is a total status/event no-op. That limitation predates this patch; I am not presenting it as a second introduced F11 finding. The report should not claim that the new tests establish whole-handler wrong-session protection.

Similarly, the new binding is separate from the subsequent status/usable-completion transaction. Existing status CAS checks do not make the binding's earlier null observation atomic. Ordinary first-bind event counts remain intact, but in B1 the discarded preserved ID violates the intent documented by `emit_preserved_conversation` at `commands/conversations.rs:2554`: rows created by a rebind must be made visible.

## Actual added-test coverage

The four added tests call the production `handle_internal_event` with an `InternalEventEnvelope` and real in-memory SQLite. They improve the F11 audit gap substantially; they are not merely legacy-handler tests.

- `internal_completion_binds_missing_external_id_for_all_stop_reasons`: tests **four representative strings** (`end_turn`, `empty`, `refusal`, `cancelled`) with and without a sidecar, asserting DB binding and status. Its name says “all,” but it does not individually enumerate `max_tokens`, `max_turn_requests`, `unknown`, or `auth_required`. Binding is unconditional on reason in the implementation, so this is not an independent correctness finding.
- `internal_completion_binds_captured_session_after_live_identity_changes`: covers same-row/new-row/removed live state, captured-row binding, untouched newer row, and preservation of live external ID/turn-in-flight fields.
- `internal_completion_preserves_existing_external_id`: covers same/different IDs already installed before handler entry; it does not cover binding after the initial null read.
- `internal_completion_without_owned_identity_does_not_bind_newer_session`: covers empty event identity and sidecar-free conflicting live identity, asserting only that DB external ID remains null. It does not assert status, emitted event contents/counts, or sidecar-free missing-connection behavior.

These are production-**handler** tests. They construct sidecars directly; they do not themselves run the event capture → lifecycle ingress → FIFO worker chain. That is a reasonable narrow test boundary for F11, provided the report does not describe it as end-to-end ingress coverage.

## Final assessment

The patch meets F11's happy-path requirement and selects immutable completion identity correctly. It needs the expected-null conditional write in B1 before approval; the broader rebinding transaction is the wrong mutation for a completion-only fallback. No speculative lock framework, worker restructuring, or generic binder redesign is needed.

Runtime validation remains with the backend implementer. No green cargo result is claimed by this review; the reviewed results artifact reports the initial RED run (2 failed, 2 passed) and says GREEN/legacy validation is pending.
