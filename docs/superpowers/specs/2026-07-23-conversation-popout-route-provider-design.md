# Conversation Popout Route Provider Design

Date: 2026-07-23

Status: Design approved in conversation; written-spec review pending

## Summary

Prevent detached conversation windows from crashing when their session surface
renders `BranchDropdown`. The detached provider tree must include
`WorkbenchRouteProvider`, matching the context contract already established by
the main workspace.

This is a provider-wiring correction. It does not change branch operations,
workbench navigation, conversation popout handoff, ACP ownership, or connection
lifecycle behavior.

## Incident Evidence

The failing production window was the detached Grok conversation route for
conversation `1266`. The main workspace and Settings window remained healthy.
An isolated development replay mounted the same detached session surface and
captured the uncaught exception:

```text
Error: useWorkbenchRoute must be used within WorkbenchRouteProvider
    at useWorkbenchRoute
    at BranchDropdown
```

The detached page renders `ConversationSessionSurface` inside
`DetachedShellProviders`. `BranchDropdown`, reached through the conversation
context bar, calls `useWorkbenchRoute`. The main workspace wraps this surface
in `WorkbenchRouteProvider`; `DetachedShellProviders` currently does not.

## Goals

1. Preserve the invariant that every rendered conversation session surface has
   a `WorkbenchRouteProvider` ancestor.
2. Stop detached conversation windows from falling into the generic Next.js
   client-side exception page.
3. Add a regression test that fails with the observed missing-provider error.
4. Keep the existing strict behavior of `useWorkbenchRoute` outside a valid
   provider tree.

## Non-Goals

- Change `BranchDropdown` behavior or hide it in detached windows.
- Add a nullable or fallback workbench route context.
- Add workbench navigation UI to detached windows.
- Change conversation popout ready/commit-ack handoff.
- Change ACP connection ownership, auto-connect, or teardown behavior.
- Refactor the main workspace provider hierarchy.

## Alternatives Considered

### Selected: Add the Provider to the Detached Shell

Wrap detached children with the existing `WorkbenchRouteProvider` inside
`WorkspaceProvider`. This restores the same context contract as the main
workspace and protects all current and future descendants, not only the
component that exposed the omission.

### Wrap Only `BranchDropdown`

This would stop the current stack trace but assign provider ownership to a leaf
component. Any other detached descendant calling `useWorkbenchRoute` would
still crash, and the true shell-level dependency would remain undocumented.

### Give `useWorkbenchRoute` a Default Value

This would avoid the exception globally, but it would weaken the hook's useful
invariant and hide future provider-wiring mistakes. The main and detached
shells should satisfy the contract explicitly instead.

## Design

### Provider Ownership

`DetachedShellProviders` imports `WorkbenchRouteProvider` and nests it directly
inside `WorkspaceProvider`:

```tsx
<WorkspaceProvider>
  <WorkbenchRouteProvider>{children}</WorkbenchRouteProvider>
</WorkspaceProvider>
```

The provider initializes its existing in-memory default route,
`"conversations"`. Calls such as `openConversations()` remain valid and local
to the detached React tree. No new route state is persisted or synchronized
with the main window.

The provider has no dependency on `TabProvider`, so the detached shell keeps
its intentional memory-only synthetic tab seeding and continues to omit opened
tab hydration and persistence.

### Error Handling

No fallback is added. `useWorkbenchRoute` continues to throw when a caller is
truly outside the provider contract. The fix removes the known invalid tree
rather than masking the error at the hook or component level.

## Testing Strategy

Follow red-green order:

1. Add a focused component test for `DetachedShellProviders`.
2. Replace unrelated heavyweight providers with transparent test boundaries,
   while keeping the real detached shell and real workbench route context.
3. Render a probe child that calls `useWorkbenchRoute` and exposes its default
   route state.
4. Verify the test initially fails with the observed
   `WorkbenchRouteProvider` error.
5. Add the provider to the detached shell and verify the focused test passes.
6. Run related conversation popout tests, the full frontend test suite, ESLint,
   and the static export build.

## Risks and Mitigations

### Provider Ordering Drift

The detached tree is a curated subset of the main workspace tree. Placing the
route provider inside `WorkspaceProvider` follows the main workspace's
ownership direction without adding unrelated providers. The focused test makes
the required contract explicit.

### Detached Navigation Side Effects

`WorkbenchRouteProvider` owns only local React state and defaults to the
conversation route. The detached page renders no workbench route selector, so
the added state cannot navigate the main window or persist a competing route.

### Test Over-Mocking

Mocks are limited to adjacent providers whose internals are irrelevant to this
wiring contract. The test uses the real `DetachedShellProviders`, real
`WorkbenchRouteProvider`, and real `useWorkbenchRoute` hook, so it fails for
the exact production omission.

## Acceptance Criteria

1. A detached conversation session surface renders without the
   `useWorkbenchRoute` missing-provider exception.
2. `BranchDropdown` retains its current behavior and strict context use.
3. `DetachedShellProviders` supplies the route context for all descendants.
4. The regression test fails before the provider addition and passes after it.
5. Related tests, full Vitest, ESLint, and `pnpm build` pass.
6. No popout handoff, ACP lifecycle, or tab persistence behavior changes.
