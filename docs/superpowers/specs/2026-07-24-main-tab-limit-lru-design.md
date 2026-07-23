# Main-window conversation tab limit (LRU eviction)

**Date:** 2026-07-24  
**Status:** Approved for implementation planning  
**Scope:** Main-window conversation tabs only (`tab-store` / `TabBar`)

## Problem

Users can accumulate unbounded conversation tabs in the main window. The existing pin/preview model only protects a tab from *preview replacement*; it does not cap total open tabs, and pin is easy to miss in day-to-day use. We need a hard limit on simultaneous main-window conversation tabs, with automatic eviction when a new tab would exceed the limit.

## Goals

- Cap main-window conversation tabs at **10** simultaneous tabs.
- When opening a new tab would exceed the cap, close the **least recently activated** tab(s) (LRU).
- Leave pin / preview-replacement behavior unchanged (no pin UI work this round).
- Detached (pop-out) conversation windows are **out of scope** for the count.
- File workspace tabs (`FileWorkspaceTabBar`) are **out of scope**.

## Non-goals

- Changing pin UX (icons, unpin, context-menu pin).
- Persisting `activationSeq` across restarts.
- User-facing “about to close” toast/animation (optional follow-up).
- Configurable limit in settings (fixed constant for v1).
- Exempting pinned tabs from eviction.

## Chosen approach: LRU via existing `activationSeq`

Alternatives considered:

| Approach | Idea | Why not |
|----------|------|---------|
| A – FIFO `openSeq` | Evict earliest-opened | Can close the user’s main working tab if it was opened first but still in active use |
| B – Leftmost tab strip order | Evict left edge | Breaks after user reorders tabs |
| **C – LRU `activationSeq` (chosen)** | Evict lowest activation order | Protects the working set; reuses existing field |

**Product decision:** Prefer evicting the least-recently-activated tab over strict “first opened,” so switching among existing tabs updates who is safe from eviction.

## Behavior matrix

| Scenario | Behavior |
|----------|----------|
| Activate an already-open tab | Switch only; stamp `activationSeq`; no eviction |
| Preview replace (`openTab` with `pin=false` replaces first unpinned) | Count unchanged → no eviction |
| Append would make `length > 10` | Evict lowest-`activationSeq` tab(s) until `length ≤ 10`, then keep the new tab active |
| Tabs with missing/`0` `activationSeq` (e.g. post-hydrate) | Treat as oldest; ties broken by **lower index in `rawTabs`** (left-to-right) |
| Hydrate / remote `tabs://changed` already has `> 10` | Same eviction rules; **prefer retaining** the resolved `activeTabId` |
| New tab is active | New tab id is in the keep set and is never evicted in that operation |
| Pin state | **No exemption** — pinned tabs can be LRU-evicted |
| Detached windows | Not counted toward the 10 |

Constant: `MAX_MAIN_CONVERSATION_TABS = 10` defined once in `src/stores/tab-store.ts`.

## Architecture

All logic lives in **`src/stores/tab-store.ts`** (single choke point for open/hydrate/restore). UI components keep calling existing actions; no TabBar API change required.

### Helper: `evictTabsToLimit`

```text
evictTabsToLimit(
  tabs: TabItemInternal[],
  options: { keepTabIds: ReadonlySet<string> | string[] }
) → { tabs: TabItemInternal[]; evictedIds: string[] }
```

Rules while `tabs.length > MAX_MAIN_CONVERSATION_TABS`:

1. Candidate set = tabs whose `id` is **not** in `keepTabIds`.
2. If candidate set is empty, stop (should not happen if keep set is small).
3. Pick victim: minimum `(activationSeq ?? 0)`; on ties, smallest index in the current array.
4. Remove victim; append id to `evictedIds`; repeat.

Callers that open a new tab should pass `keepTabIds` including at least the new tab id (and the intended `activeTabId` if different).

### Call sites that can increase tab count

Apply eviction **before** `set` when the post-mutation list would exceed the limit:

