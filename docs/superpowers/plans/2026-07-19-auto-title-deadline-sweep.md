# Auto-Title Deadline Sweep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote stuck first-turn auto-title jobs after 300s using the first user task plus current partial assistant text (possibly empty), without refining after a successful generate, via a 101s coordinator sweep.

**Architecture:** A new migration adds write-once `first_prompt_at`. Capture uses conditional DB updates. Usable completion and deadline promote race via `first_assistant_text IS NULL` CAS. A pure `visible_assistant_text` helper is shared by TurnComplete and the sweep. `AutoTitleCoordinator` runs a fault-tolerant 101s sweep (immediate first pass) that batch-snapshots live partials, promotes eligible `awaiting_turn` rows to `ready`, and re-notifies so lost wakes self-heal. Claim allows `Some("")` but rejects Ready + `None`.

**Tech Stack:** Rust 2021, SeaORM/SQLite, Tokio, `ConnectionManager` / `SessionState`, existing `HiddenAgentRunner`, Vitest not required (backend-only).

**Spec:** `docs/superpowers/specs/2026-07-19-auto-title-deadline-sweep-design.md`

## Global Constraints

- Deadline: **300s** from `first_prompt_at` (`AUTO_TITLE_DEADLINE`)
- Sweep interval: **101s** (`AUTO_TITLE_DEADLINE_SWEEP_INTERVAL`)
- Batch size: **64** candidates per pass (`AUTO_TITLE_DEADLINE_SWEEP_BATCH`)
- No settings UI; constants only (injectable in tests)
- No post-success refine; successful finalize is final
- `retry_wait` is never deadline-promoted
- Pre-migration rows with existing `first_user_text` and NULL `first_prompt_at`: end-turn only
- New forward migration only — do **not** edit `m20260716_000001_auto_title`
- Retain claim index `(state, updated_at, conversation_id)`; **add** deadline index `(state, first_prompt_at, conversation_id)`
- Empty assistant snapshot is `Some("")`; Ready + `None` is invalid
- Capture first fields only when both `first_user_text` and `first_prompt_at` are NULL
- Partial assembly must match TurnComplete; never use `latest_live_reply` for titles
- Desktop + server (`--no-default-features`) must both build
- Every behavior starts with a focused failing test (TDD)
- Frontend: no new API or settings
- **Concurrency tests are mandatory:** pooled WAL (or multi-connection on the same
  file DB) with barriers for capture races, deadline-vs-end-turn both orders,
  completion-vs-claim, and select-then-delete/Off. Sequential “pre-seed winner”
  tests are **not** substitutes for those cases.
- Usable-turn progress must use **atomic** `usable_turn_seq` advance (SQL
  `+ 1` or reload/retry on CAS miss), never a stale in-memory `current_seq + 1`
  without a sequence guard.

## Review History

- 2026-07-19: Initial plan committed (`54509cba`).
- 2026-07-19: Codex plan review (`fd756a51-ec24-48c4-b4c7-652fef839aed`) — seven
  Important findings incorporated below (atomic completion, claim retry, real
  concurrency tests, coordinator liveness hooks, correct state Arc collection,
  migration TDD discoverability, TurnComplete `live_message = None` case).

## File Map

| File | Responsibility |
| --- | --- |
| `src-tauri/src/db/migration/m20260719_000001_auto_title_first_prompt_at.rs` | New column + deadline index + down |
| `src-tauri/src/db/migration/mod.rs` | Register migration last |
| `src-tauri/src/db/entities/auto_title_job.rs` | `first_prompt_at: Option<DateTimeUtc>` |
| `src-tauri/src/acp/session_state.rs` | `visible_assistant_text`; TurnComplete uses it; clear-on-empty |
| `src-tauri/src/auto_title/service.rs` | Capture CAS; promote; claim rules; completion CAS |
| `src-tauri/src/auto_title/partial_source.rs` (new) | `PartialAssistantTextSource` trait + manager impl |
| `src-tauri/src/auto_title/mod.rs` | Export new items |
| `src-tauri/src/acp/manager.rs` | Batch partial snapshot helper (or used by trait impl) |
| `src-tauri/src/auto_title/coordinator.rs` | Sweep loop, error isolation, ready re-notify, inject source/constants |
| `src-tauri/src/auto_title/types.rs` | Only if claim/promote types need fields |
| Tests in the modules above + optional `src-tauri/tests/api_integration.rs` | Coverage |

---

### Task 1: Schema — `first_prompt_at` + deadline index

**Files:**
- Create: `src-tauri/src/db/migration/m20260719_000001_auto_title_first_prompt_at.rs`
- Modify: `src-tauri/src/db/migration/mod.rs` (declare module + register **last** in Step 1 skeleton)
- Modify: `src-tauri/src/db/entities/auto_title_job.rs`
- Modify: `src-tauri/src/auto_title/service.rs` — `enroll_new_conversation` ActiveModel must set `first_prompt_at: Set(None)` as soon as the entity field exists
- Touch **every** exhaustive `auto_title_job::ActiveModel { ... }` under `src-tauri/` (grep `first_user_text: Set`)

