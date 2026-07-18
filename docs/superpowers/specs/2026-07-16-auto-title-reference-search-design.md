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
  limited to the first task and first usable visible response.
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
- Hiding title-runner sessions from the selected agent CLI's own history UI,
  or deleting that CLI's raw session files. Codeg hides those sessions from
  every Codeg list, import, statistic, and detail path.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Title agent scope | One global enabled and available base agent; profiles are excluded |
| Title trigger | First usable `end_turn` for new root and delegated conversations |
| Title context | First task plus first usable successful visible final response |
| Title language | Current application interface language |
| Native titles | Always run the configured title agent; native titles cannot replace success |
| Title retry | One retry, only after the target conversation's next usable successful turn |
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
    pub revision: u64,
}
```

`auto_title_agent` defaults to `None`, which disables automatic title
generation. The title
picker lists only base agents that are both enabled and currently available;
delegation profiles are never listed. If a saved agent later becomes
unavailable, the setting remains visible with an unavailable state so the
user can change it, but the runtime does not silently fall back to another
agent.

`reference_search_limit` defaults to 50 and is clamped to 10 through 500 in
the shared backend core. The same value applies independently to files,
conversations, and commits. Page size is an internal fixed value of five and
is not exposed as a setting. Existing installations begin at revision zero;
the first successful setter advances it to one.

Persist the two user fields under separate `app_metadata` keys and persist one
monotonic settings revision under a third key. A combined get returns
`ConversationExperienceSettings`, but updates use independent
`set_auto_title_agent` and `set_reference_search_limit` cores. A stale client
changing one control therefore cannot overwrite a newer value for the other.
Each setter updates only its own field, increments `revision`, and returns the
full post-write document in one transaction. Each core is shared by its Tauri
command and HTTP handler.

Saving the title agent validates that the type is a base agent and is currently
enabled and available. Runtime availability is checked again when an attempt
starts, together with the enabled flag, because agent settings and installation
state may change after save. Saving `None` writes the setting and removes all
pending title jobs in one transaction, then asks the coordinator to cancel
active runners. Saving the reference limit only clamps and writes that key.

After either update commits, broadcast the returned full settings document on a
global conversation-experience settings event. Stores apply command responses
and events only when their revision is newer, so reordered broadcasts cannot
roll a window back to an older value. A limit change immediately re-truncates
cache previews without discarding cached items. The backend also advances the
reference registry's live limit epoch and cancels every old-epoch resource job;
each frontend increments its controller generation and restarts the current
non-empty query with the new limit. A bare `@` remains network-free.

The settings UI adds a compact conversation-experience group containing an
agent select with an Off option and a numeric reference-result limit. It uses
the existing global settings layout and agent availability data. New setting,
regex-error, and source-error labels are added to all ten locale catalogs.

## Architecture

### Backend-owned components

- `AutoTitleCoordinator`: owns durable title-job transitions and receives
  ready-job notifications from the central ACP lifecycle dispatcher.
- `HiddenAgentRunner`: performs an isolated, unlinked, one-turn ACP request and
  returns only the final visible text through the connection's private event
  stream. Its events never enter the global internal event bus.
- `InternalAgentSessionRegistry`: records title-runner external session IDs so
  all Codeg conversation discovery paths ignore them permanently, and provides
  a short discovery barrier around internal-session registration.
- `ReferenceSearchRegistry`: owns per-source pull cursors, cancellation tokens,
  inactivity expiry, and the five-item page contract.
- Source searchers: file walker, SQLite conversation searcher, and streaming
  Git log searcher, each behind the same registry interface.

### Frontend-owned components

- `ReferenceSearchController`: owns generation numbers, request cancellation,
  source snapshots, merge/rank behavior, and subscriptions.
- `ReferenceSearchCache`: a window-scoped module singleton, registered with the
  existing backend-scoped reset registry, owns cache buckets independently of
  popup and composer mounts.
- A shared delegation-profile store is initialized during backend-scoped
  workspace bootstrap, before the composer reference extension is enabled,
  rather than lazily on the first `@`.
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

Fork persistence copies `auto_title_finalized` onto the historical sibling as
well as retaining it on the live row. A Codeg-added fork label may decorate the
live title, but neither branch becomes eligible for later native-title refresh
when the copied title was generated.

### Pending job table

Add an `auto_title_jobs` table with one row per eligible conversation:

| Column | Purpose |
| --- | --- |
| `conversation_id` | Primary key and foreign key |
| `state` | `awaiting_turn`, `ready`, `running`, or `retry_wait` |
| `attempts` | Started attempts, constrained to 0 through 2 |
| `first_user_text` | Sanitized first root task or delegation task; nullable until the first accepted prompt |
| `first_assistant_text` | First usable successful final response, initially null |
| `locale` | Resolved interface locale; nullable until the first linked prompt |
| `usable_turn_seq` | Count of distinct usable target-conversation completions observed |
| `attempt_turn_seq` | `usable_turn_seq` captured when the current attempt starts |
| `last_usable_turn_token` | Backend turn token used to make completion processing idempotent |
| `updated_at` | Recovery and diagnostics timestamp |

`attempts`, `usable_turn_seq`, and `attempt_turn_seq` are non-null integers with
zero defaults and database checks preventing negative values or more than two
attempts. `last_usable_turn_token` is nullable before the first usable turn.
The table has a `(state, updated_at, conversation_id)` index for deterministic
ready-job draining and startup recovery; workers order by that tuple before
attempting a conditional claim.

There is no job row for historical conversations or conversations created
while automatic titles are disabled. Imported raw CLI sessions and the sibling
row that preserves pre-fork history are historical for this feature and never
receive jobs; an existing pending job remains attached to the live row that
continues on the forked session. Every new live Codeg conversation insertion
path (`regular`, `chat`, automation-created root, and delegated child) reads the
title setting and inserts the conversation plus an eligible job in one database
transaction. This removes the race with concurrently disabling the setting.

Both production conversation send routes, `send_prompt` for an already-linked
connection and `send_prompt_linked` for a new/adopted row, call one shared
pre-enqueue title hook. The hook is a no-op for an unlinked or internal-purpose
connection. Otherwise, after the conversation ID is known and before enqueue,
it fills `first_user_text` once and records the effective locale. This covers UI,
chat-channel, automation, resumed, and delegated sends rather than assuming all
producers use the linked entry point.

The production send APIs therefore receive the application database plus an
optional `PromptCaptureContext { visible_text, locale }`. Callers without
explicit display text use the safe block projection below; channel producers
pass their channel locale. The low-level unlinked send used by
`HiddenAgentRunner` does not invoke the hook.

The existing per-connection prompt lock serializes the whole admission path.
After linking and validation, the manager reserves command-channel capacity
before any title-context write. Reserve failure or caller cancellation therefore
leaves no staged title state. With the permit held, the hook transaction stores
the first task once and the effective locale. A database failure drops the
permit and returns the send error; it does not enqueue a prompt whose title
context is missing. After that transaction succeeds, the manager performs no
further await: it synchronously installs the turn token/locale on `SessionState`
and calls the infallible `permit.send`. A fast completion therefore cannot beat
capture persistence, and no asynchronous rollback window exists. Later accepted
prompts never replace the first task but may update locale while the job waits
for its one retry. The state locale is updated even when this conversation has
no title job, so a later delegated child can inherit the parent's effective UI
locale.

The root `acp_prompt` request carries optional `visibleText` from the existing
`PromptDraft.displayText`, alongside the wire blocks. This is the authoritative
user-visible text: it excludes hidden mandatory-delegation directives, retains
reference and embedded-attachment display names, and contains no attachment
bytes. Delegated sends use the broker's task text. Non-UI/older clients that
omit `visibleText` use a backend fallback projection that drops complete
internal directive blocks, folds resource links to labels, derives safe names
from resource URIs where possible, and ignores image data plus embedded
resource `text`/`blob` contents. Later accepted prompts never replace the first
task.

Terminal outcomes do not need permanent job records. Success updates the
conversation and deletes the job in one transaction. Exhaustion, manual
rename, or disabling the feature deletes the job. Absence of a row therefore
means that no future automatic-title attempt is allowed.

Soft-deleting a conversation deletes its pending job in the same transaction
and cancels any active runner after commit. The job foreign key uses
`ON DELETE CASCADE` as a defense for future hard deletion, but soft-delete
cleanup remains explicit.

### Internal session exclusion

Add an `internal_agent_sessions` table keyed by `(agent_type, external_id)`
with `purpose` and `created_at`. The title runner marks its connection purpose
before spawn and creates its working directory under a reserved
Codeg-owned internal-title root. It then takes the registry's exclusive
discovery lease and spawns the connection. Under one `SessionState` read lock it
snapshots an already-arrived external ID and subscribes to the private stream;
if the ID is absent, it waits for `SessionStarted`. This closes the
subscribe-after-event race. The runner puts the observed ID into the in-memory
exclusion set, persists it, and only then releases the lease and sends its
prompt. If spawn, handshake, or registry persistence fails, it disconnects and
aborts without sending.

The exclusive lease budget starts immediately before spawn and ends when the ID
is persisted or after 15 seconds, whichever comes first. Reaching 15 seconds
releases only the discovery lease: the reserved-root rule keeps the raw session
quarantined while the runner may continue waiting for `SessionStarted` inside
its 90-second overall budget. No title prompt is sent until the ID is durable.
Spawn failure, overall timeout, or registry persistence failure disconnects and
aborts. This prevents a broken or cold internal agent launch from indefinitely
blocking parser-backed history/statistics operations without turning the lease
duration into the full runner timeout.

Raw parser-list and import operations take a shared discovery lease and load
the union of persisted and in-memory exclusions before scanning. All
parser-backed list, folder, statistics, sidebar, import, refresh, and detail
entry points use the same filter. The reserved parent root persists even after
per-run directories are removed. A parsed session is internal when its
`(agent_type, external_id)` is excluded or its lexically normalized absolute
working-directory record is below that canonical parent root (with Windows
case rules applied); the child path itself need not still exist. The path rule
is the fallback for an agent that created metadata but never delivered
`SessionStarted`, or for a registry write that failed before any title prompt
was sent. Sessions with no ID and no recorded working directory remain
metadata-only because the runner never sends them content. The lease closes the
normal interval between a CLI creating its raw session and Codeg persisting the
external ID; the ID row and reserved-root rule keep it hidden after restart or
manual import.

Codeg does not delete raw files owned by external CLIs: there is no uniform,
safe deletion API across agents. The exclusion guarantee is scoped to Codeg;
the selected agent's own CLI may retain the internal session in its history.

## Automatic Title Lifecycle

### Eligibility and context capture

Every admitted prompt receives a backend-generated opaque turn token, stored on
`SessionState` until that turn completes. While applying `TurnComplete` under
the state write lock, the emit path creates a backend-only immutable completion
snapshot: conversation ID, turn token, resolved locale, and an `Arc` of
`last_assistant_text`. It attaches that snapshot only as a sidecar on the
process-internal lifecycle-bus delivery. The public `EventEnvelope`, private
connection stream, recent-event replay ring, webview, and WebSocket payload keep
their existing shape and never retain or serialize the sidecar. The lifecycle
worker must use this event-owned snapshot, not re-read mutable session state
after queueing; otherwise a fast next turn can clear or replace the first turn's
text before the worker sees it.

The central ACP lifecycle handler performs its normal status update and the
title-job transition in one short database transaction. For a delegated child
this transition happens before broker completion can disconnect the child. It
then notifies `AutoTitleCoordinator` and continues normal broker work using the
same immutable final text; the lifecycle worker never waits for a title model
request. Root conversations use the same path. Re-delivery of the same turn
token is a no-op, so a duplicate lifecycle event cannot increment the retry
sequence twice.

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

The visible task is captured from the shared pre-enqueue hook's explicit
`visibleText` or its fallback projection, not from
`SessionState.pending_user_message`: delegated
prompts deliberately do not create that user-message state, and root wire
blocks can contain hidden routing directives. The fallback sanitizer drops
only whole blocks whose every non-empty line matches Codeg's structured
mandatory-route prefix; ordinary user text that merely discusses that phrase
is retained.

After normalization, cap each task and response to 4,000 Unicode scalar values.
When truncation is needed, retain the first 2,995 scalars, the five-scalar ASCII
marker `\n...\n`, and the last 1,000 scalars. The marker is included in the
4,000-scalar bound. The combined title request therefore has a deterministic
text-size ceiling while preserving the task opening and the final outcome;
token counts remain model-tokenizer dependent.

The browser sends `visibleText` and its resolved `AppLocale` as optional fields
on every root ACP prompt. The backend accepts only one of the ten supported
locale identifiers; invalid or absent values take the same fallback path as a
non-UI producer. The shared pre-enqueue hook stores the effective locale on the
job and in `SessionState`. A delegated child inherits the parent connection's
latest resolved locale; a non-UI producer uses its channel locale when
available, otherwise the stored language setting and finally English. A later
accepted target prompt updates the job locale while it waits for retry, and the
immutable completion snapshot binds that locale to the reply that triggers the
retry. This is required because server-side system mode cannot infer a remote
browser's actual interface language.

The normalized fields are the only conversation content duplicated into the
job table. They are deleted when the job reaches a terminal outcome.

### Durable state transitions

```text
awaiting_turn --usable end_turn--> ready
ready --worker claim--> running(attempts += 1)
running --valid title--> generated (job deleted)
running(attempts = 1) --failure, no newer usable turn--> retry_wait
running(attempts = 1) --failure, newer usable turn exists--> ready(attempt 2)
retry_wait --next usable end_turn--> ready(attempt 2)
running(attempts = 2) --failure--> exhausted (job deleted)
```

For a usable completion whose turn token differs from
`last_usable_turn_token`, the transaction stores the token, increments
`usable_turn_seq`, and preserves the first assistant text once. The same token
delivered again makes no change. Claims are conditional database updates, so
duplicate listeners and repeated events cannot start the same attempt twice.
`ready` jobs are durable queue entries and are drained after startup. A process
exit while a job is `running` counts that started attempt as failed: startup
moves attempt one to `ready` if `usable_turn_seq > attempt_turn_seq`, otherwise
`retry_wait`, and deletes an interrupted attempt-two job.

Every distinct usable target `end_turn` increments `usable_turn_seq`, even while
a title attempt is running. Thus, if the target's next reply finishes before
attempt one eventually fails or times out, attempt two can start immediately
after that failure instead of incorrectly waiting for a third reply.

Claiming an attempt snapshots the currently selected title agent. Changing the
selection to another agent affects the next claim and does not cancel an
already-running attempt. Disabling the setting deletes pending jobs, cancels
active runners, and invalidates any result that still arrives. Re-enabling it
only affects conversations created afterward.

Soft deletion and manual rename follow the same cancel-after-commit rule.
Changing the configured agent does not itself retry a `retry_wait` job; the
target conversation must still complete its next usable turn.

### Hidden agent execution

`HiddenAgentRunner` uses the selected base agent's configured provider and
environment plus the agent-advertised default model, mode, and config values.
It does not use a frontend window's last per-session selector values. It adds a
`ConnectionPurpose::InternalTitle` launch purpose and changes the execution
envelope:

- Use a dedicated temporary working directory, never the target project.
- Use `EventEmitter::Noop` and a connection with no `conversation_id`.
- Do not inject Codeg delegation, question, feedback, or session MCP tools.
- Reject permission requests and interactive questions as attempt failures.
- Read `SessionStarted`, content, errors, and `TurnComplete` from the existing
  per-connection event stream; `Noop` intentionally sends nothing to the
  process-wide lifecycle, pet, chat-channel, webview, or WebSocket paths.
- Apply a 90-second overall timeout from spawn through final output and always
  disconnect on completion.
- Remove the Codeg-owned temporary working directory after disconnect on a
  best-effort basis; this does not touch the agent CLI's own session storage.
- Acquire one of two process-wide attempt permits before conditionally claiming
  a `ready` job; a job is never marked `running` while merely waiting for
  capacity. Hold the permit until the attempt reaches failure, cancellation, or
  a committed title. This bounds both concurrent model executions and generated
  outputs waiting in memory for a database retry to two.

The runner's events never enter the lifecycle subscriber, and its unlinked
connection has no conversation or title job. A title request therefore cannot
recursively create another title request.

### Prompt and output contract

The prompt contains the bounded normalized task and response plus the job's
resolved interface locale. It instructs the agent to return only a concise
title in that locale, with no Markdown, quotes, prefix, or explanation, and not
to use tools.

The backend accepts only a normal completed turn, then:

1. Takes the first non-empty output line.
2. Removes heading/list prefixes and one paired outer layer of quotes,
   backticks, or Markdown emphasis markers.
3. Removes non-whitespace control characters, collapses whitespace, and trims
   the result.
4. Truncates safely to 80 Unicode scalar values.
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
turn completion, missing final output, or empty normalized output is an attempt
failure. Attempt one waits for the target conversation's next usable turn;
there is no timer retry. Attempt two ends the job.

A valid generated title followed by a database write failure reuses the same
in-memory output; it never spends another model request immediately. The commit
path retries after 100 ms and 500 ms (matching the existing lifecycle DB
backoff), then every 5 seconds while the process remains alive, until success
or job cancellation. If the process exits first, startup treats the persisted
`running` attempt as failed and applies the normal `retry_wait`/exhausted rule.
An executed conditional update that affects no row is cancellation or lost
precedence, not a transient database failure, and stops retrying immediately.
The current conversation title remains untouched until a transaction commits,
and failures produce structured logs without user notifications or fallback
agents.

## Reference Picker Interaction

### Bare mention

When the active query is empty, the controller returns all enabled base agents
and enabled CodeBuddy delegation profiles synchronously. It starts no file,
conversation, commit, regex-validation, or cache-validation request and shows
no resource loading state.

A profile is effectively enabled only when the global delegation setting, the
profile's own `enabled` flag, and its backing base agent are all enabled. Agent
runtime availability is displayed using the existing agent state but does not
trigger a mention-time probe.

To make that zero-request guarantee real, delegation profiles move out of the
current first-`@` lazy fetch into a backend-scoped shared store. Workspace
bootstrap awaits both the enabled-agent snapshot and a versioned profile
document before enabling the composer reference extension. A bare `@` therefore
reads the store snapshot only; profile loading or refresh is never triggered by
opening the picker. Every delegation-settings-only, profile-only, or atomic
settings-and-profiles save increments a persisted profile-catalog revision in
its transaction and broadcasts the normalized document, effective delegation
flag, and revision on a new global delegation-profile event. Stores drop older
or duplicate revisions, which keeps conversation windows synchronized with the
separate settings window even when events arrive out of order.

An applied catalog or agent change recomputes bare and literal Agent/Profile
rows locally. For an open regex query it invalidates only that group's old
membership and reruns the batched helper; file, conversation, and commit jobs
are not restarted merely because the profile catalog changed.

Bootstrap failure does not leave the composer permanently disabled: the store
enters a ready-with-error state, bare `@` can still show the available agent
snapshot, and normal backend reconnect/focus recovery retries profile loading.
Opening the picker itself never triggers that retry. A localized profile-group
error remains until a successful bootstrap or revisioned event supplies the
catalog.

### Non-empty mention

For a non-empty literal query, agent/profile candidates are filtered
immediately from already-loaded settings data. File, conversation, and commit
searches start as three independent operations. In regex mode, an exact cache
snapshot may publish immediately; the authoritative Agent/Profile matcher and
all three resource starts run independently. Every backend core compiles the
pattern before registering work, so an invalid pattern creates no resource job
even though requests may arrive concurrently. Each source publishes its first
page as soon as it arrives and requests the next page only while below its
per-source cap.

The UI never waits on `Promise.all`. A source error, slow source, or empty
source cannot delay the other groups.

The current `ReferenceSearch` promise contract is replaced with a controller
contract: `setQuery()` synchronously publishes bare-agent or cache snapshots,
`subscribe()` publishes independent source revisions, and `close()` cancels
live jobs without clearing the window cache. Only one page request per source
may be in flight. The popup no longer owns one all-groups promise or resets all
groups when any source resolves.

### Active group and selection

Candidate identity is the existing stable reference URI. The popup stores
`selectedUri`, not only `selectedIndex`.

- Merging, sorting, and inserting pages preserves the selected URI.
- When a literal query changes, preserve the selected URI only if the
  provisional literal matcher still accepts it. For an exact repeated regex,
  preserve it only if its URI exists in that expression's authoritative cache
  snapshot. For a new regex expression the frontend cannot test membership, so
  old continuity rows are non-selectable and selection remains empty until an
  authoritative source returns a match.
- If re-ranking would place it below the visible cap, it occupies one visible
  slot and the lowest-ranked unselected item is omitted.
- A newly non-empty source does not switch the active group while the current
  group has a valid selected item.
- If the selected item is explicitly invalidated, selection moves to the
  nearest surviving item at the same index, then the previous item, then none.
- Switching groups intentionally chooses that group's first item.

Agent/Profile results are not governed by the resource-result limit: a bare
`@` means all enabled entries, and a non-empty query means all matching enabled
entries. The limit applies only to file, conversation, and commit groups.

## Reference Cache

Cache buckets are isolated by backend identity and source scope. File buckets
also use the canonical workspace root; conversation buckets are backend-global
because their search spans every active project; commit buckets use the
canonical repository root plus the Git epoch described below. They live in a
module-level store for the browser-window lifetime, survive popup/composer
unmounts, and register a reset with the existing backend-scoped store-reset
registry. They are cleared on logout or backend identity change.

Commit buckets additionally include the repository's current branch and HEAD
epoch supplied by the workspace Git state. A branch switch, checkout, reset, or
history rewrite selects a new bucket, so commits cached from a different
history never appear as provisional matches for the current branch. Until that
Git identity is known, the controller does not publish a provisional commit
cache; authoritative commit search may still initialize the identity.

For literal queries, every returned item is added to a URI-keyed item index.
The controller filters that accumulated index synchronously on later queries,
publishes a provisional result, ranks it, truncates it to the current per-source
limit, and then starts authoritative searches.

For regex queries, results are cached by the complete exact expression.
Only an exact repeated expression is reused synchronously. This avoids
claiming that JavaScript evaluated a Rust regular expression identically. A
regex snapshot stores only ordered URI/rank references into the item index, not
duplicate candidate metadata.

Refresh builds a separate working snapshot for that expression. On successful
source completion it atomically replaces the old expression snapshot with the
current collected set, including a limit-truncated set; on cancellation or
source failure the last complete snapshot remains. Replacing a snapshot never
evicts the underlying URI-keyed item, because that resource may match another
query.

The window store has one 10,000-item LRU safety cap across all buckets, not one
cap per workspace. Currently selected and visible items are reference-counted
pins and are not eligible for eviction. Regex snapshots have a separate
window-wide 200-expression LRU cap, keyed by bucket plus expression; every
active expression is pinned. Empty bucket shells are removed. Evicting an item
also removes dangling references from every regex snapshot. If pins temporarily
prevent either index from reaching its cap, pruning runs again as soon as the
relevant controller closes or changes query. These global caps prevent
window-long growth when the user visits many workspaces without changing the
per-query display limit.

Fresh pages merge by stable URI and update changed metadata. Absence from a
limited fresh result is not evidence that a cached item was deleted, so fresh
search pages never remove a cached item merely by omission.

The window cache also subscribes once to the existing global conversation
upsert/delete stream. An upsert refreshes metadata for an already-indexed
conversation URI without adding an unrelated candidate. Because JavaScript
cannot recompute Rust-regex membership/rank, that URI's regex snapshot
references become stale; a currently selected URI stays visible as cache-only
pending validation, while unselected stale references are omitted until refresh.
A delete authoritatively evicts the URI and all snapshot references, allowing
open picker selection to move immediately. File and commit caches continue to
use fresh pages and selection-scoped validation because they have no equivalent
global event.

Validation is deliberately selection-scoped so it cannot delay the first
search page. A candidate returned by the current generation is already fresh.
When selection lands on a cache-only file, conversation, or commit, the
controller starts one logically cancelable `validate_reference_candidate`
request with a client-generated validation request ID. A new selection aborts
the HTTP request where supported and always invalidates the old ID. Enter on an
unvalidated cached candidate awaits that same validation for at most one
second. The typed outcomes are:

- `match`: merge the returned authoritative candidate metadata, keep the item,
  and mark it selectable for this generation.
- `not_match`: remove it only from the current provisional result because the
  resource still exists; merge its returned metadata because it may match
  another query. Revisiting a later provisional query may show and validate it
  again.
- `not_found`: evict the URI from the item index and every regex snapshot in
  that bucket because the authoritative source says the resource is gone.

Both negative outcomes move selection. A timeout or backend/transport error
preserves the cached item and still allows insertion. Before inserting after a
one-second timeout, the controller invalidates the validation request ID so a
late negative response cannot mutate the closed picker. Non-selected cache
entries are validated only when the user reaches them. A validation request
includes the current exact query identity and its response applies only if the
request ID, cache bucket, query generation, selected URI, and captured per-item
mutation revision still match. Revisions are assigned from a window-monotonic
cache mutation clock, so eviction and re-addition cannot reuse a value. Any
fresh page or newer validation that merges the same URI advances that revision,
so an older `not_found` cannot evict a newer authoritative result.

Validation uses source semantics: files must still resolve under the canonical
workspace root, conversations must still be live database rows, and commits
must still be reachable from the bucket's captured branch/HEAD rather than
merely existing somewhere in the Git object database. The same backend matcher
also determines `match` versus `not_match`, correcting any provisional
JavaScript/Rust case-normalization difference without globally evicting a live
resource.

Commit validation first compares the supplied opaque epoch with the
repository's current branch/HEAD. A mismatch is a typed `source_epoch_changed`
control result, not `not_found`: the controller refreshes Git identity and
switches buckets, while the old bucket remains valid for its historical epoch.
This known mismatch blocks Enter-time insertion from the inactive bucket; it is
not treated like a timeout that may proceed.

## Reference Search Protocol

The shared transport exposes source-neutral operations backed by
source-specific jobs:

```ts
type ReferenceSearchSource = "file" | "conversation" | "commit"

