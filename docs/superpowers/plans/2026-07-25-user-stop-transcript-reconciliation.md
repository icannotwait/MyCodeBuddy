# User Stop Transcript Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close HEAD gaps so user Stop preserves live Codex content, attaches typed interruption metadata, and abort-fence reconciles under the design contract (soft fence, `owner_preserve`, Branch A/B, no-bump migration). **No self-managed codex-acp binary.**

**Architecture:** Amend FE/Rust runtime + parser only. Production keeps public Npx `@agentclientprotocol/codex-acp@1.1.7`. Managed-prefix / seed / private pin (**old AC3 packaging**) is **out of scope** (product decision 2026-07-27).

**Tech Stack:** Rust (ACP, parser), TypeScript/React (runtime store, ACP context), vitest + cargo.

**Design baseline:** `docs/superpowers/specs/2026-07-24-user-stop-transcript-reconciliation-design.md` (Round 4e + 2026-07-27 adapter decision)

**Worktree:** `D:\MyCodeBuddy\.worktrees\b2d-user-stop-transcript-reconciliation`  
**Branch:** `feature/b2d-user-stop-transcript-reconciliation`  
**Reviewed baseline SHA:** `4e23b90542d4366a293f22c138792a40a196e071` (main tip at worktree create)

## Global Constraints

- User Stop never resumes/retries/resends the interrupted prompt.
- Ordinary `end_turn` keeps no-refetch path; idle Cancel emits no `TurnComplete` / no soft fence arm.
- Only `UserCancelled` sets `termination_source = "user_stop"`.
- Coordinator never applies via `FETCH_DETAIL_SUCCESS`; only `RECONCILE_CANCELLED_TURN` after fence match.
- Recovery is **best-effort under append-order**.
- `cancelDestructiveSuppress = softFenceActive OR pendingCancel OR ownerPreserve`.
- **Do not** implement managed codex-acp seed/prefix/private pin; keep public `registry.rs` Npx pin.
- PowerShell-friendly; **never `git add -A`**; local commits only for parent repo.
- Parent session does not implement; Grok implementer + Codex reviewer per Task under SDD.
- Live outcome-only id: **RETAIN** tree spelling `cancel-outcome:${connectionId}:${completionSeq}`.

## Dependency DAG (normative) — post 2026-07-27

```text
Task 1 (parser)  ─────────────────────────────┐
Task 2 (suppress SM) ──► Task 3 (Branch A/B) ──► Task 4 (migration + envelope)
                                                    │
Task 6 (presentation RETAIN audit) ◄───────────────┘
Task 7 (verification of runtime/parser scope) ◄── 1–4 + 6

Task 0 / 5a / 5b / 5c  ── CANCELLED (no self-managed adapter)
```

- **Serial on shared FE files:** Task 2 → 3 → 4 only.
- **Parallel lanes:** {Task 1} ∥ {Task 2→3→4}; Task 6 after 1–4 preferred.
- Delivery complete for v1 when Tasks 1–4 + 6 + scoped Task 7 pass; adapter marker residual accepted.

## Fixed symbols

| Symbol | Role |
| --- | --- |
| `cancelDestructiveSuppress(session)` | softFence OR pendingCancel OR ownerPreserve |
| `noteUserStopTurnOwnership(runtimeId)` | Stop-time ownership + soft fence enter |
| `ownerPreserve` | Durable suppress |
| Branch A / Branch B | reconcile merge |

## File Map

| Area | Files |
| --- | --- |
| Parser | `src-tauri/src/parsers/codex.rs` |
| Runtime | `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts` |
| Envelope / dual-path | `src/contexts/acp-connections-context.tsx`, `src/contexts/user-stop-dual-path.test.ts`, `src/components/conversations/conversation-session-surface.tsx`, spot-check `conversation-detail-panel.tsx` |
| Presentation | `src/lib/adapters/ai-elements-adapter.ts`, `src/components/message/message-list-view.tsx`, `src/i18n/messages/*.json` |
| AC3 / vendor packaging | **Cancelled** — do not touch for this plan |

---

### Task 0 / 5a / 5b / 5c: CANCELLED — no self-managed codex-acp

**Product decision (2026-07-27):** Codeg will not manage a codex-acp binary,
seed payload, or private pin in this delivery. Keep
`@agentclientprotocol/codex-acp@1.1.7` via Npx.

- [x] **Cancelled** — no implementation; residual documented in design
  (adapter may still emit synthetic interrupt text; `activeTurnId` not
  guaranteed from public pin).

---

### Task 1: Parser — display-only null id + post-abort residual fixture

**Files:**
- Modify: `src-tauri/src/parsers/codex.rs`

