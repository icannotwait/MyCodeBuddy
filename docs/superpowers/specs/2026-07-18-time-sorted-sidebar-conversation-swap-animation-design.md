# Time-Sorted Sidebar Conversations and Swap Animation Design

## Status

Approved on 2026-07-18. This document records the refined Approach 2 selected
after the original proposal and two independent design reviews.

## Problem

The sidebar card already displays relative time from `updated_at`, but regular
folder conversations default to sorting by `created_at`. A card can therefore
say it was active moments ago while remaining below older-looking cards.

The list also has no reorder transition. When a conversation becomes active and
its rank changes, the virtualized rows jump directly to their new positions.
The desired behavior is the familiar message-list exchange: the active root
conversation rises within its section while painted rows make room smoothly.

The implementation must preserve the existing `virtua` windowing, folder and
worktree grouping, pinned section, delegation subtrees, memoization boundaries,
and cross-client backend authority.

## Goals

- Sort every root-conversation bucket by the same effective activity time shown
  on the card.
- Make updated-time ordering the only sidebar conversation ordering mode.
- Raise an existing conversation immediately when the user actually dispatches
  a message, without waiting for an IPC or HTTP round trip.
- Use existing authoritative state patches for assistant turn start and turn
  completion.
- Animate a root conversation and its expanded descendants as one contiguous
  block while painted displaced rows make room.
- Preserve the reader's viewport instead of scrolling to the newly active row.
- Remain correct under rejected sends, rapid consecutive activity, stale
  snapshots, out-of-order events, remote clients, and reduced-motion settings.

## Non-Goals

- Replacing `virtua` or rendering the full conversation list.
- Animating an invented path for rows that were not mounted before the reorder.
- Reordering delegation children by activity or promoting a root because one of
  its children changed.
- Updating activity on stream chunks, queue insertion, permission responses,
  pin toggles, folder drag, or passive conversation viewing.
- Changing the database schema or the backend meaning of `updated_at`.
- Changing section order, folder order, completed filtering, or pinned
  visibility rules.
- Adding a browser-test framework solely for this feature.

## Decision

Keep authoritative timestamps and optimistic presentation state separate.

- `DbConversationSummary.updated_at` remains backend-owned.
- A small frontend `optimisticActivityById` overlay represents a user dispatch
  until a newer backend timestamp acknowledges it.
- Sorting and the card label both resolve one `effectiveUpdatedAt` value from
  the authoritative summary plus that overlay.
- Backend `conversation://changed` state patches remain the only activity
  source for assistant start and completion.
- A sidebar-local layout controller animates stable inner row wrappers with the
  Web Animations API. It never transforms `virtua`'s positioning element.

No new activity endpoint is needed. The existing prompt path writes
`InProgress`, bumps `updated_at`, and broadcasts a state patch before delivering
the prompt. Turn completion similarly writes `PendingReview` or `Cancelled` and
broadcasts its resulting state patch.

## Ordering Model

### Updated-only mode

Remove `SidebarSortMode` from the sidebar component boundary, stop reading and
writing `workspace:sidebar-sort-mode`, and remove the sort radio group from the
view menu. A stored `created` value becomes inert, so no local-storage migration
or destructive cleanup is required.

Leave the old translation strings unused in place for this change to avoid
unrelated ten-locale catalog churn.

### Bucket rules

Root conversations are ordered only inside their current display bucket:

- `pinned` for every pinned root, regardless of folder or chat kind;
- `chat` for unpinned folderless chat roots;
- `folder:<display-folder-id>` for regular roots, where worktree child folders
  use the parent folder selected by the existing `childToParent` map.

There is no cross-bucket exchange animation. Pinning and unpinning remain an
instant section transfer, and folder or section order never changes because of
conversation activity.

Regular folder and chat buckets sort by:

1. effective activity time descending;
2. `created_at` descending;
3. `id` descending.

The pinned bucket sorts by:

1. effective activity time descending;
2. `pinned_at` descending;
3. `id` descending.

