# View Sub-Agent Session as Tab + MRU Close Design

## Context

Opening a delegated sub-agent transcript via「查看会话」currently mounts
`SubAgentSessionDialog`, which always remounts a full `MessageListView`,
refetches detail, and tears down the runtime session on close. Large child
transcripts (hundreds of tool calls) feel laggy even when the same child is
already open as a main-window tab, because the dialog never reuses that tab.

Users want「查看会话」to behave like opening a conversation: focus an existing
tab or open one. Esc should close that tab and return to the previously viewed
tab (typically the parent agent). Closing a tab via the tab chrome (×, middle
click, context menu) should use the same return rule.

## Goals

- Replace the sub-agent session dialog with `openTab` focus-or-open for the
  child conversation.
- Esc closes the active conversation tab (when no higher-priority UI claims it).
- When the closed tab was active and other tabs remain, pick the next active tab
  by **MRU** (`activationSeq`), matching `detachTab`.
- All paths that call `closeTab` share that MRU behavior (×, middle-click, Esc,
  `closeConversationTab`, etc.).

## Non-goals

- Tool-level virtualization, detail API projection, or transcript payload
  thinning for large child sessions.
- A hard-coded parent_id navigation stack or “only return to parent agent”
  rule (MRU covers the parent-after-view-session case without special casing).
- File workspace tabs, terminal tabs, or non-conversation tab close semantics.
- Configurable shortcut settings for Esc (fixed Escape for this change).
- Keeping `SubAgentSessionDialog` as a fallback path.

## Decisions (approved)

1. Approach **B**: open/focus tab for view; Esc closes the active tab; next tab
   is MRU-based.
2. **All** conversation-tab closes that go through `closeTab` use MRU when
   replacing the active tab — not Esc-only.

## Behavior

### View session (「查看会话」)

**Entry points**

- `DelegatedSubThread` header action
- `SubAgentOverlay` row open action

**Action**

Call existing `openTab(folderId, childConversationId, agentType, pin = false,
title?)`.

| Condition | Behavior |
|-----------|----------|
| Valid child id + agentType + folderId | Focus existing tab or create/replace preview tab via current `openTab` rules |
| Child already open | Activate only |
| Child detached | Existing `focusDetachedConversation` path; do not mirror into main |
| Missing id / agentType / folderId | No-op (keep button non-actionable when id missing) |

**folderId resolution order**

1. Workspace conversation list entry for the child (`folder_id`)
2. Active tab’s `folderId` (delegated children share the parent folder in normal use)
3. If still unresolved → do not open

**agentType**: from the delegation card model. Missing → do not open.

**UI**: full conversation pane (interactive). Permission and ask-question flows
use the main panel paths. Do not mount `SubAgentSessionDialog`.

### Esc closes active tab

- Listen for `keydown` Escape in the conversation workspace shell (or
  equivalent always-mounted chat chrome).
- Invoke only when no higher-priority surface owns Escape (open Radix
  Dialog / AlertDialog / dropdown menu / combobox, etc.). Prefer
  “event not defaultPrevented / no modal open” checks consistent with other
  dismiss handlers in the app.
- Action: `closeTab(activeTabId)` when `activeTabId` is set.
- Not added to user shortcut settings in this change.

### closeTab → MRU

When closing the **currently active** tab and remaining tabs are non-empty:

1. Choose `nextActive` as the remaining tab with the highest `activationSeq`.
2. If all seqs are missing or ≤ 0, fall back to the previous neighbor-index
   rule (`Math.min(index, next.length - 1)`).
3. Stamp the new active tab via existing `stampActiveTab` so MRU stays coherent.

When the closed tab is **not** active, only remove it (unchanged).

When no tabs remain, keep existing empty/replacement-draft behavior.

**Shared helper**: extract `pickMruTabId(tabs, options?)` (or equivalent) used by
both `closeTab` and `detachTab` so MRU logic does not drift.

**Paths that automatically inherit MRU** (anything calling `closeTab`):

- Tab × close
- Middle-click close (if wired to `closeTab`)
- Context menu “Close”
- Esc handler
- `closeConversationTab`

**Unchanged**:

- `closeOtherTabs` / `closeAllTabs` / `closeTabsByFolder` (not “close one active
  and pick successor” in the same sense)
- Non-conversation tab systems

### activationSeq hygiene

`switchTab` and successful `openTab` activation already stamp `activationSeq`.
No new stamp sites are required for the parent→child→Esc→parent path beyond
ensuring `openTab` activation continues to stamp (already true).

## Architecture

```text
[查看会话 click]
    → resolve { folderId, childConversationId, agentType, title? }
    → openTab(...)
    → MessageListView in main tab (existing)

[Esc | tab × | middle-click | closeConversationTab]
    → closeTab(tabId)
    → if was active && remaining.length > 0
         next = pickMruTabId(remaining) || neighborFallback
    → stampActiveTab + recomputeTabs
```

Suggested small frontend helpers (names flexible):

- `resolveDelegatedChildOpenTarget(...)` — folder/agent/title resolution
- Optional `openDelegatedChildSession(...)` wrapping `openTab` for both entry
  points

Esc listener lives near conversation workspace chrome so it is active whenever
conversation tabs are, without depending on a specific message component.

## Testing

1. **tab-store unit**: close active tab with multiple stamped seqs → highest
   remaining seq becomes active; zero/missing seqs → neighbor fallback; close
   non-active tab does not change active.
2. **detachTab still MRU** after helper extraction (no regression).
3. **View session entries**: click openDetail calls `openTab` with resolved
   ids; does not render `SubAgentSessionDialog`.
4. **Esc**: with active tab and no modal, Escape calls `closeTab(active)`;
   when a dialog is open, Esc does not close the tab (or is swallowed by the
   dialog first).

## Migration / cleanup

- Stop importing and mounting `SubAgentSessionDialog` from
  `delegated-sub-thread` and `sub-agent-overlay`.
- Remove or leave orphaned `sub-agent-session-dialog.tsx` only if nothing else
  references it; prefer delete when unused to avoid dead dialog path.
- Update component tests that mock the dialog to expect `openTab` instead.
- i18n keys for `openDetail` can stay (label still means open conversation).

## Risks

| Risk | Mitigation |
|------|------------|
| Esc conflicts with input/modals | Guard on open overlays; respect preventDefault |
| folderId wrong for rare cross-folder children | Prefer list `folder_id`; fall back carefully |
| Users who liked neighbor-based close | Accepted product change: all close uses MRU |
| Preview tab replace semantics surprise | Unchanged `openTab` preview rules |

## Success criteria

- From parent,「查看会话」opens/focuses child tab with no modal dialog.
- Esc closes that tab and returns to the previously focused tab (parent in the
  common path).
- Tab × on the same active child yields the same successor as Esc.
- Large child transcripts benefit from the main tab path (no second cold dialog
  tree); further transcript perf is out of scope.
