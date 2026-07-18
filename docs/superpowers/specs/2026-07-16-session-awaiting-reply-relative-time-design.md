# Session list: awaiting-reply marker + relative time from latest activity

**Date:** 2026-07-16
**Status:** Approved for implementation planning
**Approach:** nullable generation token + compare-and-set clear + relative-time format fix

## Problem

Managing many sessions in the sidebar is hard for two reasons:

1. **Relative time** on the right is easy to misread: it follows sort mode
   (`updated_at` vs `created_at`), and in the hour range it only shows `Nh`, so
   `3 hours ago` and `3h 50m ago` look the same.
2. **Needs user attention** is weak: after a user-facing main-agent turn
   finishes, the row only gets the normal blue `pending_review` dot. There is no
   clear signal that a newly completed result has not yet been viewed.

## Goals

| Goal | Success criteria |
|------|------------------|
| Latest-activity time | Sidebar time always uses the conversation's backend `updated_at`, independent of sort mode |
| Sub-hour precision under 10h | When elapsed hours are in `[1, 10)`, the label includes hours and remaining minutes, for example `3h25m` |
| Awaiting reply is obvious | Eligible root sessions with a newly completed, not-yet-viewed result show a red status dot and red time label |
| View-once clears globally | The first qualifying view on any client clears the marker for every client sharing the database |
| New completions cannot be lost | A delayed clear from an older turn cannot clear a later turn's marker |
| Cancelled stays distinct | Cancelled uses a gray status dot; the existing red right-side X remains unchanged |
| Pending review stays unchanged | `pending_review` labels remain unchanged; awaiting reply is a separate transient display state |

## Non-goals

- A new `ConversationStatus` enum value
- Renaming i18n `Review` / `待复查` to `Awaiting reply` / `待回复`
- Per-client or per-device read receipts; the shared SQLite value is global
- Unread counts, sidebar filtering, auto-pinning, or sort-order changes
- Awaiting-reply styling for delegation sub-sessions
- Treating automation or chat-channel turns as awaiting a reply in the Codeg UI
- A permanent visible text badge in the right-side slot

## Product rules

### Relative time

- **Source:** always `conversation.updated_at`, never `created_at`, for the
  sidebar card label. Sort mode affects order only.
- **Clock:** `formatRelative(iso, now)` remains deterministic by receiving one
  list-wide `now` value refreshed once per minute.
- **Minute formatting:** remaining minutes are not zero-padded: `3h5m`, not
  `3h05m`.

| Elapsed | Label |
|---------|-------|
| `< 1m` | `now` |
| `< 60m` | `Nm`, for example `5m` |
| `1h <= h < 10`, remaining minutes `0` | `Nh`, for example `3h` |
| `1h <= h < 10`, remaining minutes `1-59` | `NhNm`, for example `3h5m` or `3h25m` |
| `10h <= h < 24` | `Nh` only |
| `>= 1d` | Existing `Nd` / `Nmo` / `Ny` buckets |

Invalid timestamps still return an empty string. Future timestamps remain
clamped to `now`, matching current behavior.

### Awaiting-reply marker

The persisted state is a nullable opaque token:

```text
awaiting_reply_token: Option<String>
```

- `null` means no unseen completed result.
- A new `uuid::Uuid::new_v4().to_string()` token means one eligible completed
  turn is awaiting a qualifying view.
- The token is a generation identifier, not a secret and not display data.
- The UI derives `awaiting_reply` from `awaiting_reply_token != null`; no
  redundant boolean is stored.
- `updated_at`, written in the same completion transition, remains the displayed
  activity time.

An opaque token is used instead of a timestamp so compare-and-set equality does
not depend on SQLite/JSON timestamp precision and two fast turns cannot share a
generation accidentally.

### Eligible turns

Eligibility belongs to a **turn**, not permanently to a connection. Connections
can be reused by different callers, so owner labels and `parent_id` alone are
not sufficient.

The prompt command carries an explicit `mark_awaiting_reply` policy through the
connection loop and copies it onto `TurnComplete`:

| Turn source | `mark_awaiting_reply` |
|-------------|-----------------------|
| Desktop/web UI root prompt | `true` |
| Delegation child prompt | `false` |
| Automation prompt | `false` |
| Chat-channel prompt | `false` |

A marker is created only when all of these are true:

1. `TurnComplete.stop_reason == "end_turn"`.
2. The row is a root conversation (`parent_id == null`).
3. The completed turn carried `mark_awaiting_reply == true`.
4. The row still transitions from `InProgress` to `PendingReview` through the
   completion CAS.

Historical/imported rows start with `null`. Manual status changes never create
a token, including manually selecting `pending_review`.

### Global seen semantics

The marker is shared database state. The first qualifying view on any desktop
window or web client clears it globally. All other clients receive the same
state patch and stop showing awaiting-reply styling. There is intentionally no
claim that another client retains its own unread marker.

If a client is already showing the conversation when an eligible turn
completes, that client clears the new token only when the view is genuinely
visible and focused. The resulting clear is still global.

### Qualifying view

A conversation counts as viewed only while all of these conditions hold:

1. Its tab is the active conversation tab.
2. The workbench route is `conversations`.
3. The conversation pane is not fully hidden by a maximized file pane.
4. `document.visibilityState == "visible"` and `document.hasFocus()` are true.
5. In tile mode, only the active tile qualifies; visible inactive tiles keep
   their marker until activated.

The clear observer re-evaluates when any of the following changes:

- active tab or bound conversation id
- awaiting-reply token
- workbench route or conversation-pane visibility
- document `focus`, `blur`, or `visibilitychange`
- tab hydration/restoration or transport reconnect

This covers sidebar selection, search/deep-link opens, existing-tab switches,
restored tabs, returning from Automations, and an already-focused conversation
receiving a completion event.

## State transitions

| Trigger | Status result | Token result | `updated_at` |
|---------|---------------|--------------|--------------|
| Prompt accepted | `InProgress` | Clear to `null` | Set to backend `now` |
| Eligible root `end_turn`, CAS succeeds | `PendingReview` | Set new token | Set to the same backend `now` |
| Ineligible root `end_turn`, CAS succeeds | `PendingReview` | `null` | Set to backend `now` |
| Delegation child `end_turn`, CAS succeeds | `PendingReview` | `null` | Set to backend `now` |
| Failure stop reason | `Cancelled` | `null` | Set to backend `now` |
| Manual status change | Requested status | `null` | Existing status-update behavior |
| Qualifying view with matching token | Unchanged | Clear to `null` | Unchanged |
| Clear with stale token | Unchanged | Unchanged | Unchanged |

Every status transition writes status, token state, and `updated_at` in one SQL
statement. The eligible completion transition is a compare-and-set:

```sql
UPDATE conversation
SET status = 'pending_review',
    awaiting_reply_token = :new_token,
    updated_at = :now
WHERE id = :id
  AND status = 'in_progress';
```

For an ineligible turn, `:new_token` is `NULL`. The lifecycle emits a state
event only when the CAS changed the row. A prior `Completed` or `Cancelled`
write therefore wins over a delayed `TurnComplete`.

The clear operation is also a compare-and-set:

```sql
UPDATE conversation
SET awaiting_reply_token = NULL
WHERE id = :id
  AND awaiting_reply_token = :expected_token;
```

This prevents a delayed clear for turn A from clearing turn B.

## Architecture

```text
UI prompt
  -> ConnectionCommand::Prompt { mark_awaiting_reply: true }
  -> status transition: InProgress + token null

Automation/chat/delegation prompt
  -> ConnectionCommand::Prompt { mark_awaiting_reply: false }

TurnComplete(end_turn, mark_awaiting_reply)
  -> load immutable parent_id
  -> CAS InProgress -> PendingReview
     status + token + updated_at in one SQL statement
  -> emit authoritative conversation state patch

Qualifying active view
  -> clear_awaiting_reply(id, expected_token)
  -> CAS token -> null without touching status/updated_at
  -> return current state patch
  -> if changed, broadcast the same patch globally

Sidebar
  -> showAwaiting = status == pending_review
                  && awaiting_reply_token != null
                  && !isSelected
  -> time = formatRelative(updated_at, now)
```

