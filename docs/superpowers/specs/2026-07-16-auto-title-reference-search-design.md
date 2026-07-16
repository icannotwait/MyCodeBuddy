# Automatic Conversation Titles and Incremental Reference Search

Date: 2026-07-16

Status: Design approved; written specification awaiting final review

## Summary

This change adds two independent conversation-experience features:

1. A globally selected base agent generates a final automatic title after a
   new root or delegated conversation first completes successfully.
2. The composer `@` picker becomes cache-first, source-independent,
   incrementally paged, regex-capable, selection-stable, and safe after IME
   composition.

The title workflow is backend-owned because it must continue to work for
background conversations, closed tabs, delegated children, desktop mode, and
server mode. The reference-picker interaction remains frontend-owned, while
the backend owns matching, paging, cancellation, and resource validation for
files, conversations, and commits.

## Goals

- Let the user select one enabled base agent as the global automatic-title
  generator, or disable generated titles entirely.
- Generate one title for both root conversations and delegated subagent
  conversations after their first usable successful response.
- Never let a generated title overwrite a manual rename, and never let a
  later native CLI title overwrite a successfully generated title.
- Keep title requests hidden, isolated from the project, non-recursive, and
  limited to user-visible first-turn context.
- Show enabled agents and enabled CodeBuddy delegation profiles immediately
  for a bare `@`, without starting resource searches.
- Return file, conversation, and commit matches independently in fixed pages
  of five, stopping each source when its configured result limit is reached.
- Reuse in-window reference results immediately, refresh them in the
  background, and preserve the selected reference by stable identity.
- Support explicit Rust-compatible regular expressions through an `re:`
  prefix.
- Re-open or refresh the picker after Chinese or other IME composition ends.
- Keep all behavior equivalent through Tauri commands and Axum handlers.

## Non-goals

- Retitling conversations that existed before the feature was enabled.
- Disabling or changing title generation performed internally by third-party
  agent CLIs or providers.
- Per-project title-agent overrides, per-source title-agent mappings, or title
  generation through delegation profiles.
- A persistent reference cache across application restarts.
- A full filesystem, conversation, or Git search index.
- Globally optimal search ranking after the configured result cap is filled.
- Fuzzy spelling correction, pinyin matching, or edit-distance matching.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Title agent scope | One global enabled and available base agent; profiles are excluded |
| Title trigger | First usable `end_turn` for new root and delegated conversations |
| Title context | First task plus first successful visible final response |
| Title language | Current application interface language |
| Native titles | Always run the configured title agent; native titles cannot replace success |
| Title retry | One retry, only after the target conversation's next successful turn |
| Existing conversations | No retroactive generation |
| Bare `@` | Enabled base agents plus enabled CodeBuddy profiles; no resource calls |
| Search sources | Files, all-project conversations, and current-repository commits |
| Search strategy | On-demand, independent, cancellable source searches; no full index |
| Page size | Five results for the first and every subsequent page |
| Result cap | One global setting, independently applied to each resource source |
| Stop condition | Stop as soon as each source fills its cap; accept non-global ranking |
| Regex activation | `re:` prefix |
| Ranking | Relevance inside the collected set, then stable source order |
| Cache lifetime | Current window only |

## Settings Model

Add a shared settings document used by both runtimes:

```rust
pub struct ConversationExperienceSettings {
    pub auto_title_agent: Option<AgentType>,
    pub reference_search_limit: u16,
}
```

`auto_title_agent = None` disables automatic title generation. The title
picker lists only base agents that are both enabled and currently available;
delegation profiles are never listed. If a saved agent later becomes
unavailable, the setting remains visible with an unavailable state so the
user can change it, but the runtime does not silently fall back to another
agent.

`reference_search_limit` defaults to 50 and is clamped to 10 through 500 in
the shared backend core. The same value applies independently to files,
conversations, and commits. Page size is an internal fixed value of five and
is not exposed as a setting.

Persist the settings in `app_metadata` and expose one get/set core used by the
Tauri command and HTTP handler. Saving validates the selected agent type and
updates a live settings snapshot after the database transaction commits.

The settings UI adds a compact conversation-experience group containing an
agent select with an Off option and a numeric reference-result limit. It uses
the existing global settings layout and agent availability data.

## Architecture

### Backend-owned components