**Interfaces:**
- Produces: `auto_title_job::Model.first_prompt_at: Option<DateTimeUtc>`
- Produces: index name `idx_auto_title_jobs_deadline` on `(state, first_prompt_at, conversation_id)` in that column order
- Preserves: `idx_auto_title_jobs_queue` on `(state, updated_at, conversation_id)`

- [ ] **Step 1: Register a compilable skeleton + write failing assertions**

In **the same step** (so cargo discovers the test):

1. Add `mod m20260719_000001_auto_title_first_prompt_at;` and
   `Box::new(m20260719_000001_auto_title_first_prompt_at::Migration)` as the
   **last** entry in `Migrator::migrations()`.
2. Skeleton `Migration` with `up`/`down` that currently **no-op** `Ok(())` so the
   crate compiles.
3. Entity field `first_prompt_at: Option<DateTimeUtc>` + fix
   `enroll_new_conversation` and all ActiveModel seeds with `Set(None)`.
4. Migration unit test that **fails** until real up/down land:

```rust
#[tokio::test]
async fn up_adds_first_prompt_at_and_deadline_index() {
    // Build minimal conversation + auto_title_jobs + existing queue index
    // (same SQL as previously listed in plan history).
    // Seed row with first_user_text = 'old task', first_prompt_at absent.
    Migration.up(&SchemaManager::new(&conn)).await.unwrap();

    // PRAGMA table_info: first_prompt_at present; legacy row NULL.
    // PRAGMA index_list: both idx_auto_title_jobs_queue and
    // idx_auto_title_jobs_deadline present.
    // PRAGMA index_info(idx_auto_title_jobs_deadline): columns in order
    // state, first_prompt_at, conversation_id.
    Migration.down(...).await.unwrap();
    // column gone; queue index still present.
}

#[tokio::test]
async fn migrator_registers_deadline_migration_last() {
    let migrations = Migrator::migrations();
    let last = migrations.last().expect("non-empty");
    assert_eq!(last.name(), "m20260719_000001_auto_title_first_prompt_at");
}

#[tokio::test]
async fn legacy_captured_prompt_keeps_null_first_prompt_at_after_upgrade() {
    // After up: row with pre-existing first_user_text has first_prompt_at NULL.
    // Document: later capture path (Task 3) must NOT backfill when first_user
    // already set — assert here only the migration NULL default.
}
```

- [ ] **Step 2: Run tests — expect FAIL on assertions (not “0 tests”)**

```powershell
cargo test --features test-utils m20260719_000001_auto_title_first_prompt_at -- --nocapture
```

Expected: tests **run** and FAIL (column/index missing), not filter with zero tests.

- [ ] **Step 3: Implement real migration up/down**

```rust
// m20260719_000001_auto_title_first_prompt_at.rs — core of up/down
const IDX_AUTO_TITLE_JOBS_DEADLINE: &str = "idx_auto_title_jobs_deadline";

async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(AutoTitleJobs::Table)
                .add_column(
                    ColumnDef::new(AutoTitleJobs::FirstPromptAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name(IDX_AUTO_TITLE_JOBS_DEADLINE)
                .table(AutoTitleJobs::Table)
                .col(AutoTitleJobs::State)
                .col(AutoTitleJobs::FirstPromptAt)
                .col(AutoTitleJobs::ConversationId)
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .drop_index(
            Index::drop()
                .name(IDX_AUTO_TITLE_JOBS_DEADLINE)
                .table(AutoTitleJobs::Table)
                .to_owned(),
        )
        .await?;
    manager
        .alter_table(
            Table::alter()
                .table(AutoTitleJobs::Table)
                .drop_column(AutoTitleJobs::FirstPromptAt)
                .to_owned(),
        )
        .await?;
    Ok(())
}
```

Entity:

```rust
// auto_title_job.rs Model
pub first_prompt_at: Option<DateTimeUtc>,
```

Register as the **last** migration in `mod.rs`.

Update exhaustive ActiveModel seeds: `first_prompt_at: Set(None)` (or `Some(now)` where tests need deadline eligibility).

- [ ] **Step 4: Run tests to verify pass**

```powershell
cargo test --features test-utils m20260719_000001_auto_title_first_prompt_at
cargo test --features test-utils auto_title::service
```

Expected: PASS (service may need only field init fixes).

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/db/migration/m20260719_000001_auto_title_first_prompt_at.rs `
  src-tauri/src/db/migration/mod.rs `
  src-tauri/src/db/entities/auto_title_job.rs
