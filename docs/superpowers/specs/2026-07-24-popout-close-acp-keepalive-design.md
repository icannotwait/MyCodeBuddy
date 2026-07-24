# Pop-out Close ACP Keepalive Design

Date: 2026-07-24

Status: Approved — implemented via
[plan](../plans/2026-07-25-popout-close-acp-keepalive.md)
(Route A; design + plan review findings incorporated)

Related:

- [Conversation Pop-out Window](./2026-07-20-conversation-popout-window-design.md)
  (amends **Close / lifecycle** for detached windows; parent close/orphan/API
  sections updated to match this doc)
- [Implementation plan](../plans/2026-07-25-popout-close-acp-keepalive.md)
- Main-window tab unmount policy (`shouldDisconnectOnUnmount`)
- ACP idle sweep (`ConnectionManager::sweep_idle`)

## Summary

Closing a conversation pop-out window must **not** hard-kill a live ACP agent
that is still running a turn, waiting on permission, or holding outstanding
background work. Ownership returns to `main` via reverse rebind; only **idle**
resources still stamped with the closed incarnation may be reaped.

This aligns pop-out close with main-window hide-to-tray and with the existing
“busy owner keeps the process” unmount / idle-sweep rules. It **does not**
implement disconnect-and-resume catch-up of mid-turn streaming (Route B) — that
remains out of scope.

## Problem

### Observed

Logs (`codeg.2026-07-23.log`) show a repeatable sequence after handoff:

1. Connection spawns or lives under ownership.
2. Forward rebind stamps `(owner_window_label=conversation-{id}, owner_operation_id=op)`.
3. User closes the OS window.
4. Backend logs
   `[ACP] disconnect by owner window+op owner_window=conversation-* … count=1`
5. Session goes `Connected → Disconnected` within milliseconds.

Main window close does **not** do this: `CloseRequested` is prevented and the
window hides to tray; ACP stays up.

### Root cause (current code)

`handle_conversation_window_closed` calls `decide_abort`:

| Phase | Decision today | Effect |
| --- | --- | --- |
| `HandoffComplete` | `AlreadyComplete` — **no reverse** | Residual `disconnect_by_owner_window_and_operation` kills the live agent |
| `Opening` / `ReadyPending` + gen | `NeedReverse` | Reverse then residual reap (correct when reverse succeeds) |
| No gen | `NeverRebound` | Residual reap of everything on `(label, op)` |

Additionally, after commit-ack the detached frontend clears
`suppressFrontendDisconnect`, so React unmount may race a bare `acpDisconnect`
with backend cleanup.

The original pop-out design intentionally said: close detached → incarnation
scoped disconnect, **no re-dock**, and “must not leave orphan agents when the
only UI is closed.” That product choice conflicts with keep-alive of running
work and with main-window behavior.

### Why “disconnect then restore missed dialogue” is not the fix

Disconnect kills the agent CLI process. Reconnect can at best:

- `session/load` / `session/resume` (agent-dependent), and
- reload **already persisted** transcript / DB history.

It cannot reattach the same live turn stream or guarantee mid-tool continuity.
Full catch-up would be a separate architecture (Route B). **Route A** keeps the
process alive instead.

## Goals

1. **Busy never dies on pop-out close.** If status is `Prompting`, or
   `pending_permission` is set, or active background work is outstanding,
   close must not disconnect that connection.
2. **Prefer reverse rebind to `main`.** Successful or in-flight handoff close
   always attempts reverse ownership onto `main` before any reap.
3. **Residual reap is idle-only.** After reverse (or best-effort reverse), only
   connections still tagged `(conversation-{id}, operationId)` **and** idle may
   be disconnected.
4. **Frontend unmount does not race-kill.** Detached owner unmount never issues
   destructive `acpDisconnect`; backend owns incarnation cleanup.
5. **Preserve incarnation fences.** Label-reuse ABA, tombstone, close
   reservation, and `operationId` scoping stay intact.
6. **No re-dock of main tabs** on close (unchanged UX). User reopens from
   sidebar; open path discovers the live main-owned connection and claims UI.
7. **App quit still tears everything down** (unchanged).

## Non-goals

