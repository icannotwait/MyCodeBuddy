# Conversation Pop-out Workspace Bootstrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a cold detached conversation auto-connect after handoff commit ack and show its authoritative Git branch.

**Architecture:** Seed the exact loaded conversation summary into the detached webview's workspace store so the existing durable ACP gate can open without waiting for a full-list refresh. Mount the existing `AppWorkspaceProvider` around the minimal detached provider tree so that active-folder Git HEAD polling and workspace event synchronization match the main window, while continuing to omit `TabProvider`.

**Tech Stack:** Next.js 16 static export, React 19, TypeScript strict mode, Zustand, Vitest, Testing Library, pnpm.

## Global Constraints

- Cold ACP connection remains disabled until the pop-out handoff reaches commit ack.
- Live ACP discovery, owner rebind, ownership claim, abort, and close compensation behavior must not change.
- Every detached cold connect must continue forwarding `ownerOperationId`.
- The detached window must not mount `TabProvider`, hydrate opened tabs, or persist the main tab set.
- `AppWorkspaceProvider` supplies state lifecycle only; do not add workspace sidebar, tab strip, aux panels, or terminal UI.
- Cancelled conversations retain the existing durable reconnect policy.
- Pop-out remains local-desktop only; do not expand web or remote-workspace support.
- Do not modify or stage the user's existing `src-tauri/Cargo.toml` change.

---

### Task 1: Seed the exact detached conversation summary for cold ACP

**Files:**
- Modify: `src/app/conversation/_components/detached-shell.test.tsx`
- Modify: `src/app/conversation/_components/detached-shell.tsx`
- Modify: `src/app/conversation/page.tsx`

**Interfaces:**
- Consumes: `DbConversationDetail.summary: DbConversationSummary`
- Consumes: `useAppWorkspaceStore.getState().applyConversationUpsert(summary)`
- Produces: `seedDetachedConversationSummary(summary: DbConversationSummary): void`
- Preserves: `seedDetachedSessionTab(...)` as the only synthetic-tab seed and active-folder selector

- [ ] **Step 1: Add a failing detached summary-seed regression test**

Update the imports in `detached-shell.test.tsx`:

```tsx
import type { ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import {
  DetachedShellProviders,
  seedDetachedConversationSummary,
} from "./detached-shell"
```

Add a complete root-summary fixture and reset the per-test Zustand singleton:

```tsx
const summary: DbConversationSummary = {
  id: 42,
  folder_id: 7,
  title: "Cold pop-out",
  title_locked: false,
  auto_title_finalized: false,
  agent_type: "codex",
  status: "pending_review",
  awaiting_reply_token: null,
  kind: "regular",
  model: null,
  git_branch: null,
  external_id: "session-42",
  message_count: 1,
  child_count: 0,
  created_at: "2026-07-23T00:00:00.000Z",
  updated_at: "2026-07-23T00:00:00.000Z",
  pinned_at: null,
  parent_id: null,
  parent_tool_use_id: null,
  delegation_call_id: null,
}

beforeEach(() => {
  resetAppWorkspaceStore()
})
```

Add the regression:

```tsx
describe("detached workspace state seeding", () => {
  it("seeds the persisted summary used by the durable ACP gate", () => {
    seedDetachedConversationSummary(summary)

    expect(useAppWorkspaceStore.getState().conversations).toEqual([summary])
  })
})
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```powershell
pnpm test -- src/app/conversation/_components/detached-shell.test.tsx
```

Expected: FAIL because `seedDetachedConversationSummary` does not exist yet, or because the summary remains absent from `conversations`. The existing route-provider regression must still pass once the test module loads.

- [ ] **Step 3: Implement the minimal summary seed**

In `detached-shell.tsx`, add `DbConversationSummary` to the type import and export this helper next to `seedDetachedFolder`:

```tsx
import type {
  AgentType,
  DbConversationSummary,
  FolderDetail,
} from "@/lib/types"