# plus any test seed fixes
git commit -m "feat(db): add auto_title_jobs.first_prompt_at for deadline sweep"
```

---

### Task 2: Pure `visible_assistant_text` + TurnComplete parity

**Files:**
- Modify: `src-tauri/src/acp/session_state.rs` (helper + TurnComplete path ~812–863)
- Prefer public crate-visible `pub fn visible_assistant_text(live: Option<&LiveMessage>) -> String` in `session_state.rs` so auto_title / manager can call it

**Interfaces:**
- Produces: `visible_assistant_text(live: Option<&LiveMessage>) -> String`
- Consumes: `LiveMessage`, `LiveContentBlock`
- Must: no truncation; no thinking/tool fallback; empty ⇒ `""`; TurnComplete clears `last_assistant_text` when trim-empty

- [ ] **Step 1: Write failing unit tests**

```rust
#[test]
fn visible_assistant_text_uses_text_after_last_tool_only() {
    let live = LiveMessage {
        id: "m".into(),
        content: vec![
            LiveContentBlock::Text { text: "before ".into() },
            LiveContentBlock::ToolCallRef { tool_call_id: "t1".into() },
            LiveContentBlock::Thinking { text: "noise".into() },
            LiveContentBlock::Text { text: "answer".into() },
        ],
        started_at: Utc::now(),
        // fill remaining LiveMessage fields as the struct requires
        ..Default::default() // only if Default exists; else set all fields
    };
    assert_eq!(visible_assistant_text(Some(&live)), "answer");
}

#[test]
fn visible_assistant_text_none_and_thinking_only_are_empty() {
    assert_eq!(visible_assistant_text(None), "");
    let live = LiveMessage {
        id: "m".into(),
        content: vec![LiveContentBlock::Thinking { text: "…".into() }],
        started_at: Utc::now(),
        ..
    };
    assert_eq!(visible_assistant_text(Some(&live)), "");
}

#[tokio::test]
async fn turn_complete_clears_stale_when_live_message_is_none() {
    // Current code leaves stale last_assistant_text when live_message is None
    // (no re-assembly). This is the failing case that forces the helper path.
    // Setup: last_assistant_text = Some("stale"), live_message = None.
    // apply TurnComplete; assert last_assistant_text is None.
}