- Re-inserting the conversation into main `opened_tabs` when the pop-out closes.
- Persisting / restoring detached windows across restart.
- Disconnect-and-resume mid-turn catch-up (Route B).
- Changing idle-sweep timeout defaults or main hide-to-tray policy.
- Changing server/web multi-window behavior (pop-out remains local desktop only).
- Guaranteeing a main-window **composer surface** immediately after close
  without user navigation (ownership lives on `main`; UI attaches on reopen).

## Confirmed product decisions

| Area | Decision |
| --- | --- |
| Running task on pop-out close | **Keep ACP alive** (Route A) |
| Idle connection on pop-out close | May disconnect if still tagged with closed incarnation after reverse attempt |
| Ownership after close | Reverse rebind to **`main`** |
| Main tabs | **No re-dock** (sidebar reopen) |
| Orphan policy | Idle main-owned connection is subject to existing idle sweep / activity touch when UI reattaches |
| Missed live stream recovery | **Not required**; process was never killed while busy |
| Main window close | Unchanged (hide-to-tray) |
| App quit | Unchanged (full teardown) |

## Alternatives considered

### A. Reverse + busy-safe residual (selected)

Close always attempts reverse to `main`. Residual disconnect only idle
incarnation leftovers. Detached FE never bare-disconnects.

**Pros:** Matches main/busy unmount policy; no new resume protocol; small
behavioral surface relative to process architecture.
**Cons:** Brief periods with no attached owner UI while agent runs under
`main` ownership; idle sweep must continue to protect busy state (already does).

### B. Disconnect + transcript catch-up

Kill process; on reopen load history / resume session.

**Pros:** No orphan processes.
**Cons:** Loses in-flight turns; cannot restore missed streaming; agent-dependent
resume quality. Rejected for running tasks.

### C. Hide pop-out instead of destroy (tray-like)

Prevent close; hide window like main.

**Pros:** Trivial keepalive.
**Cons:** Diverges from “close means dismiss window” UX; multi-window clutter;
conflicts with “no restart restore / no re-dock” mental model. Deferred.

### D. Keep ownership on closed label until idle

Do not reverse; skip disconnect while busy; reverse or reap later.

**Pros:** Fewer rebind CAS paths.
**Cons:** Ownership points at a destroyed window label; late reconnect and
registration fences become harder. Rejected.

## Selected design

### Policy matrix (pop-out close)

Let `conn` match `(owner_window_label=conversation-{id}, owner_operation_id=op)`.

| Connection state at residual pass | Action |
| --- | --- |
| Already rebound to `main` (successful reverse) | Skip (not matched) |
| Still on incarnation, `Prompting` | **Do not disconnect**; attempt best-effort reverse if still on child label |
| Still on incarnation, `pending_permission` | **Do not disconnect**; attempt reverse |
| Still on incarnation, active background work | **Do not disconnect**; attempt reverse |
| Still on incarnation, idle `Connected` (and not above) | After reverse attempt: if still stamped → disconnect allowed |
| Terminal instances same stamp | **Rebind all** matching `(label, op)` terminals to `main`; **never kill** on pop-out close (v1) |

**Residual rule (single, aligned with matrix):** for every connection still
stamped `(label, op)` after the primary reverse, attempt a **best-effort reverse**
(label + operationId CAS; generation optional), then disconnect **only if** it is
still stamped **and** idle under lock. Busy leftovers that cannot reverse are
logged and left for idle sweep / manual cancel — never force-killed.

#### Terminals (v1 decision)

`TerminalInstance` has no conversation/connection/activity linkage and many
spawns stamp `owner_operation_id: None`. Therefore:

1. **On close reverse:** call `TerminalManager::rebind_owner_window_by_operation(from_label, op, to_label="main")` for all terminals that **do** match `(label, op)`.
2. **Never** call `kill_by_owner_window_and_operation` from any pop-out close path.
3. Terminals that never carried the incarnation op stamp are out of scope (same as parent doc aux-terminal non-goal); they are not invented as “busy ACP PTYs.”
4. Orphan PTYs that rebind to `main` with no UI are accepted until app quit or a future terminal idle sweep (document in Risks). No v1 terminal idle predicate.

### Backend: close decision vs abort decision

`decide_abort` remains the **API abort / compensation** path:

- `HandoffComplete` → `AlreadyComplete` (do not reverse a completed handoff when
  main is aborting a failed/in-flight transfer that already finished elsewhere).