export function seedDetachedConversationSummary(
  summary: DbConversationSummary
): void {
  useAppWorkspaceStore.getState().applyConversationUpsert(summary)
}
```

In `page.tsx`, import the helper:

```tsx
import {
  DetachedOpenTabKeysRegistrar,
  DetachedShellProviders,
  seedDetachedConversationSummary,
  seedDetachedFolder,
  seedDetachedSessionTab,
} from "./_components/detached-shell"
```

Seed the exact loaded summary before creating the active synthetic tab:

```tsx
setConversation(c)
setFolder(f)
setError(null)
seedDetachedFolder(f)
seedDetachedConversationSummary(c.summary)
const seededTabId = seedDetachedSessionTab({
  folderId: parsed.folderId,
  conversationId: parsed.conversationId,
  agentType: parsed.agentType,
  workingDir: f.path,
  title: c.summary.title ?? undefined,
})
```

Do not change `resolveDetachedConnectGate`: commit ack remains the point that changes the cold surface to active.

- [ ] **Step 4: Verify GREEN and the cold-connect contracts**

Run:

```powershell
pnpm test -- src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/_components/detached-bootstrap-flow.test.ts src/lib/conversation-popout-detached-bootstrap.test.ts src/components/conversations/conversation-session-surface.test.ts
```

Expected: PASS. In particular, the existing missing-summary policy remains fail-closed, while the detached bootstrap now supplies the summary that policy requires.

- [ ] **Step 5: Commit Task 1 only**

```powershell
git add -- src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/_components/detached-shell.tsx src/app/conversation/page.tsx
git diff --cached --check
git commit -m "fix(ui): seed detached conversation state"
```

Expected: the commit contains only the three frontend files above. `src-tauri/Cargo.toml` remains unstaged.

---

### Task 2: Mount workspace lifecycle and initialize detached branch state

**Files:**
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/app/conversation/_components/detached-shell.test.tsx`
- Modify: `src/app/conversation/_components/detached-shell.tsx`

**Interfaces:**
- Consumes: `AppWorkspaceProvider({ children })`
- Consumes: `getGitHead(activeFolderPath): Promise<GitHeadInfo>`
- Consumes: `useAppWorkspaceStore.getState().applyGitHead(folderId, head)`
- Produces: detached children mounted beneath `AppWorkspaceProvider`
- Preserves: `TabProvider` remains absent from the detached tree

- [ ] **Step 1: Lock the existing active-folder Git HEAD contract**

In `app-workspace-context.test.tsx`, add `waitFor` to the Testing Library
import:

```tsx
import { act, render, screen, waitFor } from "@testing-library/react"
```

Add this property inside the existing `vi.hoisted` object, after
`listAllFolders`:

```tsx
getGitHead: vi.fn(async () => ({
  is_repo: false,
  branch: null,
  detached: false,
  short_sha: null,
})),
```

Inside the existing `vi.mock("@/lib/api", ...)` factory, replace its inline
`getGitHead: vi.fn(...)` entry with:

```tsx
getGitHead: h.getGitHead,
```

Add to `beforeEach`:

```tsx
h.getGitHead.mockReset()
h.getGitHead.mockResolvedValue({
  is_repo: false,
  branch: null,
  detached: false,
  short_sha: null,
})
```

Add this characterization test:

```tsx
describe("AppWorkspaceProvider active-folder Git HEAD sync", () => {
  it("loads and applies the active folder head on mount", async () => {
    const folder = makeFolder({ id: 17, path: "/repo/active" })
    const head = {
      is_repo: true,
      branch: "feature/popout",
      detached: false,
      short_sha: null,
      canonical_repo: "/repo/active",
      head_sha: "0123456789abcdef",
      reference_source_epoch: "v1:test",
    }
    h.listOpenFolders.mockResolvedValue([folder])
    h.listAllFolders.mockResolvedValue([folder])
    h.getGitHead.mockResolvedValue(head)
    useAppWorkspaceStore.setState({
      allFolders: [folder],
      activeFolderId: folder.id,
    })

    await mountProvider()

    await waitFor(() => {
      expect(h.getGitHead).toHaveBeenCalledWith(folder.path)
      expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
        "feature/popout"
      )
    })
    expect(useAppWorkspaceStore.getState().gitHeads.get(folder.id)).toEqual(
      head
    )
  })
})
```

This test should already pass. It records the established behavior that the detached shell will reuse rather than reimplement.

- [ ] **Step 2: Add failing detached-provider and branch-fallback tests**

In `detached-shell.test.tsx`, extend the type and helper imports:

```tsx
import type { DbConversationSummary, FolderDetail } from "@/lib/types"
import {
  DetachedShellProviders,
  seedDetachedConversationSummary,
  seedDetachedFolder,
} from "./detached-shell"
```

Mock the lifecycle provider with a visible boundary:

```tsx
vi.mock("@/contexts/app-workspace-context", () => ({
  AppWorkspaceProvider: ({ children }: { children: ReactNode }) => (
    <div data-testid="app-workspace-provider">{children}</div>
  ),
}))
```

Extend the provider test with this assertion:

```tsx
const route = screen.getByTestId("route-context")
expect(screen.getByTestId("app-workspace-provider")).toContainElement(route)
```

Import `FolderDetail` and `seedDetachedFolder`, define a complete folder fixture, then add two focused seed tests:

```tsx
const folder: FolderDetail = {
  id: 7,
  name: "repo",
  path: "/repo",
  git_branch: "feature/popout",
  default_agent_type: null,
  last_agent_type: null,
  last_opened_at: "2026-07-23T00:00:00.000Z",
  sort_order: 0,
  color: "inherit",
  parent_id: null,
  kind: "regular",
  alias: null,
}

it("seeds a non-null folder branch as an immediate fallback", () => {
  seedDetachedFolder(folder)

  expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
    "feature/popout"
  )
})

it("does not replace a polled branch with a null folder fallback", () => {
  useAppWorkspaceStore.getState().setBranch(folder.id, "feature/live-head")

  seedDetachedFolder({ ...folder, git_branch: null })

  expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
    "feature/live-head"
  )
})
```