Delegation children retain the existing `created_at DESC, id DESC` order. A
root's expanded descendants move with that root but never independently change
rank.

### Effective activity time

For a root summary, the presentation helper returns the later valid timestamp
from:

- authoritative `summary.updated_at`; and
- `optimisticActivityById.get(summary.id)?.effectiveAt`.

The same helper drives the comparator and `formatRelative`, so list position and
the right-side label cannot disagree during the optimistic window.

Optimistic milliseconds are monotonic within the client:

```text
effectiveMs = max(Date.now(), previousOptimisticMs + 1, baselineMs + 1)
```

This gives two genuine dispatches a deterministic latest-first order even when
they occur in the same clock millisecond.

## Optimistic Activity State

Add presentation-only state beside, not inside, the authoritative summaries:

```ts
interface OptimisticConversationActivity {
  token: string
  baselineUpdatedAt: string
  effectiveAt: string
}
```

The workspace store exposes three narrow operations:

```ts
beginConversationActivity(id: number): string | null
rollbackConversationActivity(id: number, token: string): void
applyConversationStatePatch(patch: ConversationStatePatch): void
```

`beginConversationActivity` returns `null` for an unknown id. The global root
list does not contain delegation children, so this also prevents accidental
child-to-root promotion. A new begin replaces any older optimistic entry for
that id and returns a fresh token.

Rollback removes an entry only when both id and token still match. A failure
from an older request therefore cannot erase a later dispatch.

The store also advances a small activity sequence with the affected root id for
optimistic begins and accepted newer state patches. The animation controller
consumes this sequence. Refreshes, hydration, pin changes, filtering, and folder
drag do not advance it.

`updateConversationLocal` must stop inventing `updated_at` for title and manual
status changes. It continues updating those requested fields optimistically but
leaves the existing timestamp untouched; the backend upsert supplies their
authoritative timestamp. This restores a single ownership rule for every value
in `DbConversationSummary.updated_at`.

## Dispatch Triggers

### Normal prompts

Begin optimistic activity at the innermost frontend dispatch boundary, after a
connection has been found and any requested mode change has succeeded, but
immediately before calling `acpPrompt`.

Resolve the row id from the explicit prompt option first and the connection's
bound conversation id second. This boundary covers:

- a normal existing-conversation send;
- a queued draft when it actually flushes;
- the send following a successful fork;
- the legacy free-text question response, which is sent as a prompt.

It deliberately does not cover queue insertion or the outer composer handler.
Those paths may defer, reject, or still be creating a conversation row.

On any prompt rejection, including `TurnInProgress`, roll back the matching
token before propagating the existing error or requeue behavior. On success,
leave the overlay in place until authoritative reconciliation.

A newly created conversation may not yet be visible in the root store when its
first prompt dispatches. In that case begin is a no-op; the fresh row already has
a current backend timestamp and naturally appears at the top.

### Structured question answers

The blocking `ask_user_question` answer uses a dedicated endpoint rather than
`acpPrompt`. The root `ConversationTabView` answer wrapper therefore begins
activity immediately before `acpActions.answerQuestion` and rolls it back if the
answer request rejects. A successful answer remains optimistic until the
continuing turn emits its next state patch.

The sub-agent session dialog does not perform this root bump. Child activity
remains local to the child by design.

### Assistant activity

Do not create client timestamps for assistant start or completion.

- Prompt start already persists `InProgress` plus a fresh `updated_at` and emits
  `conversation://changed` kind `state`.
- Turn completion already persists `PendingReview` or `Cancelled` plus a fresh
  `updated_at` and emits the same state-patch shape.

The existing `AppWorkspaceProvider` subscription applies these patches. If
another conversation became newer during a long turn, the completion patch can
raise this root again. Stream tokens do not change activity.

Manual rename and manual status operations continue to reorder after their
backend upsert because those backend operations already bump `updated_at`. They
receive no additional optimistic bump and are not required to animate.

## Authoritative Reconciliation