#[tokio::test]
async fn turn_complete_matches_visible_assistant_text_helper() {
    // Same LiveMessage content fed to visible_assistant_text and to a
    // SessionState that only has that live_message; after TurnComplete,
    // last_assistant_text.as_deref().unwrap_or("") equals the helper output
    // (trim-empty ⇒ both empty/None).
}
```

If `LiveMessage` has no `Default`, copy field layout from existing tests around line 2713 in `session_state.rs`.

- [ ] **Step 2: Run tests — expect FAIL**

```powershell
cargo test --features test-utils visible_assistant_text -- --nocapture
```

- [ ] **Step 3: Implement helper and refactor TurnComplete**

```rust
pub fn visible_assistant_text(live: Option<&LiveMessage>) -> String {
    let Some(live) = live else {
        return String::new();
    };
    let after_last_tool_call = live
        .content
        .iter()
        .rposition(|b| matches!(b, LiveContentBlock::ToolCallRef { .. }))
        .map(|i| i + 1)
        .unwrap_or(0);
    live.content[after_last_tool_call..]
        .iter()
        .filter_map(|b| match b {
            LiveContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// In TurnComplete arm:
let assembled = visible_assistant_text(self.live_message.as_ref());
self.last_assistant_text = if assembled.trim().is_empty() {
    None
} else {
    Some(assembled)
};
// completion snapshot uses last_assistant_text / empty Arc as today
```

- [ ] **Step 4: Run tests — expect PASS**

```powershell
cargo test --features test-utils session_state
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/session_state.rs
git commit -m "refactor(acp): share visible_assistant_text for title partials"
```

---

### Task 3: Conditional first-prompt capture

**Files:**
- Modify: `src-tauri/src/auto_title/service.rs` — `capture_prompt_context`
- Modify tests that assert first_user write-once

**Interfaces:**
- Consumes: job row, visible text, locale
- Produces: write-once `first_user_text` + `first_prompt_at` when both NULL; locale may always refresh

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn capture_sets_first_user_and_first_prompt_at_once() {
    // enroll job, capture with visible "task A"
    // assert first_user_text == Some("task A") and first_prompt_at.is_some()
    // capture again with "task B"
    // assert still "task A" and same first_prompt_at
}

#[tokio::test]
async fn concurrent_captures_only_one_writes_first_fields() {
    // REQUIRED: two Database connections on one WAL temp file (not sequential
    // simulation). Barrier both threads immediately before the first-fields
    // UPDATE. Exactly one writer sets first_user_text + first_prompt_at;
    // the other only refreshes locale. first_user_text must equal the winner's
    // visible text; first_prompt_at set once.
}
```

- [ ] **Step 2: Run — expect FAIL** (no `first_prompt_at` write / no CAS)

- [ ] **Step 3: Implement conditional capture**

Replace read-modify-write first-user assignment with:

```rust
// After computing visible_text + locale:
let now = Utc::now();
let first_write = auto_title_job::Entity::update_many()
    .col_expr(auto_title_job::Column::FirstUserText, Expr::value(visible_text.clone()))
    .col_expr(auto_title_job::Column::FirstPromptAt, Expr::value(now))
    .col_expr(
        auto_title_job::Column::Locale,
        Expr::value(app_locale_to_wire(locale).to_string()),
    )
    .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
    .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
    .filter(auto_title_job::Column::FirstUserText.is_null())
    .filter(auto_title_job::Column::FirstPromptAt.is_null())
    .exec(conn)
    .await?;

if first_write.rows_affected == 0 {
    // Job may exist with first fields set: refresh locale only.
    auto_title_job::Entity::update_many()
        .col_expr(
            auto_title_job::Column::Locale,
            Expr::value(app_locale_to_wire(locale).to_string()),
        )
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
        .exec(conn)
        .await?;
}
// If no job row, both updates affect 0 rows — fine.
```

Remove the old `find_by_id` + `ActiveModel` path for first-user writes (or keep find only if needed for other logic — prefer pure update_many).

- [ ] **Step 4: Run service capture tests — PASS**

```powershell
cargo test --features test-utils capture_prompt -- --nocapture
cargo test --features test-utils first_user_text
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/service.rs
git commit -m "fix(auto-title): conditional first_prompt_at capture"
```

---

### Task 4: Claim allows `Some("")`, rejects Ready `None`

**Files:**
- Modify: `src-tauri/src/auto_title/service.rs` — `claim_next_ready` (~245–253)

**Interfaces:**
- Claim requires non-empty trimmed `first_user_text`
- `first_assistant_text == Some("")` → claim
- `first_assistant_text == None` → delete Ready row, continue
- Do not use `unwrap_or_default()` for assistant

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn claim_accepts_empty_assistant_some_empty_string() { /* Ready + Some("") → Some(claim) */ }

#[tokio::test]
async fn claim_deletes_ready_with_none_assistant() { /* Ready + None → job deleted, no claim */ }

#[tokio::test]
async fn claim_still_deletes_empty_user() { /* unchanged */ }

#[tokio::test]
async fn claim_retries_after_usable_turn_seq_changes_between_read_and_cas() {
    // REQUIRED barrier: thread A reads Ready candidate with seq=1;
    // thread B applies usable completion advancing seq to 2 (or updates seq);
    // A then CAS with stale seq must not hang and must not return
    // attempt_turn_seq mismatched from the row actually claimed.
    // Preferred implementation under test: lost CAS → rollback txn, loop with
    // a fresh begin(); OR atomic update-returning that sets attempt_turn_seq
    // from the row's current usable_turn_seq in one statement.
}
```

- [ ] **Step 2: Run — expect FAIL** (current code deletes empty assistant; no seq race handling)

- [ ] **Step 3: Implement**

```rust
// Empty-user / None-assistant delete rules as in the table above.

// Claim loop MUST not keep a single long-lived transaction that reuses a
// stale candidate after a lost CAS. Required pattern:

loop {
    let txn = conn.begin().await?;
    let job = /* select oldest Ready under txn */;
    let Some(job) = job else { txn.commit().await?; return Ok(None); };

    // validate user/assistant; delete bad rows inside txn; commit; continue

    // Option A (preferred if SeaORM allows): single UPDATE … RETURNING that
    // sets state=running, attempts=attempts+1, attempt_turn_seq=usable_turn_seq
    // WHERE state=ready AND conversation_id=?  (no stale seq filter needed
    // if attempt_turn_seq is taken from the same row version being updated).

    // Option B: UPDATE with filters state=ready AND usable_turn_seq = observed
    // If rows_affected == 0: txn.rollback().await?; continue; // fresh txn

    // On success: commit; return AutoTitleClaim { attempt_turn_seq: claimed_seq, ... }
}
```

Do **not** leave a zero-row CAS inside one open transaction that then selects
the next candidate with a dirty snapshot without documenting SQLite isolation
behavior — always rollback/re-begin after a lost claim CAS.

- [ ] **Step 4: Run claim tests — PASS**

```powershell
cargo test --features test-utils claim_ -- --nocapture
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/service.rs
git commit -m "fix(auto-title): claim empty-assistant and safe claim CAS retry"
```

---

### Task 5: Predicate-safe `apply_usable_completion`

**Files:**
- Modify: `src-tauri/src/auto_title/service.rs` — `apply_usable_completion`

**Interfaces:**
- Always advance token/seq/locale for new usable turns when job exists
- First-assistant write only when `first_assistant_text IS NULL` and state is `awaiting_turn` or needs first snapshot
- `retry_wait → ready` without overwriting `first_assistant_text`
- After deadline `Some("")`, end-turn must not replace assistant text

- [ ] **Step 1: Write failing race / semantics tests**

```rust
#[tokio::test]
async fn end_turn_does_not_overwrite_deadline_assistant_snapshot() {
    // Concurrent or ordered: deadline promote writes Some("partial");
    // then usable completion with different final text.
    // first_assistant remains "partial"; seq still advances.
}

#[tokio::test]
async fn concurrent_end_turn_and_deadline_both_orders_wal() {
    // REQUIRED: two connections + barriers for promote vs apply_usable_completion
    // in BOTH orders. Exactly one first-assistant snapshot; job ends Ready or
    // progresses without panic; no double first-ready corruption.
}

#[tokio::test]
async fn two_distinct_usable_tokens_advance_seq_twice() {
    // REQUIRED: two concurrent completions with different turn tokens on the
    // same job (e.g. already Ready after deadline). usable_turn_seq must become
    // +2, not +1 from lost concurrent current_seq+1 writes.
}

#[tokio::test]
async fn end_turn_from_awaiting_sets_assistant_and_ready() { /* classic path */ }

#[tokio::test]
async fn retry_wait_becomes_ready_without_replacing_assistant() {
    // retry_wait + Some("snap") + usable completion → ready, assistant unchanged
}
```

- [ ] **Step 2: Run — expect FAIL** where overwrite / lost seq still happens

- [ ] **Step 3: Implement atomic progress + conditional first-ready**

**Forbidden:** read `current_seq`, compute `new_seq = current_seq + 1` in Rust,
then unconditional PK update without a sequence/token guard.

**Required pattern inside the lifecycle transaction:**

```rust
// 0) Early exit if stop_reason unusable or final_text empty (unchanged).

// 1) Atomic progress (token idempotent):
// UPDATE auto_title_jobs SET
//   usable_turn_seq = usable_turn_seq + 1,
//   last_usable_turn_token = $token,
//   locale = $locale,
//   updated_at = $now
// WHERE conversation_id = $id
//   AND (last_usable_turn_token IS NULL OR last_usable_turn_token <> $token)
//
// If rows_affected == 0 → duplicate token or missing job → return no-op transition.

// 2) First-ready from awaiting_turn (write-once assistant):
// UPDATE … SET first_assistant_text = $bounded, state = 'ready', updated_at = $now
// WHERE conversation_id = $id
//   AND state = 'awaiting_turn'
//   AND first_assistant_text IS NULL
// became_ready |= rows_affected == 1

// 3) retry_wait → ready WITHOUT touching first_assistant_text:
// UPDATE … SET state = 'ready', updated_at = $now
// WHERE conversation_id = $id AND state = 'retry_wait'
// became_ready |= rows_affected == 1

// 4) Read back usable_turn_seq for CompletionTransition return value
//    (SELECT after updates, or RETURNING if available).
```

Duplicate token must remain a full no-op (no seq bump, no locale thrash if the
progress UPDATE already filtered it).

- [ ] **Step 4: Run completion tests — PASS**

```powershell
cargo test --features test-utils apply_usable_completion
cargo test --features test-utils auto_title::service
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/service.rs
git commit -m "fix(auto-title): CAS usable completion against deadline snapshot"
```

---

### Task 6: `promote_deadline_elapsed_jobs` service API

**Files:**
- Modify: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/auto_title/mod.rs` exports

**Interfaces:**

```rust
pub struct DeadlinePromoteParams {
    pub now: DateTime<Utc>,
    pub deadline: Duration,
    pub batch_limit: usize,
}

/// partials: conversation_id -> raw partial (already or not bound_context inside)
pub async fn promote_deadline_elapsed_jobs(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
    partials: &std::collections::HashMap<i32, String>,
) -> Result<usize, DbError>;

pub async fn list_deadline_candidates(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
) -> Result<Vec<i32>, DbError>;
```

`list_deadline_candidates` selects ids; caller fills `partials`; `promote` applies CAS per id (missing partial key ⇒ `""`).

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn promote_deadline_ready_with_partial_and_empty() { /* age job first_prompt_at = now-301s */ }

#[tokio::test]
async fn promote_skips_young_and_retry_wait_and_null_prompt_at() {}

#[tokio::test]
async fn promote_cas_loses_to_end_turn() {
    // End-turn wins first; promote returns 0 and leaves final assistant.
}

#[tokio::test]
async fn promote_select_then_delete_before_cas_is_noop() {
    // REQUIRED: list candidates (or hold ids), then cancel_job / soft-delete /
    // Off deletes the job, then promote_by_ids → promoted == 0, no panic.
}
```

- [ ] **Step 2: Run — FAIL** (functions missing)

- [ ] **Step 3: Implement**

```rust
pub async fn list_deadline_candidates(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
) -> Result<Vec<i32>, DbError> {
    let cutoff = params.now - chrono::Duration::from_std(params.deadline).unwrap();
    let rows = auto_title_job::Entity::find()
        .filter(Column::State.eq(AwaitingTurn))
        .filter(Column::FirstUserText.is_not_null())
        .filter(Column::FirstPromptAt.is_not_null())
        .filter(Column::FirstPromptAt.lte(cutoff))
        .filter(Column::FirstAssistantText.is_null())
        .order_by_asc(Column::FirstPromptAt)
        .order_by_asc(Column::ConversationId)
        .limit(params.batch_limit as u64)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|r| r.conversation_id).collect())
}

pub async fn promote_deadline_elapsed_jobs(...) -> Result<usize, DbError> {
    let mut promoted = 0usize;
    let cutoff = ...;
    for id in list_deadline_candidates(conn, params).await? {
        // Prefer re-check list inside promote OR accept prelisted ids:
        let partial = partials.get(&id).cloned().unwrap_or_default();
        let bounded = bound_context(&partial);
        let res = auto_title_job::Entity::update_many()
            .col_expr(Column::State, Expr::value(Ready))
            .col_expr(Column::FirstAssistantText, Expr::value(bounded))
            .col_expr(Column::UpdatedAt, Expr::value(params.now))
            .filter(Column::ConversationId.eq(id))
            .filter(Column::State.eq(AwaitingTurn))
            .filter(Column::FirstAssistantText.is_null())
            .filter(Column::FirstPromptAt.is_not_null())
            .filter(Column::FirstPromptAt.lte(cutoff))
            .exec(conn)
            .await?;
        if res.rows_affected == 1 {
            promoted += 1;
        }
    }
    Ok(promoted)
}
```

Note: If `list` is called twice (once for partials, once inside promote), that is fine; coordinator should call `list` once, fetch partials, then promote those ids with a variant that takes `&[i32]` to avoid double select — implement:

```rust
pub async fn promote_deadline_jobs_by_ids(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
    ids: &[i32],
    partials: &HashMap<i32, String>,
) -> Result<usize, DbError>
```

- [ ] **Step 4: PASS service promote tests**

```powershell
cargo test --features test-utils promote_deadline
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/service.rs src-tauri/src/auto_title/mod.rs
git commit -m "feat(auto-title): deadline promote CAS for awaiting_turn jobs"
```

---

### Task 7: Batch partial source (multi-connection safe)

**Files:**
- Create: `src-tauri/src/auto_title/partial_source.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Modify: `src-tauri/src/acp/manager.rs` — add `snapshot_partial_assistant_text_for_conversations`

**Interfaces:**

```rust
#[async_trait::async_trait]
pub trait PartialAssistantTextSource: Send + Sync {
    async fn partials_for(&self, conversation_ids: &[i32]) -> HashMap<i32, String>;
}

pub struct ManagerPartialSource {
    manager: ConnectionManager, // or Arc clone_ref handle
}

// ConnectionManager method (single map lock pass):
pub async fn snapshot_partial_assistant_text_for_conversations(
    &self,
    conversation_ids: &[i32],
) -> HashMap<i32, String>
```

Selection rule when multiple connections share an id:

1. Prefer states with `live_message.is_some()`
2. Max `live_message.started_at`
3. Tie-break connection id ascending
4. Value = `visible_assistant_text(live.as_ref())` (raw; service applies `bound_context`)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn picks_newest_live_message_among_matches() { /* pure scorer if extracted */ }

#[test]
fn equal_started_at_tie_breaks_by_connection_id_ascending() { ... }

#[tokio::test]
async fn snapshot_does_not_call_find_connection_by_conversation_id() {
    // Multi-match: two AgentConnections, same conversation_id, different
    // live_message.started_at → returns newer live text only.
}

#[tokio::test]
async fn snapshot_releases_map_lock_before_state_read() {
    // Optional lock-order test: while a state write lock is held on one conn,
    // snapshot still completes (map lock not held across state.read).
}
```

- [ ] **Step 2: FAIL**

- [ ] **Step 3: Implement — exact lock pattern**

`AgentConnection` is **not** `Clone`. The clonable handle is
`conn.state: Arc<RwLock<SessionState>>` (`src-tauri/src/acp/connection.rs`).

```rust
// REQUIRED pattern for snapshot_partial_assistant_text_for_conversations:
//
// let handles: Vec<(String /*conn id*/, i32 /*conv*/, Arc<RwLock<SessionState>>)> = {
//     let guard = self.connections.lock().await;
//     guard.iter()
//         .map(|(id, conn)| (id.clone(), conn.state.clone()))
//         // conversation_id is inside state — either:
//         //  (a) only clone state Arcs here, filter after drop, OR
//         //  (b) if conversation_id is only in state, collect all state Arcs
//         //      then filter after releasing the map lock.
//     .collect()
// }; // map MutexGuard dropped HERE — end of block
//
// // ONLY NOW await state reads:
// for (conn_id, state) in handles {
//     let s = state.read().await;
//     let Some(cid) = s.conversation_id else { continue };
//     if !wanted.contains(&cid) { continue };
//     // score: prefer live_message.is_some(), max started_at, then conn_id
// }
//
// PROHIBITED:
// - find_connection_by_conversation_id (holds map lock across state.read)
// - awaiting state.read() while `connections` MutexGuard is still in scope
// - assuming AgentConnection: Clone
```

- [ ] **Step 4: PASS**

```powershell
cargo test --features test-utils snapshot_partial
cargo test --features test-utils partial_source
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/partial_source.rs `
  src-tauri/src/auto_title/mod.rs `
  src-tauri/src/acp/manager.rs
git commit -m "feat(auto-title): batch multi-connection partial assistant snapshots"
```

---

### Task 8: Coordinator deadline sweep loop + liveness

**Files:**
- Modify: `src-tauri/src/auto_title/coordinator.rs`
- Modify: `build_production_coordinator` to pass `PartialAssistantTextSource` + durations

**Interfaces:**

```rust
// On AutoTitleCoordinator:
partial_source: Arc<dyn PartialAssistantTextSource>,
deadline: Duration,           // default 300s
sweep_interval: Duration,     // default 101s
batch_limit: usize,           // default 64
// test constructor overrides
```

`recover_and_start` under the same `started` CAS that starts `notification_loop`, also spawn `deadline_sweep_loop`.

```rust
async fn deadline_sweep_loop(self: Arc<Self>) {
    loop {
        if let Err(error) = self.run_deadline_sweep_once().await {
            tracing::warn!(%error, "auto-title deadline sweep failed");
        }
        tokio::time::sleep(self.sweep_interval).await;
    }
}

async fn run_deadline_sweep_once(&self) -> Result<(), DbError> {
    let params = DeadlinePromoteParams {
        now: Utc::now(),
        deadline: self.deadline,
        batch_limit: self.batch_limit,
    };
    let ids = list_deadline_candidates(&self.db.conn, &params).await?;
    let partials = if ids.is_empty() {
        HashMap::new()
    } else {
        self.partial_source.partials_for(&ids).await
    };
    let promoted =
        promote_deadline_jobs_by_ids(&self.db.conn, &params, &ids, &partials).await?;
    let has_ready = auto_title_job::Entity::find()
        .filter(Column::State.eq(Ready))
        .limit(1)
        .one(&self.db.conn)
        .await?
        .is_some();
    if promoted > 0 || has_ready {
        self.notify_ready();
    }
    Ok(())
}
```

Immediate first pass: call `run_deadline_sweep_once` once before entering sleep loop (or structure loop as promote → sleep).

Empty source for inert tests: `struct EmptyPartialSource; async fn partials_for(...) { HashMap::new() }`.

- [ ] **Step 1: Write failing coordinator tests (mandatory liveness)**

Add `#[cfg(any(test, feature = "test-utils"))]` hooks on the coordinator:

- `sweep_pass_count: AtomicU64`
- `sweep_fail_once: AtomicBool` (or mutex) — first `run_deadline_sweep_once` body
  returns an error without promoting; clears itself
- `notification_loop_starts: AtomicU64` / `sweep_loop_starts: AtomicU64`

```rust
#[tokio::test(start_paused = true)]
async fn startup_runs_immediate_sweep_before_interval() {
    // recover_and_start; do NOT advance time; wait until sweep_pass_count >= 1
    // (use yield/timeout helpers). Proves immediate pass, not only after 101s.
}

#[tokio::test(start_paused = true)]
async fn sweep_continues_after_transient_failure() {
    // arm sweep_fail_once; recover_and_start; after first pass error,
    // advance_time(sweep_interval); assert sweep_pass_count increases again.
}

#[tokio::test(start_paused = true)]
async fn lost_wake_ready_row_is_renotified_and_claimed() {
    // Insert Ready job while suppress_ready_notify or without notify.
    // Next sweep pass must notify_ready; inert/mock runner or claim_calls
    // proves drain attempted (not merely state == Ready).
}

#[tokio::test]
async fn double_recover_and_start_single_notification_and_sweep_loops() {
    // call recover_and_start twice; notification_loop_starts == 1 and
    // sweep_loop_starts == 1.
}

#[tokio::test]
async fn sweep_promotes_and_notifies_ready_drain() {
    // eligible awaiting_turn + stub partial source; after recover_and_start,
    // job becomes Ready and worker observes claim (claim_calls or finalize).
}
```

Construct with `deadline: Duration::ZERO` (or 0s), `sweep_interval: Duration::from_secs(1)` under paused time, and `EmptyPartialSource` / stub partials as needed.

- [ ] **Step 2: FAIL**

- [ ] **Step 3: Implement wiring**

Update `AutoTitleCoordinator::new` / `build_production_coordinator`:

```rust
pub fn build_production_coordinator(...) -> Arc<AutoTitleCoordinator> {
    let manager = connection_manager.clone_ref();
    // existing runner setup...
    let partial: Arc<dyn PartialAssistantTextSource> =
        Arc::new(ManagerPartialSource::new(manager));
    AutoTitleCoordinator::new_with_deadline(
        db,
        runner,
        emitter,
        partial,
        Duration::from_secs(300),
        Duration::from_secs(101),
        64,
    )
}
```

Keep `new_inert_for_test` compiling with `EmptyPartialSource` and short/default constants.

- [ ] **Step 4: PASS**

```powershell
cargo test --features test-utils auto_title::coordinator
cargo check
cargo check --no-default-features --bin codeg-server
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/coordinator.rs src-tauri/src/auto_title/mod.rs
git commit -m "feat(auto-title): 101s deadline sweep with ready re-notify"
```

---

### Task 9: End-to-end / integration hardening

**Files:**
- Modify: `src-tauri/tests/api_integration.rs` (optional one test) **or** keep module-level integration in `service`/`coordinator` if sufficient
- Grep for any remaining `ActiveModel` missing `first_prompt_at`

**Coverage checklist (must all exist somewhere after Tasks 1–8):**

| # | Case | Concurrency |
| --- | --- | --- |
| 1 | Capture write-once first_prompt_at | — |
| 2 | Two-connection capture: one first-field writer | **WAL barrier** |
| 3 | Promote age ≥ deadline with partial / empty | — |
| 4 | Young job not promoted | — |
| 5 | retry_wait / ready / running not promoted | — |
| 6 | Deadline vs end-turn **both orders** | **WAL barrier** |
| 7 | Two distinct tokens advance `usable_turn_seq` twice | **WAL barrier** |
| 8 | Claim `Some("")` ok; `None` deleted | — |
| 9 | Claim lost-race: completion between read and CAS | **WAL barrier** |
| 10 | Select candidates then delete/Off before promote CAS | barrier or two-step |
| 11 | `live_message = None` clears stale; helper equality | — |
| 12 | Multi-connection newest live + id tie-break | — |
| 13 | Immediate startup sweep; fail-once continues; lost-wake re-notify | paused time |
| 14 | Double `recover_and_start` → one notify loop + one sweeper | — |
| 15 | Migration last + index column order + down preserves queue index | — |
| 16 | Legacy NULL `first_prompt_at` never deadline-promoted; end-turn still works | — |
| 17 | Full path capture → promote → claim → finalize (mock runner) | preferred |

- [ ] **Step 1: Add any missing tests from the table**

- [ ] **Step 2: Run full auto_title + related**

```powershell
cargo test --features test-utils auto_title
cargo test --features test-utils visible_assistant
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
```

Expected: all PASS; clippy clean.

- [ ] **Step 3: Commit**

```powershell
git add -A src-tauri/src/auto_title src-tauri/src/acp/session_state.rs src-tauri/src/acp/manager.rs src-tauri/src/db
git commit -m "test(auto-title): complete deadline sweep coverage"
```

---

## Self-Review (plan vs spec)

| Spec requirement | Task |
| --- | --- |
| 300s / 101s / batch 64 | Task 8 constants; Task 6 params |
| `first_prompt_at` + new migration + deadline index + keep claim index | Task 1 |
| Conditional capture both NULL | Task 3 |
| Dual ready paths + no refine | Tasks 5–6 |
| Atomic usable_turn_seq + completion CAS | Task 5 |
| Claim empty/`None` + lost-CAS retry | Task 4 |
| Pure visible_assistant_text + TurnComplete clear including `live=None` | Task 2 |
| Multi-connection batch snapshot without map lock across await | Task 7 |
| Two-phase sweep, error isolation, ready re-notify, single start | Task 8 |
| retry_wait not deadline-promoted | Task 6 filters |
| Legacy NULL first_prompt_at end-turn only | Task 1 + Task 6 + Task 9 |
| Real concurrency + liveness tests | Tasks 3–9 (Codex review) |
| No frontend / settings | Global constraints |

**Placeholder scan:** No TBD; sequential race simulations removed after Codex review.

**Type consistency:** `DeadlinePromoteParams`, `promote_deadline_jobs_by_ids`,
`PartialAssistantTextSource::partials_for`, `visible_assistant_text`,
`AgentConnection.state: Arc<RwLock<SessionState>>` collection pattern.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-19-auto-title-deadline-sweep.md`.

After Codex plan review + Important fixes land, choose:

1. **Subagent-Driven (recommended)** — subagent-driven-development per task  
2. **Inline Execution** — executing-plans in this session  

(Do not start implementation until the user confirms post-Codex plan review.)