1. **`openTab`** — only branches that **append** a new tab (pinned open, or all tabs already pinned then unpinned preview append). Not the “activate existing” or “preview replace in place” branches.
2. **`openNewConversationTab` / `openChatModeTab`** — when creating a new draft because no singleton draft exists.
3. **`restoreDetachedTab`** — re-inserting a popped-out tab into the main strip.
4. **`hydrate` / remote tabs apply** — after building `restored` / applied list, if `length > 10`, evict with `keepTabIds` containing the resolved active id when present.

### Removal semantics vs `closeTab`

- Eviction removes tabs from `rawTabs` without the “closed last tab → spawn replacement draft” path unless the final list would be empty (should not occur when opening a new tab).
- Prefer a shared internal `removeTabsFromState(ids, { preserveActivePreference })` or: build next array via `evictTabsToLimit`, then single `set` with `activeTabId` = new tab, `stampActiveTab` on the survivor list.
- **Do not** call `closeTab` in a loop in a way that reassigns active via MRU between steps and leaves the wrong final active tab.
- Side effects that today follow close (e.g. preview-replaced callbacks, ACP disconnect for removed sessions) should remain consistent with existing close/replace behavior where the product already tears down resources for closed tabs. Match existing `closeTab` / preview-replace patterns; do not invent new disconnect policy in this change unless an existing path already requires it for removed tab ids.

### Interaction with existing `activationSeq`

| Use | Direction |
|-----|-----------|
| LRU eviction | Prefer **lowest** positive/zero seq |
| Close-tab MRU focus | Prefer **highest** seq (`pickMruTabId`) |

Same field, opposite ends — intentional and consistent with “recently used.”

`activationSeq` remains **memory-only** (not written to `opened_tabs`). After cold start, all restored tabs look equally old until the user activates them; tie-break is strip order.

## Data flow (open beyond limit)

```text
openTab / openNew… (append path)
  → construct newTab
  → next = [...rawTabs, newTab]  (or equivalent insert)
  → { tabs, evictedIds } = evictTabsToLimit(next, { keepTabIds: [newTab.id] })
  → set({ rawTabs: stampActiveTab(tabs, newTab.id), activeTabId: newTab.id })
  → recomputeTabs()
  → activateConversationPane()
```

## Error handling / edge cases

- **Existing tab re-open:** no count increase; only activation stamp.
- **Exactly 10, preview replace:** still 10; no eviction.
- **Keep set larger than limit:** theoretically impossible for v1 (keep is new + maybe active). If it occurs, stop when no candidates remain and leave length > 10 rather than deleting kept ids (defensive).
- **Tile mode:** still subject to the same main-window tab list and limit.

## Testing

Add/extend unit tests under `src/stores/` (e.g. `tab-store-tab-limit.test.ts` or extend existing tab-store tests):

1. With 10 tabs of increasing `activationSeq`, open 11th → length 10, lowest-seq tab gone, new tab active.
2. Switch to an early-opened tab (raise its seq), then open 11th → that early tab is **kept**, a colder tab is evicted.
3. Preview replace (`pin=false` with an unpinned slot) → length unchanged; no spurious eviction.
4. Hydrate 11 items with a known active → length 10 and active retained when possible.
5. `restoreDetachedTab` at capacity → length ≤ 10, restored tab active, some LRU victim removed.

Regressions: existing close-MRU and openTab activationSeq tests in `tab-store-close-mru.test.ts` / popout tests should still pass.

## Implementation notes

- Export `MAX_MAIN_CONVERSATION_TABS` and/or `evictTabsToLimit` only if tests need them; otherwise keep helper module-private and test via public actions.
- No i18n strings required for v1 (silent eviction).
- No backend/schema change: `opened_tabs` continues to persist whatever remains after eviction on the next debounced save.

## Success criteria

- Main window never shows more than 10 conversation tabs after any open/hydrate/restore path covered above.
- Actively used tabs (high `activationSeq`) survive under pressure better than idle ones.
- Pin/preview behavior and detached windows behave as before, aside from the new cap.
