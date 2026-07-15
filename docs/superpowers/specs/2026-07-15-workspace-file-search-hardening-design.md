# Workspace File Tree and Search Hardening Design

## Goal

Keep ignore-aware workspace search and the default lightweight file tree while
allowing users to opt into viewing ignored files, and fix the three search
correctness and resource-management regressions identified in commit
`d7df476e`.

## Approved Scope

This change delivers four related behaviors:

1. Add a global, persisted "Show ignored files" file-tree option. It defaults
   to off.
2. Stop obsolete workspace file searches instead of leaving their blocking
   directory walks running.
3. Never leave results from an earlier file query visible or selectable while
   a newer query is pending.
4. Preserve case-insensitive matching for non-ASCII file names and paths.

Workspace mention search and command-palette file search always honor ignore
rules. The display option affects only the auxiliary file tree.

## File Tree Display Option

### User Interface and Persistence

Add a checkbox menu item labeled "Show ignored files" to both the workspace
root context menu and the file-tree background context menu. Extend the shared
context-menu wrapper with its native checkbox-item primitive rather than
representing a binary setting as a plain command.

Persist the preference under the global `localStorage` key
`workspace:file-tree-show-ignored`. A missing, unreadable, or invalid value
resolves to `false`, so a fresh profile and a storage failure both retain the
current ignore-aware default. The setting is shared across workspaces in the
same client profile. A same-window
`codeg:file-tree-show-ignored-changed` custom event and the browser `storage`
event keep the preference reactive across mounted consumers and other windows,
following the existing office-preview preference pattern. The reactive hook
starts from `false` during server render and hydrates the stored value after
mount, avoiding an SSR/client state split.

All ten locale files receive the new visible label. The preference helper is a
small client module with explicit load and save functions so its default and
failure behavior can be unit tested independently of the large file-tree
component.

The one-shot tree lifecycle lives in a focused hook rather than adding another
set of request-generation and coalescing branches directly to the existing
2,000-line file-tree component. The hook consumes the folder path, fallback
workspace tree, workspace sequence, and envelope subscription; it returns the
active source tree, display mode, loading state, a setter, and an explicit
refresh command. It also accepts a user-visible error callback tagged as
`enable` or `manual`; background refresh failures keep the last successful
tree but do not call that callback. `FileTreeTab` remains responsible for
rendering, lazy child overrides, Git styling, and user-visible toasts.

### Default Mode

With the option off, the auxiliary tree continues to use
`useWorkspaceStateStore`. Its depth-two snapshots remain pruned during the
backend walk by `.gitignore`, `.ignore`, `.rgignore`, Git global/exclude rules,
and the existing hard exclusions. No extra full-tree request is made.

### Show-Ignored Mode

Turning the option on does not create a second workspace-state stream. Instead,
the file-tree component requests a one-shot depth-two tree through
`get_file_tree` with `include_ignored=true` and uses that response as its root
tree source while the option remains enabled.

The existing workspace stream continues to provide Git state, health, and
filesystem event envelopes. Those envelopes already include ordinary paths
matched by Git ignore files; only the existing hard watch exclusions are
dropped. While show-ignored mode is active, a root-tree refresh is scheduled
when an envelope represents create/remove activity, changes an ignore-control
file, or carries an empty changed-path list that requires a conservative
sweep. Ordinary file-content `modify` envelopes do not rebuild the root tree.

Live seq-gap/overflow recovery can replace the workspace snapshot without
forwarding a normal envelope. The hook therefore remembers the latest envelope
sequence and also observes the workspace-store sequence. A store sequence that
advances without a matching envelope schedules the same conservative sweep;
normal events do not schedule a duplicate refresh. Enabling the mode or
changing folders first baselines the current workspace sequence, so the initial
one-shot load is not immediately followed by a redundant sweep. Refreshes are
coalesced so at most one request is active and one follow-up refresh is queued
when events arrive during that request.

Lazy directory expansion calls `get_file_tree` with the current display mode.
The existing lazy-cache invalidation remains driven by changed paths. Manual
reload refreshes the workspace stream for Git state and explicitly refreshes
the one-shot root tree when show-ignored mode is active.

Every folder or display-mode transition increments one tree-generation counter
and clears lazy child caches before applying the new source. Otherwise children
fetched in pruned mode would override an include-ignored root with incomplete
data, or children fetched in include-ignored mode would leak ignored rows after
the option is turned off. Expanded directories are then reloaded under the new
mode.

Root and lazy-child requests capture the folder, mode, and tree generation and
verify all three before writing. The in-flight directory structure records
`path -> generation` instead of a plain path set. An old request may delete its
in-flight marker in `finally` only if the stored generation still matches; it
cannot clear or overwrite a newer request for the same directory.

