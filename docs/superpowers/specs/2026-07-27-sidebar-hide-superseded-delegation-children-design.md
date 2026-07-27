# Sidebar Hide Superseded Delegation Children Design

Date: 2026-07-27

Status: Draft for user review (brainstorm approved: path 1 + rule A)

## Summary

Sidebar sub-session trees currently list every non-soft-deleted child of a
parent. After replacement recovery (`delegate_to_agent` with `replaces_task_id`),
the failed predecessor child remains visible next to the new child. That
duplicates lineage noise (dozens of `unresumable` predecessors in production)
without adding navigation value.

Filter **sidebar-facing** child lists so a child conversation is hidden when
any of its runs has been replaced by a later run. Keep failed tips that were
never replaced. Do not soft-delete rows. Keep unfiltered child identity
available for parent-transcript meta injection so old tool cards still bind to
their historical child.

## Evidence

Production `codeg.db` (2026-07-27 snapshot):

- ~104 visible failed/canceled delegate rows (`deleted_at IS NULL`).
- ~33 distinct child conversations that are replacement targets (another run
  has `replaced_task_id` pointing at one of their `task_id`s).
- `provisional_admission_rejected` soft-delete path works (0 still visible);
  the remaining noise is post-admission failure + replacement, not provisional
  orphans.

Runtime error tool cards can disappear after restart (hot UI / incomplete cold
meta), while superseded children remain in the sidebar because `list_children`
only filters `deleted_at IS NULL`.

## Goals

1. Sidebar parent expand lists only **lineage tips**: children not superseded by
   a replacement run.
2. `child_count` uses the **same** visibility predicate as the expanded list
   (`child_count > 0` iff the list would return rows).
3. Multiple continue generations on one `child_conversation_id` still appear as
   **one** row.
4. Failed/canceled tips with **no** successor replacement remain visible
   (rule A).
5. Parent-transcript cold recovery for historical tool cards continues to
   resolve **all** non-deleted children (including superseded ones).
6. No durable soft-delete or row mutation of superseded children.

## Non-goals

- Hiding failed tips that were never replaced.
- Soft-deleting superseded children or rewriting conversation lifecycle.
- Changing top-level conversation list rules (`parent_id IS NULL`).
- Collapsing distinct work units / roles into one row (only replacement
  lineage is collapsed).
- Changing parent tool-card grouping in the message stream (overlay may keep
  its own replacement markers).
- Backfilling or rewriting historical `replaced_task_id` links.
- A user-facing "show superseded" toggle in v1 (may be a follow-up).

## Product Rules (Approved)

| Choice | Decision |
| --- | --- |
| Implementation locus | Backend `list_children` + aligned `child_count` (path 1) |
| Failed tip never replaced | **Still show** (rule A) |
| Superseded predecessor | **Hide** from sidebar-facing lists |
| Continue same child | One row (unchanged) |
| Durable hide mechanism | Query filter only; no `deleted_at` |

## Terminology

- **Child conversation**: `conversation` row with `kind = delegate` and
  `parent_id = P`.
- **Run**: `delegation_task_runs` row; many runs may share one child
  (continuation generations).
- **Replacement**: a run `N` with non-null `replaced_task_id = R.task_id`, where
  `R` belongs to a different child than `N` (fresh child after
  unresumable / budget / not_supported recovery).
- **Superseded child**: a child that owns at least one run `R` such that some
  run `N` has `N.replaced_task_id = R.task_id`.
- **Lineage tip (sidebar)**: a non-deleted child that is not superseded.

Note: `N.replaced_task_id` stores the **prior terminal run id**, not the prior
child id. Hide is derived by joining through runs.

## Canonical Hide Predicate

A child conversation `C` is **superseded** (hidden from sidebar-facing lists)
iff:

```text
EXISTS run R
  WHERE R.child_conversation_id = C.id
AND EXISTS run N
  WHERE N.replaced_task_id = R.task_id
```

Properties:

1. **Chain A→B→C**: B replaces a run on A, C replaces a run on B → A and B
   hidden, C shown (as long as nothing replaces C).
