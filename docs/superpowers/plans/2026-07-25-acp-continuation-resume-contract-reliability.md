# ACP Continuation Resume Contract Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `continue_delegation` accept standard ACP resume/load responses that omit `sessionId`, settle bootstrap refusals reliably under SQLite contention, and surface real terminal codes on cold task reports.

**Architecture:** Fix attach gating in `session_attach` so `ResumeExistingOnly` emits the request id when the agent omits/blank-returns an id (still refuse on explicit mismatch). Route all in-scope bootstrap refusals through one typed `settle_bootstrap_unresumable` that claims process-local terminal intent before durable CAS via `settle_with_retry`, uses a non-destructive terminal-intent resolver for status/replay/closed-handoff/parent-end, and generalizes the persistence retry worker to the original terminal payload. Share one cold-report message helper for store and legacy DB paths.

**Tech Stack:** Rust 2021, Tokio, SeaORM/SQLite (`RunStore` / `DbDelegationTaskStore`), existing ACP connection + delegation broker in `src-tauri/src/acp/`.

**Spec:** `docs/superpowers/specs/2026-07-25-acp-continuation-resume-contract-reliability-design.md`

## Global Constraints

- Do not change correlation, lineage, generation, replacement, ownership, or budget rules.
- Do not change default user-session `resume -> load -> new` fallback.
- `ResumeExistingOnly` must never call `session/new`.
- Do not add a DB column/migration for terminal error messages.
- Do not change serialized `DelegationTaskReport` wire shape (message text only).
- Do not probe `_meta.sessionId` in this change.
- Parent-facing messages never include raw SQL / credentials / agent response bodies.
- Work only in worktree `feat/acp-continuation-resume-contract-reliability`.
- After each task: focused `cargo test --features test-utils` for touched modules; final task runs full regression from the design.

## File Map

| File | Responsibility |
| --- | --- |
| `src-tauri/src/acp/session_attach.rs` | Attach decision matrix; missing/blank id emit under ResumeExistingOnly |
| `src-tauri/src/acp/connection.rs` | Resume/load attach call sites + contract tests (no-id ACP bodies) |
| `src-tauri/src/acp/delegation/types.rs` | Shared cold-report message helper |
| `src-tauri/src/acp/delegation/store.rs` | `PersistedTask::to_report` uses helper; optional frozen flag on retry record |
| `src-tauri/src/acp/delegation/broker.rs` | Bootstrap settle, claim, parent-end, resolver, worker, cold `db_report` |

---

### Task 1: Session attach identity matrix

**Files:**
- Modify: `src-tauri/src/acp/session_attach.rs`
- Test: same file `#[cfg(test)]`

**Interfaces:**
- Consumes: existing `SessionAttachMode`, `verify_external_session_id`, `ExternalIdVerifyResult`
- Produces: `gate_session_started_for_attach` returns `Emit { session_id: requested }` when ResumeExistingOnly + agent id omitted/blank; `RefuseUnresumable` only on present mismatch

- [ ] **Step 1: Write the failing tests (replace reverse expectations)**

In `session_attach.rs` tests, replace `gate_resume_existing_refuses_when_agent_omits_id` with:

```rust
#[test]
fn gate_resume_existing_emits_when_agent_omits_or_blanks_id() {
    assert_eq!(
        gate_session_started_for_attach(
            SessionAttachMode::ResumeExistingOnly,
            "sess-x",
            None,
        ),
        SessionStartedDecision::Emit {
            session_id: "sess-x".into(),
        }
    );
    assert_eq!(
        gate_session_started_for_attach(
            SessionAttachMode::ResumeExistingOnly,
            "sess-x",
            Some("   "),
        ),
        SessionStartedDecision::Emit {
            session_id: "sess-x".into(),
        }
    );
}

#[test]
fn gate_resume_existing_refuses_on_explicit_mismatch() {
    match gate_session_started_for_attach(
        SessionAttachMode::ResumeExistingOnly,
        "sess-old",
        Some("sess-new"),
    ) {
        SessionStartedDecision::RefuseUnresumable { reason } => {
            assert!(reason.contains("mismatch"));
        }
        other => panic!("expected refuse on mismatch, got {other:?}"),
    }
}
```

Keep `gate_default_always_emits_expected` and match-emit tests. Update or remove `decide_session_started_refuses_on_missing_actual` if `decide_session_started` is no longer the ResumeExistingOnly gate path for missing actuals.