Window close uses a **separate** decision entry point and a **new** enum so the
API path’s `AbortDecision` stays stable:

```rust
// Pseudocode — do not reuse AbortDecision for close
enum CloseDecision {
  NeedReverse { generation: u64 },
  NeedReverseBestEffort, // generation CAS skipped; label+op still required
  // Prior close/API abort already committed a terminal outcome that is NOT
  // "skip reverse for HandoffComplete". See ordering rules below.
  Done { outcome: AbortOutcome },
}
```

| Phase / gen / outcome (evaluate **top to bottom**) | `decide_close` |
| --- | --- |
| Close path already committed a terminal outcome via `commit_close_reverse` (`Reversed` / `ConnectionGone` / `Superseded` / `ReverseUncertain`) | `Done { outcome }` (idempotent) |
| `rebind_in_flight` | Bounded wait (same poll as today). If still in flight after bound → treat as `ReverseUncertain` risk: proceed with `NeedReverseBestEffort` + residual pass; never mass-kill |
| Stored outcome is **API-abort** `AlreadyComplete` or `NeverRebound` | **Ignore for reverse skip** — fall through to generation rows (close ≠ API provenance) |
| Stored outcome is **API-abort** `Reversed` / `ConnectionGone` / `Superseded` (connections already moved or gone via API path) | `Done { outcome }` — do **not** re-reverse; still run residual idle pass + terminal rebind for the closed `(label, op)` stamp |
| Any remaining phase with `ownership_generation = Some(g)` including **`HandoffComplete`** | `NeedReverse { generation: g }` |
| Any remaining phase with `ownership_generation = None` | `NeedReverseBestEffort` — **not** `NeverRebound` + mass kill |

Rationale: close ownership recovery is independent of API abort provenance for
`AlreadyComplete` / `NeverRebound` skip signals. If API abort **already**
committed a real ownership terminal outcome (`Reversed` / `ConnectionGone` /
`Superseded`), close must not invent a second reverse; residual still cleans
idle leftovers stamped with the closed incarnation.

`abort_for_close` in the close flow reuses the existing close/abort reservation
machinery (`abort_reserved` / close fence): it marks the op as close-driven so
late `record_rebind` takes the forced-reverse + **idle-only** residual path.
If an API `abort_inner` races the same op: first writer to commit a terminal
outcome wins the `abort_outcome` field; close still runs residual + terminal
rebind for `(label, op)` and never upgrades residual to full disconnect.

`handle_conversation_window_closed` calls `decide_close` only (not `decide_abort`).

#### Close reverse commit path (specified)

Add `commit_close_reverse(operation_id, outcome)` (name flexible) that:

1. **Bypasses** `abort_inner`’s `HandoffComplete → AlreadyComplete` short-circuit.
2. Sets `phase = Aborted`, stamps `abort_outcome = outcome`, clears
   `abort_reserved` / `rebind_in_flight` as appropriate.
3. Accepts `Reversed { generation }`, `ConnectionGone`, `Superseded`, or
   `ReverseUncertain` from the close path only.
4. Clears `abort_reserved` and `rebind_in_flight` on every terminal commit
   including `ReverseUncertain` (so residual pass and optional one-shot
   reconciliation are not stuck behind a stuck in-flight bit). A second reverse
   attempt under close is only the residual best-effort reverse, not a second
   `commit_close_reverse` for the same op after `Done`.

**`AbortOutcome` enum:** add `ReverseUncertain` (close-path honest failure).
Wire it through `conversation-window://closed` `abortOutcome` JSON so main FE
classifies it as **non-reclaimable** (no main-owner lease). API abort path does
not emit this variant.

`decide_abort` / API `abort_inner` retain the HandoffComplete short-circuit.

Unit lock: `HandoffComplete + commit_close_reverse(Reversed{g}) → Aborted +
Reversed{g}`; `decide_abort` on HandoffComplete still returns `AlreadyComplete`.

Wire `abortOutcome` on `conversation-window://closed` so main can reclaim when a
transfer fence still matches. **Post-complete close emitting `Reversed`:** after
handoff `complete()` the main fence is already cleared, so FE reclaim is a
no-op — intentional; do not “fix” by mapping back to `AlreadyComplete`.

