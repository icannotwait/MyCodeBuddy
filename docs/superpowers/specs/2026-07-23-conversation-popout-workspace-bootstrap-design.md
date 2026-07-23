# Conversation Pop-out Workspace Bootstrap Design

Date: 2026-07-23

Status: Approved

## Summary

Detached conversation windows currently mount the session UI without the
workspace-state lifecycle used by the main window. This leaves two related
gaps:

1. A cold detached conversation cannot auto-connect to ACP because its
   persisted summary is absent from `useAppWorkspaceStore.conversations`.
2. Its branch selector shows no branch because `branches` and `gitHeads` are
   never populated by active-folder Git HEAD polling.

The fix will mount the existing `AppWorkspaceProvider` in the detached shell
and immediately seed the exact conversation summary and folder metadata that
the detached page has already loaded. The detached window will continue to
omit `TabProvider`, preserving its memory-only tab behavior.

## Root Cause

### Cold ACP connection

`ConversationSessionSurface` treats a positive conversation id as persisted.
Its durable auto-connect policy intentionally rejects a persisted conversation
when no authoritative summary exists in `useAppWorkspaceStore.conversations`.

The detached page loads `DbConversationDetail`, but currently seeds only the
folder and synthetic tab. Its per-webview workspace store therefore has no
summary, so `autoConnectAllowed` remains false after the pop-out commit ack.
The live path still works because it claims an already-running connection
instead of relying on cold auto-connect.

### Branch display

`BranchDropdown` reads the current branch from the workspace store's
`branches` and `gitHeads` maps. In the main window, `AppWorkspaceProvider`
polls `getGitHead()` for the active folder and updates both maps. The detached
shell does not mount that provider. Seeding a `FolderDetail` alone does not
populate Git HEAD state, and the database `git_branch` field is commonly null.

## Goals

- After a cold pop-out reaches handoff commit ack, automatically connect ACP
  with the existing detached `ownerOperationId` lease.
- Show the detached conversation's real Git branch and keep it updated while
  the window remains open.
- Reuse the main window's established workspace-state lifecycle.
- Preserve the live connection claim/rebind protocol unchanged.
- Keep detached tabs memory-only and never hydrate or persist the main opened
  tab set.

## Non-goals

- Changing ACP ownership, rebind, commit-ack, or close compensation semantics.
- Allowing cancelled conversations to bypass the durable reconnect policy.
- Adding workspace sidebar, tab strip, aux panels, or terminal UI to the
  detached window.
- Extending pop-out support to web or remote workspaces.

## Considered Approaches

### 1. Existing lifecycle plus exact state seed (selected)

Mount `AppWorkspaceProvider` in the detached shell, seed the already-loaded
summary and folder immediately, and let the provider own ongoing subscriptions
and Git HEAD polling.

This reuses tested behavior, avoids a cold-connect dependency on a full-list
request, and prevents detached-specific synchronization logic from drifting.

### 2. Detached-only workspace bootstrap

Implement a smaller detached component that fetches the summary, polls Git
HEAD, and subscribes to relevant events. This reduces unrelated initialization
but duplicates lifecycle code and creates another synchronization contract to
maintain.

### 3. Relax the safety gate

Permit auto-connect without a workspace summary and use `FolderDetail`'s
database branch as the display fallback. This weakens the deliberate durable
connection guard and does not reliably resolve branch state because the stored
branch is often null or stale.

## Architecture

The detached provider tree becomes:

```text
RemoteConnectionGate
  AppWorkspaceProvider
    AlertProvider
      GitCredentialProvider
        TaskProvider
          AcpConnectionsProvider
            ConversationRuntimeProvider
              DelegationProvider
                WorkspaceProvider
                  WorkbenchRouteProvider
                    detached conversation page
```

`AppWorkspaceProvider` is a state-lifecycle component, not the full workspace
layout. It does not mount the sidebar, tab strip, aux panels, or `TabProvider`.
It supplies the initial workspace fetches, ACP agent registry subscription,
conversation/folder change subscriptions, and active-folder Git HEAD polling
that shared session components already expect.

The detached metadata effect will seed state in this order:

1. Upsert the loaded `FolderDetail` into the per-window workspace store.
2. Seed a non-null `folder.git_branch` as an immediate fallback when present.
3. Upsert `DbConversationDetail.summary` through
   `applyConversationUpsert()`.
4. Seed the memory-only tab and set its folder as active.

The exact summary seed is required even though `AppWorkspaceProvider` also
refreshes all conversations. It makes cold connection independent of full-list
latency or a transient full-list failure.

## Data Flow

### Cold conversation

1. The detached route mounts `AppWorkspaceProvider` and begins normal workspace
   initialization.
2. The page loads the exact conversation detail and folder.
3. The page seeds the summary, folder, synthetic tab, and active folder.
4. Discovery confirms there is no live ACP connection and emits ready.
5. The main window completes the handoff and sends commit ack.
6. The detached surface mounts active. Its durable policy finds the seeded
   summary and permits `useConnectionLifecycle` to auto-connect.
7. The connect call retains the pop-out `ownerOperationId` lease.

### Live conversation

The existing rebind and `claimConnectionOwnership` path is unchanged. Seeded
workspace state only supplies shared UI state around the already-claimed
connection.

### Branch state

1. Seeding the synthetic tab sets the detached folder as active.
2. `AppWorkspaceProvider` resolves its path and immediately calls
   `getGitHead()`.
3. `applyGitHead()` updates both `branches` and `gitHeads`.
4. `BranchDropdown` rerenders with the branch name or detached-HEAD label.
5. Existing polling keeps external branch changes synchronized.

## Error Handling

- Exact conversation or folder load failure keeps the existing detached-page
  error state and does not allow ACP connection.
- A full conversation-list refresh failure does not block a cold connection,
  because the exact loaded summary has already been seeded.
- A Git HEAD read failure leaves the existing fallback UI in place and uses
  the provider's retry schedule. It does not block ACP or the conversation UI.
- A cancelled summary remains denied by the existing durable auto-connect
  policy; explicit reconnect behavior is unchanged.
- Commit-ack timeout, abort, live rebind failure, and close compensation retain
  their current fail-closed behavior.

## Testing

Tests will follow red-green TDD:

1. Extend the detached shell provider test to prove that children are mounted
   under `AppWorkspaceProvider` while still omitting `TabProvider`.
2. Add a detached state-seeding test proving the loaded folder and conversation
   summary enter the workspace store. The seeded persisted summary must make
   the existing durable auto-connect policy eligible for a non-cancelled
   conversation.
3. Cover active-folder Git HEAD initialization so a detached provider lifecycle
   populates the current branch instead of leaving the branch maps empty.
4. Run the existing cold/live bootstrap, connection lifecycle, workspace
   provider, and route-context regression suites.
5. Run changed-file lint, the full frontend test suite, and the static export
   build.

## Success Criteria

- Popping out a conversation before ACP is connected causes ACP to connect
  automatically after handoff commit ack.
- Popping out an already-connected conversation continues to transfer the live
  connection without spawning a second agent.
- The detached branch selector displays the actual branch or detached HEAD and
  tracks subsequent branch changes.
- The detached window does not hydrate, save, or alter the main opened-tab set.
- Closing or aborting the detached window retains the existing ownership and
  incarnation cleanup guarantees.