- [ ] **Step 3: Run both tests and verify RED at the detached boundary**

Run:

```powershell
pnpm test -- src/contexts/app-workspace-context.test.tsx src/app/conversation/_components/detached-shell.test.tsx
```

Expected: the active-folder characterization passes, while detached tests fail because `AppWorkspaceProvider` is not mounted and `seedDetachedFolder` does not seed a non-null branch.

- [ ] **Step 4: Mount `AppWorkspaceProvider` and seed the branch fallback**

In `detached-shell.tsx`, import the provider:

```tsx
import { AppWorkspaceProvider } from "@/contexts/app-workspace-context"
```

Wrap the current minimal provider tree at its outer boundary:

```tsx
export function DetachedShellProviders({ children }: { children: ReactNode }) {
  return (
    <AppWorkspaceProvider>
      <AlertProvider>
        <GitCredentialProvider>
          <TaskProvider>
            <AcpConnectionsProvider>
              <ConversationRuntimeProvider>
                <DelegationProvider>
                  <WorkspaceProvider>
                    <WorkbenchRouteProvider>
                      {children}
                    </WorkbenchRouteProvider>
                  </WorkspaceProvider>
                </DelegationProvider>
              </ConversationRuntimeProvider>
            </AcpConnectionsProvider>
          </TaskProvider>
        </GitCredentialProvider>
      </AlertProvider>
    </AppWorkspaceProvider>
  )
}
```

Keep `TabProvider` absent. Update `seedDetachedFolder` without allowing null database metadata to overwrite a newer Git HEAD result:

```tsx
export function seedDetachedFolder(folder: FolderDetail): void {
  const store = useAppWorkspaceStore.getState()
  store.upsertFolder(folder)
  if (folder.git_branch) {
    store.setBranch(folder.id, folder.git_branch)
  }
}
```

- [ ] **Step 5: Verify GREEN for provider composition and branch state**

Run:

```powershell
pnpm test -- src/contexts/app-workspace-context.test.tsx src/app/conversation/_components/detached-shell.test.tsx
```

Expected: PASS. The detached route-context assertion remains green beneath the new outer provider, and branch state is initialized without clobbering a polled value.

- [ ] **Step 6: Commit Task 2 only**

```powershell
git add -- src/contexts/app-workspace-context.test.tsx src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/_components/detached-shell.tsx
git diff --cached --check
git commit -m "fix(ui): initialize detached workspace lifecycle"
```

Expected: the commit contains only the three frontend files above. `src-tauri/Cargo.toml` remains unstaged.

---

### Task 3: Regression verification and delivery audit

**Files:**
- Verify only; no planned production edits

**Interfaces:**
- Verifies: cold handoff gate, owner-operation forwarding, live claim, workspace polling, route context, and static export

- [ ] **Step 1: Run the focused detached/ACP/workspace suites**

```powershell
pnpm test -- src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/_components/detached-bootstrap-flow.test.ts src/lib/conversation-popout-detached-bootstrap.test.ts src/lib/conversation-popout-acp-bridge.test.ts src/contexts/app-workspace-context.test.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts src/components/conversations/conversation-session-surface.test.ts
```

Expected: all focused test files pass with zero failures.

- [ ] **Step 2: Run focused lint on every changed TypeScript file**

```powershell
pnpm eslint src/app/conversation/_components/detached-shell.tsx src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/page.tsx src/contexts/app-workspace-context.test.tsx
```

Expected: exit code 0.

- [ ] **Step 3: Run the full frontend suite**

```powershell
pnpm test
```

Expected: exit code 0. Record existing unrelated React `act(...)` warnings separately; do not treat them as introduced failures without a changed assertion or new stack path.

- [ ] **Step 4: Run repository-wide lint**

```powershell
pnpm eslint .
```

Expected target: exit code 0. If the known untouched CRLF `prettier/prettier: Delete CR` findings remain, record them with paths, confirm focused lint is clean, and do not rewrite unrelated files.

- [ ] **Step 5: Run the production static export build**

```powershell
pnpm build
```

Expected: Next.js compiles, TypeScript passes, and all static routes generate successfully.

- [ ] **Step 6: Audit the final diff and worktree**

```powershell
git status --short --branch
git log -5 --oneline --decorate
git diff HEAD~2..HEAD -- src/app/conversation/_components/detached-shell.tsx src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/page.tsx src/contexts/app-workspace-context.test.tsx
```

Expected: the two implementation commits contain only the planned frontend changes. The only unrelated working-tree entry remains the user's unstaged `src-tauri/Cargo.toml` modification.