### Backend: reverse then idle residual

```text
reserve close + abort_for_close
wait until !rebind_in_flight (bounded)
decision = decide_close(op)
match decision:
  NeedReverse(gen) / NeedReverseBestEffort:
    reverse_owner(conversation-*, main, op, expected_generation)
    commit_close_reverse(outcome)  // never fabricate Reversed
  Done(existing close outcome):
    use stored outcome
tombstone (label, op)
// Residual pass (ALL close-reachable sites share this helper):
//   for each still-stamped (label, op) connection:
//     best-effort reverse (label+op CAS)
//     then disconnect_idle_by_owner_window_and_operation
// Terminals: rebind_owner_window_by_operation(label, op, main); never kill
wait inflight registrations → final residual pass (same helper)
emit conversation-window://closed { conversationId, operationId, abortOutcome }
```

#### Shared idle residual helper (all close sites)

**Every** residual disconnect reachable from pop-out close **must** use the same
busy-safe helper — not only `handle_conversation_window_closed`. Enumerated
sites (non-exhaustive; audit at implement time):

1. `handle_conversation_window_closed` residual + final scan after inflight wait
2. Close-reserved **forced reverse** / late forward-rebind race path in
   `record_rebind` (today calls unfiltered
   `disconnect_by_owner_window_and_operation` after `abort_after_forced_reverse`)
3. Any other close-fence residual that currently uses full incarnation disconnect

Full `disconnect_by_owner_window_and_operation` remains available for non-close
paths that intentionally tear down an incarnation.

#### New manager APIs

```rust
// Pseudocode

/// Reverse ownership for connections matching source label + operation_id
/// (root + descendants). Generation CAS when expected_generation is Some.
/// Source match MUST include operation_id on the root (and retain descendant
/// expansion rules); label-only reverse is forbidden (ABA).
fn rebind_connection_owner_window(
  conversation_id,
  from_label,
  to_label,
  expected_generation: Option<u64>,
  expected_operation_id: &str, // NEW requirement for close reverse
) -> Result<...>;

/// Idle-only residual. Two-phase, sweep_idle-style:
/// 1) Snapshot candidates matching (label, op)
/// 2) Under manager lock at removal: re-validate
///    - owner_window_label == label
///    - owner_operation_id == op
///    - connection_incarnation unchanged
///    - status == Connected
///    - pending_permission.is_none()
///    - !has_active_background_work(now)
/// Skip (do not remove) if any check fails.
fn disconnect_idle_by_owner_window_and_operation(label, op) -> usize;

/// Terminals: stamp rebind only; no kill on close.
/// Canonical name: `rebind_owner_window_by_operation` on TerminalManager
/// (same verb-first style as ACP idle residual helpers).
fn rebind_owner_window_by_operation(from_label, op, to_label) -> usize;
```

Busy leftovers that **failed** reverse must **not** be force-killed. Log
loudly; leave process for idle sweep max-age / manual cancel. Optional metric:
`popout_close_busy_stranded`.

**Post-reverse operation stamp:** after successful reverse, connections may
retain `owner_operation_id = <pop-out op>` while `owner_window_label = main`.
That is accepted for v1 (main hide-to-tray does not reap by op; app quit kills
all). Document as assumption; do not clear op unless a later design needs it.
Future designs that introduce main-side op-scoped reap **must** clear or migrate
these stamps on reverse.

#### Reverse failure taxonomy (close)

| Error | Outcome | Residual |
| --- | --- | --- |
| Connection not found | `ConnectionGone` | Idle residual only (usually 0) |
| Generation / owner label / **operationId** CAS | `Superseded` | Residual reverse + idle-only; **never** kill busy |
| Manager reverse did not succeed (unknown / partial) | **`ReverseUncertain`** (non-reclaimable) — **do not** emit `Reversed` | Residual reverse + idle-only; **never** kill busy; log + optional retry once under close reservation |
| Successful manager reverse | `Reversed { generation }` only | Residual usually 0 |

**Hard rule:** emit `Reversed` **only** after a successful manager reverse result.
Never fabricate `Reversed` to unblock the FE. Main FE treats `Reversed` as a
main-owner lease; `ReverseUncertain` must map to non-reclaimable (no false
lease). Sidebar reopen still discovers live connections by conversation_id.