Timestamp parsing uses the existing invalid-to-zero convention. State fields
are treated as one versioned tuple:

```text
(status, awaiting_reply_token, updated_at)
```

When applying a state patch:

1. Ignore the tuple when its timestamp is older than the current authoritative
   summary timestamp.
2. Apply it when equal or newer.
3. Clear a matching optimistic entry only when the accepted timestamp is newer
   than that entry's `baselineUpdatedAt`.
4. Advance the activity sequence only when authoritative time advanced, not for
   an idempotent equal-timestamp replay.

This prevents a stale state event from acknowledging a send or moving a row
backward. Clearing on an advance from the baseline, rather than comparing with
the client `effectiveAt`, also works when browser and server clocks differ.
Once cleared, the new server timestamp remains the newest value in the server's
own timestamp domain and should retain the correct bucket rank.

Full upserts still need to carry metadata that may not bump activity, including
pin and automatic-title changes. If an upsert is older than the current summary,
merge its metadata but preserve the current state tuple. If it is equal or
newer, replace normally and reconcile the optimistic entry with the same
baseline rule. This is an ordering safeguard, not a general per-field conflict
resolution system.

### Refresh races

`refreshConversations` currently replaces the complete array, so an old request
can overwrite a newer event-applied state. Add request ordering and a store
conversation revision:

- Only the latest refresh request may commit.
- Advance the conversation revision on every authoritative conversation-array
  mutation: state patch, upsert, delete, or local field patch. The separate
  optimistic overlay and activity sequence do not advance this revision.
- Capture the revision when a request starts.
- If the revision is unchanged on response, replace from the snapshot after
  applying deletion tombstones and the optimistic baseline reconciliation rule.
- If the revision changed, merge snapshot rows by id with current rows using the
  monotonic state-tuple rule, then append current-only non-tombstoned rows in
  their existing relative order. Downstream bucket sorting supplies final
  display order.

An uncontended later refresh still performs a clean replacement, so a server
deletion missed by the event channel is eventually removed. Optimistic overlays
remain separate during either form of refresh and are pruned for ids known to be
deleted.

## Row And Block Model

Extend the flattened conversation-row metadata with:

```ts
rootId: number
bucketKey: "pinned" | "chat" | `folder:${number}`
```

`pushConversationRow` receives the root id and bucket key and passes both
unchanged while recursively appending descendants. The resulting contiguous
rows with the same `rootId` are one animation block.

Every virtual row gets a stable inner wrapper marked with its row key, root id,
and bucket key. The child element's existing stable React key continues to give
`virtua` stable item identity. The new wrapper is the only element whose
`transform` or `opacity` the animation controller owns; `virtua` retains sole
ownership of its absolute-positioned outer item.

## Animation Controller

### Eligibility

Animate only when all of the following are true:

- the workspace activity sequence advanced;
- a root's rank changed inside the same bucket;
- the bucket has the same root membership before and after;
- the row structure is otherwise stable; and
- no folder drag, hydration, refresh replacement, filter change, section or
  folder collapse, delegation expansion, or pin transfer is in progress.

The controller consumes every sequence even when eligibility fails, preventing
an old activity signal from animating a later unrelated layout change.

### FLIP over painted wrappers

The controller stores geometry only for currently rendered inner wrappers,
including `virtua`'s bounded buffer. Before a qualifying React update, its
layout-effect cleanup captures current visual rectangles and commits/cancels any
in-flight animations. After the new layout commits, the next layout effect:

1. captures the new wrapper rectangles;
2. restores the viewport anchor when required;
3. computes `deltaY = first.top - last.top` for stable painted keys;
4. animates from `translateY(deltaY)` to `translateY(0)` for 230 ms with
   `cubic-bezier(0.2, 0, 0, 1)`; and
5. clears controller-owned styles and references on finish or cancellation.

Because every row in an expanded root subtree changes by the same layout delta,
the root and descendants read as one moving block. Painted rows displaced by the
promotion animate from their former positions and make room.

