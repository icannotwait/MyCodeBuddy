# Pop-out Close ACP Keepalive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Closing a conversation pop-out window reverse-rebinds ACP ownership to `main` and never force-kills busy agents (prompting, pending permission, or active background work); residual reap is idle-only on every close-reachable path; detached FE never bare-`acpDisconnect`.

**Architecture:** Split window-close decisioning from API abort (`decide_close` / `commit_close_reverse` vs `decide_abort` / `abort_inner`). Introduce a shared busy-safe residual helper that revalidates under lock (sweep-style). Reverse requires source label + operationId (+ generation when present). Terminals matching the incarnation rebind to `main` (no kill on close). Detached FE keeps bridge-level disconnect suppress for the full window lifetime and wires it into unmount disconnect call sites.

**Tech Stack:** Rust (Tauri 2 / ConnectionManager / ConversationPopoutState), React 19 + TypeScript (Next static export), Vitest, Cargo tests with `--features test-utils`.

**Spec:** `docs/superpowers/specs/2026-07-24-popout-close-acp-keepalive-design.md` (worktree copy is source of truth for this branch).

## Global Constraints

- Route A only (keep process alive while busy); no Route B mid-turn resume.
- No main-tab re-dock on close; sidebar reopen discovers live main-owned connection.
- App quit and main hide-to-tray behavior unchanged.
- Incarnation fences (`operationId`, tombstone, close reservation, generation CAS) preserved.
- `decide_abort` API semantics unchanged (`HandoffComplete` → `AlreadyComplete`).
- Emit `Reversed` only after successful manager reverse; use `ReverseUncertain` otherwise.
- Full `disconnect_by_owner_window_and_operation` remains for non-close paths; close paths use idle-only helper only.
- Parent session must not implement code; Grok implements, Codex reviews per SDD.
- Working directory: `D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive` on branch `feat/popout-close-acp-keepalive`.

---

## File map

| File | Responsibility |
| --- | --- |
| `src-tauri/src/acp/manager.rs` | `disconnect_idle_by_owner_window_and_operation`; source **operationId** CAS on reverse |
| `src-tauri/src/commands/conversation_popout.rs` | `CloseDecision`, `decide_close`, `commit_close_reverse`, `AbortOutcome::ReverseUncertain`, close handler + late-rebind residual wiring |
| `src-tauri/src/terminal/manager.rs` | `rebind_owner_window_by_operation` |
| `src/lib/conversation-popout-detached-bootstrap.ts` | Gate always-suppress for detached lifetime |
| `src/lib/conversation-popout-acp-bridge.ts` | Lifetime suppress; disconnect paths honor suppress |
| `src/app/conversation/page.tsx` | Do not clear suppress on commit-ack |
| `src/hooks/use-connection-lifecycle.ts` | Optional explicit suppress on unmount if bridge not consulted there |
| `src/contexts/acp-connections-context.tsx` | Ensure owner disconnect honors `isFrontendDisconnectSuppressed` |
| `src/lib/conversation-popout.ts` | FE reclaim: `ReverseUncertain` non-reclaimable |
| Parent + this design docs | Lifecycle amendment |

---

### Task 1: Idle residual + op-scoped reverse (ACP manager)

**Files:**
- Modify: `src-tauri/src/acp/manager.rs` (near `disconnect_by_owner_window_and_operation` ~4357 and `rebind_connection_owner_window` ~4395; tests near existing disconnect/rebind tests ~7100+)
- Test: same file `#[cfg(test)]` modules / existing test helpers

**Interfaces:**
- Consumes: existing connection fields (`owner_window_label`, `owner_operation_id`, `ownership_generation`, `connection_incarnation`, `ConnectionStatus`, `pending_permission`, `has_active_background_work`)
- Produces:
  - `pub async fn disconnect_idle_by_owner_window_and_operation(&self, owner_window_label: &str, operation_id: &str) -> usize`
  - `pub async fn rebind_stamped_connections_owner_window(&self, from_label: &str, operation_id: &str, to_label: &str) -> usize` — best-effort residual reverse for **every** connection still matching `(from_label, operation_id)` (no conversation root lookup; advances gen; used after primary reverse so late children are not stuck on a dead label)
  - `rebind_connection_owner_window`: root source must match `operation_id` (error text **must** include `"owner operation CAS"` so close classifier maps Superseded)