This tightens today’s “unknown reverse → still Reversed then full disconnect”
path, which was the kill vector when reverse failed.

### Child / delegation tree

Reverse continues to use existing `rebind_connection_owner_window` root+descendant
expansion (conversation graph + `parent_connection_id`). No change to child
spawn parent-generation adoption fences.

Busy-safe residual applies per connection after reverse: a busy child still on
the old label is not disconnected.

### Frontend: detached unmount

**Invariant-load-bearing (not race polish):** main-window
`shouldDisconnectOnUnmount` treats busy as
`status === "prompting" || backgroundOutstanding > 0` only — it does **not**
include `pending_permission`. Backend `sweep_idle` **does** skip
`pending_permission`. A detached owner with `Connected` + pending permission +
zero background work will therefore bare-`acpDisconnect` on unmount today and
kill the agent mid permission-gate. Always-suppress on detached is required for
Invariant 1, not merely to avoid racing backend cleanup.

#### Suppression must be wired into actual teardown (not gate-only)

Today’s risk: `resolveDetachedConnectGate` may compute suppress flags, but if
the page only consumes `gate.isActive` and `useConnectionLifecycle` has **no**
suppress option, the pure gate is not load-bearing for unmount. Commit-ack also
clears bridge-level suppress in `applyAck` / `setSuppressFrontendDisconnect`.

**Required wiring (v1):**

1. **Bridge / module suppress set** (`conversation-popout-acp-bridge`):
   - Set suppress for the conversation when detached owner mode starts.
   - **Do not clear** on commit-ack / `applyAck` while the window is still the
     detached owner.
   - Clear only when the detached window context is tearing down *after*
     backend close ownership transfer is no longer FE’s job (window death is
     sufficient — suppress dies with the JS context).
   - Any path that would call `acpDisconnect` / destructive disconnect for this
     conversation must consult the suppress set and **no-op**.

2. **Lifecycle unmount path** (`useConnectionLifecycle` or page-level cleanup):
   - Detached owner unmount **must not** call destructive disconnect even when
     `shouldDisconnectOnUnmount` would return true (idle Connected).
   - Implement by **either**:
     - (Preferred) consulting the same bridge suppress set inside the disconnect
       path used by unmount; or
     - passing an explicit `suppressDisconnectOnUnmount: true` / equivalent into
       the lifecycle hook for detached owner mode for the full detached lifetime.
   - Relying solely on a pure `resolveDetachedConnectGate` return value is
     **insufficient** unless that value is actually threaded into unmount
     cleanup.

3. **Gate function** may still return `suppressFrontendDisconnect: true` for
   documentation and tests, but implementation acceptance requires an
   integration-style unit test: post-ack destroy/unmount with idle or
   `pending_permission` owner → **zero** `acpDisconnect` invocations.

```text
// Detached lifetime (concept) — both must be effective at call sites
bridge.suppressDestructiveDisconnect(conversationId) = true  // for full lifetime
lifecycle / disconnect path honors suppress → no acpDisconnect on unmount
// pure gate flags alone do not count unless wired into those call sites
```

Rationale: backend close handler is the single writer for incarnation destroy /
reverse. FE unmount only detaches React ownership (viewer-style).

Per-window suppress state lives in the detached window’s own JS context and dies
with the window — no main-window leak.

### Frontend: main after closed

| Case | Behavior |
| --- | --- |
| Transfer fence still matches op + mainReleased / reverse lease | Existing `reclaimAfterAbort` / late terminal recovery |
| Successful handoff long ago; fence cleared; close emits `Reversed` | No automatic tab re-dock; reclaim no-ops (fence gone); ownership is `main`; sidebar open discovers live connection |
| `ConnectionGone` / `ReverseUncertain` / `Superseded` | Non-reclaimable for dead/uncertain lease; live discovery may still find a surviving process by conversation_id |

Closed listener continues to drop detached cache. Optional UX (non-blocking):
toast “Session still running in background” when reverse succeeded and session
was busy — **not required for v1 correctness**.

### Open from sidebar after close

Unchanged entry: open/activate main tab for conversation.

Must continue to support **live discovery + claim** for a connection already on
`main` (no second spawn). Existing main-window connect/dedup paths cover this;
add a regression test if missing: “after pop-out close reverse, sidebar open
reattaches same connection_id”.