**Interfaces:**
- null/empty `turn_id` + `interrupted` → display-only outcome (no `provider_turn_id`)
- non-empty id → matchable fence (RETAIN)
- post-abort in-scope record fixture documents v1 first-match residual (may miss post-abort content)

- [ ] **Step 1: Failing tests**
  1. Rewrite null/empty id test: outcome **present**, `provider_turn_id` absent.
  2. **Two-phase residual fixture** (deterministic contract, not “may drop”):
     - Phase A fixture: full pre-abort content + matching `turn_aborted` only.
       Parse → assert matchable fence present; assert specific post-abort marker
       content **absent**.
     - Phase B: same file with one additional in-scope agent_message after abort.
       Parse → assert that later content **is present**.
     - Document in test: v1 coordinator first-match apply uses Phase A snapshot;
       design does **not** auto re-apply after first accepted reconcile. (If a
       dedicated FE first-apply test is clearer for the residual, put Phase A
       “authorize apply without later content” in Task 3/4 runtime tests; keep
       both parser snapshots deterministic here.)

- [ ] **Step 2: Run**

```powershell
cd src-tauri
cargo test --features test-utils turn_aborted_null -- --nocapture
cargo test --features test-utils turn_aborted_two_phase -- --nocapture
```

- [ ] **Step 3: Implement** display-only branch; implement two-phase tests with **hard asserts** for each phase.

- [ ] **Step 4: Full turn_aborted suite**

```powershell
cargo test --features test-utils turn_aborted -- --nocapture
```

- [ ] **Step 5: Commit** `src-tauri/src/parsers/codex.rs` (+ snaps if any)

---

### Task 2: Runtime — soft fence, `owner_preserve`, `cancelDestructiveSuppress`

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts`
- Consumes: nothing from Task 1
- Produces: suppress predicate + states for Tasks 3–4

**Interfaces:**
- Extend `noteUserStopTurnOwnership` (or same Stop call path) to arm **soft fence** only when cancel targets an **active prompt** (idle Cancel must not arm).
- Soft-fence age-out **30s** → `ownerPreserve` (still suppresses).
- `cancelDestructiveSuppress` used at **all** automatic destructive commit sites (replace `sessionHasPendingCancel` alone): store fetch apply, viewer sync, delegate terminal sync.
- Explicit clear of `ownerPreserve`: new prompt, Manual Reload, session remove, identity reset.

- [ ] **Step 1: Failing tests**
  1. Soft fence on Stop ownership; destructive no-op.
  2. Idle Cancel does **not** arm soft fence.
  3. 30s age-out → ownerPreserve; still suppressed.
  4. `user_stop` without `provider_turn_id` (simulate via store API or test helper): outcome recorded path + ownerPreserve; no pending coordinator key.
  5. pendingCancel still suppresses (regression).
  6. Manual Reload / new prompt / remove restore eligibility.
  7. Retry exhaustion clears pending key, **keeps** ownerPreserve.

- [ ] **Step 2–4: TDD implement + vitest + eslint on touched files**

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts
pnpm exec eslint src/stores/conversation-runtime-store.ts src/stores/cancel-reconcile.test.ts
```

- [ ] **Step 5: Commit**

---

### Task 3: Runtime — Branch A/B reconcile

**Files:** same store + cancel-reconcile tests  
**Consumes:** Task 2 suppress states  
**Produces:** Branch A/B semantics for Task 4

- Branch A: detail cancelled-turn non-empty OR both empty → replace detail, clear overlays, clear suppress.
- Branch B: fence match + detail empty + local non-empty → skip detail install, keep overlays, clear pending/timers, **keep ownerPreserve**.
- Empty = no non-empty text/thinking/tool blocks (outcome-only ≠ content).
- **Plan-lock generation:** Branch A success need not bump `cancelGeneration` if suppress is fully cleared; exhaustion must not clear `ownerPreserve`.

- [ ] **Step 1: Failing tests** Branch A non-dup replace; Branch B retain + post-apply automatic destructive still suppressed; thinking/tool-only non-empty classification.

- [ ] **Step 2–4: Implement + pass tests**

- [ ] **Step 5: Commit**

---

### Task 4: Migration no-bump, unbound id, dual-path envelope/surface

**Files:**
- Modify: `src/stores/conversation-runtime-store.ts`, `src/stores/cancel-reconcile.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`, `src/contexts/user-stop-dual-path.test.ts`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Spot-check: `conversation-detail-panel.tsx` (must not double-start coordinator)

**Consumes:** Tasks 2–3  
**Produces:** final runtime+envelope behavior for Task 6/7

