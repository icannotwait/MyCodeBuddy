# Conversation Opening, Thinking Visibility, and Last Agent Design

## Status

Approved on 2026-07-16.

## Problem

Three related chat usability issues need independent fixes:

1. Opening an uncached long conversation starts at the top and visibly animates
   the scrollbar toward the bottom while virtualized rows are measured. The
   conversation should appear at the bottom immediately on its first successful
   history render.
2. Thinking/reasoning blocks are always visible. Users need a per-agent display
   preference that can hide both live and historical thinking while retaining
   the agent's current activity, tools, elapsed time, and edit statistics.
3. A project without an explicitly pinned default agent falls back to agent
   ordering. It should instead remember the agent used by the most recently
   created conversation in that project.

These concerns must remain separate. A display preference must not mutate an
agent's runtime configuration, and implicit recency must not overwrite the
existing explicit project default.

## Goals

- Instantly show the bottom of an uncached conversation on its first successful
  history render.
- Preserve the cached tab's scroll position on later tab switches and reloads.
- Add a globally persisted `show_thinking` preference for each agent.
- Default thinking display to off for existing and newly created agent rows.
- Hide thinking only from the on-screen message presentation.
- Preserve thinking in canonical state, copying, and exports.
- Persist the last agent used to create a normal root conversation per project.
- Keep the existing explicit-default and contextual-inheritance behaviors.
- Support both Tauri and Axum through shared backend core functions.

## Non-Goals

- Deleting, redacting, or suppressing thinking data at ingestion or on disk.
- Changing what agents emit or how parsers represent thinking.
- Restoring a persisted scroll position after a tab has been closed.
- Recording agent selection from an unsent draft or from opening old history.
- Recording delegation children, imported sessions, automations, or folderless
  Chat mode as project recency.
- Translating the new setting into all ten supported languages in this change.
  Author the new copy in `en` and `zh-CN`; use the English value for the same
  keys in the other locale catalogs so every locale remains runtime-safe. Full
  localization is deferred.

## Decision

Use explicit database fields for both persisted concepts and a mount-local
initial-scroll lifecycle for the uncached conversation behavior.

- `agent_setting.show_thinking` owns the per-agent presentation preference.
- `folder.last_agent_type` owns implicit per-project recency.
- `ConversationTabView`/`MessageListView` owns a one-shot initial history scroll
  phase for the lifetime of its existing keep-alive tab component.

Do not store these values in local storage, native agent config JSON, or the
existing `folder.default_agent_type` field.

The three user-visible changes are independently testable and will use three
implementation plans: agent thinking visibility, project last-agent recall,
and uncached-conversation initial scroll. Each plan must leave the repository in
a working state without depending on a later plan.

## Data Model

Add independent migrations so either persisted feature can land on its own:

```text
m20260716_000001_agent_show_thinking
  agent_setting.show_thinking BOOLEAN NOT NULL DEFAULT FALSE
m20260716_000002_folder_last_agent
  folder.last_agent_type TEXT NULL
```

Adding `show_thinking` with a database default of `FALSE` intentionally makes
thinking hidden after upgrade for every existing agent. New agent rows created
by `ensure_defaults` also start with thinking hidden.

Expose the values through the existing cross-runtime models:

- Rust `AcpAgentInfo.show_thinking: bool`
- TypeScript `AcpAgentInfo.show_thinking: boolean`
- Rust `FolderDetail.last_agent_type: Option<AgentType>`
- TypeScript `FolderDetail.last_agent_type: AgentType | null`

Invalid or unknown serialized agent values parse as no recency and therefore
fall through to the next selection candidate.

## Agent Thinking Preference

### Settings UI

The selected agent's detail panel receives a labeled `Show thinking` switch.
The switch is global for that agent across all projects and conversations.

The UI updates optimistically. On persistence failure it restores the previous
value and shows an error toast.

### Persistence API

Add a dedicated command and matching web handler:
`acp_update_agent_display_preferences(agent_type, show_thinking)`. It updates
only `agent_setting.show_thinking` and emits the existing
`app://acp-agents-updated` event. Chat consumers refresh through the shared
`useAcpAgents` subscription. The originating settings panel owns a separate
local `agents` array, so it updates that row optimistically and rolls it back on
failure instead of waiting for the shared hook.