2. **Continue only**: generations share `child_conversation_id` → no second
   child → still one row.
3. **Failed tip, no replacement**: no `N.replaced_task_id` points at its runs →
   shown.
4. **Soft-deleted provisional**: already excluded by `deleted_at IS NULL` →
   still hidden.
5. **Partial / malformed data**: if `replaced_task_id` is set but points at a
   missing task id, no child is hidden by that edge (fail open for visibility
   of tips; do not invent links).

## Architecture

```text
                    ┌─────────────────────────────────────┐
                    │ conversation_service                  │
                    │                                       │
  sidebar / API ──►│ list_children (default)                │
  child_count   ──►│   deleted_at IS NULL                    │
                    │   AND NOT superseded(child)           │
                    │                                       │
  detail inject ──►│ list_children_including_superseded     │
                    │   deleted_at IS NULL only              │
                    └─────────────────────────────────────┘
```

### API surface

| Function | Superseded filter | Callers |
| --- | --- | --- |
| `list_children` | **Yes** (default) | `list_child_conversations` (Tauri + web), `fill_child_counts`, any sidebar path |
| `list_children_including_superseded` (new name; exact identifier free) | **No** | `get_conversation_detail` / historical path that calls `inject_delegation_meta` |

Do **not** change the public command name `list_child_conversations`. Behavior
change is intentional: the sidebar contract becomes "lineage tips only".

Document in both function rustdocs:

- Default list is sidebar visibility.
- Meta injection **must** use the including-superseded variant so old
  `parent_tool_use_id` → child rows still match after replacement.

### `fill_child_counts`

Today: `GROUP BY parent_id` where `deleted_at IS NULL`.

Required: same group, but count only non-superseded children.

Implementation options (pick one in plan; both correct if equivalent):

1. **Subquery / NOT EXISTS** on `delegation_task_runs` matching the hide
   predicate for each child id in the group.
2. Count via a filtered child-id set query then aggregate in SQL.

Invariant (unchanged wording, stronger meaning):

> `child_count > 0` iff `list_children` would return rows for that parent.

### SQL sketch (informative)

```sql
-- child C is visible under parent P when:
--   C.parent_id = P AND C.deleted_at IS NULL
--   AND NOT EXISTS (
--     SELECT 1 FROM delegation_task_runs R
--     JOIN delegation_task_runs N ON N.replaced_task_id = R.task_id
--     WHERE R.child_conversation_id = C.id
--   )
```

Indexes: rely on existing keys on `delegation_task_runs.task_id` (PK) and
`child_conversation_id` if present; plan should verify query plans under a
parent with many children. No schema migration required for v1.

## Live Updates And Frontend

### Backend events

When a replacement run is admitted (new child created with `replaced_task_id`):

- Existing parent/child summary emits must continue to fire for the **new**
  child.
- Parent `child_count` on the next summary fetch must **not** include the
  superseded predecessor.
- No requirement to emit a synthetic "hide" event for the old child if the
  frontend refetches children on expand or merges by re-list; preferred:
  **lazy expand** already refetches via `listChildConversations` → filtered
  list.

### Frontend (`sidebar-conversation-list`)

Minimal change expected:

1. Trust backend filter; no client-side superseded heuristic in v1.
2. When a live child-created event inserts a row under a parent while the
   tree is expanded, keep current merge-by-id behavior. If an old superseded
   child is already in the in-memory cache from a pre-fix or mid-flight
   fetch:
   - Either refetch parent children after replacement-related parent updates,
     or
   - Drop children whose ids disappear on the next successful
     `listChildConversations` for that parent (already true when toggling /
     ensure load replaces snapshot — verify merge does not permanently retain
     tombstoned-by-filter rows).

Plan must include one frontend test or store assertion: after refetch, a
superseded child id is not present even if it was previously cached.

### Overlay / message cards

Out of scope for sidebar list filtering. Replacement markers and
`isUncorrelatedDelegationFailure` stay as-is. Parent cards may still link to
superseded children via detail meta injection + run snapshots.