interface StartReferenceSearchRequest {
  searchSessionId: string
  sourceSequence: number
  requestId: string
  source: ReferenceSearchSource
  query: string
  workspacePath?: string
}

interface NextReferenceSearchPageRequest {
  searchSessionId: string
  sourceSequence: number
  requestId: string
  source: ReferenceSearchSource
  pageIndex: number
}

interface CancelReferenceSearchRequest {
  searchSessionId: string
  sourceSequence: number
  requestId: string
  source: ReferenceSearchSource
}

interface ReferenceSearchPage {
  sourceSequence: number
  requestId: string
  pageIndex: number
  items: ReferenceCandidate[]
  sourceEpoch?: string
  done: boolean
  doneReason?: "exhausted" | "limit"
}

interface ValidateReferenceCandidateRequest {
  validationRequestId: string
  source: ReferenceSearchSource
  uri: string
  query: string
  workspacePath?: string
  sourceEpoch?: string
}

type ReferenceCandidateValidation =
  | {
      validationRequestId: string
      status: "match" | "not_match"
      candidate: ReferenceCandidate
      regexRank?: { fieldTier: number; start: number; length: number }
    }
  | { validationRequestId: string; status: "not_found" }
```

`workspacePath` is required for file and commit starts/validation and omitted
for backend-global conversation operations. The backend canonicalizes it and
rejects a file root that is not an open workspace or a commit root that is not
inside the resolved repository. Search requests do not carry a result limit:
the registry snapshots the authoritative live `reference_search_limit` and its
limit epoch when it registers a job. A settings write updates that snapshot and
cancels old-epoch jobs under the registry lock, so a stale or custom HTTP client
cannot exceed the global setting.

Commit pages return a canonical `sourceEpoch` containing the resolved repo,
branch, and HEAD captured when the job starts. Every page in that job carries
the same epoch; the controller discards a page whose epoch no longer matches
the active workspace Git state and keys committed cache entries by it.

Each resource candidate includes a stable `sourceOrdinal`. Regex candidates
also include backend-computed match metadata (`fieldTier`, `start`, and
`length`). The frontend never re-executes a Rust regex to rank results; exact
regex cache entries retain this metadata. `match_reference_regex` returns the
same metadata for Agent/Profile IDs.

The client generates `searchSessionId` once per controller and maintains two
different counters. A frontend-only query generation increments on query,
limit, or close transitions and guards every async state update. Independently,
each resource source increments its own positive safe-integer `sourceSequence`
whenever that source starts or restarts and generates a new `requestId`. A
source-only recovery therefore does not invalidate healthy sibling sources.
Registry identity is `(searchSessionId, source)` with the current
`(sourceSequence, requestId)` as a compare guard. A higher-sequence start
atomically replaces and cancels the prior job; an equal sequence is accepted
only as an idempotent retry of the same request ID and arguments; a lower
sequence is a typed stale-start result. Therefore neither a late start nor a
late cancel from an old query can disturb its replacement.

The registry performs that ordering preflight and advances its sequence
high-water mark before source-specific query validation. An invalid higher
sequence therefore cancels the old job and blocks late lower-sequence starts,
but consumes no registered-job slot.

Cancellation must also work when its request reaches the backend before the
corresponding start handler registers. An unknown cancel therefore records a
30-second pre-cancel tombstone keyed by session, source, sequence, and request
ID. Start checks and consumes that tombstone under the same registry lock before
allocating a job and returns typed cancellation without scanning. IDs stored in
tombstones must be canonical UUIDv4 values. Tombstones and sequence high-water
records share a 256-entry process-wide guard-table cap and never affect a
different `requestId`. The table holds at most one record per
`(searchSessionId, source)`; a higher sequence supersedes lower tombstone state.

`start_reference_search` registers the supplied identity before scanning and
returns page index 0. Repeating start with the same identity and identical
immutable arguments joins an in-flight first-page computation or replays page
0; reusing one `requestId` with different arguments is invalid. This makes a
lost start response retryable without starting a second scan.

Page computations are registry-owned tasks; transport handlers only await their
shared result. Dropping an HTTP/Tauri waiter does not drop the scan or its replay
record. Explicit guarded cancellation/replacement, registry expiry, or the page
deadline owns task termination and source cleanup. A task publishes its page
only if the full source identity and limit epoch are still current.

`next_reference_search_page` supplies the desired page index and advances that
exact job by at most five matches. The registry retains immutable page 0 for
start replay plus the most recently returned page. Repeating the latest index
replays that page without advancing, the next index advances once, and any other
index is a typed stale-page error. Concurrent requests for the same next index
share one advancement. Page 0 may also be replayed through the next-page
operation when the original start response was lost. Because the controller
advances only after receiving the current page, no other historical page needs
retention; an entry stores at most ten candidates. `cancel_reference_search`
removes an existing entry only when the session, source, sequence, and request
ID all match. The controller still permits only one new-page call in flight per
source.

Jobs are pull-driven and apply backpressure: a worker does not scan the entire
source while waiting for the client. Query changes abort frontend calls,
cancel all three old identities, and increment a local generation. Starting
the replacements is also a cancellation backstop if an explicit cancel is
lost. Any response from an older generation is discarded even if transport
cancellation arrived too late. Closing the popup cancels live jobs but retains
the cache.

A final page releases its walker, database cursor, or Git process immediately,
but its lightweight registry entry retains page 0 and that final page for replay
until explicit cancel/replacement or 30 seconds of inactivity. Non-final jobs
use the same idle expiry. Each page request has a separate 30-second wall-clock deadline
covering both concurrency-permit wait and source scanning; timeout cancels and
releases the source and returns a typed source-local error. Thus an active but
stuck scan cannot evade idle reaping forever, and a lost final HTTP response
remains retryable.

`searchSessionId`, `requestId`, and `validationRequestId` are canonical UUIDv4
strings, and `sourceSequence` is constrained to JavaScript's positive
safe-integer range. The registry retains a lightweight
per-session/source high-water record containing the sequence, request ID, and
immutable-argument fingerprint (including the internal limit epoch) for five
minutes after the current entry is removed. This prevents a very late old start
from becoming current after cancel, invalid-pattern rejection, or expiry, while
preserving exact retries.
When removal has a terminal control/error result, the record replays that result
for an exact equal-sequence retry; starting new work requires incrementing the
source sequence.
Expired guard records are swept before admission; when all 256 entries are
still live, a request needing a new guard returns typed overload rather than
evicting a live ordering/cancellation guarantee. The registry allows at most
one current entry per
`(searchSessionId, source)`, at most 64 registered entries including completed
replay entries, and registered source caps of 24 file, 32 conversation, and
eight commit entries. At most 12 pages scan concurrently process-wide and at
most four per source. Extra registered scans wait cancelably for permits within
those entry caps. Registry or guard-table overload returns a typed source-local
error instead of spawning or retaining unbounded work.

### Source implementations

Files retain an ignore-aware walker between page requests. They return both
files and directories, honor the existing ignore rules and hard-skipped names,
use stable name ordering within directories, and stop after the configured
number of matches. Directory symlinks are not followed, and every returned URI
is checked to remain under the canonical workspace root.

Conversations query the application SQLite database rather than reparsing all
agent files. Search covers root regular/chat conversations in all active
projects, excludes delegated children, loop rows, and soft-deleted rows, and
iterates by `updated_at DESC, id DESC` with a keyset cursor between pages. The
query joins folder metadata so each cross-project candidate can display and
match its owning project name/path. Each job retains a bounded seen-ID set, so a
row changing sort position between page requests cannot be returned twice;
concurrent inserts/updates may wait for the next query rather than forcing a
long-lived SQLite read transaction.

Commits use a lightweight, cancellable `git log` stream for the current branch
of the current repository; the mention search does not request raw diffs,
numstat, or file lists. It iterates newest first and terminates the child
process on cancellation or limit completion. In detached-HEAD state it searches
the captured `HEAD` history and records a detached marker rather than inventing
a branch name.

## Matching and Ranking

The query identity is the exact Unicode text after `@`; neither runtime trims
it, normalizes Unicode, or changes line endings for cache/protocol identity.
The ASCII, case-sensitive prefix `re:` selects regex mode and is removed only
for compilation. All other non-empty query text is literal. This exact identity
is also what guards validation responses.

Literal search is case-insensitive. Searchable fields are:

| Source | Primary fields | Secondary fields |
| --- | --- | --- |
| Agent/Profile | Display name | Agent type, description, model |
| File | Name | Relative path |
| Conversation | Title | ID, agent, status, branch, project |
| Commit | Short/full hash, subject | Full message, author |

For example, `re:^src/.*\.tsx$` is regex mode. Regex matching is case-sensitive
by default and supports Rust inline flags such as `(?i)`. An empty `re:` pattern
is invalid. Literal queries are limited to 512 UTF-8 bytes; regex patterns are
limited to 256 UTF-8 bytes. Compilation also uses an explicit 1 MiB compiled
automaton size limit in addition to the Rust `regex` crate's linear-time
matching guarantee.

Rust `regex` is authoritative for every group; lookaround and backreferences
are unsupported. For regex mode, the controller calls a lightweight
`match_reference_regex` core with the enabled Agent/Profile descriptors while
starting the three resource requests independently. The helper both validates
the expression and returns matching stable IDs. Repeated expressions may show
their exact cached snapshot while refreshes run. This avoids pretending
JavaScript and Rust regular expressions have identical Unicode or inline-flag
semantics and prevents a transient Agent/Profile helper error from blocking
otherwise healthy resource groups.

Each helper request accepts at most 1,024 descriptors and 4,096 UTF-8 bytes of
searchable text per descriptor; it is a matcher for already-known catalog rows,
not a general regex execution API. A larger enabled catalog is partitioned in
stable order into 1,024-entry batches, with at most four helper calls in flight,
and the controller merges their stable IDs/ranks without truncation. Any batch
failure is isolated to the Agent/Profile group and never blocks resource groups.

After the sequence preflight, every resource start compiles and validates the
pattern before acquiring a registered-job slot, so an HTTP client cannot bypass
regex bounds or syntax checks by skipping the Agent/Profile helper. Invalid
concurrent starts return the same typed pattern error without registering jobs.

An invalid pattern starts no resource jobs, retains the previous/cache rows for
visual continuity, marks them non-selectable, and publishes a localized pattern
error. Once the pattern becomes valid, normal selection resumes.

The backend scans in source order until it has the configured number of
matches. The frontend ranks only the collected set:

1. Exact primary-field match.
2. Primary-field prefix match.
3. Primary-field word-boundary match.
4. Primary-field substring match.
5. Secondary-field match.
6. Stable source order as the final tie-breaker.

When several fields match one candidate, both literal and regex modes retain
the best tuple according to the declared primary/secondary field order; a worse
secondary match cannot replace a primary match's rank metadata.

Regex ranking uses the returned match metadata: primary fields first, then an
earlier match start, then a shorter match, then `sourceOrdinal`. Because search
stops at the configured cap, a better match later in the source is
intentionally allowed to remain unseen. `start` and `length` are UTF-8 byte
offsets from Rust `regex`; the frontend compares them numerically and never uses
them to slice JavaScript strings.

## IME Behavior

The mention plugin must not search, move selection, select a candidate, or
submit while `editor.view.composing`, `event.isComposing`, or key code 229
indicates active composition.

On `compositionend`, schedule one microtask after ProseMirror applies the final
text, then re-read the current document and caret and re-run mention matching.
If the caret still follows a valid `@query`, open or update the picker exactly
once. A composition sequence number plus comparison with the suggestion
plugin's current range/query makes this idempotent when the final ProseMirror
transaction already reopened the suggestion. This applies equally to Chinese
and English characters entered through an IME. The Enter used to confirm an
IME candidate remains consumed by IME handling and cannot select a reference
or submit the message.

## Error Handling and Isolation

Both transports expose the same structured reference-search error codes:

| Code | Controller behavior |
| --- | --- |
| `cancelled`, `stale_start` | Silent control flow; never mutate current state |
| `job_expired`, `stale_page`, `limit_epoch_changed` | Restart only that source with a higher source sequence when the query generation is still current |
| `invalid_pattern` | Keep continuity rows non-selectable and show one localized pattern error |
| `source_epoch_changed` | Refresh Git identity, switch commit bucket, and do not evict historical cache |
| `source_timeout`, `registry_overloaded`, `source_failed` | Retain cache preview and show a localized group error |
| `invalid_request` | Retain cache preview, log protocol detail, and show a generic group error |

- Source failures are isolated per group and never clear other groups.
- A failed source retains its cache preview and shows a group-local error.
- Cancellation, generation expiry, replay-entry expiry, and limit-epoch changes
  are normal control flow, not user-facing errors.
- An unknown/expired job or stale page index returns a typed control-flow result
  so the client can restart only that source if the query is still current.
- Invalid regex keeps old rows visible but non-selectable until corrected.
- Candidate validation errors preserve cache entries; only authoritative
  `not_found` invalidates one.
- Backend path validation prevents file validation or search from escaping the
  canonical workspace root.
- Server-side limits, pattern bounds, concurrency limits, and cancellation are
  enforced even when an HTTP client bypasses the frontend.
- Hidden title execution has no target-project working directory, injected
  collaboration tools, or interactive permission path.

## Events and Runtime Parity

The existing conversation-upsert event carries successful title changes; no
new title-specific frontend event is required. A separate global
conversation-experience settings event carries the revisioned full settings
snapshot after either independent setter commits. Delegation settings/profile
mutations similarly broadcast the revisioned normalized catalog for mentions.
The settings cores, regex matcher, candidate validator, and search
registry are reachable through both Tauri command registration and Axum
handlers. WebSocket broadcasting remains responsible for propagating
conversation and settings updates to other clients.

Reference cache and generation state are intentionally per frontend window.
Search jobs are backend process state and are namespaced by bounded random
`searchSessionId` plus source and guarded by `(sourceSequence, requestId)`.
Source sequences are generated and interpreted only within their controller
session; they have no cross-source or cross-window ordering meaning. IDs are
unguessable rather than authorization tokens, and the existing transport
authentication remains the security boundary.

## Testing Strategy

### Automatic title tests

- Independent settings writes cannot clobber each other and broadcast one
  converged settings snapshot after commit.
- Reordered settings events cannot overwrite a higher applied revision.
- Migration defaults make all existing conversations ineligible.
- Every Codeg root/delegated creation path atomically creates exactly one job
  when enabled; raw-session imports and fork-history siblings never do, while a
  pending job follows the live side of a fork.
- The shared send hook covers linked and already-linked producers, prefers UI
  `visibleText`, uses the safe fallback for older/non-UI callers, captures
  root/delegated tasks once, preserves display labels, strips internal
  directives/bytes, and enforces both 4,000-scalar context caps.
- Reserve failure/cancellation creates no title capture, capture-transaction
  failure enqueues no prompt, and a fast successful turn cannot race ahead of
  capture persistence.
- Root effective locale reaches the job, delegated children inherit it, and
  non-UI producers take the documented fallback.
- Abnormal and empty turns do not consume an attempt.
- First success captures only visible task/reference labels/final response.
- Two rapid turns delayed in the lifecycle queue retain their own immutable
  final text, locale, and token rather than re-reading the newest session state.
- Completion sidecars reach lifecycle consumers but never serialize or enter
  the per-connection recent-event replay ring.
- Re-delivery of one turn token cannot increment `usable_turn_seq` or
  double-claim a job.
- Success writes without changing conversation recency and broadcasts once.
- Manual rename wins races before, during, and after hidden execution.
- Native title refresh cannot replace a finalized generated title.
- Forking a finalized conversation preserves generated-title protection on both
  the live row and historical sibling without creating a sibling title job.
- First failure waits for the target's next usable success; second failure
  terminates.
- If that next usable success occurs while attempt one is still running, its persisted
  turn sequence makes attempt two ready immediately after failure.
- Startup recovery preserves ready jobs and accounts for interrupted attempts.
- Disabling or changing settings follows the specified pending-attempt rules.
- Manual rename, disabling, and soft delete cancel active work and make late
  results fail their atomic predicate.
- A `Noop` internal connection publishes only to its private stream, skips MCP
  injection, and never reaches lifecycle or transport subscribers.
- SessionStarted arriving before private-stream subscription is recovered from
  the atomic identity snapshot rather than timing out.
- A handshake exceeding 15 seconds releases discovery scans but remains hidden
  by the reserved-root rule and sends no prompt before durable ID registration.
- The discovery lease, ID exclusion, and reserved-root fallback precede prompt
  sending and exclude every Codeg list, statistic, detail, and import entry
  point, including handshake and registry-write failures.
- Ready jobs are not claimed while waiting for an attempt permit, and database
  commit retry cannot retain more than two generated outputs.
- A fake ACP runner covers success, timeout, permission request, abnormal stop,
  malformed output, disconnect, and registry failure without external CLIs.

### Reference backend tests

- Every source returns pages of five except the final partial page.
- Each source stops exactly at its independent configured cap.
- A client-generated request can be canceled before its first page returns;
  cancel-before-register tombstones stop it even when HTTP requests reorder,
  while an old start or cancel cannot replace/remove the newer sequence.
- File ignore behavior and stable traversal order remain intact.
- Conversation search spans projects, excludes children/loop/deleted rows, and
  keyset-pages without duplicates.
- Commit search stays on the current branch, omits diff payloads, streams newest
  first, and kills its process on cancellation.
- Literal and regex fields, syntax bounds, and rank scores match the contract.
- Regex candidates carry authoritative match metadata and the frontend sorts
  them without executing the pattern again.
- Regex Agent/Profile matching uses the Rust helper; invalid regex registers
  zero resource jobs, helper failure does not block sibling resource groups,
  and stale continuity rows remain non-selectable.
- Agent/Profile regex catalogs over 1,024 descriptors batch without truncation
  or exceeding four concurrent helper calls.
- Duplicate starts, first/final-page response replay, concurrent/stale page
  indices, pre-cancellation, inactivity expiry, per-page timeout, registry
  overload, and hard limits work.
- Registered-entry and active-scan tests enforce the 24/32/8 source caps, the
  64-entry total, and the 12-global/four-per-source scan gates.
- An expired source restarts with only its own higher `sourceSequence`; healthy
  sibling entries and frontend results remain current.
- The backend ignores client attempts to raise the global result cap, and a
  limit-epoch change cancels every old-epoch job, including a registration race.
- Selection-scoped URI validation returns `match`, `not_match`, and `not_found`
  distinctly without delaying search pages; only `not_found` means deletion.
- Commit epoch mismatch returns `source_epoch_changed`, switches the active
  bucket, blocks stale insertion, and does not evict an otherwise valid
  historical candidate.
- Tauri and HTTP handlers call the same core behavior.

### Reference frontend tests

- All ten locale catalogs contain the new settings and picker-state keys.
- The profile store loads outside mention opening; bare `@` performs zero
  mention-triggered backend calls and includes every effectively enabled
  profile.
- A profile change from the settings window updates the open picker/catalog
  through the revisioned global profile event without a focus refresh, and an
  older event cannot roll it back.
- Catalog changes recompute literal rows or rerun only the regex helper without
  restarting resource searches.
- Cached literal and repeated-regex results publish before network completion.
- File, conversation, and commit pages update independently.
- Conversation upserts refresh already-cached metadata and deletes evict the URI
  without waiting for selection validation; regex membership is invalidated
  rather than re-evaluated in JavaScript.
- Popup close cancels jobs but cache survives remount; backend reset clears it.
- A result-limit change re-truncates cache and restarts only a current non-empty
  search under a new generation.
- Late generations never mutate current groups.
- Selection follows URI through insertion, sorting, paging, and cache refresh.
- Explicitly deleted selected items move selection to the nearest survivor.
- Late validation for an old generation/selection cannot remove the new item.
- A late `not_found` captured before a fresh page returned the same URI is
  rejected by the per-item mutation revision.
- `not_match` removes a URI only from the current provisional result and keeps
  its refreshed metadata available to other queries; `not_found` evicts it from
  the item index and every regex snapshot.
- Enter-time validation success blocks known-negative insertion, while timeout
  permits insertion and invalidates the late response before closing.
- Cache LRU never evicts selected or visible candidates.
- Item and regex-snapshot LRUs enforce their independent caps without copying
  candidate metadata, including across many workspace buckets.
- Source errors retain cached candidates and do not affect sibling groups.
- Invalid regex keeps old rows visible but prevents stale selection/insertion.
- A new uncached regex keeps no provisional selection until an authoritative
  group returns a match; an exact cached expression preserves selection by URI.
- IME composition suppresses action and `compositionend` re-matches Chinese
  and English final text exactly once.
- The candidate-confirmation Enter never inserts a reference or submits.

## Acceptance Criteria

- When the configured title agent returns a valid result, a newly created root
  or delegated conversation receives one generated title after its first usable
  successful response, even when its tab is closed.
- The title request contains only bounded visible task/final-response text and
  uses the effective interface locale supplied by the originating session.
- The title appears immediately in every connected sidebar and persists after
  restart without changing list recency.
- Manual titles are never overwritten, and generated titles are never replaced
  by native CLI title metadata.
- A failed title request runs at most once more and only after the target's next
  usable successful response.
- A bare `@` immediately shows enabled agents and profiles and makes no file,
  conversation, Git, or profile request as a consequence of opening the picker.
- Non-empty searches show cached matches immediately and independently append
  five-result pages from each resource source.
- Each source stops at the configured limit, and changing the query cancels and
  isolates every prior generation, including a scan that has not returned its
  first page. A direct transport caller cannot raise that backend-owned limit.
- The selected candidate does not jump when pages arrive or ranking changes;
  while the query and active group stay the same, it moves only when the user
  moves it or the backend proves it no longer exists or no longer matches. A
  changed query may pick a new item when the old URI no longer matches.
- `re:` queries, source-local errors, and IME completion behave consistently in
  desktop and server deployments.