- `AutoTitleCoordinator`: owns durable title-job transitions and receives
  successful turn notifications from the central ACP lifecycle dispatcher.
- `HiddenAgentRunner`: performs an isolated, unlinked, one-turn ACP request and
  returns only the final visible text.
- `InternalAgentSessionRegistry`: records title-runner external session IDs so
  all conversation listing and import paths ignore them permanently.
- `ReferenceSearchRegistry`: owns per-source pull cursors, cancellation tokens,
  inactivity expiry, and the five-item page contract.
- Source searchers: file walker, SQLite conversation searcher, and streaming
  Git log searcher, each behind the same registry interface.

### Frontend-owned components

- `ReferenceSearchController`: owns generation numbers, request cancellation,
  source snapshots, cache buckets, merge/rank behavior, and subscriptions.
- The existing `use-reference-search.ts` adapts agent/profile data and the
  controller snapshots to suggestion groups.
- `suggestion-popup.tsx` keeps active source and selection by stable URI rather
  than array index.
- `mention-suggestion.ts` suppresses work during composition and re-matches the
  current query after `compositionend`.

The two features do not share a task abstraction. They only reuse existing
settings persistence, `AppState`, ACP lifecycle events, database access, and
the desktop/web transport boundary.

## Automatic Title Persistence

### Conversation state

Add `auto_title_finalized: bool NOT NULL DEFAULT false` to conversations.
Existing rows migrate to `false`. Native title refresh is allowed only while
both `title_locked = false` and `auto_title_finalized = false`.

`title_locked` retains its current meaning: the user manually chose the title.
`auto_title_finalized` means the configured title agent successfully chose the
automatic title. A manual rename may replace either kind of title.

### Pending job table

Add an `auto_title_jobs` table with one row per eligible conversation:

| Column | Purpose |
| --- | --- |
| `conversation_id` | Primary key and foreign key |
| `state` | `awaiting_turn`, `ready`, `running`, or `retry_wait` |
| `attempts` | Started attempts, constrained to 0 through 2 |
| `first_user_text` | Sanitized first root task or delegation task; nullable until the first root prompt |
| `first_assistant_text` | First usable successful final response, initially null |
| `updated_at` | Recovery and diagnostics timestamp |

There is no job row for historical conversations or conversations created
while automatic titles are disabled. A new eligible root conversation creates
its job as part of its creation flow; the first linked prompt fills
`first_user_text` once. Delegated conversation creation fills the same field
from the delegation task. Later prompts never replace it.

Terminal outcomes do not need permanent job records. Success updates the
conversation and deletes the job in one transaction. Exhaustion, manual
rename, or disabling the feature deletes the job. Absence of a row therefore
means that no future automatic-title attempt is allowed.

### Internal session exclusion

Add an `internal_agent_sessions` table keyed by `(agent_type, external_id)`
with `purpose`, `created_at`, and an optional cleanup timestamp. The title
runner must receive `SessionStarted`, persist this row, and only then send its
prompt. If the row cannot be persisted, it aborts without sending.

The connection is marked internal in `SessionState` before it is spawned.
`SessionStarted` persistence for an internal connection records the external
ID exclusion before any normal conversation persistence or broadcast can run,
closing the import race between agent session creation and registration.

All raw parser-list, database-import, refresh, and startup-import entry points
must consult one shared exclusion service. This prevents a hidden title
session from appearing after a later restart or manual import. Raw agent files
are removed on a best-effort basis after disconnect, but the exclusion record
is retained because cleanup cannot be guaranteed for every CLI.

## Automatic Title Lifecycle

### Eligibility and context capture

The central ACP lifecycle handler continues its normal status update and
delegation completion work, then notifies `AutoTitleCoordinator` without
waiting for a model request.

Only `TurnComplete { stop_reason: "end_turn" }` with non-empty visible final
text is usable. Cancellation, refusal, protocol failure, token limit, and an
empty successful turn leave an `awaiting_turn` job untouched. The first later
usable successful turn supplies `first_assistant_text`.

Context normalization is identical for root and delegated conversations:

- Keep the first task's user-visible text.
- Fold Markdown reference links to their display labels.
- Keep the first usable assistant final text.
- Exclude reasoning, tool calls, tool results, permission interactions,
  attachment bytes, and binary content.

The normalized fields are the only conversation content duplicated into the
job table. They are deleted when the job reaches a terminal outcome.

