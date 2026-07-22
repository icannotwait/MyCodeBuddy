# Final Fix Report: view-subagent-as-tab-mru-close (whole-branch review)

## Status

**DONE**

Important findings from the whole-branch review of view-subagent-as-tab-mru-close
are fixed and verified:

1. **openTab stamps `activationSeq`** on every activation path (existing,
   pin, preview replace, append).
2. **Escape overlay guards** expanded to match file-workspace; **mod+w is not
   blocked** by open dialogs/menus.
3. **Live child connection attach** verified already works via
   `conversationId` discovery — no ACP refactor.

## Commits

```text
556942021515f887c5e9a93ad8473d8fd2f39439
fix(tabs): stamp openTab MRU seq and Escape-only overlay guards
```

Files in this fix set:

- `src/stores/tab-store.ts`
- `src/stores/tab-store-close-mru.test.ts`
- `src/components/tabs/tab-bar.tsx`
- `.superpowers/sdd/final-fix-report.md`

## Fix 1: openTab must stamp activationSeq

### Problem

`switchTab` / `closeTab` / `detachTab` already used `stampActiveTab`, but
`openTab` set `activeTabId` without stamping. Opening a sub-agent child via
`openTab` left the child (and re-activated parents) without a fresh
`activationSeq`, so `closeTab` MRU could prefer an older high-seq sibling
instead of the true previous tab (parent).

### Fix

Every `openTab` branch that activates a tab now calls
`stampActiveTab(rawTabs, newActiveId)`:

- existing tab activate (with optional pin)
- existing tab activate (non-pin, active change)
- new pinned tab append
- preview replace
- unpinned append when no preview slot

Also:

- `nextActivationSeq` advances past both the module counter **and** any
  leftover `activationSeq` already on tabs (restore tokens / seeded state).
- `resetTabStore` resets `activationSeqCounter` to 0.

### Tests (TDD-style proof)

Added in `src/stores/tab-store-close-mru.test.ts`:

- `parent → other → parent → child → close(child) returns to parent`
- `stale high leftover on other loses after openTab re-activates parent then child`
- `openTab stamps newly appended and activated existing tabs`
- `preview replace openTab stamps the new active tab`

Command:

```powershell
pnpm test -- src/stores/tab-store-close-mru.test.ts
```

Result: **GREEN** — 9/9 passed.

## Fix 2: Escape overlay guards; do not block mod+w

### Problem

`tab-bar.tsx` skipped **both** Escape and configured close (mod+w) when any
`role=dialog[data-state=open]` was present. That regressed pre-Escape behavior
where mod+w still closed the tab. Escape guards were also thinner than
file-workspace (no alertdialog / radix menu focus check).

### Fix

In `src/components/tabs/tab-bar.tsx`:

- Overlay guards run **only when `isEscapeClose` is true**.
- `isConfiguredClose` (mod+w) always proceeds when an active tab exists.
- Escape guards expanded to match `file-workspace-tab-bar.tsx`:
  - `role=dialog[data-state=open]`
  - `role=alertdialog`
  - focus inside radix popper/menu wrappers / `role=menu`

Out of scope: no change to file-workspace Esc.

### Tests

`should-close-tab-on-escape` pure predicate suite unchanged (still GREEN).
File-workspace escape guard suite remains GREEN for the pattern we mirrored.

## Fix 3: Live child connection attach (verify only)

### Claim

Final reviewer suggested a live child connection may not attach when opening
as a main tab.

### Verification (no code change)

1. **Main tab surface already passes `conversationId`**

   `ConversationSessionSurface` → `useConnectionLifecycle`:

   ```ts
   conversationId: dbConversationId ?? undefined
   workingDir: workingDir ?? folder?.path
   ```

2. **Auto-connect forwards conversationId to `connect()`**

   `useConnectionLifecycle` auto-connect effect (when `isActive` +
   `workingDir`) calls:

   ```ts
   connConnect(agentType, workingDir, sessionId, conversationId, ...)
   ```

3. **`connect()` attaches by conversationId for live owners**

   `acp-connections-context.tsx` (~5374–5421): when
   `conversationId != null && conversationId > 0`, calls
   `acpFindConnectionForConversation` and, if another client owns the live
   connection, `connectAsViewer(...)` — does not spawn a new agent.

4. **Delegation children with null workingDir on the tab**

   Child tabs opened via `openTab` carry `folderId`. Surface resolves
   `workingDirForConnection = workingDir ?? folder?.path`, so auto-connect
   still runs with the folder path once the folder is known. Empty/missing
   workingDir would skip auto-connect (pre-existing gate; not a one-line
   attach bug). No ACP large-refactor performed.

Conclusion: **attach-by-conversationId already works** for delegated children
opened as main tabs. Document only.

## Test commands and results

### Covering suite (all GREEN)

```powershell
pnpm test -- `
  src/stores/tab-store-close-mru.test.ts `
  src/stores/tab-store-popout.test.ts `
  src/lib/should-close-tab-on-escape.test.ts `
  src/lib/open-delegated-child-session.test.ts `
  src/hooks/use-connection-lifecycle.test.ts `
  src/components/files/file-workspace-tab-bar.test.tsx
```

```text
 ✓ src/lib/should-close-tab-on-escape.test.ts (5 tests)
 ✓ src/stores/tab-store-close-mru.test.ts (9 tests)
 ✓ src/stores/tab-store-popout.test.ts (12 tests)
 ✓ src/lib/open-delegated-child-session.test.ts (7 tests)
 ✓ src/components/files/file-workspace-tab-bar.test.tsx (25 tests)
 ✓ src/hooks/use-connection-lifecycle.test.ts (10 tests)

 Test Files  6 passed (6)
      Tests  68 passed (68)
```

### Focused MRU stamp suite

```powershell
pnpm test -- src/stores/tab-store-close-mru.test.ts
```

```text
 ✓ src/stores/tab-store-close-mru.test.ts (9 tests)
 Test Files  1 passed (1)
      Tests  9 passed (9)
```

## Remaining concerns

1. **Escape guards are inline in `tab-bar.tsx`** — not extracted to a shared
   helper with file-workspace. Behavior is aligned; optional follow-up to
   share a pure guard for unit tests without mounting TabBar.
2. **`openTab` does not re-stamp when the target is already active** (matches
   `switchTab`). Re-clicking the same conversation in the sidebar does not
   bump MRU; activation only stamps on real active changes / new opens.
3. **Auto-connect still requires `workingDir`** — if a child conversation’s
   folder path never resolves, connect/attach will not run until path is
   available. Pre-existing; not introduced by this branch.
4. **SubAgentSessionDialog stays removed** — out of scope to reintroduce.

## Out of scope (confirmed not done)

- Reintroduce `SubAgentSessionDialog`
- Change file-workspace Esc behavior
- Large ACP attach refactor
