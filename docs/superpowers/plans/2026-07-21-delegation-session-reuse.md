# Delegation Session Reuse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add durable per-run delegation lifecycles and a `continue_delegation` MCP tool so same work-unit revisions reuse one child conversation and external session while keeping immutable parent cards.

**Architecture:** Split **thread** (child conversation + external session) from **run** (`delegation_task_runs` row per parent MCP tool call). Initial `delegate_to_agent` creates generation-1; `continue_delegation` mints a new run, resumes with `resume_existing_only`, and fences settlement by `(task_id, generation, child_connection_id)`. Platform recovery rails use `admission_class` + budget tables; card summaries are validated frontend display data only.

**Tech Stack:** Rust (SeaORM/SQLite, Axum MCP companion, ACP), TypeScript/React frontend, existing `DelegationTaskStore` / Broker patterns.

## Global Constraints

- Spec (authoritative): `docs/superpowers/specs/2026-07-21-delegation-session-reuse-design.md`
- Never fall through to `session/new` on continue path (`resume_existing_only`)
- Run table is authoritative for MCP status/cancel; conversation columns are latest-run projection only
- Counters charge only at prompt-admission success (`reached_running_at` set in same transaction)
- Card `card_summary_json` never appears in parent-facing MCP results
- Required agents Grok/Codex remain hard blockers when unavailable; no agent substitution
- Desktop + server + `codeg-mcp` surfaces must stay in sync
- Prettier/ESLint/TS strict; Rust clippy `-D warnings` with project feature flags from `Agents.md`

## File Structure

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/db/migration/m20260721_000001_delegation_task_runs.rs` | Create run + budget tables; backfill; conversation `delegation_run_generation` |
| `src-tauri/src/db/entities/delegation_task_run.rs` | SeaORM entity for runs |
| `src-tauri/src/db/entities/delegation_lineage_budget.rs` | Budget entities |
| `src-tauri/src/acp/delegation/run_store.rs` | Canonical run CRUD, settlement, projection CAS, budgets |
| `src-tauri/src/acp/delegation/store.rs` | Re-key load/settle/prefix to run task_id; keep conversation projection helpers |
| `src-tauri/src/acp/delegation/broker.rs` | Continue dispatch, settlement fence, summary extract, events |
| `src-tauri/src/acp/delegation/card_summary.rs` | Parse/validate `codeg-card-summary-v1` |
| `src-tauri/src/acp/delegation/types.rs` | Run status, typed errors, reports |
| `src-tauri/src/acp/delegation/tool_schema.json` | MCP schemas for continue + optional replacement fields |
| `src-tauri/src/acp/connection.rs` | `resume_existing_only` mode (no session/new) |
| `src-tauri/src/acp/manager.rs` | Connection incarnation / no retire-race reuse |
| `src/lib/delegation-run-snapshot.ts` | Historical card DTO client |
| `src/lib/delegation-binding-reduce.ts` | Resolve by task_id / parent_tool_use_id only |
| `src/components/message/delegation-status-card.tsx` | Structured summary rendering |
| `.agents/skills/brainstorm-to-delivery/SKILL.md` | Thread table + continue routing (later task) |

---

### Task 1: Migration, entities, and backfill

**Files:**
- Create: `src-tauri/src/db/migration/m20260721_000001_delegation_task_runs.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Create: `src-tauri/src/db/entities/delegation_task_run.rs`
- Create: `src-tauri/src/db/entities/delegation_lineage_budget.rs`
- Create: `src-tauri/src/db/entities/delegation_work_unit_budget.rs`
- Modify: `src-tauri/src/db/entities/mod.rs`, `conversation.rs` (add `delegation_run_generation`)
- Test: `src-tauri/tests/delegation_task_runs_migration.rs` (or extend `delegation_columns.rs`)

**Interfaces:**
- Consumes: existing `conversation` delegation columns
- Produces: tables `delegation_task_runs`, `delegation_lineage_budgets`, `delegation_work_unit_budgets`; column `conversation.delegation_run_generation`

- [ ] **Step 1: Write failing migration test** asserting generation-1 backfill uses `task_id = delegation_call_id`, collision losers get `history_only=true` + `legacy_parent_tool_use_id`, and never-running priors leave `reached_running_at` NULL.

- [ ] **Step 2: Run test — expect FAIL** (migration missing)

Run from `src-tauri/`:
```powershell
cargo test --features test-utils delegation_task_runs_migration -- --nocapture
```
Expected: compile or test failure referencing missing migration/table.

- [ ] **Step 3: Implement migration + entities** per design schema (all columns, partial unique indexes, backfill rules). Include:
  - unique `(child_conversation_id, generation)`
  - unique `(parent_conversation_id, parent_tool_use_id)` WHERE parent_tool_use_id IS NOT NULL
  - partial unique one non-terminal run per child
  - partial unique one non-terminal gen-1 per `(parent_conversation_id, work_unit_key)`
  - PK budget tables