### Durable state transitions

```text
awaiting_turn --usable end_turn--> ready
ready --worker claim--> running(attempts = 1)
running --valid title--> generated (job deleted)
running --failure--> retry_wait
retry_wait --next usable end_turn--> running(attempts = 2)
running(attempts = 2) --failure--> exhausted (job deleted)
```

Claims are conditional database updates, so duplicate listeners and repeated
events cannot start the same attempt twice. `ready` jobs are durable queue
entries and are drained after startup. A process exit while a job is `running`
counts that started attempt as failed: startup moves attempt one to
`retry_wait` and deletes an interrupted attempt-two job.

Changing the selected title agent affects the next attempt. Disabling the
setting deletes pending jobs, cancels active runners, and invalidates any
result that still arrives. Re-enabling it only affects conversations created
afterward.

### Hidden agent execution

`HiddenAgentRunner` uses the selected base agent's normal model, mode, provider
environment, and launch configuration, but changes the execution envelope:

- Use a dedicated temporary working directory, never the target project.
- Use `EventEmitter::Noop` and a connection with no `conversation_id`.
- Do not inject Codeg delegation, question, feedback, or session MCP tools.
- Reject permission requests and interactive questions as attempt failures.
- Apply an internal overall timeout and always disconnect on completion.
- Limit title executions to two concurrent jobs process-wide.

The runner's unbound `TurnComplete` is ignored by `AutoTitleCoordinator`, so a
title request cannot recursively create another title request.

### Prompt and output contract

The prompt contains the normalized task and response plus the current
application locale. It instructs the agent to return only a concise title in
that locale, with no Markdown, quotes, prefix, or explanation, and not to use
tools.

The backend accepts only a normal completed turn, then:

1. Takes the first non-empty output line.
2. Removes Markdown heading/list prefixes and paired wrapping quotes.
3. Collapses whitespace and trims the result.
4. Truncates safely to 80 Unicode characters.
5. Treats an empty result as an attempt failure.

### Atomic title write and precedence

Success runs one transaction with a conditional update:

- The job is still `running` for the claimed attempt.
- The conversation exists and is not deleted.
- `title_locked = false`.
- `auto_title_finalized = false`.

The transaction writes the title, sets `auto_title_finalized = true`, and
deletes the job. It does not change `updated_at`, because title metadata must
not reorder the conversation list. After commit, the existing conversation
upsert event immediately refreshes the sidebar and open tabs.

Manual rename sets `title_locked = true` and deletes any title job in the same
transaction, then cancels an active runner for that conversation. A late
runner result then fails its job predicate and cannot overwrite the manual
name. Native CLI title refresh retains its existing atomic lock check and adds
`auto_title_finalized = false` to its predicate.

### Failure behavior

Spawn failure, unavailable agent, timeout, interactive request, abnormal title
turn completion, empty output, invalid output, or write failure is an attempt
failure. Attempt one waits for the target conversation's next usable turn;
there is no timer retry. Attempt two ends the job. The current conversation
title remains untouched, and failures produce structured logs without user
notifications or fallback agents.

## Reference Picker Interaction

### Bare mention

When the active query is empty, the controller returns all enabled base agents
and enabled CodeBuddy delegation profiles synchronously. It starts no file,
conversation, commit, regex-validation, or cache-validation request and shows
no resource loading state.

### Non-empty mention

For a non-empty query, agent/profile candidates are filtered immediately from
already-loaded settings data. File, conversation, and commit searches start as
three independent operations. Each source publishes its first page as soon as
it arrives and requests the next page only while below its per-source cap.

The UI never waits on `Promise.all`. A source error, slow source, or empty
source cannot delay the other groups.

### Active group and selection

Candidate identity is the existing stable reference URI. The popup stores
`selectedUri`, not only `selectedIndex`.

- Merging, sorting, and inserting pages preserves the selected URI.
- When the query text changes, preserve the selected URI only if it still
  matches the new query; otherwise choose the first provisional candidate.
- If re-ranking would place it below the visible cap, it occupies one visible
  slot and the lowest-ranked unselected item is omitted.
- A newly non-empty source does not switch the active group while the current
  group has a valid selected item.
- If the selected item is explicitly invalidated, selection moves to the
  nearest surviving item at the same index, then the previous item, then none.
- Switching groups intentionally chooses that group's first item.

