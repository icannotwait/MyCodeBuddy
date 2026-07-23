# Session Switch Compositor Persistence Mitigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove inactive non-tiled conversation panels from WebView2 layout and paint without unmounting their React trees or stopping background work.

**Architecture:** Keep the existing `tabs.map` keep-alive ownership and stable `key={tab.id}`. Add the native HTML `hidden` attribute to inactive non-tiled wrappers and remove the visibility-only fallback class; rely on the existing ResizeObserver-based virtualized message layout when the same DOM becomes visible again.

**Tech Stack:** React 19, TypeScript strict mode, Next.js 16 static export, Tailwind CSS v4, Vitest, Virtua, Tauri 2/WebView2.

## Global Constraints

- Inactive conversation React subtrees must remain mounted under stable tab IDs.
- ACP connections, background turns, drafts, local component state, and scroll state must remain alive.
- Visual-only animations may pause while a panel is hidden.
- Tiled mode must continue to display every conversation panel.
- Do not add forced layout reads, transform toggles, timers, synthetic resize events, prefetching, cache changes, or global badge/animation changes.
- Do not include or revert the unrelated existing `src-tauri/Cargo.toml` modification.

---

## File Structure

- Modify `src/components/conversations/conversation-detail-panel-layout.test.ts`: add the focused rendering-contract regression test beside the existing stable-tab-identity assertions.
- Modify `src/components/conversations/conversation-detail-panel.tsx`: apply native `hidden` to inactive non-tiled keep-alive wrappers and remove the `visibility:hidden` fallback.
- No new runtime module or abstraction is introduced; the behavior belongs to the existing tab wrapper.

### Task 1: Remove Inactive Panels From Paint While Keeping Them Mounted

**Files:**
- Modify: `src/components/conversations/conversation-detail-panel-layout.test.ts`
- Modify: `src/components/conversations/conversation-detail-panel.tsx:447`

**Interfaces:**
- Consumes: existing `canTile: boolean`, per-tab `active: boolean`, and stable `tab.id` identity inside `ConversationDetailPanel`.
- Produces: wrapper contract `hidden={!canTile && !active}`; no new exported API.

- [ ] **Step 1: Write the failing layout-contract test**

Add this focused describe block after the `panelSource` fixture declarations and before the existing conversation layout suites:

```ts
describe("ConversationDetailPanel inactive panel paint isolation", () => {
  it("uses native hidden outside tile mode instead of visibility-only hiding", () => {
    expect(panelSource).toContain("hidden={!canTile && !active}")
    expect(panelSource).not.toContain(
      '"absolute inset-0 invisible pointer-events-none"'
    )
  })
})
```

The first assertion encodes the exact non-tiled/inactive predicate. The second prevents regression to a painted `visibility:hidden` keep-alive surface. Existing tests already assert `key={tab.id}`, so do not duplicate that assertion.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```powershell
pnpm test -- src/components/conversations/conversation-detail-panel-layout.test.ts
```

Expected: FAIL in `ConversationDetailPanel inactive panel paint isolation` because `panelSource` does not contain `hidden={!canTile && !active}` and still contains the visibility-only class.

- [ ] **Step 3: Implement the minimal keep-alive paint isolation**

Change only the tab wrapper in `ConversationDetailPanel`:

```tsx
<div
  key={tab.id}
  hidden={!canTile && !active}
  ref={(el) => {
    if (el) {
      tileTabRefs.current.set(tab.id, el)
    } else {
      tileTabRefs.current.delete(tab.id)
    }
  }}
  className={cn(
    canTile
      ? cn(
          "relative h-full min-w-[24rem] flex-1 overflow-hidden",
          index > 0 && "border-l border-border/50"
        )
      : active
        ? "h-full"
        : undefined
  )}
  onPointerDownCapture={
    canTile && !active ? () => switchTab(tab.id) : undefined
  }
>
```