- [ ] **Step 2: Run tests to verify failure**

```powershell
cd src-tauri
cargo test --features test-utils gate_resume_existing_emits_when_agent_omits -- --nocapture
```

Expected: FAIL (still refuses on omit).

- [ ] **Step 3: Implement gate matrix**

Change `gate_session_started_for_attach` to:

```rust
pub fn gate_session_started_for_attach(
    mode: SessionAttachMode,
    expected_external_id: &str,
    agent_returned_session_id: Option<&str>,
) -> SessionStartedDecision {
    if !mode.is_resume_existing_only() {
        return SessionStartedDecision::Emit {
            session_id: expected_external_id.trim().to_string(),
        };
    }
    let actual = agent_returned_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match actual {
        None => SessionStartedDecision::Emit {
            session_id: expected_external_id.trim().to_string(),
        },
        Some(actual) => match verify_external_session_id(expected_external_id, Some(actual)) {
            ExternalIdVerifyResult::Match => SessionStartedDecision::Emit {
                session_id: expected_external_id.trim().to_string(),
            },
            ExternalIdVerifyResult::Mismatch { expected, actual } => {
                SessionStartedDecision::RefuseUnresumable {
                    reason: format!(
                        "external session id mismatch: expected `{expected}`, got `{actual}`"
                    ),
                }
            }
            ExternalIdVerifyResult::MissingActual { .. } => SessionStartedDecision::Emit {
                session_id: expected_external_id.trim().to_string(),
            },
        },
    }
}
```

Update module docs to match. **Decision:** keep `decide_session_started` for match/mismatch unit tests if still referenced; change `gate_session_started_for_attach` to own the ResumeExistingOnly matrix (omit/blank → Emit). If `decide_session_started` becomes fully unreferenced, delete it and its tests rather than leave dead `pub` API. Do not duplicate mismatch tests (reuse existing `gate_resume_existing_refuses_on_mismatch`).

- [ ] **Step 4: Run tests to verify pass**

```powershell
cargo test --features test-utils session_attach -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/session_attach.rs
git commit -m "fix(acp): accept omitted sessionId on ResumeExistingOnly attach"
```

---

### Task 2: Connection contract tests (no-id resume/load)

**Files:**
- Modify: `src-tauri/src/acp/connection.rs` — extend test mock agent so it can answer `session/resume` and `session/load` (existing `SuspensionLoopMockAgent` only handles prompt/mode; add method counters for resume/load/new)
- Production path already uses `gate_session_started_for_attach` at connection.rs ~4133 and ~4329; change only if a second refuse path remains

**Interfaces:**
- Consumes: Task 1 gate behavior
- Produces: contract tests proving resume/load success with standard no-id body admits exactly one prompt; mismatch admits none; never `session/new`; `reused_session == Some(true)` after omit-id attach + prompt admission

- [ ] **Step 1: Extend mock agent for resume/load/new counting**

Add (or extend) a test double that:
- responds to `session/resume` / `session/load` with configurable bodies
- increments `resume_count`, `load_count`, `session_new_count`, `prompt_count`
- can return success without `sessionId` (Codex-like: modes/config only), success with mismatch id, or error

- [ ] **Step 2: Write failing contract tests**

1. Resume success **without** `sessionId` → SessionStarted requested id; `prompt_count == 1`; `session_new_count == 0`; `reused_session == Some(true)`.
2. Load success without id → same.
3. Resume error then load success without id → exactly one prompt.
4. Explicit different returned id → refuse, no prompt, no `session/new`; `reused_session` not true.
5. Resume **and** load both error under ResumeExistingOnly → no prompt, no `session/new`, unresumable settle path.
6. At least one body matching bundled Codex ACP response shape (modes/config, no sessionId).
7. Keep existing extract tests for camelCase / snake_case `sessionId` (or re-assert in Task 1).

- [ ] **Step 3: Run tests; fix any remaining refuse sites**

```powershell
cargo test --features test-utils -- resume_existing_accepts_standard -- --nocapture
```

- [ ] **Step 4: Commit**

```powershell
git add src-tauri/src/acp/connection.rs
git commit -m "test(acp): contract-cover no-id resume/load under ResumeExistingOnly"
```

---

### Task 3: Cold failure report helper