## Reference Cache

Cache buckets are isolated by backend identity, canonical workspace identity,
and source kind. They live for the browser window lifetime only.

For literal queries, every returned item is added to a URI-keyed item index.
The controller filters that accumulated index synchronously on later queries,
publishes a provisional result, and then starts authoritative searches.

For regex queries, results are cached by the complete normalized expression.
Only an exact repeated expression is reused synchronously. This avoids
claiming that JavaScript evaluated a Rust regular expression identically.

Each bucket has a 10,000-item LRU safety cap. Currently selected and visible
items are not eligible for eviction. The cap prevents an unbounded window-long
memory leak without changing the per-query display limit.

Fresh pages merge by stable URI and update changed metadata. Absence from a
limited fresh result is not evidence that a cached item was deleted. The first
request for a source carries the currently visible cached URIs, and the backend
returns `invalidatedUris` only for resources it explicitly proves no longer
exist:

- Files: path metadata check under the same canonical workspace root.
- Conversations: live, non-deleted database row check.
- Commits: batched Git object-existence check.

Only explicit invalidation removes cached entries or the selected item.

## Reference Search Protocol

The shared transport exposes source-neutral operations backed by source-specific
jobs:

```ts
type ReferenceSearchSource = "file" | "conversation" | "commit"

interface StartReferenceSearchRequest {
  source: ReferenceSearchSource
  query: string
  workspacePath?: string
  limit: number
  validateUris: string[]
}

interface ReferenceSearchPage {
  searchId: string
  items: ReferenceCandidate[]
  invalidatedUris: string[]
  done: boolean
  doneReason?: "exhausted" | "limit"
}
```

`start_reference_search` creates a job and returns its first page.
`next_reference_search_page` advances one job by at most five matches.
`cancel_reference_search` removes it and cancels its worker. The server clamps
the requested limit to 10 through 500 regardless of client input.

Jobs are pull-driven and apply backpressure: a worker does not scan the entire
source while waiting for the client. Query changes abort frontend calls,
cancel all three old jobs, and increment a local generation. Any response from
an older generation is discarded even if transport cancellation arrived too
late. Finished jobs are removed immediately; inactive jobs expire after 30
seconds.

### Source implementations

Files retain an ignore-aware walker between page requests. They honor the
existing ignore rules and hard-skipped names, use stable name ordering within
directories, and stop after the configured number of matches.

Conversations query the application SQLite database rather than reparsing all
agent files. Search covers all projects and iterates live rows by
`updated_at DESC, id DESC`, preserving a keyset cursor between pages.

Commits keep a cancellable Git process or equivalent streaming reader for the
current repository. They iterate newest first and terminate the child process
on cancellation or limit completion.

## Matching and Ranking

Literal search is case-insensitive. Searchable fields are:

| Source | Primary fields | Secondary fields |
| --- | --- | --- |
| Agent/Profile | Display name | Agent type, description, model |
| File | Name | Relative path |
| Conversation | Title | ID, agent, status, branch, project |
| Commit | Short/full hash, subject | Full message, author |

`re:` switches the query to regex mode; for example,
`re:^src/.*\.tsx$`. The prefix is removed before compilation. Regex mode is
case-sensitive by default and supports Rust inline flags such as `(?i)`.

The accepted grammar is the safe intersection used by Rust `regex`: no
lookaround or backreferences, with a bounded pattern length. Invalid patterns
start no resource jobs, retain current/cache candidates, and publish a
localized pattern error. Shared Rust/TypeScript fixtures enforce equivalent
acceptance for the small in-memory Agent/Profile preview, while backend source
results remain authoritative.

The backend scans in source order until it has the requested number of
matches. The frontend ranks only the collected set:

1. Exact primary-field match.
2. Primary-field prefix match.
3. Primary-field word-boundary match.
4. Primary-field substring match.
5. Secondary-field match.
6. Stable source order as the final tie-breaker.

Regex ranking prefers primary fields, then an earlier match start, then a
shorter match, then source order. Because search stops at the configured cap,
a better match later in the source is intentionally allowed to remain unseen.

## IME Behavior

The mention plugin must not search, move selection, select a candidate, or
submit while `editor.view.composing`, `event.isComposing`, or key code 229
indicates active composition.

