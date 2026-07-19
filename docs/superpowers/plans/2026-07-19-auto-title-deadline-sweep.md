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
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/auto_title_job.rs`
- Touch every `auto_title_job::ActiveModel { ... }` initializer in tests that exhaustively set fields (grep `first_user_text: Set` under `src-tauri/`)

**Interfaces:**
- Produces: `auto_title_job::Model.first_prompt_at: Option<DateTimeUtc>`
- Produces: index name `idx_auto_title_jobs_deadline` on `(state, first_prompt_at, conversation_id)`
- Preserves: `idx_auto_title_jobs_queue` on `(state, updated_at, conversation_id)`

- [ ] **Step 1: Write the failing migration test**

```rust
// In m20260719_000001_auto_title_first_prompt_at.rs
#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn up_adds_first_prompt_at_and_deadline_index() {
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        conn.execute_unprepared("PRAGMA foreign_keys=ON").await.unwrap();
        // Minimal tables matching prior auto_title shape (conversation + auto_title_jobs).
        conn.execute_unprepared(
            "CREATE TABLE conversation (id INTEGER PRIMARY KEY NOT NULL)",
        )
        .await
        .unwrap();
        conn.execute_unprepared(
            "CREATE TABLE auto_title_jobs (
                conversation_id INTEGER PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                first_user_text TEXT,
                first_assistant_text TEXT,
                locale TEXT,
                usable_turn_seq INTEGER NOT NULL DEFAULT 0,
                attempt_turn_seq INTEGER NOT NULL DEFAULT 0,
                last_usable_turn_token TEXT,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(conversation_id) REFERENCES conversation(id) ON DELETE CASCADE
            )",
        )
        .await
        .unwrap();
        conn.execute_unprepared(
            "CREATE INDEX idx_auto_title_jobs_queue
             ON auto_title_jobs (state, updated_at, conversation_id)",
        )
        .await
        .unwrap();
        conn.execute_unprepared(
            "INSERT INTO conversation (id) VALUES (1);
             INSERT INTO auto_title_jobs
               (conversation_id, state, updated_at, first_user_text)
             VALUES (1, 'awaiting_turn', '2026-07-01T00:00:00Z', 'old task')",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&conn)).await.unwrap();

        let cols = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".into(),
            ))
            .await
            .unwrap();
        assert!(cols.iter().any(|r| {
            r.try_get::<String>("", "name").ok().as_deref() == Some("first_prompt_at")
        }));

        let first_prompt: Option<String> = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT first_prompt_at FROM auto_title_jobs WHERE conversation_id = 1".into(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "first_prompt_at")
            .unwrap();
        assert!(first_prompt.is_none());

        let indexes = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA index_list(auto_title_jobs)".into(),
            ))
            .await
            .unwrap();
        let names: Vec<String> = indexes
            .iter()
            .filter_map(|r| r.try_get::<String>("", "name").ok())
            .collect();
        assert!(names.iter().any(|n| n == "idx_auto_title_jobs_queue"));
        assert!(names.iter().any(|n| n == "idx_auto_title_jobs_deadline"));

        Migration.down(&SchemaManager::new(&conn)).await.unwrap();
        let cols_after = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".into(),
            ))
            .await
            .unwrap();
        assert!(!cols_after.iter().any(|r| {
            r.try_get::<String>("", "name").ok().as_deref() == Some("first_prompt_at")
        }));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `src-tauri/`):

```powershell
cargo test --features test-utils m20260719_000001_auto_title_first_prompt_at -- --nocapture
```

Expected: FAIL (module / Migration not found).

