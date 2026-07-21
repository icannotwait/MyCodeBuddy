# Delegation Session Reuse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add durable per-run delegation lifecycles and a `continue_delegation` MCP tool so same work-unit revisions reuse one child conversation and external session while keeping immutable parent cards.

**Architecture:** Split **thread** (child conversation + external session) from **run** (`delegation_task_runs` row per parent MCP tool call). Initial `delegate_to_agent` creates generation-1 with a full launch snapshot; `continue_delegation` mints a new run, resumes with `resume_existing_only`, and fences settlement by `(task_id, generation, child_connection_id)`. Platform recovery rails use `admission_class` + budget tables; card summaries are validated frontend display data only.

**Tech Stack:** Rust (SeaORM/SQLite, Axum MCP companion, ACP), TypeScript/React frontend, existing `DelegationTaskStore` / Broker / Spawner patterns.

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
| `src-tauri/src/db/migration/m20260721_000001_delegation_task_runs.rs` | Run + budget tables; backfill; `conversation.delegation_run_generation` |
| `src-tauri/src/db/entities/delegation_task_run.rs` | SeaORM run entity |
| `src-tauri/src/db/entities/delegation_lineage_budget.rs` | Lineage budget entity |
| `src-tauri/src/db/entities/delegation_work_unit_budget.rs` | Work-unit budget entity |
| `src-tauri/src/acp/delegation/run_store.rs` | Run CRUD, projection CAS, budgets, fingerprint, task_preview |
| `src-tauri/src/acp/delegation/store.rs` | Re-key load/settle/prefix to run task_id |
| `src-tauri/src/acp/delegation/spawner.rs` | Gen-1 run insert + launch snapshot; existing-child spawn path for continue |
| `src-tauri/src/acp/delegation/broker.rs` | Continue dispatch, settlement fence, events with summary |
| `src-tauri/src/acp/delegation/card_summary.rs` | Parse/validate card summary comment |
| `src-tauri/src/acp/delegation/capability.rs` (or route extension) | Per-agent continue capability gate → `not_supported` |
| `src-tauri/src/acp/delegation/types.rs` | Run status, typed errors, reports, event DTOs |
| `src-tauri/src/acp/delegation/tool_schema.json` | MCP schemas |
| `src-tauri/src/acp/delegation/listener.rs` / `companion.rs` / `transport.rs` / `meta_writer.rs` / `attention.rs` | Tool registration, parent tool correlation without requiring agent_type on continue; attention open/reconcile by run task_id |
| `src-tauri/src/acp/lifecycle.rs` | Recognize `continue_delegation` parent tool correlation / missing `_meta.tool_use_id` |
| `src-tauri/src/acp/connection.rs` | `resume_existing_only` |
| `src-tauri/src/acp/manager.rs` | Connection incarnation fence |
| `src-tauri/src/commands/delegation.rs` + web handlers/router | Task-id snapshot DTO (desktop + web) |
| `src/lib/delegation-run-snapshot.ts` | Client for historical run snapshot |
| `src/lib/types.ts` | Mirror run DTO / completion event summary field |
| `src/lib/delegation-binding-reduce.ts` | Resolve by task_id / parent_tool_use_id only |
| `src/lib/delegation-child-projection-cache.ts` | Latest overlay only; no historical overwrite |
| `src/components/message/content-parts-renderer.tsx` | Recognize continue tool cards |
| `src/components/message/message-list-view.tsx` | Card discovery for continue |
| `src/components/message/delegation-status-card.tsx` | Summary rendering |
| Overlay components under `src/components/message/` | Group by child conversation |
| `.agents/skills/brainstorm-to-delivery/SKILL.md` | Continue routing + work_unit_key |

---

### Task 1: Migration, entities, and backfill