If the promoted root did not have a first rectangle because it was offscreen,
do not invent a slide path. If it is mounted at the destination, fade its inner
wrapper from opacity 0 to 1 for 120 ms while stable displaced wrappers use their
real deltas, using the same easing curve. If it remains outside the rendered
window, no animation is created.

A second eligible reorder during the 230 ms window commits the current computed
visual state, cancels the old animations, and uses that visual state as the next
First snapshot. Animations never stack. User scrolling, folder dragging,
unmounting, or an ineligible structural change cancels all active animations and
rebases the geometry cache.

The controller must not set `virtua`'s `shift` prop. That option preserves an
end-anchored reverse-infinite list and is not valid for arbitrary mid-list
permutations.

### Reduced motion

With `prefers-reduced-motion: reduce`, reorder and anchor correction still
occur, but all transforms and fades are skipped. No animation-specific text or
announcement is added; semantic row order changes immediately in the DOM.

## Scroll Anchoring

The sidebar explicitly disables browser overflow anchoring. For an eligible
activity reorder, preserve the first fully visible stable rendered row that is
not part of the promoted block:

1. record its row key and viewport-relative top in the First snapshot;
2. find the same wrapper after reorder;
3. adjust the real OverlayScrollbars viewport by the top-position delta before
   paint; and
4. remeasure Last geometry after the correction before starting animations.

At absolute scroll top there is no correction, allowing the promoted row to
become visible naturally. If no stable anchor survives, keep the current scroll
offset and skip correction. The feature never calls `scrollToActive` and never
automatically reveals the promoted conversation.

Programmatic anchor correction is marked internally so it does not trigger the
user-scroll cancellation path. Existing sticky-folder recomputation runs after
the corrected offset.

## Error Handling

- A failed or busy prompt rolls back only its matching optimistic token, then
  preserves the existing requeue or error behavior.
- A failed structured question answer rolls back its matching token and leaves
  the question card retryable.
- A stale rollback cannot remove a newer optimistic dispatch.
- A stale state patch cannot replace a newer state tuple or acknowledge an
  optimistic entry.
- A dropped state event leaves the optimistic value until a later state patch,
  upsert, or reconnect refresh observes a timestamp newer than its baseline.
- Unknown or deleted roots cannot gain optimistic state.
- Invalid timestamps sort as zero, matching existing behavior.
- Animation failures degrade to the correct final order without affecting data
  state. All animation and frame handles are disposed on cancellation/unmount.

## Testing

### Pure ordering and row model

- Regular folder and chat roots always sort by effective updated time with the
  documented tie-breakers.
- Pinned roots use effective updated time, then `pinned_at`, then id.
- Worktree roots sort together inside the mapped parent display bucket.
- Delegation children remain created-time ordered.
- Expanded descendants receive their root id and bucket key and stay contiguous
  when the root moves.
- The card label and comparator resolve the same effective timestamp.

### Workspace store

- Begin is a no-op for unknown or child-only ids.
- Monotonic begins return tokens and the latest begin wins.
- Matching rollback clears; stale-token rollback does not.
- A newer state patch applies the state tuple, clears the matching optimistic
  entry, and advances activity sequence.
- Equal replays are idempotent; older patches are ignored.
- An older full upsert preserves the newer state tuple while applying metadata.
- `updateConversationLocal` never changes authoritative `updated_at`.
- Concurrent refresh/event tests prove old snapshots cannot regress time or
  remove a newly event-created root, while a later uncontended refresh can
  remove a server-missing row.

### Dispatch paths

- Queue insertion does not begin activity; queue flush does at actual dispatch.
- A mode-change failure and a missing connection do not begin activity.
- Normal, fork-following, and legacy question prompt sends use the same inner
  dispatch boundary.
- Busy and failed sends roll back the correct token before requeue/error.
- A successful send retains its optimistic entry until an authoritative advance.
- Root structured question answers begin and roll back correctly; child answers
  do not promote a root.