On `compositionend`, schedule one microtask after ProseMirror applies the final
text, then re-read the current document and caret and re-run mention matching.
If the caret still follows a valid `@query`, open or update the picker exactly
once. This applies equally to Chinese and English characters entered through
an IME. The Enter used to confirm an IME candidate remains consumed by IME
handling and cannot select a reference or submit the message.

## Error Handling and Isolation

- Source failures are isolated per group and never clear other groups.
- A failed source retains its cache preview and shows a group-local error.
- Cancellation, generation expiry, and cursor expiry are normal control flow,
  not user-facing errors.
- A stale or unknown cursor returns a typed expired result so the client can
  restart only that source if the query is still current.
- Invalid regex is reported before replacing existing candidates.
- Backend path validation prevents file validation or search from escaping the
  canonical workspace root.
- Server-side limits, pattern bounds, concurrency limits, and cancellation are
  enforced even when an HTTP client bypasses the frontend.
- Hidden title execution has no target-project working directory, injected
  collaboration tools, or interactive permission path.

## Events and Runtime Parity

The existing conversation-upsert event carries successful title changes; no
new title-specific frontend event is required. The settings core and search
registry are reachable through both Tauri command registration and Axum
handlers. WebSocket broadcasting remains responsible for propagating
conversation updates to other connected clients.

Reference cache and generation state are intentionally per frontend window.
Search jobs are backend process state and are namespaced by opaque search IDs,
so one client cannot cancel another client's job by reusing a local generation
number.

## Testing Strategy

### Automatic title tests

- Migration defaults make all existing conversations ineligible.
- New root and delegated conversations create exactly one job when enabled.
- Abnormal and empty turns do not consume an attempt.
- First success captures only visible task/reference labels/final response.
- Duplicate `TurnComplete` events cannot double-claim a job.
- Success writes without changing conversation recency and broadcasts once.
- Manual rename wins races before, during, and after hidden execution.
- Native title refresh cannot replace a finalized generated title.
- First failure waits for the target's next success; second failure terminates.
- Startup recovery preserves ready jobs and accounts for interrupted attempts.
- Disabling or changing settings follows the specified pending-attempt rules.
- Hidden session registration precedes prompt sending and excludes every list
  and import entry point.
- A fake ACP runner covers success, timeout, permission request, abnormal stop,
  malformed output, disconnect, and cleanup without external CLIs.

### Reference backend tests

- Every source returns pages of five except the final partial page.
- Each source stops exactly at its independent configured cap.
- File ignore behavior and stable traversal order remain intact.
- Conversation search spans projects, excludes deleted rows, and keyset-pages
  without duplicates.
- Commit search streams newest first and kills its process on cancellation.
- Literal and regex fields, syntax bounds, and rank scores match the contract.
- Query cancellation, stale cursors, inactivity expiry, and hard limits work.
- Explicit URI validation distinguishes deletion from limited non-return.
- Tauri and HTTP handlers call the same core behavior.

### Reference frontend tests

- Bare `@` performs zero resource-search and validation calls.
- Cached literal and repeated-regex results publish before network completion.
- File, conversation, and commit pages update independently.
- Late generations never mutate current groups.
- Selection follows URI through insertion, sorting, paging, and cache refresh.
- Explicitly deleted selected items move selection to the nearest survivor.
- Cache LRU never evicts selected or visible candidates.
- Source errors retain cached candidates and do not affect sibling groups.
- IME composition suppresses action and `compositionend` re-matches Chinese
  and English final text exactly once.
- The candidate-confirmation Enter never inserts a reference or submits.

## Acceptance Criteria

- A newly created root or delegated conversation receives one generated title
  after its first usable successful response, even when its tab is closed.
- The title appears immediately in every connected sidebar and persists after
  restart without changing list recency.
- Manual titles are never overwritten, and generated titles are never replaced
  by native CLI title metadata.
- A failed title request runs at most once more and only after the target's next
  usable successful response.
- A bare `@` immediately shows enabled agents and profiles and makes no file,
  conversation, or Git request.
- Non-empty searches show cached matches immediately and independently append
  five-result pages from each resource source.
- Each source stops at the configured limit, and changing the query cancels and
  isolates every prior generation.
- The selected candidate does not jump when pages arrive or ranking changes;
  it moves only when the user moves it or the backend proves it no longer
  exists.
- `re:` queries, source-local errors, and IME completion behave consistently in
  desktop and server deployments.