Turning the option off immediately discards the one-shot source and renders the
latest pruned tree already held by the workspace-state store. Folder switches
clear one-shot and lazy caches and fetch the selected folder using the current
global preference. Ignore-rule preview reads and muted-path computation run
only while show-ignored mode is active.

Every asynchronous tree response is guarded by folder, mode, and tree
generation. A response started for an old folder or old option value cannot
replace the current root or lazy children.

### Backend Tree Contract

Extend `get_file_tree` and its TypeScript wrappers with an optional
`includeIgnored` argument that defaults to `false` in both Tauri and web modes.
The synchronous builder receives the resolved boolean:

- `false`: use the ignore-aware walk introduced by `d7df476e`.
- `true`: disable Git/custom ignore matching but retain deterministic ordering,
  depth bounds, no symlink following, and hard exclusions.

The display option retains the current hard exclusions precisely: `.git` and
`__pycache__` directories remain pruned, `.DS_Store` files remain pruned, and
symlinked directories are not followed. A linked worktree's existing `.git`
control file behavior is unchanged. The option also does not change workspace
search results.

## Obsolete Search Cancellation

Each live search consumer owns a stable, globally unique `searchSessionId`,
created on the client only when its first search is issued. Each actual query
also receives a fresh `requestId`. The composer reference search and the
command dialog use separate session IDs, so they cannot cancel each other even
when searching the same workspace. Both identifiers are unique across windows
and web clients and are generated with the repository's `randomUUID()` helper.

Extend `search_workspace_files` with optional `searchSessionId` and `requestId`
fields. The backend keeps a registry from session ID to the active request ID
and cancellation token. Registering a new request for an existing session
cancels the previous token before starting the new blocking walk. The walker
checks cancellation before handling each entry and exits without continuing
the filesystem scan.

Add `cancel_workspace_file_search(searchSessionId, requestId)` in both Tauri
and web transports. Query-change, folder-change, popup-close, tab-change, and
component-unmount cleanup issue this command for the request they started. The
backend cancels only when both IDs still match the registry entry. A delayed
cancel from an old query therefore cannot cancel the replacement query.
Starting a replacement request still cancels its predecessor as a fallback
when the explicit cancel call is delayed or lost.

The cancel command atomically removes a matching registry entry before
cancelling its token. Mutex ordering makes both races safe: if cancellation
wins, the replacement inserts afterward; if replacement wins, the old request
ID no longer matches and the delayed cancel is a no-op.

Registration returns an RAII lease held across semaphore acquisition and the
blocking task. Dropping the command future for any reason cancels its token and
removes the registry entry only when the entry still contains that lease's
request ID and token. This covers HTTP disconnects and aborted callers, where
cleanup placed only after `.await` would never run. An older request finishing
after its replacement must not remove the replacement token. Calls without a
session ID and request ID retain standalone behavior for backward compatibility
and direct tests. Supplying exactly one ID, an empty ID, or an ID longer than
128 bytes returns `invalid_input`; partial identifiers must not silently create
an uncancellable request. Search and explicit-cancel commands share this
validation.

A dedicated global semaphore permits at most four active workspace search
walks. Waiting for a permit is cancellation-aware, and cancellation is checked
again after acquisition before `spawn_blocking` starts. The owned semaphore
permit moves into the blocking closure and is released only after the walker
actually exits. Dropping the async command future therefore cannot admit a
fifth walk while its detached, newly-cancelled blocking task is still winding
down. Per-consumer replacement stops stale work; the semaphore also bounds
simultaneous work from separate windows, consumers, or session IDs.

Cancellation is an expected stale-result outcome, not an application error.
The cancelled request resolves with an empty, non-truncated result; the
frontend's existing abort/generation guard discards it. This avoids noisy HTTP
500 responses and logs while still stopping disk work promptly.

## Query Result Freshness

Move command-dialog file searching into a focused hook that owns the stable
session ID, per-query request ID, debounce timer, request generation, and
query-tagged result. The dialog renders only the hook's current rows.

The hook stores the query and folder alongside the file result they answer.
Changing either makes the prior result stale immediately. While the 200 ms
debounce or backend search is pending, stale rows are not returned, rendered,
or selectable. Success installs the returned rows only when folder, query,
enabled state, and request generation are still current. Failure installs an
empty result for the current query. Effect cleanup explicitly cancels the
active request with its matching request ID.

This mirrors the existing query-tagged contract in `suggestion-popup.tsx`
instead of relying only on a loading boolean.

## Unicode Case Matching