### Idle sweep interaction

After reverse, connection is `main`-owned. Idle sweep already:

- skips `Prompting`, pending permission, active background work;
- requires idle `Connected` past timeout;
- re-validates owner lease under lock.

No sweep change required for correctness. Detached keepalive pings stop when
the window dies; **busy** state still protects until the turn settles, then
normal idle timeout applies if no main UI reattached and activity is stale.

### App quit / main exit

Unchanged: full disconnect of remaining connections. Route A does not keep
agents across process exit.

## State machine (amended)

```text
Detached Owning
    │ user closes window
    ▼
CloseReserved + tombstone-pending
    │
    ├─ ownership_generation Some → reverse(conversation→main, CAS gen)
    └─ None → reverse best-effort (label CAS only)
    │
    ▼
AbortOutcome::Reversed | ConnectionGone | Superseded | …
    │
    ▼
Idle residual reap (label+op ∩ idle only)
    │
    ▼
emit closed → main cache drop → (optional fence reclaim)
    │
    ▼
Agent (if busy): lives under main until turn ends + idle sweep
User: reopen from sidebar → claim live owner UI
```

## Invariants

1. **Busy-safe close:** no close path (including late rebind / forced reverse)
   may disconnect a connection that is prompting, permission-blocked, or has
   active background work — residual helpers re-check busy under the removal
   lock (not only at scan).
2. **Incarnation scope:** reverse and destructive residual require matching
   `operationId` (never label alone). Generation CAS when present.
3. **Close ≠ API abort:** `HandoffComplete` + API abort stays `AlreadyComplete`
   for the API path; **window close still reverse-first** even after that
   stored outcome.
4. **Single destructive authority for detached:** backend close residual
   (idle-only helper only); detached FE never bare-`acpDisconnect` (gate +
   bridge for full detached lifetime).
5. **No re-dock:** `opened_tabs` not mutated on close.
6. **ABA:** close captures `operationId` at open; delayed close/reverse for op
   A cannot reverse or reap a reopened incarnation B on the same label.
7. **Honest reverse outcomes:** `Reversed` only after successful manager reverse;
   never fabricate ownership for FE reclaim.
8. **Terminals on close:** rebind matching stamps to `main`; never kill on
   pop-out close (v1).

## Testing plan

### Rust unit

1. `decide_close` on `HandoffComplete` + gen → `NeedReverse` (not `AlreadyComplete`).
2. `decide_abort` on `HandoffComplete` still → `AlreadyComplete` (API path unchanged).
3. `commit_close_reverse` from `HandoffComplete` accepts `Reversed`; phase
   becomes `Aborted` with that outcome.
4. `decide_close` after API abort stored `AlreadyComplete` on `HandoffComplete`
   still returns `NeedReverse` / reverse-first (close ≠ API provenance).
5. `disconnect_idle_by_owner_window_and_operation`:
   - kills idle Connected + matching op;
   - skips Prompting / pending_permission / background_outstanding;
   - ignores wrong op / wrong label;
   - **TOCTOU:** connection idle at snapshot, becomes Prompting before remove
     under lock → not disconnected;
   - delayed residual for op A does not reap op B on same label (ABA).
6. Reverse success → residual count 0 for same connection.
7. Reverse CAS fail (label / gen / **operationId**) + busy → residual count 0.
8. Reverse with wrong operationId does not move a newer incarnation (ABA).
9. Unknown reverse failure → `ReverseUncertain`, not fabricated `Reversed`.

### Rust integration / manager

10. Spawn + forward rebind to conversation label → close reverse → connection
    remains Connected under `main` (busy preserved through reverse).
11. Cold connect on conversation with op, no gen → best-effort reverse (label+op)
    → main owner; busy not killed.
12. Close-reserved forced reverse / late forward-rebind race: busy leftover still
    on `(label, op)` → uses idle-only helper; process survives (count 0 for busy).
13. Busy **delegation child** still on old label after root reverse → residual
    skips disconnect.
14. Terminals matching `(label, op)` rebind to `main` on close; kill path not
    invoked; busy ACP keeps PTY.
15. Re-pop-out after close reverse: forward rebind from `main` stamps new op/gen
    without corrupting live connection.