**Files:**
- Modify: `src-tauri/src/acp/delegation/types.rs` (add helper)
- Modify: `src-tauri/src/acp/delegation/store.rs` (`PersistedTask::to_report`)
- Modify: `src-tauri/src/acp/delegation/broker.rs` (`db_report`)
- Test: unit tests next to helper + existing store/broker tests

**Interfaces:**
- Produces:

```rust
/// Shared cold-report message selection (wire shape unchanged).
pub fn cold_task_report_message(
    status: TaskStatus,
    error_code: Option<&str>,
    child_conversation_id: i32,
) -> Option<String>
```

Rules:
- Running → `"Running."`
- Completed → cache-miss guidance with child session id
- Failed → `Delegation failed ({code}): … Open child session {id} for details.` where code is `error_code` or `"unknown"`
- Canceled → cancellation phrasing; if code missing synthesize field elsewhere as today (`"canceled"`) and include it in message
- Unknown → existing unknown-task text
- Never include raw SQL

- [ ] **Step 1: Write failing unit tests for the helper**

```rust
#[test]
fn cold_message_failed_includes_error_code_not_cache_miss() {
    let msg = cold_task_report_message(TaskStatus::Failed, Some("unresumable"), 1693).unwrap();
    assert!(msg.contains("unresumable"));
    assert!(msg.contains("1693"));
    assert!(!msg.contains("Result no longer cached"));
}

#[test]
fn cold_message_failed_non_unresumable_uses_generic_phrase() {
    let msg = cold_task_report_message(TaskStatus::Failed, Some("host_restarted"), 9).unwrap();
    assert!(msg.contains("host_restarted"));
    assert!(!msg.contains("could not be resumed safely"));
    assert!(!msg.contains("Result no longer cached"));
}

#[test]
fn cold_message_completed_keeps_cache_miss() {
    let msg = cold_task_report_message(TaskStatus::Completed, None, 7).unwrap();
    assert!(msg.contains("Result no longer cached"));
}
```

- [ ] **Step 2: Implement helper and wire `to_report` + `db_report`**

```rust
// types.rs
pub fn cold_task_report_message(
    status: TaskStatus,
    error_code: Option<&str>,
    child_conversation_id: i32,
) -> Option<String> {
    match status {
        TaskStatus::Running => Some("Running.".into()),
        TaskStatus::Completed => Some(format!(
            "Result no longer cached; open child session {} for the full output.",
            child_conversation_id
        )),
        TaskStatus::Failed => {
            let code = error_code.unwrap_or("unknown");
            let detail = match code {
                "unresumable" => {
                    "the existing agent session could not be resumed safely"
                }
                _ => "see child session for details",
            };
            Some(format!(
                "Delegation failed ({code}): {detail}. Open child session {child_conversation_id} for details."
            ))
        }
        TaskStatus::Canceled => {
            let code = error_code.unwrap_or("canceled");
            Some(format!(
                "Delegation canceled ({code}). Open child session {child_conversation_id} for details."
            ))
        }
        TaskStatus::Unknown => Some(
            "Unknown task id — it never existed, isn't owned by this session, \
             or its result was evicted with no stored record."
                .into(),
        ),
    }
}
```

In `PersistedTask::to_report`, set `message` via the helper for all status branches (Running/Completed/Failed/Canceled/Unknown). Preserve any existing `text: result_text` field semantics unchanged — the helper replaces only the **message** selection, not the optional full-output `text` override. In `db_report`, keep synthesizing `error_code: Some("canceled")` for missing cancel codes if that is current behavior, then pass into the helper.

- [ ] **Step 3: Run focused tests**

```powershell
cargo test --features test-utils cold_task_report_message -- --nocapture
cargo test --features test-utils to_report -- --nocapture
```

- [ ] **Step 4: Commit**

```powershell
git add src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/broker.rs
git commit -m "fix(delegation): surface stable error_code in cold task reports"
```

---

### Task 4a: Bootstrap settle — typed helper, claim-first, settle_with_retry, call-site routing