Normalize the trimmed query, file name, and relative path with Rust's Unicode
`to_lowercase()` before substring matching. Do not mix Unicode query lowering
with ASCII-only candidate lowering. This restores behavior for names such as
`Ä.TXT` queried with `ä` while preserving existing ASCII behavior.

Full Unicode normalization and locale-specific collation are outside scope;
the contract remains lowercase substring matching.

## Error Handling

- Preference read/write failures are ignored and fall back to the default off
  value.
- The initial include-ignored request keeps the currently rendered pruned tree
  until it succeeds. If that first request fails, the hook reverts and persists
  the option to off so a checked control never claims to show data that was not
  loaded; `FileTreeTab` emits one error toast. The user retries by enabling the
  option again.
- A failed background refresh keeps the last successful include-ignored tree
  rather than blanking the panel and does not toast on every filesystem event.
  A failed user-initiated manual refresh emits one error toast.
- Cancelled searches are silent stale work. Real walk errors retain the current
  API error behavior and frontend empty-result fallback.

## Testing Strategy

Implementation follows red-green TDD for each behavior.

### Rust

1. Prove `build_file_tree_sync(..., include_ignored=false)` prunes all three
   ignore-file types and `include_ignored=true` returns their matched entries.
2. Prove the async/default API resolves omitted `include_ignored` to false.
3. Prove a pre-cancelled search token exits with an empty result.
4. Prove both identifiers may be omitted for compatibility while partial,
   empty, and over-128-byte identifiers are rejected.
5. Prove registering a replacement in one search session cancels the prior
   token and that finishing or dropping the prior lease cannot remove the
   replacement.
6. Prove explicit cancellation requires both session and request IDs: a delayed
   cancel for an old request cannot cancel its replacement.
7. Prove semaphore waiting is cancellable and no more than four blocking walks
   can be admitted concurrently.
8. Prove `Ä.TXT` matches both `ä` and `Ä` queries.

### Frontend

1. Prove the preference defaults to false, persists true/false, tolerates
   unavailable storage, and reacts to same-window and `storage` events.
2. Prove `getFileTree` and `searchWorkspaceFiles` serialize the new optional
   arguments and that `cancelWorkspaceFileSearch` reaches both
   transport-facing wrappers.
3. Test the extracted command-dialog search hook with fake timers: a query or
   folder change hides earlier rows before the new promise resolves, stale
   responses cannot replace current results, and one mounted consumer sends a
   stable session ID with fresh request IDs and cancels the matching request on
   cleanup.
4. Test separate hook instances to prove they send distinct session IDs.
5. Test the extracted ignored-tree hook: default mode makes no extra request;
   enabling requests `includeIgnored=true`; structural/ignore/sweep events
   coalesce; ordinary modify events do not scan; seq-only recovery forces a
   sweep; stale responses cannot replace current state; initial failure reverts
   the preference; background failure preserves the last tree.
6. Test mode-transition integration around `FileTreeTab` helpers: lazy caches
   clear, expanded directories reload with the new `includeIgnored` value, and
   old-generation lazy responses neither write children nor clear a newer
   in-flight marker for the same path. Verify ignore styling is disabled in
   default mode. Keep one focused source assertion only for wiring both
   context-menu checkbox locations.
7. Keep the existing reference-search and workspace-state suites green.

## Non-Goals

- Making ignored files appear in mention search or command-palette search.
- Adding per-workspace preference storage.
- Creating a second mode-specific workspace-state stream.
- Showing hard-excluded metadata/cache paths.
- Exactly muting entries ignored only by Git global configuration or
  `.git/info/exclude`; muted styling continues to use workspace-visible
  `.gitignore`, `.ignore`, and `.rgignore` files.
- Adding fuzzy ranking, locale collation, or full Unicode normalization.

## Acceptance Criteria

1. A fresh profile hides ignored file-tree entries without an extra full-tree
   request.
2. The global checkbox survives reloads and shows ignored entries when enabled.
3. Show-ignored mode stays current through existing filesystem event batches,
   seq-gap/overflow recovery, lazy expansion, manual reload, and folder
   changes.
4. Obsolete searches for one consumer stop walking without cancelling another
   consumer.
5. No more than four workspace search walks run concurrently, and dropped
   callers release their registry entries.
6. Closing or changing a search explicitly cancels its active request, and a
   delayed cancel cannot stop a newer request.
7. Switching display mode cannot reuse lazy children loaded under the opposite
   mode.
8. Old command-dialog rows are never selectable for a new query.
9. Uppercase non-ASCII file names match lowercase queries.
10. Desktop and server transports compile and behave consistently.
11. Focused tests, frontend build, Rust desktop/server checks, and relevant
   clippy commands pass.