This command must not call the agent environment/config update functions, alter
the session configuration fingerprint, mark running connections stale, or show
a restart-required notice.

### Rendering

Resolve `show_thinking` once near the conversation/message-list boundary and
pass the boolean into historical and live render paths. Do not subscribe every
individual message row to the complete agent registry.

When the setting is false:

- historical `reasoning` parts render nothing;
- live transcript `thinking` segments render nothing;
- no Markdown renderer is mounted for hidden thinking content;
- plans, text, tool calls, generated images, sub-agent activity, and permission
  or question controls render normally;
- `LiveTurnStats` continues to inspect canonical live content, so thinking vs.
  streaming activity, elapsed time, tool counts, and edit statistics remain
  available.

Historical rendering passes `showThinking` through `HistoricalMessageGroup` to
`ContentPartsRenderer`; the renderer skips reasoning in its recursive
`renderPart` function, including reasoning nested in a goal run. Live rendering
filters thinking segment IDs while building `LiveFooterItem[]`, before mounting
`LiveTranscriptSegmentView`, so a hidden thinking segment has no per-delta view
subscription or Markdown work. If all current live footer items are hidden
thinking, `LiveTranscriptRow` returns no message body while the separate live
activity row continues to render.

Agent settings load asynchronously. Until a matching persisted record is
available, the view must fail closed and hide thinking to avoid a content flash.

The preference is presentation-only. Adapters, transcript stores, historical
turns, copy helpers, and export helpers retain the complete reasoning data.
Turning the switch back on reveals already-loaded thinking without a transcript
reload. Copying a message or exporting a conversation keeps the current complete
data behavior even while thinking is hidden on screen.

## Project Last-Agent Recall

### Recording

After a normal project root conversation row has been created successfully,
record its agent in that regular folder's `last_agent_type`.

The standard desktop and server create paths share `create_conversation_core`,
so the recency update belongs in the shared backend flow. It runs only after the
conversation insert succeeds. Because the conversation already exists at that
point, failure of the auxiliary recency write must be logged as a warning and
must not turn the create request into an error; otherwise a frontend retry could
create a duplicate conversation.

After the shared core returns, both the Tauri command wrapper and Axum handler
emit the existing `folder://changed` upsert with the fresh `FolderDetail`.
`AppWorkspaceProvider` applies that event to `folders` and `allFolders`, so all
open clients immediately see the new recency without a new frontend-only patch
path. A dropped event reconciles on the existing refresh/reconnect paths.
Concurrent clients use last successful database write wins semantics.

Do not update recency for:

- changing the agent selector in an unbound draft;
- opening or switching to an existing conversation;
- delegated child conversations;
- imported sessions or automation-created sessions;
- hidden folders backing folderless Chat mode;
- failed conversation creation.

### Resolution Priority

Extend the pure default-agent resolver with the folder's last agent. A new draft
uses the first usable candidate in this order:

1. `folder.default_agent_type`, when explicitly configured;
2. the active conversation's agent when the caller explicitly requests
   `inheritFromActive`;
3. `folder.last_agent_type`;
4. the first enabled and available agent in the user's saved order;
5. the existing global hard fallback.

The existing contextual inheritance behavior therefore remains stronger than
project recency. Normal sidebar/title-bar/shortcut new-conversation entry points
do not request inheritance and use recency when no explicit default exists.

When the fresh agent registry proves that `last_agent_type` is disabled or
unavailable, skip it without deleting the persisted value and continue to the
ordered fallback. During cold hydration, a recency-derived draft remains
provisional until the fresh enabled/available list can validate it.

Concretely, a non-null recent agent is returned as provisional while `fresh` is
false. Once `fresh` is true, use it only when `sortedTypes` contains it;
otherwise continue to `sortedTypes[0]`. Explicit folder defaults and requested
inheritance keep their existing confirmation/fallback semantics.

## Initial Uncached Conversation Scroll

### Existing Keep-Alive Boundary

`ConversationDetailPanel` already renders every open tab and hides inactive tabs
with CSS. Each `ConversationTabView` remains mounted until its tab is closed.
That component lifetime is the cache boundary, so no global visited-session set
or persisted scroll metadata is required.

### First-History Lifecycle