Desktop and server modes share the same `_core` service, response type, and
global conversation event shape.

## Data model

### Migration

Add a date-stamped SeaORM migration after the current latest migration:

```text
conversation.awaiting_reply_token TEXT NULL
```

- Existing rows remain `NULL`; no backfill.
- New insert paths explicitly initialize `NULL` or use the database default.
- Register the migration in `src-tauri/src/db/migration/mod.rs`.

### Entity and summaries

- `conversation::Model` / `ActiveModel` gain
  `awaiting_reply_token: Option<String>`.
- `DbConversationSummary` and its TypeScript mirror expose
  `awaiting_reply_token` as an always-present nullable value.
- All summary mappers include the field.
- Pin updates do not alter the token or `updated_at`.
- Title and external-id writes do not alter the token.

### Authoritative state patch

Add a compact backend/wire model:

```text
ConversationStatePatch {
  id: i32,
  status: String,
  awaiting_reply_token: Option<String>,
  updated_at: DateTime<Utc>,
}
```

The corresponding TypeScript type uses `number`, `string`, `string | null`, and
ISO `string` fields. The backend creates this patch from the row written by the
transition; the frontend never invents `updated_at` for backend state events.

## Backend lifecycle

### Per-turn attention policy

- Add `mark_awaiting_reply: bool` to `ConnectionCommand::Prompt`.
- The connection loop includes the same value as a required boolean on the
  emitted `AcpEvent::TurnComplete`; add it to the TypeScript event mirror even
  though frontend runtime reducers do not consume it.
- Core prompt APIs require an explicit policy; convenience wrappers encode the
  correct choice for UI, delegation, automation, and chat-channel callers.
- The attention-policy wrappers do not change mandatory profile-route
  registration. Chat-channel prompts keep their existing route registration
  behavior while passing `mark_awaiting_reply = false`.
- A reused connection therefore follows the source of the current turn rather
  than the source that originally spawned the process.

### Status service

Provide focused service helpers:

- Generic status transition: writes the requested status, clears the token, and
  updates `updated_at`; returns `ConversationStatePatch`.
- Conditional status transition: same behavior with an expected-status filter;
  returns `None` when the CAS loses.
- End-turn transition: reads immutable `parent_id`, chooses a new token only for
  an eligible root, then performs one `InProgress -> PendingReview` CAS.
- Clear helper: accepts `(conversation_id, expected_token)`, clears only the
  matching token, never changes `status` or `updated_at`, and returns both the
  current patch and whether a row changed.

Any remaining direct `ActiveModel.status = Set(...)` write must move through a
helper or explicitly clear the token in the same write.

### Clear API

Expose one explicit transport operation in both runtimes:

```text
clear_awaiting_reply(conversation_id, expected_token)
  -> ConversationStatePatch
```

- Do not put this side effect in `get_folder_conversation`; that getter is also
  used by background refetch, metadata sync, details dialogs, and sub-session
  readers.
- Matching token: clear, return the resulting patch, broadcast it globally.
- Stale/already-cleared token: do not mutate or emit; fetch and return the
  current patch so the caller converges.
- Missing/deleted conversation: return the existing not-found command error.

### Events

The global `conversation://changed` channel is the sole authority for sidebar
conversation state:

```text
ConversationChange::State { patch: ConversationStatePatch }
```

- `Upsert` remains for full-summary create/title/pin updates.
- `Deleted` remains unchanged.
- `State` replaces the lightweight status-only sidebar patch and carries exact
  backend status, token, and `updated_at`.
- Per-connection `ConversationStatusChanged` remains available to ACP runtime
  consumers, but the app-workspace store no longer applies that second stream.
- Remove the automatic status-to-global bridge from `emit_with_state`; each
  successful DB status transition explicitly emits its returned state patch.
  This prevents duplicate cross-channel writes from reapplying stale state.
- A reconnect still performs the existing full conversation-list refresh.

## Frontend

### Store

Add `applyConversationStatePatch(patch)`:

- Replace only `status`, `awaiting_reply_token`, and `updated_at` on a known root
  summary.