**HEAD invert (normative):**
- Today `MIGRATE_CONVERSATION` sets `pendingCancel: null` and bumps generations so late envelopes are **stale**.
- Required: runtime-key migration **migrates** pendingCancel (rewrite `runtimeConversationId`), soft fence, ownerPreserve, ownership, timers, **`recordedTurnOutcomeKeys`** (both ids), and **does not bump** `cancelGeneration` (move counter value).
- **Invert** existing tests that expect post-migrate stale fence / cleared pending.
- Identity replacement / true rebind still bumps + clears.

- [ ] **Step 1: Failing tests**
  1. migrate: same cancelGeneration; pending rewritten not null; soft/owner/timers migrated; **recordedTurnOutcomeKeys** migrated; duplicate envelope after migrate does **not** second footer.
  2. In-flight deferred reconcile applies against **post-migration** identity (no gen bump).
  3. Identity replacement: bump + clear suppress + cancel coordinator.
  4. Unbound detail id (`<=0`): outcome, no coordinator, ownerPreserve.
  5. Late envelope after 30s age-out still current → may start coordinator; stale gen no-ops.
  6. Status-edge / viewer / delegate destructive under suppress no-ops.
  7. Panel does not start coordinator for open owner tabs (spot assert).

- [ ] **Step 2–4: Implement +**

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts src/contexts/user-stop-dual-path.test.ts
```

- [ ] **Step 5: Commit**

---

### Task 5a / 5b / 5c: CANCELLED

See top-of-plan cancellation. Do not implement vendor publish, managed prefix,
seed packaging, or pin change. Public Npx \@agentclientprotocol/codex-acp@1.1.7remains the production launch path.

---

### Task 6: Presentation RETAIN audit

**Depends:** Tasks 1–4 preferred (stable outcome shape)

- [ ] **Step 1: Evidence report** — fingerprint 6 fields; FE19 duration-only cache; all 10 locales `responseInterrupted`; outcome-only grouping; copy exclusion. Paths + greps in `.superpowers/sdd/task-6-presentation-retain.md`.

- [ ] **Step 2: Only if gap found, TDD fix**

```powershell
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts
```

Do **not** use `--passWithNoTests` as a green pass for missing suites.

- [ ] **Step 3: Commit only if code changed**

---

### Task 7: Verification sweep (runtime/parser scope)

**Depends:** Tasks 1–4 + 6 claimed complete. **No** vendor/AC3 packaging checks.

- [ ] **Frontend (scoped minimum if full suite too heavy; prefer full when practical)**

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts src/contexts/user-stop-dual-path.test.ts src/lib/adapters/ai-elements-adapter.test.ts
pnpm exec eslint src/stores/conversation-runtime-store.ts src/contexts/acp-connections-context.tsx src/stores/cancel-reconcile.test.ts
# Preferred full matrix when closing delivery:
# pnpm eslint .
# pnpm test
# pnpm build
```

- [ ] **Desktop Rust (focused + preferred full)**

```powershell
cd src-tauri
cargo test --features test-utils --lib turn_aborted
cargo test --features test-utils --lib user_stop
# Preferred when closing delivery:
# cargo check
# cargo test --features test-utils
# cargo clippy --all-targets --features test-utils -- -D warnings
```

- [ ] **Server / codeg-mcp** — preferred on close; not blocked by cancelled AC3

```powershell
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

- [ ] **Do not** require stage-codex-acp, seed layout, private pin, or smoke-codex-acp for v1 complete.

- [ ] Write `.superpowers/sdd/user-stop-b2d-verification.md` with outcomes + residual: public 1.1.7 may still emit synthetic interrupt text.

---

## Spec coverage checklist

| Design requirement | Task |
| --- | --- |
| Display-only null abort id | Task 1 |
| Post-abort first-match residual fixture | Task 1 |
| Soft fence + owner_preserve + cancelDestructiveSuppress | Task 2 |
| user_stop without provider id → owner_preserve | Task 2 |
| Branch A/B | Task 3 |
| Migration no-bump + recordedTurnOutcomeKeys + late envelope | Task 4 |
| Unbound detail id | Task 4 |
| Dual-path / surface / panel | Task 4 |
| Self-managed adapter / private pin | **Cancelled** |
| Presentation RETAIN | Task 6 |
| Verification (runtime scope) | Task 7 |

## Execution status (2026-07-27)

| Task | Status |
| --- | --- |
| 0 / 5* | **Cancelled** (no self-managed codex-acp) |
| 1–4 | **Done** + Codex task review |
| 6 | **Done** + Codex task review |
| 7 | Remaining: preferred full matrix optional; focused suites already green |

Final global Codex review may proceed on runtime branch without AC3 packaging.