## Error Handling And Edge Cases

| Case | Behavior |
| --- | --- |
| Self-loop / corrupt `replaced_task_id` pointing at own run only | If join requires another run id, same-run loops do not hide; if N and R share child, still "superseded" only if EXISTS — prefer hide only when **another** child owns N (see refinement) |
| Replacement run on **same** child (should not happen) | Platform replacement creates a new child; if data ever violates this, treat as superseded only when `N.child_conversation_id != R.child_conversation_id` |
| Concurrent list during replacement admit | Eventual consistency; next list is correct |
| Parent with only superseded children | `child_count = 0`, no chevron |
| Nested grandchildren | Same predicate on each level independently |

**Refinement (binding):** hide `C` only when:

```text
EXISTS R, N
  WHERE R.child_conversation_id = C.id
    AND N.replaced_task_id = R.task_id
    AND N.child_conversation_id IS NOT NULL
    AND N.child_conversation_id != C.id
```

This prevents pathological same-child edges from hiding the only row.

## Testing

### Backend (`conversation_service` / command tests)

1. Parent with one completed child, no replacement → list length 1,
   `child_count = 1`.
2. Parent with failed child A + replacement child B (`B.replaced_task_id` →
   run on A) → list contains only B; `child_count = 1`.
3. Chain A→B→C → list only C; `child_count = 1`.
4. Failed tip without replacement → still listed; `child_count` includes it.
5. Soft-deleted provisional still excluded (regression).
6. Continue: two runs same child → still one list row.
7. `list_children_including_superseded` returns A and B when B replaced A.
8. Detail path: `inject_delegation_meta` still binds a tool card whose
   `parent_tool_use_id` points at superseded child A (uses including-superseded
   list).
9. Ordering unchanged among visible children (`updated_at` DESC, id DESC).

### Frontend (narrow)

1. Mock `listChildConversations` returning only tips; expand shows those ids.
2. Optional: cached superseded id removed after refetch merge policy decided in
   plan.

### Broker

No change to replacement admission rules. Existing broker tests that call
`list_children` and expect a provisional/failed orphan may need expectation
updates only if they also created replacement links (unlikely). Prefer adding
focused service tests rather than rewriting broker suites.

## Rollout And Compatibility

- Behavior change for `list_child_conversations`: callers that assumed "all
  non-deleted children" must switch to the including-superseded API if they
  need full history. Known required switch: detail `inject_delegation_meta`.
- Grep all `list_children` call sites in-tree and classify sidebar vs meta vs
  test.
- No DB migration.
- No i18n strings required for v1 (visibility only).

## File Map (Expected)

| Area | Files (approximate) |
| --- | --- |
| Predicate + list | `src-tauri/src/db/service/conversation_service.rs` |
| Detail inject caller | `src-tauri/src/commands/conversations.rs` |
| Models rustdoc | `src-tauri/src/models/conversation.rs` (`child_count` comment) |
| Web | no route shape change; behavior via core |
| Frontend | only if cache/refetch merge needs a fix; else none |
| Tests | conversation_service unit tests; inject meta regression |

## Alternatives Rejected

| Alternative | Why rejected |
| --- | --- |
| Frontend-only filter | `child_count` / chevron drift; dual implementation |
| Soft-delete on replacement | Changes audit/open-by-id lifecycle; heavier than needed |
| Hide all failed tips (rule B) | User chose A; removes recoverable tips from sidebar |

## Success Criteria

1. Expanding a parent after multi-round replacement shows one child per
   replaced lineage tip, not the full predecessor stack.
2. Parent chevron absent when every child is superseded or soft-deleted.
3. Historical parent detail still injects meta for superseded children's tool
   cards.
4. Focused unit tests for the matrix above pass under desktop `cargo test`
   features used by conversation_service.

## Open Follow-ups (Out of Scope)

- Optional UI to reveal superseded predecessors.
- Hiding canceled tips that were abandoned without replacement (rule B).
- Compacting sidebar status badges for failed tips.

## Review Amendments

None yet.