- Use the backend `updated_at` exactly.
- Preserve conversation order and stats reference behavior already used by
  lightweight status patches.
- Unknown ids remain a no-op.
- Existing child-session consumers in `tab-store` and `use-subsession-sync`
  consume the same `state` variant and merge all three patch fields. The
  tab-store's in-flight seed buffer stores the whole patch so a late fetch
  cannot overwrite newer child status, token, or backend `updated_at`.

The global conversation-change subscriber applies the new `state` variant.
Remove the app-workspace update performed by `ConversationStatusEventBridge` so
sidebar state has one authoritative event stream.

### Clear observer

Add one client-side observer near the tab/workbench providers. It:

1. Finds the active persisted conversation and its current token.
2. Evaluates the qualifying-view conditions.
3. Deduplicates in-flight calls by `(conversation_id, token)`.
4. Calls the explicit clear API with that exact token.
5. Applies the returned state patch, whether the CAS matched or lost.

Do not optimistically erase the token. The selected-row guard hides the red
chrome immediately; retaining server state on failure ensures the badge returns
when the user switches away. Retry on the next qualifying focus/visibility/
route transition or transport reconnect, without a tight retry loop.

### Relative time

- `sidebar-conversation-list.tsx` always passes `conv.updated_at` to
  `formatRelative`, regardless of sort mode.
- `formatRelative` adds remaining minutes while total hours are below 10.
- The once-per-minute shared `now` mechanism remains unchanged.

### Card and status precedence

```text
showAwaitingReply =
  status === "pending_review" &&
  conversation.awaiting_reply_token !== null &&
  !isSelected
```

Precedence is fixed:

1. `in_progress`: yellow dot + spinner
2. `cancelled`: gray dot + red X
3. `pending_review` with a token and not selected: red dot + red time
4. Normal status colors + muted time

The explicit `pending_review` gate makes terminal states defensive against
stale or malformed input even though the backend maintains the invariant.

### Accessibility and colors

- Awaiting-reply time uses semantic destructive/attention text color and a
  stronger font weight, so the distinction is not hue-only.
- The status dot uses the matching semantic red background.
- Add required localized `statusAwaitingReplyBadge` text for all supported
  locales.
- Render the localized text through `title` and an `sr-only` label associated
  with the row/meta; it is not optional.
- Change global cancelled dot color to gray; keep the existing red `XCircle`.

| State | Dot | Right meta |
|-------|-----|------------|
| Awaiting reply, not selected | Red | Red, stronger-weight relative time |
| `in_progress` | Yellow | Spinner |
| `pending_review`, no marker | Blue | Muted relative time |
| `completed` | Green | Muted relative time |
| `cancelled` | Gray | Red X |

## Error and race handling

| Case | Required result |
|------|-----------------|
| Old clear A arrives after completion B | Token mismatch; B remains marked |
| User starts a new turn before clear A lands | Prompt-start write clears A; stale clear is a no-op |
| User marks Completed before delayed `end_turn` | End-turn CAS loses; terminal state and null token remain |
| Duplicate `TurnComplete(end_turn)` | First CAS wins; duplicate emits no new token/event |
| Selected tab is hidden by Automations/background window | Do not clear until it becomes a qualifying view |
| Clear request fails | Keep persisted/local marker; retry on a later qualifying transition |
| Clear event is dropped for another client | Reconnect list refresh converges |
| Completion arrives while view is focused | State patch creates token, observer clears that exact token globally |
| Tile mode shows inactive panel | It remains marked until the tile becomes active |
| Background detail fetch | Never clears; getter is read-only with respect to attention state |

## Testing

### Backend

- Migration adds a nullable column and leaves historical rows `NULL`.
- Summary serialization always includes `awaiting_reply_token` as string/null.
- UI root `end_turn` performs `InProgress -> PendingReview`, creates a token,
  and returns/emits backend `updated_at`.
- Automation, chat-channel, and delegation turns never create a token.
- Duplicate or terminal-racing `end_turn` loses the CAS and emits no state patch.
- Prompt start, Completed, Cancelled, and manual PendingReview clear any token in
  the same status write.