- [ ] **Step 3: Implement migration + entity field + register**

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
async fn turn_complete_clears_stale_last_assistant_when_no_answer_text() {
    // Build SessionState with last_assistant_text = Some("stale")
    // and live_message with only ToolCallRef (no trailing Text).
    // apply TurnComplete; assert last_assistant_text is None.
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
    // Two sequential CAS simulations are acceptable if true threads are hard:
    // 1) capture A succeeds
    // 2) second path that only updates locale when first fields already set
    // Prefer: begin two transactions if SQLite allows; otherwise document
    // sequential CAS proof that second update uses
    // WHERE first_user_text IS NULL AND first_prompt_at IS NULL → 0 rows.
}
```

- [ ] **Step 2: Run — expect FAIL** (no `first_prompt_at` write)

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
async fn claim_accepts_empty_assistant_some_empty_string() { /* seed Ready, user Some("t"), assistant Some("") → Some(claim) */ }

#[tokio::test]
async fn claim_deletes_ready_with_none_assistant() { /* seed Ready assistant None → None claim, job gone */ }

#[tokio::test]
async fn claim_still_deletes_empty_user() { /* unchanged */ }
```

- [ ] **Step 2: Run — expect FAIL** (current code deletes empty assistant)

- [ ] **Step 3: Implement**

```rust
let first_user = match job.first_user_text.as_deref().map(str::trim) {
    Some(u) if !u.is_empty() => job.first_user_text.clone().unwrap(),
    _ => {
        auto_title_job::Entity::delete_by_id(job.conversation_id)
            .exec(&txn)
            .await?;
        continue;
    }
};
let first_assistant = match &job.first_assistant_text {
    Some(text) => text.clone(), // includes ""
    None => {
        auto_title_job::Entity::delete_by_id(job.conversation_id)
            .exec(&txn)
            .await?;
        continue;
    }
};

// Prefer CAS that also matches observed usable_turn_seq:
let updated = auto_title_job::Entity::update_many()
    .col_expr(..., Running)
    .col_expr(..., new_attempts)
    .col_expr(..., attempt_turn_seq) // = job.usable_turn_seq observed
    ...
    .filter(Column::ConversationId.eq(...))
    .filter(Column::State.eq(Ready))
    .filter(Column::UsableTurnSeq.eq(job.usable_turn_seq))
    .exec(&txn)
    .await?;
```

- [ ] **Step 4: Run claim tests — PASS**

```powershell
cargo test --features test-utils claim_next_ready -- --nocapture
cargo test --features test-utils claim_
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/auto_title/service.rs
git commit -m "fix(auto-title): claim empty-assistant and CAS usable_turn_seq"
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
    // Job: awaiting_turn → manually set ready + first_assistant Some("partial")
    // OR: promote first then apply_usable_completion with different final text
    // assert first_assistant still "partial"; usable_turn_seq still increments
}

#[tokio::test]
async fn end_turn_from_awaiting_sets_assistant_and_ready() {
    // classic path still works
}

#[tokio::test]
async fn retry_wait_becomes_ready_without_replacing_assistant() {
    // job retry_wait, first_assistant Some("snap"), usable completion
    // state ready, first_assistant unchanged
}
```

- [ ] **Step 2: Run — expect FAIL** where overwrite still happens via ActiveModel

- [ ] **Step 3: Implement split updates**

Recommended structure (exact SeaORM API may use `update_many` + Expr):

```rust
// After token/usable checks:
let bounded = bound_context(snapshot.final_text.trim());
let new_seq = current_seq + 1;
let now = Utc::now();
let locale_wire = app_locale_to_wire(snapshot.locale).to_string();

// 1) Progress for any existing job (token not yet seen):
//    UPDATE … SET usable_turn_seq, last_usable_turn_token, locale, updated_at
//    WHERE conversation_id = ? AND (last_usable_turn_token IS NULL OR <> token)
//    — or keep in-memory check then update by id with attempt filters carefully.
// Prefer: single transaction with:
//
// A) Conditional first-ready from awaiting_turn:
update_many()
  .set first_assistant = bounded, state = ready, seq, token, locale, updated_at
  .filter state = awaiting_turn
  .filter first_assistant_text IS NULL
  .filter conversation_id = ?
//
// B) If A rows_affected == 0:
//    Conditional retry_wait → ready WITHOUT first_assistant:
update_many()
  .set state = ready, seq, token, locale, updated_at
  .filter state = retry_wait
  .filter conversation_id = ?
//
// C) If still 0: progress-only for ready/running (seq/token/locale) so retries work:
update_many()
  .set seq, token, locale, updated_at
  .filter conversation_id = ?
  .filter state IN (ready, running, awaiting_turn, retry_wait)
// Ensure token idempotency still holds before any of these.

// Return became_ready based on whether A or B transitioned into ready from non-ready.
```