- [ ] **Step 4: Re-run test — PASS**

- [ ] **Step 5: Commit**
```powershell
git add src-tauri/src/db src-tauri/tests
git commit -m "feat(db): add delegation_task_runs migration and backfill"
```

---

### Task 2: Run store + re-key DelegationTaskStore

**Files:**
- Create: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/mod.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Test: unit tests in `run_store.rs` or `src-tauri/src/acp/delegation/run_store_tests.rs`

**Interfaces:**
- Consumes: migration entities
- Produces:
  - `RunStore::insert_reserving(...) -> Result<DelegationTaskRun, RunError>`
  - `RunStore::promote_running(task_id, connection_id, admission_class) -> Result<(), RunError>`
  - `RunStore::settle_terminal(task_id, status, ...) -> Result<(), RunError>`
  - `RunStore::load_by_task_id(task_id) -> Option<DelegationTaskRun>`
  - `RunStore::resolve_unique_owned_prefix(parent_id, prefix) -> Result<String, ...>`
  - Monotonic conversation projection update using `delegation_run_generation`

- [ ] **Step 1: Failing tests** for: load by run task_id (not root call id), monotonic projection CAS, prefix recovery parent-scoped on runs, settle never uses conversation.delegation_call_id for continued runs.

- [ ] **Step 2: Run tests — FAIL**

- [ ] **Step 3: Implement run_store + re-key store trait methods** so Broker/MCP status/cancel resolve through runs. Keep conversation root `delegation_call_id` as immutable gen-1 linkage.

- [ ] **Step 4: Tests PASS**

- [ ] **Step 5: Commit**
```powershell
git commit -m "feat(delegation): run store and task_id-keyed status paths"
```

---

### Task 3: Budget rails + admission_class charging

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Test: budget unit tests

**Interfaces:**
- Produces:
  - `charge_unexpected_continue(lineage_root, work_unit_key?) -> Result<(), BudgetExhausted>`
  - `charge_replacement(lineage_root, work_unit_key?) -> Result<(), BudgetExhausted>`
  - Called only inside promote_running transaction with matching `admission_class`
  - Preflight helpers for reserving insert

- [ ] **Step 1: Failing tests** — third unexpected continue → budget_exhausted; second replacement → budget_exhausted; pre-running failure does not charge; host_restarted reserving inherits class charge on next promote.

- [ ] **Step 2: Implement** conditional UPDATEs with `rows_affected = 1`, lazy INSERT ON CONFLICT DO NOTHING.

- [ ] **Step 3: Tests PASS + commit**
```powershell
git commit -m "feat(delegation): platform recovery budget rails"
```

---

### Task 4: `resume_existing_only` ACP path

**Files:**
- Modify: `src-tauri/src/acp/connection.rs` (session/resume → load → **no** new)
- Modify: `src-tauri/src/acp/manager.rs` (incarnation / retire fence)
- Test: unit/integration tests near connection resume

**Interfaces:**
- Produces: launch flag or enum `SessionAttachMode::ResumeExistingOnly` that errors as `unresumable` on new-session fallthrough or external id mismatch

- [ ] **Step 1: Failing test** that current resume→load→new path would create new session; assert ResumeExistingOnly returns error instead.

- [ ] **Step 2: Implement gate** + external_id equality check after resume/load.

- [ ] **Step 3: Tests PASS + commit**
```powershell
git commit -m "feat(acp): resume_existing_only without session/new fallthrough"
```

---

### Task 5: MCP `continue_delegation` + replacement fields on `delegate_to_agent`