### Animation and anchoring

- Pure permutation classification accepts only same-bucket root reorders and
  rejects membership or structural changes.
- Visible root-subtree rows and displaced rows receive the expected deltas.
- An offscreen promoted root uses fade-only behavior when it first mounts.
- A second activity cancels and rebases rather than stacking animations.
- User scroll and folder drag cancel; programmatic anchor correction does not.
- Anchor math preserves the first stable row's viewport offset.
- Reduced motion creates no WAAPI animations.
- The existing per-minute relative-time tick does not rebuild the flat row model
  or animate.

The current Vitest sidebar mock deliberately renders every row and cannot prove
real windowing behavior. Do not add a browser framework for this feature. In
addition to focused Vitest coverage, perform mandatory manual smoke tests in a
desktop WebView and server browser with a large seeded conversation list.

## Manual Smoke

1. Put at least 100 roots across regular, worktree-merged, chat, and pinned
   buckets; expand one root with multiple child rows.
2. Send to a visible non-top root and verify its time changes to `now`, its whole
   expanded block rises, and painted displaced rows make room once.
3. Send to an offscreen root while scrolled in the middle and verify the current
   reading position stays fixed and the app does not auto-scroll.
4. Exercise queue flush, fork-send, legacy question reply, and structured
   question answer; verify queueing alone never changes rank.
5. Force `TurnInProgress` and a transport failure; verify there is no lasting
   phantom bump and the draft/question remains recoverable.
6. Complete another conversation during a long turn, then finish the first; its
   authoritative completion patch should raise it again only if rank changes.
7. Pin/unpin, filter completed rows, expand/collapse a folder or subtree, refresh,
   and drag folders; each changes layout without a swap animation.
8. Trigger two activity changes within 230 ms and verify motion retargets without
   a snap or stacked transform.
9. Repeat with reduced motion and verify every reorder is instantaneous.
10. Repeat from two clients against server mode and verify both converge on the
    same authoritative final order.

## Acceptance Criteria

1. Sidebar root cards and their right-side labels use the same effective activity
   time, with no user-selectable created-time mode.
2. A real user dispatch immediately shows `now` and raises an existing root in
   its current bucket; queueing or rejection does not leave a bump.
3. Assistant start and completion reorder from existing authoritative state
   patches without additional client timestamps or duplicate animations.
4. Pinned roots reorder inside Pinned by activity; regular, chat, and mapped
   worktree roots reorder only inside their own display bucket.
5. Delegation children keep their existing order and move only with their root.
6. Painted root-subtree and displaced rows transition for 230 ms; offscreen
   endpoints are never given a fabricated path.
7. Reorder does not steal scroll position or auto-reveal the active root.
8. Stale patches, upserts, refreshes, and rollbacks cannot move authoritative
   activity backward or erase a newer optimistic dispatch.
9. Reduced-motion users receive the correct instant layout with no animation.
10. Existing sidebar memoization, sticky headers, folder drag, desktop mode, and
    server mode remain functional.

## Alternatives Considered

| Approach | Decision |
|---|---|
| Updated sort plus no movement animation | Rejected: correct ordering but does not meet the exchange-animation requirement. |
| Refined optimistic overlay plus painted-row FLIP under `virtua` | Selected: preserves authority, virtualization, and the requested interaction. |
| Write client time directly into `summary.updated_at` | Rejected: state patches and stale snapshots can overwrite it and cause backward jumps. |
| Add client bumps for assistant start and completion | Rejected: the backend already writes and broadcasts both boundaries. |
| Full-list `getBoundingClientRect` FLIP | Rejected: offscreen nodes do not exist and transforming virtualizer-owned positioning conflicts with layout. |
| Replace `virtua` with Motion layout | Rejected: larger performance regression surface and still cannot animate unmounted endpoints. |
| Promote a root when a child is active | Rejected: outside the approved product semantics. |

---

*Next step after written-spec approval: create the implementation plan with the
writing-plans skill.*