Implement carefully so a duplicate token remains a no-op and `became_ready` matches lifecycle expectations (notify coordinator only when true).

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
    // set first_assistant Some("final") and state ready first; promote returns 0
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

Unit-test selection logic with two fake candidates if extracting a pure function:

```rust
#[test]
fn picks_newest_live_message_among_matches() { ... }
```

Manager test: two connections same conversation_id, different live started_at → correct partial.

- [ ] **Step 2: FAIL**

- [ ] **Step 3: Implement**

```rust
// Inside one `let connections = self.connections.lock().await;`:
// for each conn: read state with try_read or read (prefer try_read only if documented;
// spec wants correctness — use read().await carefully: cannot await while holding
// MutexGuard if lock is std::sync::Mutex — ConnectionManager uses tokio Mutex.
// Pattern: collect Arc<Connection> clones for matching ids WITHOUT nested await
// on the same guard if possible.
//
// Correct pattern:
// 1) lock map
// 2) collect Vec<(conn_id, Arc<Inner>)> for all connections
// 3) drop map lock
// 4) for each, state.read().await and score by conversation_id
// 5) reduce best per conversation_id
```

This avoids holding the global map lock across many state reads.

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

- [ ] **Step 1: Write failing coordinator tests**

```rust
#[tokio::test]
async fn sweep_promotes_and_notifies_ready_drain() {
    // inert runner that returns a fixed title OR just assert state becomes Ready
    // and claim_calls / notify path
}

#[tokio::test]
async fn double_recover_and_start_starts_single_sweep() {
    // started CAS — second call does not panic; use a counter if test-only
}

#[tokio::test]
async fn sweep_error_does_not_kill_loop() {
    // optional: inject DB close — at least unit-test that run_deadline_sweep_once
    // error is returned to loop body without panic
}
```

For timing, construct coordinator with `deadline: Duration::from_secs(0)` and `sweep_interval: Duration::from_millis(50)` in tests.

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

| # | Case |
| --- | --- |
| 1 | Capture write-once first_prompt_at |
| 2 | Concurrent/sequential CAS second capture loses first fields |
| 3 | Promote age ≥ deadline with partial / empty |
| 4 | Young job not promoted |
| 5 | retry_wait / ready / running not promoted |
| 6 | Deadline then end-turn does not overwrite assistant |
| 7 | End-turn then deadline promote CAS 0 |
| 8 | Claim Some("") ok; None deleted |
| 9 | Claim CAS usable_turn_seq |
| 10 | visible_assistant_text parity + clear stale |
| 11 | Multi-connection newest live wins |
| 12 | Sweep notifies; double start safe |
| 13 | Migration up/down + queue index retained |
| 14 | Pre-migration NULL first_prompt_at never deadline-promoted |
| 15 | Optional: full path capture → promote → claim → finalize with mock runner |

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
| Completion CAS / claim CAS | Tasks 4–5 |
| `Some("")` vs `None` | Task 4 |
| Pure visible_assistant_text + TurnComplete clear | Task 2 |
| Multi-connection batch snapshot | Task 7 |
| Two-phase sweep, error isolation, ready re-notify, single start | Task 8 |
| retry_wait not deadline-promoted | Task 6 filters |
| Legacy NULL first_prompt_at end-turn only | Task 6 + Task 9 |
| No frontend / settings | Global constraints |
| Tests for races and liveness | Tasks 3–9 |

**Placeholder scan:** No TBD steps; concrete signatures and commands included.

**Type consistency:** `DeadlinePromoteParams`, `promote_deadline_jobs_by_ids`, `PartialAssistantTextSource::partials_for`, `visible_assistant_text` used consistently across tasks.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-19-auto-title-deadline-sweep.md`.

After Codex plan review + Important fixes land, choose:

1. **Subagent-Driven (recommended)** — subagent-driven-development per task  
2. **Inline Execution** — executing-plans in this session  

(Do not start implementation until the user confirms post-Codex plan review.)