**Files:**
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/connection.rs` — sole production caller of helper today: `refuse_unresumable_bootstrap` (~2939); honor typed result (`let result = …`; never re-settle; log Existing winner code if useful)
- Modify: `src-tauri/src/acp/delegation/store.rs` — add `frozen: bool` on `PendingTerminalRetry` (default false); `put_retry` returns `bool` (true if inserted, false if existing/frozen — refuses re-own when frozen); add `fn freeze_retry(&self, task_id: &str)` on `DelegationTaskStore` and implement for `DbDelegationTaskStore`, `NoopTaskStore`, and all test mocks. After freeze, `has_retry_record` may still return true but spawn/worker must skip settle when `get_retry().frozen`

**Interfaces:**
- Produces `pub(crate)` (not MCP wire):

```rust
enum BootstrapSettleResult {
    Won,
    Existing { error_code: Option<String> },
    PendingCompensation,
    PermanentPersistenceError,
}
```

- **New helper entry shape** (connection may be absent):

```rust
pub async fn settle_bootstrap_unresumable(
    &self,
    // Prefer explicit task_id when known (pre-spawn / spawn failure).
    task_id: &str,
    // Optional; used only to unregister live incarnation when present.
    child_connection_id: Option<&str>,
    message: impl Into<String>,
) -> BootstrapSettleResult
```

Grep-confirm and route these production paths (line numbers drift — search symbols):
1. `refuse_unresumable_bootstrap` in `connection.rs` — already the only direct helper caller; update to new signature + honor result.
2. `spawn_resume_existing` failure that today calls `runs.settle_terminal` with unresumable — pass `reserved.task_id` + intended child id; delete direct settle.
3. Pre-spawn missing durable external id path that today settles inline — pass `reserved.task_id`; delete direct settle.

- **Call-site honor rule:** on `Existing` / `PendingCompensation` / `PermanentPersistenceError`, callers must **not** call `runs.settle_terminal` again. **Response mapping:** bootstrap-first / pending / permanent → parent-facing business code **`unresumable`** (stable sanitized message mapper — never `e.to_string()` of spawn/DB/ACP raw text). Parent-end-first `Existing` → returned report follows the **durable/parent winner** (not forced unresumable). Assert both race directions.
- **Helper owns overlay replace on `Existing`:** under lock, replace overlay+disposition with durable winner before returning; return carries only `error_code` for caller telemetry.
- **Stable sanitization:** add a small mapper `fn bootstrap_refuse_message(kind, stable_code) -> String` used by all refusal paths; tests assert raw ACP/SQLite fragments never appear.
- **Exactly-once accounting owner:** `finalize_durable_settlement` is the single metric/audit site for durable `Won` (helper-direct, parent-end, or worker). Worker `Existing` and second observers must not count. Permanent no-winner path counts logical `persistence_error` once inside the permanent branch (not worker).

- [ ] **Step 1: Write failing tests**

1. 1–3 forced `TaskStoreError::Transient` then durable `failed/unresumable` via helper.
2. Claim-before-first-attempt + parent-end cannot invent `parent_canceled` when bootstrap claimed first.
3. Parent-end-first: bootstrap claim insert no-op → **zero** bootstrap CAS attempts.
4. Delayed parent-end durable commit: bootstrap still does not invert claim order.
5. Permanent error: sanitized message; freeze; no spin.
6. Caller inject Existing: zero additional `settle_terminal` from caller after helper returns.
7. **Per-refusal-kind continue_delegation integration** (not helper-only):
   - pre-spawn missing durable external id
   - resume/load RPC failure under ResumeExistingOnly
   - explicit returned-id mismatch  
   Each: exactly one bootstrap helper path, zero direct caller `settle_terminal`, prompt count 0, no `session/new`, parent message sanitized.
8. Parent-end-first Existing: continue report follows durable/parent winner (not forced unresumable).
9. Accounting: helper-direct Won counts once; worker Existing does not double-count; permanent no-winner counts persistence_error once.

(Disconnect §4.10 test lives in Task 4b — depends on resolver.)

- [ ] **Step 2: Run — expect FAIL**

```powershell
cargo test --features test-utils settle_bootstrap -- --nocapture
```

- [ ] **Step 3: Implement helper + routing**

```rust
// settle_bootstrap_unresumable(task_id, child_connection_id: Option, message) -> BootstrapSettleResult
// 1) task_id is required input (callers pass reserved.task_id)
// 2) claim under pending.inner: Entry::Vacant insert ChildTerminal; Occupied → return Existing, no CAS
// 3) on win: overlay insert + status_version/notify
// 4) put_retry returns bool; if false (lost/frozen), re-peek disposition / durable and return Existing — do not spawn worker
// 5) settle_with_retry (never runs.settle_terminal)
// 6) Won: finalize_durable_settlement (single audit site); Existing: helper replaces overlay+disposition under lock then return
// 7) PendingCompensation: keep intent; spawn worker only if put_retry won; unregister live only
// 8) Permanent: same-owner replace to persistence_error; freeze_retry; count persistence_error once; sanitized msg
```

Route pre-spawn missing-id and `spawn_resume_existing` failure through the helper; delete their direct `settle_terminal` settles. Replace any `e.to_string()` parent-facing paths with the stable sanitizer.

- [ ] **Step 4: Run tests; commit**

```powershell
cargo test --features test-utils -- settle_bootstrap -- --nocapture
git add src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/connection.rs
git commit -m "fix(delegation): claim-first bootstrap settle with typed result"
```

---

### Task 4b: Non-destructive terminal-intent resolver + parent-end honor

**Files:**
- Modify: `src-tauri/src/acp/delegation/broker.rs` only

**Interfaces:**
- Produces **one** shared function used by all read paths:

```rust
/// Non-destructive. Caller must already have enforced parent ownership.
/// Clear only after durable Won/Existing finalization via clear_closed_handoff_disposition
/// (keep clear_*; remove only take_* while durable non-terminal).
fn resolve_terminal_intent(
    &self,
    task_id: &str,
) -> Option<TerminalIntent> // disposition +/or completed overlay report
```

**Authorization:** status/batch paths must enforce parent ownership **before** calling the resolver (same as today’s in-memory/DB ownership checks). Resolver itself is not a cross-parent oracle. Add a cross-parent pending-compensation status test that returns unknown/unauthorized, not the other parent’s intent.

Call sites (must not ad-hoc peek):
1. `get_task_status` / `status_from_db` (after ownership check)
2. batch `assemble_reports` / `get_tasks_status` (after ownership check)
3. `continue_closed_handoff_report` — use peek/resolver; **remove** destructive `take_closed_handoff_disposition` while durable non-terminal; keep `clear_closed_handoff_disposition` for post-durable cleanup
4. continue fingerprint / idempotent Reserving arm — return claimed terminal, not “still admitting”
5. `take_reserving_handoffs_for_parent_end` — peek existing ChildTerminal first; insert-if-absent only; never invent ParentEnded over child claim; when finalizing already-claimed ChildTerminal durable win, parent-end is finalization owner (clear intent + exactly-once audit via `finalize_durable_settlement`)

- [ ] **Step 1: Write failing tests**

1. Status + batch during pending compensation → `unresumable`.
2. Repeated closed-handoff reads during retry do not consume park or degrade to `parent_canceled`.
3. Fingerprint replay during retry reports claimed terminal, not running ack.
4. Parent-end peeks bootstrap claim (insert-if-absent).
5. After divergent Existing, repeated status shows durable winner (atomic overlay+disposition replace).
6. Disconnect during pending compensation: remains `unresumable`, never relabeled canceled / `parent_canceled` (spec §4.10).
7. Cross-parent status while pending compensation: no leak of other parent’s intent.

- [ ] **Step 2: Implement resolver + parent-end; run tests; commit**

```powershell
cargo test --features test-utils -- closed_handoff -- --nocapture
cargo test --features test-utils -- parent_end -- --nocapture
git add src-tauri/src/acp/delegation/broker.rs
git commit -m "fix(delegation): non-destructive terminal-intent resolver"
```

---

### Task 5: Persistence worker payload-driven accounting + broker cleanup handle

**Files:**
- Modify: `src-tauri/src/acp/delegation/broker.rs` (`spawn_persistence_retry_worker`)
- Modify: `src-tauri/src/acp/delegation/store.rs` (`freeze_retry` / frozen field from Task 4a)

**Interfaces:**
- Consumes: `PendingTerminalRetry.terminal: TerminalTaskWrite` (public `status` / `error_code` fields already on write type)
- Worker must apply §3.2 overlay/disposition cleanup on `Won` and `Existing`.
- **Concrete cleanup mechanism:** capture `Arc` clones the worker needs — at minimum `task_store`, `metrics`, `persistence_retry_inflight`, and a **`Weak`/`Arc` handle to broker pending state** (or a small `TerminalIntentFinalizer` trait object owned by the broker) so the worker can call the same finalization used on helper `Won`/`Existing` (clear park, replace overlay on Existing, notify waiters). Do not hand-wave “channel if available”; implement `fn finalize_durable_settlement(&self, task_id, settlement)` on the broker and call it from the worker via `Arc<BrokerFinalizer>` / interior mutability already used by the broker.

- [ ] **Step 1: Write failing tests**

1. Bootstrap compensation worker eventually persists `unresumable` (not rewritten to `persistence_error`).
2. Worker `Won`/`Existing` clears park/overlay (no leak of `closed_handoff_dispositions`).
3. Event/metric counts: refuse baseline + exactly one durable terminal accounting (`Existing` does not double-count).
4. Permanent freeze: `freeze_retry`; second spawn does not spin; `put_retry` refuses re-own.

- [ ] **Step 2: Implement worker**

```rust
// get_retry: if record.frozen { clear inflight; break } — no settle attempt
// Before settle: copy status + error_code from retry.terminal
// On Won: finalize_durable_settlement (single audit site with copied codes); remove_retry; clear inflight
// On Existing: finalize_durable_settlement (no second metric); remove_retry; clear inflight
// On permanent: freeze_retry; clear inflight; no spin
// On transient: continue loop
// spawn_persistence_retry_worker: refuse to insert inflight / start loop if get_retry is frozen
```

- [ ] **Step 3: Run tests; commit**

```powershell
cargo test --features test-utils -- persistence_retry -- --nocapture
git add src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/store.rs
git commit -m "fix(delegation): payload-driven persistence retry worker accounting"
```

---

### Task 6: Integration regression + full backend check

**Files:**
- Test-only additions if gaps remain from Tasks 1–5
- No frontend source expected

**Interfaces:**
- Consumes: all prior tasks
- Produces: green design regression suite

- [ ] **Step 1: Run focused integration cases**

Ensure coverage for:
- Codex-shaped no-id resume/load
- Bootstrap-first and parent-end-first races
- Park lost on restart → `host_restarted` (not retroactive `unresumable`)
- Companion still sets `isError: true` for failed reports (existing companion tests)

```powershell
cd src-tauri
cargo test --features test-utils session_attach -- --nocapture
cargo test --features test-utils -- settle_bootstrap -- --nocapture
cargo test --features test-utils -- cold_task_report -- --nocapture
```

- [ ] **Step 2: Full regression (design acceptance)**

```powershell
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all green. Fix any clippy dead-code from Task 1.

