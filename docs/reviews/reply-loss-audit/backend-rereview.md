# Backend F11 / B1 re-review

Date: 2026-09-24. Baseline: `093f5a4a`.

**Code-review verdict: approve within scope; no remaining Critical or Important findings.** Original F11 and B1 are addressed in the inspected implementation. **Spec:** the production completion entrypoint now fills a still-unbound row using completion-owned identity, without overwriting a competing binding. **Quality:** the existing expected-null CAS is an appropriate minimal fix; it removes the unsafe read/rebind sequence without changing general binding behavior.

**Validation status:** final post-B1 GREEN and compatibility results remain pending in the implementer's results document. This is source-review approval, not a claim that the latest Rust revision has passed its tests or is fully validated for integration.

## Scope

Reviewed the current `git diff HEAD -- src-tauri/src/acp/lifecycle.rs`, `backend-fix-results.md`, the existing service predicate, the affected helper's callers, and the new deterministic competing-bind regression. Read the unchanged status handler only to check that the binding change does not alter its transaction/event contract.

No Cargo commands, Rust/test edits, commits, resets, subagents, full audits, or store/provider/view review. Only this requested report was written.

## B1: atomic expected-null binding

At `src-tauri/src/acp/lifecycle.rs:690`, `bind_live_external_id` now calls:

```text
renormalize_external_id_alias(db_conn, conversation_id, None, session_id)
```

The service at `src-tauri/src/db/service/conversation_service.rs:1379` builds a single UPDATE with these predicates:

```sql
WHERE id = conversation_id
  AND deleted_at IS NULL
  AND external_id IS NULL
```

It sets only `external_id` and `updated_at`. The helper no longer performs a preliminary `get_by_id`, calls the generic rebind transaction, reads continuation ancestry, or receives a preserved sibling ID.

Consequences for the original interleaving:

- If B binds the row first, A's stale completion UPDATE matches no row and is a silent no-op. C remains B; no sibling is created.
- If A fills the null binding first, a later deliberate SessionStarted/first-link binding for B still goes through the unchanged history-preserving generic binder. That path retains its intended ability to rebind and announce preserved history.
- SQLite evaluates the null predicate together with the write. Per-connection worker/prompt locks are no longer needed to protect an observation made before the mutation.
- Deleted/missing rows do not match. An existing same or different ID does not match. The existing unique index remains enforced; a collision is an error, not permission to take another row's session.

`bind_live_external_id` has only the shared completion helper as a production caller, plus the new regression test. The shared helper serves production at `lifecycle.rs:250` and legacy completion at `lifecycle.rs:853`. Removing its `agent_type` argument does not affect a non-completion consumer.

`persist_live_external_id` at `lifecycle.rs:666` still uses `bind_external_id` with continuation ancestry and publishes the necessary summary/preserved-row events. The existing detail-loader caller of `renormalize_external_id_alias` remains unchanged and can still pass a non-null expected old ID. No service semantics were broadened.

## Original F11 and captured identity

| Case | Inspected behavior |
| --- | --- |
| Production completion with sidecar | Uses the captured conversation ID and the event's session ID before status handling. This closes the original bypass of the legacy binding fallback. |
| Live connection changed identity or was removed | A sidecar still supplies the old completion's conversation ID; a newer mutable external ID is not substituted into the production binding. |
| No sidecar; live external ID absent/empty or equal to event ID | Uses the live conversation ID and event ID, subject to the atomic DB-null predicate. |
| No sidecar; conflicting live external ID | Skips the new binding write. |
| Empty event session ID | Shared helper returns; it does not borrow a newer live ID in the production path. |
| Row already durably bound by another path | Atomic UPDATE does nothing, including when that binding happened immediately before the write. |
| Binding DB error | Warning-only behavior is retained; status handling continues. The patch is a best-effort fallback, not a retry/persistence guarantee during DB failures. |

Legacy completion retains its prior choice of nonempty live ID with event-ID fallback, but now uses the same atomic unbound-row mutation. No new legacy ownership policy is being inferred from the production sidecar tests.

## Deterministic regression and its limits

`internal_completion_competing_binding_does_not_split_history` at `src-tauri/src/acp/lifecycle.rs:4090` stages the contested ordering explicitly:

1. Create C and observe its null binding.
2. Bind C to B using the real generic DB binder.
3. Register a newer live connection that points to C/B.
4. Attempt A's stale completion write through the actual completion write helper.
5. Invoke the production `handle_internal_event` completion handler.

Assertions cover exactly one conversation row, C still bound to B, consistency with B's live connection ID/external ID, the expected PendingReview status, and exactly one `conversation://changed` State event with the correct row ID, status and awaiting-reply token.

This is a useful deterministic regression at the mutation boundary: it would fail if the write helper again performed a generic history-splitting rebind. The results file records that RED outcome against the unsafe implementation: **two rows / captured-session instead of one row / newer-session**.

It does **not** inject a scheduler pause inside `handle_internal_event`, start two concurrently scheduled connection tasks, or exercise capture → ingress → FIFO delivery. That is acceptable for B1 now that the production preliminary read has been eliminated: the contested property is enforced by one conditional statement, and the test drives the precise write primitive that previously violated it. A test hook in the removed read would add no necessary coverage.

The observed-null read in the test is fixture setup, not a production read that remains vulnerable. The subsequent handler call also covers the binding's normal no-op behavior after B has won.

## Status/event integrity boundaries

The expected-null UPDATE does not change status or publish events. The unchanged production handler separately commits the status/usable-completion transaction before publishing its State patch. Removing the generic rebind from completion also removes the possibility of silently creating an unannounced preserved row in B1.

The new regression asserts one normal completion State event; it does not claim that a stale completion becomes a whole-handler no-op when a binding loses. In particular:

- A no-sidecar live-session mismatch prevents **binding**, but the pre-existing status handler still independently obtains the live conversation ID. The existing negative identity test asserts only the missing binding, not suppression of all status or event effects.
- The competing-binding test deliberately expects PendingReview on C after completing A. It checks the existing status contract and binding ownership independently; it does not prove turn-token isolation of all overlapping active turns.
- Existing status CAS, delegate ownership and event-publication logic are not redesigned by this patch. No new defect in those unchanged paths is asserted by this scoped re-review.

These are the same coverage boundaries noted in the prior review, and the updated implementer report now states them explicitly.

## Evidence and pending validation

The results artifact reports these checks for the **earlier F11 version, before the B1 CAS follow-up**:

- Four production-handler tests / 16 scenario combinations passed.
- Twenty legacy `handle_event_` tests passed, including completion bindings, completed-row CAS and soft-deleted rows.
- Scoped rustfmt/diff checks and a server-library cargo check passed.

Those results establish useful prior compatibility, but they must not be represented as GREEN for the newly changed write helper. For B1, the artifact records the deterministic RED run and says final GREEN/compatibility validation is in progress. I did not run or duplicate the implementer's Cargo work.

The representative-stop-reason test name now correctly describes its four tested reason strings rather than claiming exhaustive enumeration. Captured identity tests still use real SQLite and the production handler, with synthetic internal envelopes; they are not full ingress tests.

No further source change is requested by this re-review. Integration readiness remains subject to the implementer's latest focused GREEN and compatibility results.

## Reviewed fingerprints

SHA-256 at review cutoff:

- `src-tauri/src/acp/lifecycle.rs`: `715CC7CAC9510FBCB824B594C314CFABB3C3849ABD2F5548CC99FDB0291B36E2`
- `src-tauri/src/db/service/conversation_service.rs`: `EAB6DE187EC46699AF70BEC53C038D6D40345CFA1496B95BC13250244E08208A`