- Matching clear removes the token without changing status or `updated_at` and
  emits one global state patch.
- Already-cleared/stale clear emits nothing and returns current state.
- Regression: token A clear cannot remove token B.
- Global state event serializes `status`, token, and backend `updated_at`.
- Per-connection status events no longer produce a duplicate global sidebar
  status event.
- Tauri command and Axum handler use the same clear core.

### Frontend

- `formatRelative` cases: `now`, `5m`, `59m`, `1h`, `1h1m`, `3h5m`, `3h25m`,
  `9h59m`, `10h`, `23h`, `24h`, `2d`, invalid, and future timestamps.
- Created sort still orders by `created_at` but displays `updated_at`.
- State patch applies backend `updated_at` verbatim and does not recompute stats.
- Child tab and sub-session caches consume the `state` variant, including a
  state patch that arrives while child seeding is in flight.
- Awaiting + not selected + PendingReview renders red dot/time and required
  accessible label.
- Selected, running, completed, and cancelled precedence paths do not show
  awaiting chrome; cancelled keeps gray dot + red X.
- Clear observer calls once per `(id, token)` when all visibility conditions are
  true.
- It does not call while Automations is active, the document is hidden/unfocused,
  the file pane fully covers conversations, or an inactive tile owns the token.
- It retries when focus/visibility/route returns and applies a stale-CAS response
  without erasing a newer token.
- Details dialog and background `getFolderConversation` calls do not invoke the
  clear API.
- Global state event is the only app-workspace status/token mutation path.

### Manual smoke

1. Finish two root UI sessions; both show red dot/time.
2. Open one; it clears everywhere while the other remains marked.
3. Start and finish another turn; a new marker appears.
4. Exercise stale clear versus a fast next turn; the new marker survives.
5. Complete a turn while the app is unfocused or on Automations; marker remains
   until returning to the focused conversation view.
6. Verify desktop plus browser clients converge after one client views a result.
7. Run automation, chat-channel, and delegation turns; none show awaiting reply.

## Implementation order

1. Migration, entity, summary token, and shared state-patch types
2. Atomic status/end-turn/clear service helpers with race tests
3. Per-turn attention policy through prompt and `TurnComplete`
4. Authoritative global state event and removal of duplicate sidebar status path
5. Explicit Tauri/Axum clear API and transport binding
6. Frontend store state-patch application
7. Qualifying-view clear observer
8. Relative-time source/format tests and implementation
9. Card colors, precedence, accessibility, and all locale keys
10. Focused full-suite checks and manual multi-client smoke

## Alternatives considered

| Approach | Decision |
|----------|----------|
| Plain `awaiting_reply` boolean | Rejected: an old clear can erase a later completion |
| Nullable `awaiting_reply_since` timestamp | Rejected: workable, but CAS identity depends on timestamp precision and formatting |
| Nullable opaque token | Selected: one column, exact CAS identity, no redundant boolean |
| Frontend-only localStorage seen map | Rejected: breaks shared desktop/server consistency |
| Per-client read-receipt table | Rejected for v1: product semantics are global-first-view |
| New status enum value | Rejected: awaiting reply is transient attention state, not lifecycle status |
| Implicit clear in detail getter | Rejected: background reads would falsely mark sessions viewed |
| Determine eligibility from connection owner | Rejected: connections may be reused; policy must be per turn |

## Approval

Approved for implementation planning on 2026-07-16 with these locked choices:

- Relative time always displays backend `updated_at`; hours below 10 include
  unpadded remaining minutes.
- Awaiting reply uses a nullable opaque generation token and CAS clear.
- First qualifying view on any client clears globally.
- Only eligible user-facing root turns create a marker; eligibility is carried
  per prompt/turn.
- Completion and status/token writes are atomic and CAS-guarded.
- Detail getters remain side-effect free; an explicit visibility-aware observer
  clears the marker.
- The global conversation state patch is the sole sidebar authority.
- Awaiting uses red dot/time with required accessible text; cancelled uses a
  gray dot plus the existing red X.

---

*Next step: create the implementation plan with the writing-plans skill.*