- [ ] **Step 3: Commit any remaining test fixes**

```powershell
git add -A src-tauri
git commit -m "test(acp): complete continuation resume reliability regression coverage"
```

---

## Spec coverage checklist

| Spec section | Task |
| --- | --- |
| §1 ACP session identity matrix | Task 1–2 |
| `reused_session` after attach+prompt | Task 2 |
| §2 single-owner helper + typed result + caller honor + continue_delegation integration per refusal | Task 4a |
| §3.1 claim-before-CAS + lost-claim no-CAS | Task 4a |
| §3.1 parent-end honor + finalization owner | Task 4b |
| §3.2 Won/Existing/transient/permanent sequence | Task 4a–5 |
| §3.3 non-destructive resolver (single fn) | Task 4b |
| §3.4 permanent freeze + sanitized messages | Task 4a–5 |
| §3.5 payload-driven worker + finalize on Won/Existing | Task 5 |
| §4.10 disconnect cannot relabel unresumable | Task 4a |
| §5 cold reports | Task 3 |
| Historical `host_restarted` non-goal | Task 6 |
| Full cargo matrix | Task 6 |
| Companion message text snapshots if broken by Task 3 | Task 6 |

## Placeholder scan

No TBD/TODO placeholders. Commands and primary code shapes are concrete; implementers must adapt only to local helper names already present in `broker.rs` test modules.

## Type consistency

- `Settlement::{Won,Existing}` already in `store.rs`
- `BootstrapSettleResult` is new broker-local (or pub(crate)) — do not change MCP wire types
- `TerminalTaskWrite` remains the durable write payload
- `DelegationTaskReport.error_code` field semantics unchanged

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-25-acp-continuation-resume-contract-reliability.md`.

Per brainstorm-to-delivery, this plan must be reviewed by the full document review group before SDD execution:
- CodeBuddy:GLM5.2
- CodeBuddy:KimiK3
- Codex CLI

Then workspace gate + `subagent-driven-development` (Grok implement / Codex task review).