Only a tab whose `ConversationTabView` was already bound to a persisted
conversation when it mounted begins with an `initialHistoryScrollPending`
latch. A new draft starts with the latch cleared, and binding that still-mounted
draft after its first send does not enable it. For an eligible persisted
conversation:

1. Keep `MessageThread.resize` set to `instant` while detail history is loading,
   the first historical projection is committed, and Virtua performs its first
   measurements.
2. Once historical rows exist, issue an explicit instant `scrollToBottom`.
3. Compare the shared content height and viewport `scrollHeight` on animation
   frames until both are unchanged for two consecutive frames.
4. Issue one final instant correction and clear the latch.
5. Restore the existing behavior after initialization: history-only resize uses
   `smooth`; a live transcript continues using `instant`.

The initialization controller is one-shot for that mounted tab. It is not reset
by active/inactive changes, a manual detail reload, or a draft becoming bound to
its newly created conversation.

If the initial history load fails, leave the latch pending. A later successful
retry performs the first instant placement. If the user starts wheel, touch,
pointer, PageUp, Home, or ArrowUp navigation before stabilization completes,
cancel the controller and clear the latch immediately so it does not fight user
intent.

Closing the tab unmounts its keep-alive view. Reopening it creates a new
uncached view and correctly performs the first-open behavior again.

## Error Handling

- A failed thinking-preference save rolls back the optimistic switch and reports
  the error.
- A failed recency update is warning-only after successful conversation insert.
- An unavailable or malformed recent agent falls through to the next candidate.
- Thinking remains hidden while agent preferences are unavailable or loading.
- Initial-scroll observers and animation frames are disposed on completion,
  user escape, or unmount.
- Existing live-follow escape, message navigation, and cached scroll state
  remain authoritative after the initial latch clears.

## Testing

### Rust

- Migration/default tests prove existing and new agent rows expose
  `show_thinking == false`.
- Agent preference service/command tests prove the focused update preserves
  `enabled`, `env_json`, provider binding, native config, and sort order.
- Agent listing returns the persisted display preference.
- Normal root conversation creation records `last_agent_type` only after a
  successful insert.
- The Tauri and Axum create wrappers emit a fresh `folder://changed` upsert
  after recency persistence, and the frontend applies it to both folder lists.
- Chat-mode creation, delegation children, imports, and failed creates do not
  change a regular project's recency.
- Desktop and web handlers exercise the same core behavior.

### TypeScript and React

- Resolver unit tests cover explicit default, requested inheritance, project
  recency, unavailable recency, ordered fallback, and cold-start provisional
  correction.
- Settings tests cover default-off display, optimistic success, rollback on
  failure, and cross-window refresh through the agent update event.
- Historical renderer tests prove reasoning disappears while other content
  remains.
- Live transcript tests prove thinking disappears while live activity and tool
  statistics remain.
- A hidden live thinking segment never mounts `LiveTranscriptSegmentView`, and
  a thinking-only footer leaves no empty message body.
- Copy/export tests prove hidden reasoning is still included in complete output.
- Message-thread tests prove loading/measurement uses instant resize and
  performs a final instant correction after two stable frames.
- Keep-alive tests prove switching away/back and manual reload do not re-run the
  initial scroll, while close/reopen does.
- Escape tests prove user scrolling cancels initialization without changing the
  existing live-follow behavior.

Run focused tests first, then the relevant frontend suite and the desktop/server
Rust checks required by `AGENTS.md`. The implementation plan will name exact
commands and file-level test targets.

## Acceptance Criteria

1. An uncached long conversation first appears at its bottom without a visible
   smooth traversal; a cached tab keeps its current scroll position.
2. Every agent defaults to hiding live and historical thinking after migration.
3. The per-agent switch updates all open views without restarting the agent.
4. Hidden thinking remains in canonical state, copies, and exports.
5. Activity status, tools, elapsed time, and edit statistics remain visible when
   thinking is hidden.
6. A successful normal project conversation updates that project's recent agent;
   drafts, old history, child sessions, Chat mode, and failures do not.
7. Agent selection follows explicit default, requested inheritance, project
   recency, ordered available agent, then hard fallback.
8. Tauri and server deployments share the same persisted behavior.
9. `en` and `zh-CN` contain authored setting copy; the other locale catalogs use
   the English values until full localization, without missing-key failures.
10. A successful project create broadcasts the updated folder so every open
    client uses the same recent agent for its next normal new conversation.