Do not conditionally render `ConversationTabView`, change its key, change `isActive`, or add reactivation effects. The native hidden attribute supplies `display:none`; React continues to own the same mounted subtree.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run:

```powershell
pnpm test -- src/components/conversations/conversation-detail-panel-layout.test.ts
```

Expected: PASS with no warnings. Confirm the pre-existing stable `key={tab.id}` test also passes.

- [ ] **Step 5: Run affected conversation and virtualizer tests**

Run:

```powershell
pnpm test -- src/components/conversations/conversation-detail-panel-layout.test.ts src/components/conversations/conversation-session-surface.test.ts src/components/message/virtualized-message-thread.test.tsx
```

Expected: all selected test files PASS with no unhandled errors or warnings.

- [ ] **Step 6: Inspect the scoped diff**

Run:

```powershell
git diff --check -- src/components/conversations/conversation-detail-panel.tsx src/components/conversations/conversation-detail-panel-layout.test.ts
git diff -- src/components/conversations/conversation-detail-panel.tsx src/components/conversations/conversation-detail-panel-layout.test.ts
```

Expected: only the regression test, native `hidden` attribute, and removal of the old invisible fallback appear. No conditional child render, reflow workaround, unrelated formatting, or `src-tauri/Cargo.toml` content appears.

- [ ] **Step 7: Commit the behavioral fix**

```powershell
git add -- src/components/conversations/conversation-detail-panel.tsx src/components/conversations/conversation-detail-panel-layout.test.ts
git commit -m "fix(ui): stop painting inactive conversation tabs"
```

Expected: one commit containing exactly the two frontend files.

### Task 2: Verify Frontend Gates and Embedded Runtime Behavior

**Files:**
- Verify only; no planned source changes.

**Interfaces:**
- Consumes: the `hidden={!canTile && !active}` wrapper contract from Task 1.
- Produces: test/build evidence and a WebView2 manual acceptance result.

- [ ] **Step 1: Run the full frontend test suite**

Run:

```powershell
pnpm test
```

Expected: Vitest exits 0 with every test passing and no unhandled errors.

- [ ] **Step 2: Run ESLint**

Run:

```powershell
pnpm eslint .
```

Expected: exit 0 with no warnings promoted to errors.

- [ ] **Step 3: Run the static export build**

Run:

```powershell
pnpm build
```

Expected: Next.js static export completes successfully and writes `out/`.

- [ ] **Step 4: Launch the desktop development app**

Run:

```powershell
pnpm tauri dev
```

Expected: the Tauri development window opens against the local Next.js dev server. If another dev server owns the default port, stop this attempt and restart through an available port supported by the repository scripts; do not stop the installed release application.

- [ ] **Step 5: Verify the WebView2 acceptance path**

In the development window:

1. Open a delegation-heavy conversation containing completed and running status pills.
2. Scroll to a position with several status pills and note the visible message anchor.
3. Start or observe background work in that tab.
4. Switch repeatedly to a text-only conversation and back.
5. Confirm the text-only conversation never displays pills from the old tab.
6. Confirm the original tab returns to the same scroll position, preserves its draft, and reflects current background progress.
7. Enable tiled mode and confirm every tile remains visible and selectable.

Expected: no visual persistence, no blank/stale virtual viewport, preserved state, continued background progress, and unchanged tiled behavior.

- [ ] **Step 6: Record any runtime-only failure without speculative fixes**

If reactivation produces a zero-sized or stale virtual viewport, record the exact reproduction and stop. Do not add a resize event or forced layout read in this task; return to systematic debugging and create a separate failing test/design for the observed measurement problem.

- [ ] **Step 7: Confirm final repository state**

Run:

```powershell
git status --short
git show --stat --oneline HEAD
```

Expected: the fix commit contains only the two intended frontend files. The pre-existing `src-tauri/Cargo.toml` modification may remain unstaged and must not be reverted or committed.
