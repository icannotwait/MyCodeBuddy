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

Persist the preference under one global `localStorage` key. A missing,
unreadable, or invalid value resolves to `false`, so a fresh profile and a
storage failure both retain the current ignore-aware default. The setting is
shared across workspaces in the same client profile.

All ten locale files receive the new visible label. The preference helper is a
small client module with explicit load and save functions so its default and
failure behavior can be unit tested independently of the large file-tree
component.

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
dropped. While show-ignored mode is active, each backend-debounced envelope
with changed paths schedules one root-tree refresh. Refreshes are coalesced so
at most one request is active and one follow-up refresh is queued when events
arrive during that request.

Lazy directory expansion calls `get_file_tree` with the current display mode.
The existing lazy-cache invalidation remains driven by changed paths. Manual
reload refreshes the workspace stream for Git state and explicitly refreshes
the one-shot root tree when show-ignored mode is active.

Turning the option off immediately discards the one-shot source and renders the
latest pruned tree already held by the workspace-state store. Folder switches
clear one-shot and lazy caches and fetch the selected folder using the current
global preference.

Every asynchronous full-tree response is guarded by folder, mode, and request
generation. A response started for an old folder or old option value cannot
replace the current tree.

### Backend Tree Contract

Extend `get_file_tree` and its TypeScript wrappers with an optional
`includeIgnored` argument that defaults to `false` in both Tauri and web modes.
The synchronous builder receives the resolved boolean:

- `false`: use the ignore-aware walk introduced by `d7df476e`.
- `true`: disable Git/custom ignore matching but retain deterministic ordering,
  depth bounds, no symlink following, and hard exclusions.

The display option does not expose `.git`, `__pycache__`, or `.DS_Store`; those
remain hard exclusions. It also does not change workspace search results.

## Obsolete Search Cancellation

Each live search consumer owns a stable, globally unique `searchId`, created on
the client only when its first search is issued. The composer reference search
and the command dialog use separate IDs, so they cannot cancel each other even
when searching the same workspace. IDs are also unique across windows and web
clients.

Extend `search_workspace_files` with an optional `searchId`. The backend keeps
a bounded-by-active-consumers registry from search ID to cancellation token.
Registering a new request for an existing ID cancels the previous token before
starting the new blocking walk. The walker checks cancellation before handling
each entry and exits without continuing the filesystem scan.

Completion removes a registry entry only when it still contains that request's
token. An older request finishing after its replacement must not remove the
replacement token. Calls without a search ID retain standalone behavior for
backward compatibility and tests.

Cancellation is an expected stale-result outcome, not an application error.
The cancelled request resolves with an empty, non-truncated result; the
frontend's existing abort/generation guard discards it. This avoids noisy HTTP
500 responses and logs while still stopping disk work promptly.

## Query Result Freshness

The command dialog stores the query alongside the file result it answers.
Changing the query makes the prior result stale immediately. While the 200 ms
debounce or backend search is pending, stale rows are not rendered and cannot
be selected. Success installs the returned rows only when folder, query, tab,
and request generation are still current. Failure installs an empty result for
the current query.

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
  until it succeeds. Failure leaves that tree in place and exposes a
  non-blocking retryable error; later filesystem events or manual reload retry.
- A failed background refresh keeps the last successful include-ignored tree
  rather than blanking the panel.
- Cancelled searches are silent stale work. Real walk errors retain the current
  API error behavior and frontend empty-result fallback.

## Testing Strategy

Implementation follows red-green TDD for each behavior.

### Rust

1. Prove `build_file_tree_sync(..., include_ignored=false)` prunes all three
   ignore-file types and `include_ignored=true` returns their matched entries.
2. Prove the async/default API resolves omitted `include_ignored` to false.
3. Prove a pre-cancelled search token exits with an empty result.
4. Prove registering a replacement search ID cancels the prior token and that
   finishing the prior request cannot remove the replacement.
5. Prove `Ä.TXT` matches both `ä` and `Ä` queries.

### Frontend

1. Prove the preference defaults to false, persists true/false, and tolerates
   unavailable storage.
2. Prove `getFileTree` and `searchWorkspaceFiles` serialize the new optional
   arguments in both transport-facing wrappers.
3. Prove a query change hides earlier command-dialog file rows before the new
   promise resolves.
4. Prove stale responses cannot replace current results and separate consumers
   send stable, distinct search IDs.
5. Add source/behavior coverage for the checkbox menu, include-ignored root
   refresh, lazy-load mode propagation, and event-driven refresh coalescing.
6. Keep the existing reference-search and workspace-state suites green.

## Non-Goals

- Making ignored files appear in mention search or command-palette search.
- Adding per-workspace preference storage.
- Creating a second mode-specific workspace-state stream.
- Showing hard-excluded metadata/cache paths.
- Adding fuzzy ranking, locale collation, or full Unicode normalization.

## Acceptance Criteria

1. A fresh profile hides ignored file-tree entries without an extra full-tree
   request.
2. The global checkbox survives reloads and shows ignored entries when enabled.
3. Show-ignored mode stays current through existing filesystem event batches,
   lazy expansion, manual reload, and folder changes.
4. Obsolete searches for one consumer stop walking without cancelling another
   consumer.
5. Old command-dialog rows are never selectable for a new query.
6. Uppercase non-ASCII file names match lowercase queries.
7. Desktop and server transports compile and behave consistently.
8. Focused tests, frontend build, Rust desktop/server checks, and relevant
   clippy commands pass.