**Files:**
- Create: `src-tauri/src/db/migration/m20260721_000001_delegation_task_runs.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Create entities listed above; modify `conversation` entity for `delegation_run_generation`
- Test: `src-tauri/tests/delegation_task_runs_migration.rs`

**Interfaces:**
- Produces: `delegation_task_runs`, budget tables, indexes from design

- [ ] **Step 1: Failing tests** for each backfill rule:
  - `task_id = delegation_call_id` for gen-1
  - **duplicate `delegation_call_id`**: keep newest non-deleted child as continuable run; losers skip run insert (no PK collision)
  - status map: in_progress→running, pending_review/completed→completed, cancelled→canceled
  - empty parent_tool_use_id → NULL + history_only
  - duplicate (parent, parent_tool_use_id) losers → history_only + legacy_parent_tool_use_id
  - missing external_id → history_only, non-continuable
  - deleted parent: still backfill child history; ownership checks fail closed later
  - missing reconstructible launch snapshot → non-continuable fields null
  - `lineage_root_task_id = task_id` for original gen-1; `admission_class = normal_revision`
  - `reached_running_at` set for any backfilled run that maps to `running`/`completed`/`failed`/`canceled` with prior admission reality (include `in_progress`→`running`); never invent for history_only non-admitted rows

- [ ] **Step 2: Implement migration + entities**

- [ ] **Step 3: Tests PASS + commit**
```powershell
git commit -m "feat(db): delegation_task_runs migration and backfill"
```

---

### Task 2: Run store, re-key, fingerprint, task_preview

**Files:**
- Create: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `store.rs`, `types.rs`, `mod.rs`
- Test: run_store unit tests

**Interfaces:**
- `derive_task_preview(task: &str) -> String` — redact full text then ≤200 scalars
- `request_fingerprint(tool_name, task_text, work_unit_key, replaces_task_id, replacement_reason, target_task_id, route_fingerprint_hex)` — NFC + fixed field order; absent optionals are empty strings never omitted; lowercase hex for route_fingerprint
- `RunStore::{insert_reserving, promote_running, settle_terminal, load_by_task_id, resolve_unique_owned_prefix, project_conversation}`
- Monotonic `delegation_run_generation` CAS

- [ ] **Step 1: Failing tests** — preview redaction patterns (`Bearer `, `sk-`, `ghp_`, `github_pat_`, `glpat-`, `xox`, `AKIA`, PEM), length bound, fail-closed empty; fingerprint stability + empty optionals; store re-key + CAS + prefix recovery.

- [ ] **Step 2: Implement + PASS + commit**
```powershell
git commit -m "feat(delegation): run store, fingerprint, and task_preview redaction"
```

---

### Task 3: Budget rails + admission_class

**Files:** `run_store.rs` + tests

**Interfaces:**
- `charge_unexpected_continue` / `charge_replacement` only inside `promote_running`
- Preflight at reserving; `reached_running_at` set with charge

- [ ] **Step 1: Failing tests** — third unexpected continue → budget_exhausted; second replacement → budget_exhausted; dual-row (lineage at limit, work-unit free) → budget_exhausted no partial charge; pre-running no charge; **post-running cancel/fail/restart retains charged counter (no refund)**; dual concurrent promote races; generation > 100 → budget_exhausted.

- [ ] **Step 2: Implement + PASS + commit**
```powershell
git commit -m "feat(delegation): platform recovery budget rails"
```

---

### Task 4: Gen-1 live path + launch snapshot + capability gate

**Files:**
- Modify: `spawner.rs`, `broker.rs` (initial dispatch), `route.rs` as needed
- Create or extend: capability registry for continue support
- Test: unit/integration for gen-1 run creation

**Interfaces:**
- On every successful `delegate_to_agent` reserve: insert gen-1 run with workspace_path, route_fingerprint, mode_id, allowlisted config_values_json, launch_snapshot_version, work_unit_key, request_fingerprint, task_preview, admission_class
- Live secret re-resolution at spawn (not stored)
- Capability: `agent_supports_session_reuse(agent_type) -> bool`

- [ ] **Step 1: Failing tests** — new delegate creates gen-1 run + snapshot; concurrent same-work_unit_key dual first-dispatch → one winner, loser gets `busy_thread` (or `invalid_replacement` when replacement-qualified); fingerprint match returns same run; mismatch rejects; profile deleted / snapshot unlaunchable → unresumable on continue path later; secret rotation still launches without mutating snapshot; concurrent gen-1 fence; **capability false affects only `continue_delegation` (`not_supported`), not initial `delegate_to_agent`**.

- [ ] **Step 2: Implement + PASS + commit**
```powershell
git commit -m "feat(delegation): gen-1 run rows with immutable launch snapshots"
```

---

### Task 5: Settlement fence, run-identity handoff, reconcile, card summary, resume_existing_only (gated unit)

**Files:**
- `card_summary.rs`, `broker.rs`, `lifecycle.rs`, `spawner.rs` (connection registration link), `store.rs`/`run_store.rs` reconcile, `connection.rs`, `manager.rs`, `attention.rs`
- Tests for each

**Interfaces:**
- **Run identity handoff (Critical):** replace root-only `delegation_call_id` link with a durable registration carrying `{task_id, generation, child_connection_id, child_conversation_id}` from resume/spawn through `TurnComplete`, disconnect/error, and cancel. Lifecycle must settle by run `task_id`, never by conversation root call id for continued runs.
- **Admission window:** register the new connection incarnation **before** prompt enqueue; buffer `TurnComplete`/disconnect/error/cancel while status is still `reserving`; only after promote_running process buffered events. Deterministic tests for each terminal source during the window.
- **External-id verify before SessionStarted persistence:** on mismatch, keep old conversation external_id, do not emit SessionStarted that rewrites identity, do not enqueue prompt, disconnect only the new incarnation, settle `unresumable`.
- Settlement only if `(task_id, generation, child_connection_id)` match; late gen-N event cannot settle gen N+1
- **Cold terminal resolution:** when live registration absent, resolve only a non-terminal run whose persisted `child_connection_id` matches; else no-op; never use conversation root call id
- Do not dedupe against still-retiring prior connection; new incarnation id
- Startup **before listener accepts requests**: reserving→failed/host_restarted with termination audit preserved; running→canceled/host_restarted with audit; zero non-terminal rows after gate; reserving inherits admission_class eligibility; running eligible for unexpected_continue
- `SessionAttachMode::ResumeExistingOnly` — no session/new; external id verify
- Re-key `attention.rs` open/reconcile to active run task_id (not root call id); test continued open/reply/close isolation
- Parser last well-formed summary; bounds; never in MCP report text
- Completion event carries optional validated summary field (extend Rust + TS types)

- [ ] **Step 1: Failing tests** for: handoff settles continued run by new task_id; admission-window buffering for each terminal source; external-id mismatch before SessionStarted; late old connection ignored; cold resolution match/no-op; reconcile status+audit split; resume_existing_only; prior-connection retirement race; attention re-key; summary last-match/bounds/non-exposure.

- [ ] **Step 2: Implement all of this task before any continue dispatch is merged to callers.**

- [ ] **Step 3: PASS + commit**
```powershell
git commit -m "feat(delegation): run-identity handoff, fence, reconcile, summary, resume_existing_only"
```

---

### Task 6: `continue_delegation` + replacement inputs (after Task 5)

**Files:**
- `tool_schema.json`, `listener.rs`, `companion.rs`, `transport.rs`, `meta_writer.rs`, `lifecycle.rs`, `broker.rs`, `spawner.rs` (existing-child path), `bin/codeg_mcp.rs`
- Tests: contract + continuability decision table

**Interfaces:**
- Async continue ack per design
- Typed errors + precedence tests (not_found → fingerprint handling → not_supported → busy → stale → not_continuable → budget_exhausted → unresumable → invalid_replacement)
- Continuability decision table covering completed/failed revision-eligible, host_restarted reserving inherit, canceled unexpected, **unknown-origin cancel → not_continuable**, policy reject, replacement class not on continue, superseded child, deleted-child ownership fail-closed, `run.agent_type == conversation.agent_type`
- Continue-path `duplicate_parent_tool`: matching fingerprint → idempotent return even if non-terminal (before busy/stale); mismatch/legacy-missing → reject; never from task_preview
- `delegate_to_agent` optional replaces_task_id, replacement_reason, work_unit_key
- **Bypass closure (Critical):** same work_unit_key with established lineage (`reached_running_at`) and **no** `replaces_task_id` → hard reject `invalid_replacement`
- Replacement server 7-check tests: ownership, agent, profile, workspace, terminal+latest, reason matches durable state, counter rows_affected; reason mismatch → invalid_replacement; second replacement → budget_exhausted
- Pre-admission gen-1 re-dispatch ignores never-running priors
- Pre-admission **replacement** retry: failed reserving replacement leaves counter 0; retry charges only at running
- Parent card correlation for continue without agent_type in tool input
- **Missing `_meta.tool_use_id`:** fail-closed — do not invent a card binding; return typed error (document exact code; test concurrent ambiguity)

- [ ] **Step 1: Failing contract + decision-table + bypass-closure + replacement 7-check + continue idempotency tests**

- [ ] **Step 2: Implement dispatch following design flow (fingerprint after target load, enqueue then promote)**

- [ ] **Step 3: PASS + commit**
```powershell
git commit -m "feat(mcp): continue_delegation and replacement lineage"
```

---

### Task 7: Frontend + snapshot DTO (desktop + web)

**Files:**
- Backend: `commands/delegation.rs` (or new command module), `web/handlers/*`, `web/router.rs`, register tauri command
- Frontend: `delegation-run-snapshot.ts`, `types.ts`, binding reduce, projection cache, content-parts-renderer, message-list-view, status card, overlay group components, tests
- i18n keys if new strings

**Interfaces:**
- `get_delegation_run_snapshot(task_id)` authorized for parent; returns immutable run fields + summary
- Events: DelegationCompleted includes optional summary
- Cards for both delegate_to_agent and continue_delegation tools
- Overlay groups by childConversationId with run count + latest state; replacement rows separate

- [ ] **Step 1: Failing tests** — independent cards same child; immutability; invalid summary fallback; overlay grouping; replacement marker; continue tool recognition; responsive desktop/mobile layout smoke; RTL/locale summary layout smoke (at least one RTL locale).

- [ ] **Step 2: Implement DTO both surfaces + UI**

- [ ] **Step 3: Targeted vitest + eslint PASS + commit**
```powershell
git commit -m "feat(ui): per-run cards, snapshot DTO, continue tool chrome"
```

---

### Task 8: Skill routing + recovery prompt semantics

**Files:** `.agents/skills/brainstorm-to-delivery/SKILL.md` (and package copy if mirrored)

- [ ] **Step 1: Update skill** with thread ledger, work_unit_key, continue_delegation preference, replacement rules, recovery prompt text (re-inspect repo, provisional prior reasoning, recreate undurabled reports, implementer re-audit FS + tests).

- [ ] **Step 2: Commit**
```powershell
git commit -m "docs(skill): continue_delegation routing for brainstorm-to-delivery"
```

---

### Task 9: Integration, fixtures, full verification

**Files:** e2e/contract tests under `src-tauri/tests/`

- [ ] **Step 1: Add tests**
  - Conversation 800 shape: 3 children, 12 runs
  - Conversation 832 shape: unexpected interrupt recovery → new run same child
  - Conversation 835 shape: replacement different child; original not_continuable; replacement continuable
  - Skill-forward isolation checklist (or fixture) covering all nine design scenarios: Design/Plan re-review continue same reviewer; Task fix continues Grok; Task re-review continues Codex; next Task fresh Grok+Codex; final whole-branch fresh Codex never reuses Task reviewer; resumability-failure replacement; interrupted final continues own session; business-error no substitution; skill budget caps
  - Concurrent double-continue one winner (loser `busy_thread`)
  - resume_existing_only no session/new + id mismatch unresumable
  - Migration collisions + preview redaction + summary non-exposure
  - Pre-admission re-dispatch + replacement retry
  - Budget races + no-refund after running
  - Desktop + web snapshot DTOs

- [ ] **Step 2: Full verification**
```powershell
cd src-tauri
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
cd ..
pnpm test
pnpm eslint .
pnpm build
```

- [ ] **Step 3: Commit**
```powershell
git commit -m "test(delegation): session reuse integration and verification"
```

---

## Spec Coverage Checklist

| Spec area | Task |
| --- | --- |
| Migration + backfill rules | 1 |
| Run store re-key + projection | 2 |
| task_preview redaction + fingerprint | 2 |
| Budget rails | 3 |
| Gen-1 live path + launch snapshot + secrets re-resolve | 4 |
| Capability gate API | 4 |
| Settlement fence + run-identity handoff + reconcile + summary + resume_existing_only | 5 |
| continue_delegation + replacement + error precedence | 6 |
| Pre-admission re-dispatch / replacement retry | 6 |
| Parent tool correlation / lifecycle for continue | 6 |
| Snapshot DTO desktop+web + UI immutability + overlay | 7 |
| Skill routing + recovery prompts | 8 |
| 800/832/835 + full matrix + clippy/build | 9 |

## Self-Review Notes

- Critical plan-review findings addressed: gen-1 live path (Task 4), fence/reconcile before continue (Task 5 before 6), spawner/lifecycle/transport correlation, task_preview + fingerprint on both tools, explicit dual-surface snapshot DTO.
- Tasks remain serial where safety requires (5 before 6); Task 8 skill can run after 6.
)