### Frontend unit

16. After commit ack, gate `suppressFrontendDisconnect` remains true for detached
    **and** bridge suppress remains set.
17. **Integration-style:** post-ack destroy/unmount of detached owner issues
    **zero** `acpDisconnect` / destructive disconnect calls (proves wiring into
    lifecycle/bridge call sites, not gate purity alone).
18. Detached unmount with `pending_permission` + `status=connected` +
    `backgroundOutstanding=0` → no `acpDisconnect`.
19. Closed event with `Reversed` still drives reclaim when fence matches;
    post-complete fence-cleared close with `Reversed` is a reclaim no-op
    (intentional).
20. `ReverseUncertain` / `ConnectionGone` / `Superseded` do not create a false
    main lease.
20b. OS close after API abort already committed `Reversed` / `ConnectionGone` /
    `Superseded` → `decide_close` is `Done` (no second reverse); residual still
    idle-only.

### Manual / acceptance

21. Pop out live Codex/Claude session mid-prompt → close window → agent
    continues (logs: reverse, residual count 0 for busy, no
    `Connected→Disconnected` for that session).
22. Pop out idle session → close → reverse to `main`; session survives under
    `main` and is reclaimed by the normal idle sweep if no UI reattaches
    (not “disconnect immediately on close” as the success path).
23. Sidebar reopen after (21) → same session, no second spawn.
24. Main hide-to-tray still keeps ACP.
25. App quit kills agents.

## Rollout / risk

| Risk | Mitigation |
| --- | --- |
| Orphan agents with no UI | Idle sweep after turn settles; user cancel from reopened tab; max-age background valves unchanged |
| Reverse fail leaves busy on dead label | No kill; `ReverseUncertain`; log + metric; sidebar reclaim by conversation_id |
| Terminal orphans on `main` after rebind | Accepted v1; no kill on close; future terminal sweep optional; app quit tears down |
| API abort vs close confusion | Separate `decide_close` + `commit_close_reverse`; tests lock both |
| Late rebind residual kill | All close sites share idle-only helper |
| Fabricated FE reclaim lease | `Reversed` only after manager success |
| Design doc conflict with 2026-07-20 | This doc **amends** close lifecycle; update all superseded parent sections |

## Migration / doc updates

In implementation PR(s), update
`2026-07-20-conversation-popout-window-design.md` wherever close teardown is
normative (not only the Close / lifecycle table):

| Location (parent) | Action |
| --- | --- |
| Close / lifecycle table row (detached Owning close) | Replace with reverse + idle residual rule below |
| UX / orphan wording (“must not leave orphan agents…”) | Idle-vs-busy language below |
| API / event sections that imply unconditional incarnation disconnect on close | Point to this doc as authoritative for close |
| Any duplicate “full disconnect on Destroyed” statements tied to pop-out close | Align with idle residual + op scope |

| Event | New rule |
| --- | --- |
| Close detached while Owning | Capture `operationId`. **Reverse rebind to `main`** with **label+operationId** (+ gen when present). Residual: best-effort reverse then disconnect only **idle** connections still tagged `(conversation-{id}, operationId)`, via the **shared** idle helper on **every** close-reachable site. Terminals matching stamp **rebind** to `main` (no kill). Emit `closed` with honest `abortOutcome`. No re-dock. Busy work continues under main ownership. Detached FE never bare-disconnects. |

Replace the absolute “must not leave orphan agent processes after the only UI
is closed” with:

> Must not leave **idle** orphan agents indefinitely (idle sweep). Must not kill
> **busy** agents when the only UI closes; ownership returns to `main` until the
> user reopens or the process idles out. Must not kill a reopened window’s
> session when a prior incarnation’s delayed cleanup runs. Detached frontend
> unmount always-suppresses destructive disconnect (gate + bridge); main
> `shouldDisconnectOnUnmount` no longer governs detached owner teardown.

## Key decisions

1. **Route A over Route B** — Keep process alive for running work; no mid-turn
   resume protocol.
2. **`decide_close` ≠ `decide_abort`** — Completed handoff still reverses on
   window close; API abort of completed handoff stays non-reversing for API.
   Close ignores API `AlreadyComplete` as a reverse skip.
3. **Idle-only residual everywhere on close** — Shared helper; late rebind /
   forced-reverse paths included; reverse failures never mass-kill.