- [ ] **Step 1: Write failing unit tests for idle residual**

Add tests that:

1. Idle `Connected`, matching label+op, no pending permission, no background work → disconnect count 1.
2. `Prompting` matching stamp → count 0.
3. `Connected` + `pending_permission = Some` → count 0.
4. Active background work matching stamp → count 0.
5. Wrong op / wrong label → count 0.
6. **TOCTOU (required):** arrange connection idle at snapshot; before removal flip to Prompting under the same manager (mutate session state between the public API's internal phases by calling a test-only hook **or** by implementing the method with an internal two-phase structure and unit-testing the revalidate predicate helper in isolation — preferred: extract `fn is_idle_for_residual(state, now) -> bool` and test busy transitions there; integration test still attempts full path if harness can mutate between locks).
7. Op A residual does not reap op B on same label.
8. `rebind_stamped_connections_owner_window` moves a busy child still on `(conversation-*, op)` to `main` even when root is already main.

Sketch (adapt to existing test helpers for spawning/stamping connections):

```rust
#[tokio::test]
async fn disconnect_idle_skips_prompting_and_pending_permission() {
    // arrange: two connections on conversation-1 label, same op
    // conn_idle: Connected, no permission
    // conn_busy: Prompting
    // conn_perm: Connected + pending_permission
    let n = mgr
        .disconnect_idle_by_owner_window_and_operation("conversation-1", "op-1")
        .await;
    assert_eq!(n, 1);
    // assert busy + permission still present
}
```

- [ ] **Step 2: Run tests — expect FAIL (method missing)**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils disconnect_idle -- --nocapture
```

Expected: compile error / test not found until method exists.

- [ ] **Step 3: Implement `disconnect_idle_by_owner_window_and_operation`**

Two-phase, modeled on `sweep_idle` (~1371–1461):

1. Snapshot ids matching `(label, op)`.
2. For each candidate under connections lock: re-validate label, op, incarnation, `status == Connected`, `pending_permission.is_none()`, `!has_active_background_work(now)`.
3. Only then remove and send `Disconnect` (reuse patterns from `take_connections_for_disconnect` / sweep phase 2–3).

Do **not** call the unfiltered path after filtering only at scan time.

- [ ] **Step 4: Write failing test for reverse source operationId CAS**

```rust
#[tokio::test]
async fn rebind_rejects_when_root_operation_id_mismatches() {
    // connection on conversation-9 label with owner_operation_id = "op-B"
    let err = mgr
        .rebind_connection_owner_window(
            conv_id,
            None,
            "conversation-9",
            "main",
            "op-A", // delayed close for older incarnation
            Some(gen),
        )
        .await
        .unwrap_err();
    // message should indicate operation / owner CAS failure
}
```

- [ ] **Step 5: Implement op match on reverse root**

After label CAS (~4473), before mutating:

```rust
let root_op = current_op.as_deref().unwrap_or("");
if root_op != operation_id {
    return Err(AppCommandError::task_execution_failed(format!(
        "owner operation CAS failed: expected {operation_id}, have {root_op}"
    )));
}
```

Keep already-at-target+same-op idempotent success. Descendants keep existing expansion; only reverse targets that still share `prior_label` (existing).

- [ ] **Step 6: Implement `rebind_stamped_connections_owner_window`**

Under connections lock: for each connection with `owner_window_label == from_label` and `owner_operation_id == Some(operation_id)`, set label to `to_label`, bump `ownership_generation`, keep operation stamp (v1). Return rebound count. No conversation graph required (covers late children missed by root reverse).

- [ ] **Step 7: Run manager tests — expect PASS**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils disconnect_idle -- --nocapture
cargo test --features test-utils rebind_rejects_when_root_operation -- --nocapture
cargo test --features test-utils rebind_stamped -- --nocapture
```

- [ ] **Step 8: Commit**

```powershell
git add src-tauri/src/acp/manager.rs
git commit -m "fix(acp): idle residual disconnect, stamped residual rebind, op-scoped reverse CAS"
```

---

### Task 2: Close decision path + honest outcomes + all close residual sites

**Files:**
- Modify: `src-tauri/src/commands/conversation_popout.rs`
  - `AbortOutcome` (~47)
  - `abort_inner` / `abort_after_forced_reverse` (~568–622)
  - New: `CloseDecision`, `decide_close`, `commit_close_reverse`
  - `abort_outcome_for_close_reserved_forced_reverse` (~70–84) — stop fabricating `Reversed` on unknown reverse err
  - `handle_conversation_window_closed` (~1242–1480)
  - Late rebind residual (~1108–1136)
  - Unit tests in same file (~1630+)

**Interfaces:**
- Consumes: Task 1 `disconnect_idle_by_owner_window_and_operation` + op-scoped reverse; existing `rebind_connection_owner_window`
- Produces:
  - `enum CloseDecision { Done { outcome, conversation_id }, NeedReverse { conversation_id, generation }, NeedReverseBestEffort { conversation_id } }`
  - `fn decide_close(&self, operation_id: &str) -> Result<CloseDecision, AppCommandError>`
  - `fn commit_close_reverse(&self, operation_id: &str, outcome: AbortOutcome) -> Result<AbortOutcome, AppCommandError>`
  - `AbortOutcome::ReverseUncertain` — **keep** existing serde attrs `#[serde(tag = "kind", rename_all = "snake_case")]` so wire form is `{ "kind": "reverse_uncertain" }` (do **not** change rename_all to camelCase)
  - Shared residual helper (async fn in conversation_popout or free function):
    `async fn residual_reconcile_after_close(cm, tm_opt, label, op)` that:
    1. Best-effort reverse **every** connection still stamped `(label, op)` to `main` (cannot rely only on root rebind: once root is main, root rebind is idempotent and leaves late children behind — add `rebind_stamped_connections_to_main(label, op)` on ConnectionManager if needed)
    2. Then `disconnect_idle_by_owner_window_and_operation(label, op)`
    3. Terminals: **leave kill calls as-is in Task 2** (Task 3 replaces with rebind at **all** residual sites including late rebind)

**Ordered `decide_close` (spec):**

1. Existing `abort_outcome` is close-path terminal (`Reversed` / `ConnectionGone` / `Superseded` / `ReverseUncertain`) **or** provenance already `Aborted` with those outcomes → `Done` (idempotent)
2. `rebind_in_flight` → return Err containing exact substring `"rebind is in flight"` (preserve message for close poll loop). **Caller:** after bounded wait still in flight → fall through as `NeedReverseBestEffort` + residual (do **not** early-return with `abortOutcome: null` only)
3. Stored outcome is API-only `AlreadyComplete` / `NeverRebound` → **ignore for reverse skip**; clear skip and fall through (do not return Done)
4. Stored outcome is API `Reversed` / `ConnectionGone` / `Superseded` → `Done` (no second reverse; residual still runs)
5. `ownership_generation = Some(g)` including `HandoffComplete` → `NeedReverse { g }`
6. `ownership_generation = None` → `NeedReverseBestEffort`

**First-writer rule for `commit_close_reverse`:**

- If `abort_outcome` already set to API `AlreadyComplete` or `NeverRebound`: **overwrite** with close reverse outcome is **allowed only when** the close path is committing ownership recovery (`Reversed`/`ConnectionGone`/`Superseded`/`ReverseUncertain`) and phase was `HandoffComplete` or close-reserved — preferred rule: treat API skip outcomes as **non-terminal for close** and replace them.
- If outcome already `Reversed`/`ConnectionGone`/`Superseded`/`ReverseUncertain`: return existing (idempotent).
- On `Reversed { gen }`: stamp `rec.ownership_generation = Some(gen)` (same as `abort_inner`).
- Always clear `abort_reserved` and `rebind_in_flight`; set `phase = Aborted`.

- [ ] **Step 1: Write failing unit tests + update stale locks**

New tests:

```rust
#[test]
fn decide_close_handoff_complete_needs_reverse() { /* gen Some → NeedReverse; decide_abort still AlreadyComplete */ }

#[test]
fn commit_close_reverse_from_handoff_complete_sets_aborted_reversed() { /* bypass short-circuit; stamps gen */ }

#[test]
fn decide_close_after_api_already_complete_still_needs_reverse() { /* API AlreadyComplete; close still NeedReverse */ }

#[test]
fn decide_close_after_api_reversed_is_done_no_second_reverse() { /* Done { Reversed } */ }

#[test]
fn decide_close_after_api_connection_gone_is_done() { /* Done { ConnectionGone } */ }

#[test]
fn decide_close_no_gen_is_need_reverse_best_effort() { /* NeedReverseBestEffort */ }

#[test]
fn abort_outcome_unknown_reverse_is_uncertain_not_reversed() {
    let o = abort_outcome_for_close_reserved_forced_reverse(None, Some("weird error"), 9);
    assert_eq!(o, AbortOutcome::ReverseUncertain);
}

#[test]
fn abort_outcome_operation_cas_is_superseded_not_uncertain() {
    // helper or close classifier maps "owner operation CAS failed" → Superseded
}

#[test]
fn reverse_uncertain_serializes_as_kind_snake_case() {
    let json = serde_json::to_value(AbortOutcome::ReverseUncertain).unwrap();
    assert_eq!(json["kind"], "reverse_uncertain");
}

#[test]
fn rebind_in_flight_timeout_falls_through_to_best_effort_residual() {
    // Arrange: op with rebind_in_flight=true and a known conversation_id.
    // Simulate the close handler timeout branch (or extract it as a testable
    // function): after bounded wait still in-flight → NeedReverseBestEffort
    // path runs residual_reconcile, commit_close_reverse yields non-null
    // terminal outcome (ReverseUncertain or Reversed), and abort_reserved +
    // rebind_in_flight are cleared.
}
```

**Update existing tests that lock old behavior** (must not leave them failing):

- `close_reserved_forced_reverse_not_found_is_connection_gone` and any test asserting non-not-found reverse fabricates `Reversed` with forward gen → expect `ReverseUncertain` instead.
- Serialization tests remain snake_case `kind` tag.

- [ ] **Step 2: Run — expect FAIL**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils decide_close -- --nocapture
cargo test --features test-utils commit_close_reverse -- --nocapture
```

- [ ] **Step 3: Implement types + decide_close + commit_close_reverse**

```rust
// KEEP existing attrs — only ADD the variant:
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbortOutcome {
    // ... existing variants ...
    ReverseUncertain,
}

// CAS classifier helper used by close + late rebind:
fn reverse_err_is_cas_superseded(msg: &str) -> bool {
    msg.contains("generation CAS")
        || msg.contains("owner label CAS")
        || msg.contains("owner operation CAS")
        || msg.contains("operation CAS")
}
```

Implement `decide_close` ordered table and `commit_close_reverse` per first-writer rules above.

- [ ] **Step 4: Wire `handle_conversation_window_closed`**

Replace `decide_abort` with `decide_close`. Poll loop: keep matching `"rebind is in flight"`.

**Timeout branch (replace early-return with null outcome):** when poll bound expires still in flight:

1. Log warning
2. Treat as `NeedReverseBestEffort` for conversation_id (from op record)
3. Attempt best-effort reverse + residual_reconcile
4. `commit_close_reverse(ReverseUncertain)` if reverse did not clearly succeed; if reverse Ok → `Reversed`
5. Emit closed with that outcome
6. Still tombstone

On NeedReverse / NeedReverseBestEffort (normal path):

- call `rebind_connection_owner_window(..., expected_generation = Some(g)|None, operation_id)`
- Ok → `commit_close_reverse(Reversed { gen })`
- connection gone → `commit_close_reverse(ConnectionGone)`
- CAS (gen/label/**operation**) → `commit_close_reverse(Superseded { ... })`
- other → `commit_close_reverse(ReverseUncertain)` — **never** fabricate Reversed

**Residual (always, including Done and ReverseUncertain):**

```rust
// Shared residual_reconcile_after_close:
// 1) best-effort reverse all still-stamped (label, op) connections → main
// 2) disconnect_idle_by_owner_window_and_operation(label, op)
// NEVER disconnect_by_owner_window_and_operation on close paths
// Terminals: leave kill_by_owner_window_and_operation calls UNCHANGED in Task 2
// (Task 3 replaces both handler residual sites + late-rebind site)
```

Update/remove `should_disconnect` outcome gate so residual **always** runs for close, including `ReverseUncertain` (either add variant to match list or delete the gate).

After inflight registration wait: call the **same** residual_reconcile helper again.

- [ ] **Step 5: Wire late `record_rebind` close-reserved path (~1108–1136)**

```rust
// outcome from abort_outcome_for_close_reserved_forced_reverse (fixed taxonomy)
// REPLACE abort_after_forced_reverse with:
let _ = popout.commit_close_reverse(&operation_id, outcome);
// REPLACE full disconnect with residual_reconcile (ACP idle only in Task 2):
// best-effort reverse remaining (label, op) + disconnect_idle...
// Do NOT touch non-close else branch clear_rebind_in_flight (~1137)
```

Fix `abort_outcome_for_close_reserved_forced_reverse`:

- reverse Ok → `Reversed { gen }`
- connection gone → `ConnectionGone`
- CAS strings (incl. operation) → `Superseded { ... }` (use forward_generation as current_generation fallback if unknown)
- else → `ReverseUncertain` (not fabricated Reversed)

- [ ] **Step 6: Audit close-reachable full disconnect**

Migrate sites: `handle_conversation_window_closed` residual (~1423), final-reap (~1462), late rebind (~1124). Record list in commit message. Non-close paths keep full disconnect.

- [ ] **Step 7: Run unit tests — expect PASS**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils --lib conversation_popout -- --nocapture
```

- [ ] **Step 8: Commit**

```powershell
git add src-tauri/src/commands/conversation_popout.rs src-tauri/src/acp/manager.rs
git commit -m "fix(popout): decide_close reverse-first with idle residual on all close sites"
```

---

### Task 3: Terminal rebind on close (no kill)

**Files:**
- Modify: `src-tauri/src/terminal/manager.rs` (~238+)
- Modify: `src-tauri/src/commands/conversation_popout.rs` — **all** residual sites that still call kill:
  - close residual ~1432
  - close final-reap ~1473
  - late `record_rebind` close-reserved residual (~1122 area) — add terminal rebind here (Task 2 left kills only on handler; late path may have no terminal action yet)
- Prefer folding terminal rebind into the shared `residual_reconcile_after_close` helper so every close site is covered once.
- Test: add unit tests in `terminal/manager.rs` `#[cfg(test)]` using real `TerminalManager` + inject instances if the struct allows, or test via public spawn APIs if stubs are unavailable.

**Interfaces:**
- Consumes: Task 2 residual helper call sites
- Produces: `pub fn rebind_owner_window_by_operation(&self, from_label: &str, operation_id: &str, to_label: &str) -> usize`

**Intermediate state rule:** Task 2 leaves handler kill calls unchanged (same as today). Task 3 **must** replace kill with rebind on handler residual, final-reap, **and** late rebind residual — no “no-op kill” middle commit.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn rebind_owner_window_by_operation_moves_matching_terminals() {
    // Use TerminalManager + whatever insertion path existing tests use.
    // One terminal (label, op) → main; one mismatch left alone.
    let n = tm.rebind_owner_window_by_operation("conversation-1", "op-1", "main");
    assert_eq!(n, 1);
}

#[test]
fn residual_reconcile_does_not_call_kill() {
    // Handler-level or helper-level: after residual_reconcile, kill count path unused.
    // Assert rebind invoked / no terminal removed for matching busy ACP stamp.
}

#[test]
fn close_reserved_late_rebind_rebinds_stamped_terminal_to_main() {
    // Arrange close-reserved late record_rebind path: terminal stamped
    // (conversation-label, op) exists; forced reverse + residual runs.
    // Assert: terminal.owner_window_label == "main"; terminal still alive
    // (not kill_by_owner_window_and_operation).
}

#[test]
fn late_record_rebind_busy_connection_survives_idle_residual() {
    // Spec test 12: close-reserved forced reverse + busy leftover still on
    // (label, op) → disconnect_idle count 0 for that connection; process lives.
}
```

- [ ] **Step 2: Implement rebind**

```rust
pub fn rebind_owner_window_by_operation(
    &self,
    from_label: &str,
    operation_id: &str,
    to_label: &str,
) -> usize {
    let mut terminals = self.terminals.lock().unwrap();
    let mut n = 0usize;
    for instance in terminals.values_mut() {
        if instance.owner_window_label != from_label {
            continue;
        }
        if instance.owner_operation_id.as_deref() != Some(operation_id) {
            continue;
        }
        instance.owner_window_label = to_label.to_string();
        n += 1;
    }
    n
}
```

- [ ] **Step 3: Wire into shared residual helper**

In `residual_reconcile_after_close` (Task 2):

```rust
if let Some(tm) = tm {
    let n = tm.rebind_owner_window_by_operation(label, operation_id, "main");
    tracing::info!("[TERM] close residual rebound label={} op={} count={}", label, operation_id, n);
}
// DELETE close-path kill_by_owner_window_and_operation calls at ~1433, ~1474, and any late-path kill.
```

- [ ] **Step 4: Run tests — expect PASS**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils rebind_owner_window_by_operation -- --nocapture
cargo test --features test-utils --lib conversation_popout -- --nocapture
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/terminal/manager.rs src-tauri/src/commands/conversation_popout.rs
git commit -m "fix(popout): rebind terminals to main on all close residual sites"
```

---

### Task 4: Detached FE never bare-disconnect + reclaim honesty

**Files:**
- Modify: `src/lib/conversation-popout-detached-bootstrap.ts` (`resolveDetachedConnectGate`)
- Modify: `src/app/conversation/page.tsx` (`applyAck` must not clear suppress)
- Modify: `src/lib/conversation-popout-acp-bridge.ts` (docs + ensure suppress honored)
- Modify: `src/contexts/acp-connections-context.tsx` if owner `disconnect()` can still acpDisconnect while suppressed
- Modify: `src/hooks/use-connection-lifecycle.ts` if unmount path bypasses suppress
- Modify: `src/lib/conversation-popout.ts` reclaim classification for `reverseUncertain`
- Test: `src/lib/conversation-popout-detached-bootstrap.test.ts`, `src/app/conversation/_components/detached-bootstrap-flow.test.ts`, `src/lib/conversation-popout-acp-bridge.test.ts`, `src/lib/conversation-popout.test.ts`, lifecycle tests as needed

**Interfaces:**
- Consumes: existing `setSuppressFrontendDisconnect` / `isFrontendDisconnectSuppressed`
- Produces: always-true suppress for detached lifetime at gate; suppress not cleared on commit-ack; disconnect call sites no-op when suppressed

- [ ] **Step 1: Write failing tests + update stale gate tests**

```ts
it("keeps suppress after commit ack", () => {
  expect(
    resolveDetachedConnectGate({
      bootstrapReady: true,
      isLivePath: true,
      commitAcked: true,
    }).suppressFrontendDisconnect
  ).toBe(true)
})
```

**Update** existing tests that assert `suppressFrontendDisconnect: false` after `commitAcked: true` in:

- `src/lib/conversation-popout-detached-bootstrap.test.ts` (~74, ~94)
- `src/app/conversation/_components/detached-bootstrap-flow.test.ts` (~59)

Add:

- Post-ack: `setSuppressFrontendDisconnect` remains true when `applyAck` simulated (page wiring test or pure extract).
- `classifyAbortOutcome` / reclaim: wire `{ kind: "reverse_uncertain" }` → non-reclaimable (explicit branch; not only default `unknown`).
- Fence still matching + `{ kind: "reversed", generation: N }` → reclaimable (existing recovery).
- Post-complete fence-cleared + reversed → reclaim no-op.
- `pending_permission` scenario: with suppress set, provider `disconnect()` must not call `acpDisconnect` (provider already checks suppress at ~5971–5974 — lock with a unit/integration test so it cannot regress).
- **Spec test 17:** post-ack destroy/unmount of detached owner → mock `acpDisconnect` called **zero** times (integration-style; not only gate purity).
- Audit **all** `acpDisconnect` sites in `acp-connections-context.tsx` including ~5316, ~5363, ~5571, ~5769, ~5774, ~6012, ~6073 — each must honor suppress or be proven non-detached-owner.

- [ ] **Step 2: Run FE tests — expect FAIL**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive
pnpm exec vitest run src/lib/conversation-popout-detached-bootstrap.test.ts src/app/conversation/_components/detached-bootstrap-flow.test.ts
```

- [ ] **Step 3: Implement (concrete call sites)**

1. `resolveDetachedConnectGate`: always `suppressFrontendDisconnect: true` when detached path is active (ignore `commitAcked` for suppress flag).
2. `page.tsx` `applyAck` (~349): **delete** `setSuppressFrontendDisconnect(parsed.conversationId, false)`.
3. `page.tsx` unmount effect (~391–397): **delete** the clear call / make `shouldClearSuppressOnDetachedUnmount()` always false for production detached owners so parent-first cleanup cannot clear suppress before lifecycle unmount. Prefer remove the clear path entirely (suppress dies with JS context).
4. Provider `disconnect()` already honors `isFrontendDisconnectSuppressed` at `acp-connections-context.tsx` ~5971–5974 before `acpDisconnect` ~6012. **Verify** this remains true; add regression test. If any other disconnect entry (e.g. ~5571, ~5769) can bare-disconnect a suppressed conversation, short-circuit those too.
5. `conversation-popout.ts` `classifyAbortOutcome`: explicit case for `kind === "reverse_uncertain"` → non-reclaimable classification (same family as `connection_gone`).

- [ ] **Step 4: Run FE tests — expect PASS**

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive
pnpm exec vitest run src/lib/conversation-popout-detached-bootstrap.test.ts src/app/conversation/_components/detached-bootstrap-flow.test.ts src/lib/conversation-popout-acp-bridge.test.ts src/lib/conversation-popout.test.ts src/hooks/use-connection-lifecycle.test.ts
```

- [ ] **Step 5: Commit**

```powershell
git add src/lib/conversation-popout-detached-bootstrap.ts src/lib/conversation-popout-detached-bootstrap.test.ts src/app/conversation/page.tsx src/app/conversation/_components/detached-bootstrap-flow.test.ts src/lib/conversation-popout-acp-bridge.ts src/lib/conversation-popout-acp-bridge.test.ts src/lib/conversation-popout.ts src/lib/conversation-popout.test.ts src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts src/contexts/acp-connections-context.tsx
git commit -m "fix(popout): keep detached disconnect suppress for full window lifetime"
```

---

### Task 5: Spec/docs amendment

**Files:**
- Modify: `docs/superpowers/specs/2026-07-24-popout-close-acp-keepalive-design.md` (status → approved after landing)
- Modify: `docs/superpowers/specs/2026-07-20-conversation-popout-window-design.md` (all superseded close/orphan/API sections per design Migration table)

- [ ] **Step 1: Apply parent-doc amendments** from design §Migration / doc updates (close lifecycle row, orphan wording, any unconditional disconnect-on-close statements).

- [ ] **Step 2: Mark keepalive design status approved** with link to this plan.

- [ ] **Step 3: Commit**

```powershell
git add docs/superpowers/specs/2026-07-24-popout-close-acp-keepalive-design.md docs/superpowers/specs/2026-07-20-conversation-popout-window-design.md docs/superpowers/plans/2026-07-25-popout-close-acp-keepalive.md
git commit -m "docs: amend pop-out close lifecycle for ACP keepalive"
```

---

## Verification (final, after all tasks)

```powershell
cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive\src-tauri
cargo test --features test-utils --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib

cd D:\MyCodeBuddy\.worktrees\popout-close-acp-keepalive
pnpm eslint .
pnpm test
pnpm exec vitest run src/lib/conversation-popout src/hooks/use-connection-lifecycle src/app/conversation
```

Manual acceptance (from design): mid-prompt pop-out close keeps agent; logs show reverse + residual 0 for busy; idle reverse-to-main then idle sweep; sidebar reopen same connection; tray/quit unchanged.

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| `decide_close` ≠ `decide_abort`; HandoffComplete reverse on close | 2 |
| `commit_close_reverse` bypasses AlreadyComplete short-circuit + gen stamp | 2 |
| Idle residual helper + TOCTOU revalidation | 1 |
| Residual best-effort reverse of all stamped leftovers | 1+2 |
| All close residual sites (handler + late rebind + final reap) | 2+3 |
| `rebind_in_flight` timeout → best-effort reverse + residual | 2 |
| Op-scoped reverse CAS → Superseded | 1+2 |
| `ReverseUncertain` snake_case wire; no fabricated Reversed | 2 |
| API AlreadyComplete still reverse on close; API Reversed Done | 2 |
| Terminals rebind, no kill (handler + late path) | 3 |
| FE gate + bridge + unmount wiring; reverse_uncertain reclaim | 4 |
| Parent doc amendment | 5 |
| Spec tests: busy child, late race, re-pop-out ABA, pending_permission FE | 1–4 |

## Self-review notes

- Plan amended after document review group REQUEST_CHANGES (GLM/Kimi/Codex).
- Task 2 depends on Task 1; Task 3 depends on Task 2 residual helper; Task 4 parallel-able; Task 5 last.
- Type names (`CloseDecision`, `ReverseUncertain`, `rebind_owner_window_by_operation`, `rebind_stamped_connections_owner_window`) consistent.
