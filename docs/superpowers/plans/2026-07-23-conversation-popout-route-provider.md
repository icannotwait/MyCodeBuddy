# Conversation Popout Route Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure detached conversation windows provide the workbench route context required by their session surface and no longer crash when rendering `BranchDropdown`.

**Architecture:** Keep `useWorkbenchRoute` strict and restore its existing shell-level contract by nesting `WorkbenchRouteProvider` inside the detached shell's `WorkspaceProvider`. Protect the contract with a component test that uses the real detached shell and real workbench route hook while replacing unrelated heavyweight providers with transparent boundaries.

**Tech Stack:** React 19, TypeScript strict mode, Vitest, Testing Library, Next.js 16

**Design:** `docs/superpowers/specs/2026-07-23-conversation-popout-route-provider-design.md`

## Global Constraints

- Do not change `BranchDropdown`, `useWorkbenchRoute`, or their error semantics.
- Do not change conversation popout ready/commit-ack handoff or ACP ownership.
- Do not add `TabProvider`; detached tabs remain memory-only and unpersisted.
- Do not refactor the main workspace provider hierarchy.
- Stage and commit only the frontend test and detached shell files; preserve unrelated working-tree changes such as `src-tauri/Cargo.toml`.

## File Map

- Create `src/app/conversation/_components/detached-shell.test.tsx`: regression coverage for the detached shell's route-context contract.
- Modify `src/app/conversation/_components/detached-shell.tsx`: add the existing `WorkbenchRouteProvider` to the detached provider tree.

---

### Task 1: Restore the detached route-context contract

**Files:**
- Create: `src/app/conversation/_components/detached-shell.test.tsx`
- Modify: `src/app/conversation/_components/detached-shell.tsx`

**Interfaces:**
- Consumes: `WorkbenchRouteProvider({ children }: { children: ReactNode })` and `useWorkbenchRoute()` from `@/contexts/workbench-route-context`.
- Produces: `DetachedShellProviders` guarantees that every descendant can call `useWorkbenchRoute()` and initially observes `routeId === "conversations"` and `isConversations === true`.

- [ ] **Step 1: Write the failing provider-contract test**

Create `src/app/conversation/_components/detached-shell.test.tsx` with the following content:

```tsx
import type { ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { DetachedShellProviders } from "./detached-shell"

vi.mock("@/contexts/alert-context", () => ({
  AlertProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/task-context", () => ({
  TaskProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  AcpConnectionsProvider: ({ children }: { children: ReactNode }) => children,
  useAcpActions: () => ({ registerOpenTabKeys: () => {} }),
}))

vi.mock("@/contexts/conversation-runtime-context", () => ({
  ConversationRuntimeProvider: ({ children }: { children: ReactNode }) =>
    children,
}))

vi.mock("@/contexts/delegation-context", () => ({
  DelegationProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/git-credential-context", () => ({
  GitCredentialProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/workspace-context", () => ({
  WorkspaceProvider: ({ children }: { children: ReactNode }) => children,
}))

function RouteProbe() {
  const { routeId, isConversations } = useWorkbenchRoute()
  return (
    <output data-testid="route-context">
      {routeId}:{String(isConversations)}
    </output>
  )
}

describe("DetachedShellProviders", () => {
  it("provides the workbench route context to detached session children", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})
    try {
      render(
        <DetachedShellProviders>
          <RouteProbe />
        </DetachedShellProviders>
      )
      expect(screen.getByTestId("route-context")).toHaveTextContent(
        "conversations:true"
      )
    } finally {
      consoleError.mockRestore()
    }
  })
})
```

- [ ] **Step 2: Run the focused test and verify the red state**

Run:

```powershell
pnpm test -- src/app/conversation/_components/detached-shell.test.tsx
```

Expected: FAIL with `useWorkbenchRoute must be used within WorkbenchRouteProvider`. This proves the test reaches the same strict hook that caused the production crash.

- [ ] **Step 3: Add the minimal provider wiring**

In `src/app/conversation/_components/detached-shell.tsx`, add the import:

```tsx
import { WorkbenchRouteProvider } from "@/contexts/workbench-route-context"
```

Replace the detached tree's leaf:

```tsx
<WorkspaceProvider>{children}</WorkspaceProvider>
```

with:

```tsx
<WorkspaceProvider>
  <WorkbenchRouteProvider>{children}</WorkbenchRouteProvider>
</WorkspaceProvider>
```

Do not change the surrounding provider order or add a fallback to the hook.

- [ ] **Step 4: Run focused and related tests and verify the green state**

Run:

```powershell
pnpm test -- src/app/conversation/_components/detached-shell.test.tsx src/app/conversation/_components/detached-bootstrap-flow.test.ts src/lib/conversation-popout-detached-bootstrap.test.ts
```

Expected: all selected test files pass with no uncaught provider error.

- [ ] **Step 5: Run the full frontend verification suite**

Run each command independently:

```powershell
pnpm test
pnpm eslint .
pnpm build
```

Expected:

- `pnpm test`: all Vitest tests pass.
- `pnpm eslint .`: exits successfully with no lint errors.
- `pnpm build`: Next.js static export completes successfully.

- [ ] **Step 6: Review and commit the fix**

Inspect only the intended files:

```powershell
git diff -- src/app/conversation/_components/detached-shell.tsx src/app/conversation/_components/detached-shell.test.tsx
git status --short
```

Confirm `src-tauri/Cargo.toml` remains unstaged, then commit only the fix:

```powershell
git add src/app/conversation/_components/detached-shell.tsx src/app/conversation/_components/detached-shell.test.tsx
git commit -m "fix(ui): provide route context in conversation popouts"
```

Expected: the commit contains exactly the detached shell and its new regression test.