4. **Op-scoped reverse CAS** — Label alone forbidden; ABA-safe against reopen.
5. **Honest reverse outcomes** — `Reversed` only after manager success;
   `ReverseUncertain` for ambiguous failures.
6. **No re-dock** — Ownership moves; tabs do not. Sidebar open reattaches UI.
7. **Detached FE never destructive disconnect** — Gate + bridge for full
   detached lifetime; required for `pending_permission` safety.
8. **Terminals rebind, never kill on close (v1)** — No invented busy-terminal
   predicate; orphan PTYs until app quit / future sweep.
9. **Idle sweep remains the orphan safety net** — Accepts brief no-UI busy
   agents under `main`.

## Open questions

None blocking implementation after design-review amendments. Optional polish
(toast when background continues; terminal idle sweep) can be follow-up PRs.

## PR Plan

### PR 1 — Close decision + idle residual + op-scoped reverse (backend core)

**Title:** `fix(popout): reverse on close and idle-only residual reap`

**Touches:**

- `src-tauri/src/commands/conversation_popout.rs` (`decide_close`,
  `commit_close_reverse`, close handler, **all** close-reachable residual sites
  including late rebind / forced reverse)
- `src-tauri/src/acp/manager.rs` (`disconnect_idle_by_owner_window_and_operation`
  with sweep-style revalidation; reverse requires operationId)
- Unit + race tests in both modules

**Depends on:** none

**Description:** Window close uses reverse-first decision; residual disconnect
is idle-only and shared across close sites; reverse is ABA-safe; API
`decide_abort` behavior unchanged.

### PR 2 — Terminal rebind on close (no kill)

**Title:** `fix(popout): rebind terminals to main on pop-out close`

**Touches:**

- `src-tauri/src/terminal/manager.rs` (`rebind_owner_window_by_operation`)
- Close handler wiring (no `kill_by_owner_window_and_operation` on close)
- Tests

**Depends on:** PR 1

**Description:** Matching terminals rebind with ACP reverse; no kill on close.

### PR 3 — Detached frontend suppress + recovery tests

**Title:** `fix(popout): never bare-disconnect on detached unmount`

**Touches:**

- `src/lib/conversation-popout-detached-bootstrap.ts` (+ tests)
- `src/lib/conversation-popout-acp-bridge.ts` (lifetime suppress; honor in
  destructive disconnect path)
- `src/app/conversation/page.tsx` (do not clear suppress on commit-ack; ensure
  lifecycle unmount honors suppress / detached flag)
- `src/hooks/use-connection-lifecycle.ts` if an explicit suppress prop is chosen
- `src/lib/conversation-popout.ts` closed/reclaim tests (`Reversed` /
  `ReverseUncertain`)

**Depends on:** PR 1 for end-to-end; FE suppress can land early but is
**required for Invariant 1** (pending_permission), not optional polish.

**Description:** Wire suppress into **actual** disconnect/unmount call sites
(not gate-only); eliminate post-ack and permission-blocked kills; lock reclaim
for honest outcomes. PR acceptance includes test 17 (zero acpDisconnect on
post-ack unmount).

### PR 4 — Spec amendment + acceptance notes

**Title:** `docs: amend pop-out close lifecycle for ACP keepalive`

**Touches:**

- This design (status → approved)
- All superseded close/orphan/API sections in
  `2026-07-20-conversation-popout-window-design.md`

**Depends on:** PR 1–3 merged or co-landed

## Success criteria

- Closing a pop-out during an active prompt does **not** log session
  `Connected → Disconnected` for that agent solely due to window close.
- Logs show reverse rebind and residual `count=0` for the busy connection.
- Idle pop-out sessions reverse to `main` and survive close; normal idle sweep
  reclaims them if no UI reattaches (not an immediate close-time kill as the
  happy path).
- Late rebind / forced-reverse close paths never force-kill busy connections.
- Detached unmount (including post-ack and pending_permission) never issues
  `acpDisconnect`.
- Main hide-to-tray and app quit behaviors unchanged.
- Existing handoff/abort compensation tests remain green; new tests cover
  `HandoffComplete` close reverse, op-scoped ABA reverse, busy residual skip,
  and shared residual helper on all close sites.