**Files:**
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/listener.rs` / `companion.rs` / `broker.rs`
- Modify: `src-tauri/src/bin/codeg_mcp.rs` (if tool list hardcoded)
- Test: schema/dispatch tests; `src-tauri/tests/delegation_route_contract.rs` extensions

**Interfaces:**
- Produces async ack:
```json
{
  "task_id": "...",
  "continued_from_task_id": "...",
  "child_conversation_id": 0,
  "agent_type": "codex",
  "reused_session": true,
  "status": "running"
}
```
- Typed errors: `not_found`, `stale_task_id`, `busy_thread`, `not_continuable`, `unresumable`, `not_supported`, `budget_exhausted`, `duplicate_parent_tool`, `invalid_replacement`
- `delegate_to_agent` optional: `replaces_task_id`, `replacement_reason`, `work_unit_key`

- [ ] **Step 1: Failing contract tests** for tool list inclusion, schema, ownership reject, stale id, busy, continuability.

- [ ] **Step 2: Implement dispatch** following design Continuation Flow steps 1–13 (fingerprint, admission_class, enqueue then promote).

- [ ] **Step 3: Implement replacement verification** on delegate path with `admission_class=replacement`.

- [ ] **Step 4: Tests PASS + commit**
```powershell
git commit -m "feat(mcp): continue_delegation and replacement lineage inputs"
```

---

### Task 6: Broker settlement fencing + card summary parser

**Files:**
- Create: `src-tauri/src/acp/delegation/card_summary.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs` if settlement hooks live there
- Test: parser unit tests; late-event fence tests

**Interfaces:**
- Produces: `parse_card_summary(raw: &str) -> Option<ValidatedCardSummary>`
- Settlement verifies `(task_id, generation, child_connection_id)` before mutate
- Completion event carries validated summary for frontend; MCP report text omits structured summary

- [ ] **Step 1: Failing parser tests** (last well-formed block, bounds, invalid fallback).

- [ ] **Step 2: Implement parser + wire settlement**.

- [ ] **Step 3: Failing late-event test** (old connection cannot settle new generation).

- [ ] **Step 4: Implement fence + PASS + commit**
```powershell
git commit -m "feat(delegation): settlement fence and card summary parser"
```

---

### Task 7: Startup reconcile runs

**Files:**
- Modify: `src-tauri/src/acp/delegation/store.rs` / `run_store.rs` / `broker.rs` startup
- Test: reconcile unit tests

- [ ] **Step 1: Failing test** — reserving → failed/host_restarted; running → canceled/host_restarted; counters preserved only for reached_running.

- [ ] **Step 2: Implement + PASS + commit**
```powershell
git commit -m "fix(delegation): reconcile non-terminal runs on startup"
```

---

### Task 8: Frontend historical cards + summary UI

**Files:**
- Create: `src/lib/delegation-run-snapshot.ts`
- Create: `src/lib/delegation-run-snapshot.test.ts`
- Modify: `src/lib/delegation-binding-reduce.ts`, `src/lib/delegation-child-projection-cache.ts`
- Modify: `src/hooks/use-delegation-card-model.ts`
- Modify: `src/components/message/delegation-status-card.tsx` (+ tests)
- Modify: overlay grouping components as needed
- Backend: authorized query/DTO for run by task_id (web handler + tauri command if required)

- [ ] **Step 1: Failing tests** — two cards same childConversationId keep independent summaries; later running run does not mutate terminal earlier card; invalid summary falls back.

- [ ] **Step 2: Implement snapshot API + UI**.

- [ ] **Step 3: `pnpm test` targeted files PASS + commit**
```powershell
git commit -m "feat(ui): immutable per-run delegation cards and summaries"
```

---

### Task 9: Skill routing for continue + ledger

**Files:**
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: related SDD skill docs if present under `src-tauri/experts/skills/`
- Test: document forward tests as checklist in plan report; optional fixture-driven unit if available

- [ ] **Step 1: Update skill** to:
  - Keep durable thread table keyed by work unit + role + profile
  - Prefer `continue_delegation` for same-unit revisions
  - Supply `work_unit_key` on orchestrated dispatches
  - Use replacement fields only for typed unresumable/budget paths
  - Cap unexpected continues at 2 + one replacement (platform enforced too)

- [ ] **Step 2: Commit**
```powershell
git commit -m "docs(skill): session reuse continue routing for brainstorm-to-delivery"
```

---

### Task 10: Integration / E2E / skill-forward validation

**Files:**
- Modify: `src-tauri/tests/delegation_e2e_*.rs` or add focused integration tests
- Modify: `src-tauri/tests/delegation_route_contract.rs`

- [ ] **Step 1: Add tests** covering:
  - multi-run same child conversation
  - resume_existing_only no session/new
  - concurrent double-continue → one winner
  - conversation 800 shape (3 children, N runs) as unit simulation
  - replacement lineage + not_continuable on superseded child

- [ ] **Step 2: Run**
```powershell
cd src-tauri
cargo test --features test-utils delegation
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
cd ..
pnpm test -- src/lib/delegation src/components/message/delegation
pnpm eslint src/lib/delegation-run-snapshot.ts src/components/message/delegation-status-card.tsx
```

- [ ] **Step 3: Commit**
```powershell
git commit -m "test(delegation): session reuse integration coverage"
```

---

## Spec Coverage Checklist

| Spec area | Task |
| --- | --- |
| `delegation_task_runs` + migration backfill | 1 |
| Store re-key + projection fence | 2 |
| Platform recovery rails | 3 |
| `resume_existing_only` | 4 |
| `continue_delegation` + replacement inputs | 5 |
| Settlement fence + card summary | 6 |
| Startup reconcile | 7 |
| UI immutability + overlay | 8 |
| Skill routing | 9 |
| Validation matrix / RED scenarios | 10 |

## Self-Review Notes

- No TBD placeholders in task deliverables.
- Task order is serial: schema → store → budgets → ACP → MCP → broker → UI → skill → e2e.
- Implementers must not parent-session-implement under brainstorm-to-delivery SDD; each task is a Grok subagent unit with Codex review after commit.
)
