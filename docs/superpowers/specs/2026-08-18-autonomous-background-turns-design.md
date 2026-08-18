# Autonomous Background Turns Design

## Status

Direction approved in conversation on 2026-08-18. This document is the
implementation specification.

The first release supports only Claude Code and Grok. Every other built-in or
custom ACP agent remains explicitly unsupported until its autonomous-turn
behavior has been captured and verified. No implementation plan has been
approved yet.

## Executive Decision

Codeg will generalize the existing `background_activity` overlay into a
provider-neutral autonomous-activity pipeline while keeping provider detection
and recovery rules explicit:

```rust
enum AutonomousActivityPolicy {
    ClaudeTranscript,
    GrokIdleWire,
    Unsupported,
}
```

Claude Code keeps its existing transcript-tail watcher. Grok gains an idle ACP
observer that recognizes the verified background-task callback sequence and
arms a short-lived `updates.jsonl` tail assembler. The assembler emits complete
`MessageTurn` upserts through the existing `BackgroundActivity.turns` path.

An autonomous reply is rendered as an independent assistant message with a
localized `后台续写` marker. The hidden Grok `<system-reminder>` opens the
episode but is never rendered, copied, persisted by Codeg, or exposed in an
event payload.

Codeg will not remove the frontend out-of-turn streaming guard. Ordinary wire
deltas still require `status == "prompting"`; autonomous content enters the UI
only through the overlay pipeline. This prevents a background continuation
from being grafted onto the previous completed reply.

## Problem and Evidence

Session `codeg://session/3806` established that the monitor itself was not the
failure:

1. Grok's background task completed and its callback ran.
2. Grok generated a complete follow-up turn.
3. The complete sequence was persisted to Grok's `updates.jsonl` through about
   17:39.
4. Codeg's stored/live activity stopped around 17:27, and no follow-up message
   appeared in the session UI.

The loss is deterministic in the current architecture:

- `applyStreamingAction` in
  `src/contexts/acp-connections-context.tsx` drops streaming updates whenever
  the connection is not `prompting`.
- That guard assumes every out-of-turn update will be represented by the
  `background_activity` overlay.
- `src-tauri/src/acp/background_watch.rs` produces that overlay only for
  Claude Code.
- Grok therefore has no out-of-turn render producer.
- The idle branch in `src-tauri/src/acp/connection.rs` does not finalize Grok's
  private `_x.ai/session/update` `turn_completed`; that terminal logic exists
  only in the active `session/prompt` branch.

The resulting path is:

```text
Grok idle ACP callback succeeds
  -> idle loop receives hidden trigger + assistant/tool updates
  -> backend forwards some ordinary events
  -> frontend out-of-turn guard drops them
  -> no Grok background_activity producer exists
  -> persisted follow-up remains invisible until some unrelated reload
```

The guard itself is correct. Removing it would revive the older corruption in
which an idle delta mutates the previous turn's completed `liveMessage`.

## Goals

- Surface Grok background-task follow-up turns without requiring a new user
  prompt or a manual reload.
- Keep each autonomous continuation independent from the preceding foreground
  assistant reply.
- Show a visible, localized background-continuation marker on the assistant
  message.
- Never render Grok's hidden system reminder as a user message.
- Preserve Claude's existing task accounting, keepalive, settlement, overlay,
  and watermark behavior.
- Reuse the existing `BackgroundActivity` event and frontend overlay store.
- Give every live autonomous turn a stable id across incremental whole-turn
  upserts.
- Make duplicate or replayed provider events idempotent.
- Hand live overlay content to the authoritative parser without a disappearance
  or duplicate-message race.
- Recover Grok content from `updates.jsonl` after a missed wire update,
  reconnect, application reload, or process restart.
- Prevent active background work from being reaped by the idle connection
  sweeps.
- Keep memory, transcript work, tool output, and stale episode lifetime bounded.

## Non-Goals

- Generic autonomous-turn support for Codex, Cursor, OpenCode, Gemini, Cline,
  Hermes, CodeBuddy, Kimi, Pi, DeepSeek, or custom ACP agents.
- Inferring support because another agent happens to emit a similarly named
  update.
- Changing the ACP protocol or requiring an upstream Grok change.
- Rendering the hidden trigger or its raw `<system-reminder>` body.
- Treating an autonomous episode as a Codeg-owned prompt or setting
  `turn_in_flight` for it.
- Writing synthetic autonomous turns to the Codeg database.
- Adding a database migration.
- Replacing the normal foreground streaming path.
- Patching Grok's background launch cards from `task_completed` snapshots. The
  existing tool stream and output-poll cards remain authoritative in V1.
- Guaranteeing live overlay streaming when Grok has not yet persisted the
  corresponding records. Cold-load correctness takes priority over displaying
  unconfirmed wire content.

## Terminology

**Foreground turn** means a turn initiated by a prompt sent through Codeg's
`session/prompt` path. It owns `turn_in_flight`, `prompting`, and `liveMessage`.

**Autonomous turn** means assistant work that starts while no Codeg prompt is in
flight. Background-task callbacks are one autonomous origin; scheduled
automation and agent-initiated work are other possible origins.

**Hidden trigger** means an agent-authored user-shaped record that wakes the
model but is marked not to appear in scrollback. For the verified Grok path it
is a `user_message_chunk` with `_meta.hideFromScrollback == true` and a
background-task reminder body.

**Episode** means one hidden trigger and the assistant/thinking/tool updates it
causes, ending at the matching `turn_completed`.

**Overlay** means temporary `MessageTurn` data held in
`conversation-runtime-store.backgroundTurns` until a detail parse has consumed
the same transcript bytes.

**Transcript watermark** means the count of complete bytes consumed from the
provider's authoritative session transcript. For Claude this is the Claude
JSONL file; for Grok it is `updates.jsonl`. It is never an ACP sequence number,
event count, timestamp, file mtime, or Codeg event sequence.

## Core Invariants

1. Out-of-turn content never enters ordinary `liveMessage`.
2. Autonomous content never attaches to the previous completed turn.
3. A hidden trigger never becomes visible content.
4. One autonomous assistant turn keeps one id for all incremental upserts.
5. Replayed provider events do not duplicate blocks or turns.
6. An overlay is retired only when `ConversationDetail.transcript_watermark`
   covers the same provider transcript bytes named by the overlay watermark.
7. A missed wire event remains recoverable from the provider transcript.
8. A transcript read may delay display, but it must not invent or lose content.
9. Autonomous work does not claim a Codeg prompt generation and does not emit a
   foreground `TurnComplete`.
10. A user prompt is never sent concurrently with an open Grok autonomous
    episode.
11. Claude behavior remains unchanged except for normalized origin metadata and
    provider-neutral naming.
12. Unsupported providers stay unsupported even when their event JSON looks
    superficially similar.

## Selected Architecture

### Provider-neutral coordinator

A small ACP module owns policy selection and the common event contract. It
does not contain provider heuristics. Conceptually:

```rust
impl AutonomousActivityPolicy {
    fn for_agent(agent: AgentType) -> Self {
        match agent {
            AgentType::ClaudeCode => Self::ClaudeTranscript,
            AgentType::Grok => Self::GrokIdleWire,
            _ => Self::Unsupported,
        }
    }
}
```

The connection setup uses the selected policy once and creates exactly one
adapter:

- `ClaudeTranscript`: spawn the existing connection-scoped watcher.
- `GrokIdleWire`: create an idle-wire observer plus a dormant transcript tail
  state.
- `Unsupported`: create no observer, watcher, or heuristic fallback.

The coordinator exposes narrow hooks rather than taking over the connection
loop:

```text
on_session_ready(session_id, cwd)
on_foreground_started()
on_dispatch(raw_dispatch, ownership = foreground | idle)
on_foreground_ended()
on_disconnect()
```

Claude uses the foreground hooks for its existing prompt ledger and held-turn
suppression. Grok uses them to arm/rebaseline its transcript cursor and to make
the idle/foreground ownership boundary explicit. Grok may observe task lifecycle
records under either ownership for accounting, but it may open an autonomous
episode only under `idle` ownership.

### Why Grok is wire-triggered but transcript-backed

The ACP stream supplies the fact that the sequence is occurring while the
connection is idle. The transcript supplies stable byte offsets, replay
recovery, and the exact persisted content the history parser will later return.

The Grok adapter therefore does not immediately publish unconfirmed wire
deltas. The wire observer opens and advances an episode; an immediate
`updates.jsonl` tail pass consumes the matching persisted records and emits the
whole assembled turn with the committed byte offset. A one-second active retry
handles normal file-write lag. This produces incremental updates in practice
without creating an overlay that the authoritative parser cannot yet cover.

The adapter may share a record scanner/accumulator with `parsers/grok.rs`, but
the responsibilities remain separate:

- the observer decides whether a record belongs to an idle autonomous episode;
- the scanner establishes complete-line byte offsets;
- the accumulator converts persisted records into `MessageTurn` blocks; and
- the normal Grok parser remains the cold-load authority for the full session.

### Module boundary

The intended boundary is:

```text
acp/autonomous_activity.rs
  policy selection
  normalized origin/episode contracts
  adapter lifecycle hooks

acp/background_watch.rs
  ClaudeTranscript adapter (existing implementation, minimally generalized)

acp/grok_autonomous.rs
  GrokIdleWire observer
  task ledger
  episode state machine
  event-triggered updates.jsonl tailing

parsers/grok.rs
  shared persisted-record scanner/assembler
  transcript watermark
  cold-load origin recovery
```

Provider-specific parsing must not move into the common coordinator.

## Provider Capability Policy

The initial policy table is closed:

| Agent | Policy | Detection source | Render source | V1 status |
| --- | --- | --- | --- | --- |
| Claude Code | `ClaudeTranscript` | prompt ledger + Claude transcript | Claude transcript | supported |
| Grok | `GrokIdleWire` | idle ACP sequence | Grok `updates.jsonl` tail | supported |
| Codex | `Unsupported` | none | none | unsupported |
| Cursor | `Unsupported` | none | none | unsupported |
| OpenCode | `Unsupported` | none | none | unsupported |
| Gemini | `Unsupported` | none | none | unsupported |
| Cline | `Unsupported` | none | none | unsupported |
| Hermes | `Unsupported` | none | none | unsupported |
| CodeBuddy | `Unsupported` | none | none | unsupported |
| Kimi Code | `Unsupported` | none | none | unsupported |
| Pi | `Unsupported` | none | none | unsupported |
| DeepSeek | `Unsupported` | none | none | unsupported |
| Custom ACP | `Unsupported` | none | none | unsupported |

Adding a provider later requires captured fixtures proving its trigger,
foreground/idle distinction, terminal boundary, replay identity, transcript or
other recovery authority, and safe overlay retirement. It must add a new
explicit policy variant or a reviewed mapping to an existing verified adapter.

## Normalized Autonomous Turn Model

`MessageTurn` gains optional origin metadata in Rust and TypeScript:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonomousTurnOrigin {
    BackgroundTask,
    Automation,
    AgentAutonomous,
}

pub struct MessageTurn {
    // existing fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous_origin: Option<AutonomousTurnOrigin>,
}
```

```ts
export type AutonomousTurnOrigin =
  | "background_task"
  | "automation"
  | "agent_autonomous"

export interface MessageTurn {
  // existing fields
  autonomous_origin?: AutonomousTurnOrigin | null
}
```

V1 assigns origins as follows:

- Claude task-notification episodes: `background_task`.
- Claude cron/loop records with an explicit transcript automation marker:
  `automation`.
- Other Claude episodes proven out-of-turn by the live prompt ledger:
  `agent_autonomous`.
- Grok episodes opened by the verified hidden background-task trigger:
  `background_task`.

An adapter leaves the field absent when it cannot prove the origin. It never
guesses `automation` or `agent_autonomous` from timing alone. Historical turns
that have no origin continue to serialize and render unchanged.

Origin is turn metadata, not a `ContentBlock`. This keeps the marker out of
copy/exported assistant text and prevents a model-authored string from spoofing
the marker.

## Claude Transcript Adapter

The existing Claude watcher remains structurally intact:

1. It tails the Claude JSONL transcript.
2. `PromptLedger` consumes fingerprints for Codeg-owned foreground prompts.
3. Records outside those prompt spans are grouped by the existing
   `ClaudeRecordAccumulator` and `group_into_turns` path.
4. Changed turns are emitted as whole-turn `BackgroundActivity` upserts.
5. The event watermark remains the committed Claude transcript byte offset.
6. Task launch/settlement accounting continues to drive
   `background_outstanding`, idle-sweep exemption, and existing OS
   notifications.

The generalization adds only:

- policy-based startup instead of a function named `spawn_if_claude` at the
  call site;
- `autonomous_origin` annotation when the watcher can prove it; and
- shared contract wording that no longer claims all out-of-turn activity is
  Claude-only.

The Claude detail parser must apply the same origin for transcript shapes it can
prove independently, especially `<task-notification>` continuations. A normal
overlay-to-detail handoff must not make the `后台续写` marker disappear. For an
episode classified only by the live prompt ledger and not reconstructible after
a cold parse, the parser leaves origin absent rather than inventing ownership;
this limitation does not apply to the background-task continuation in scope.

No Grok logic enters `background_watch.rs`.

## Grok Idle-Wire Adapter

### Recognized sequence

V1 recognizes only this verified sequence while no Codeg prompt is active:

```text
_x.ai/session/update task_backgrounded       (optional earlier accounting)
_x.ai/session/update task_completed          (task settlement)
session/update user_message_chunk
  _meta.hideFromScrollback == true           (hidden trigger)
session/update agent_thought_chunk | agent_message_chunk |
  tool_call | tool_call_update | supported private output
session/update | _x.ai/session/update turn_completed
```

`task_completed` alone does not create a visible turn. It updates the task
ledger and supplies identity for a later hidden trigger. The hidden trigger is
the episode-opening signal. Assistant/thinking/tool records become visible
only after that trigger and before the terminal boundary.

A visible `user_message_chunk` received while idle is not reclassified as an
autonomous turn in V1. It may have come from another client. Codeg logs a
rate-limited unsupported-shape diagnostic and relies on a later detail parse.

### Raw-dispatch observation

Observation occurs before `MatchDispatch` discards private extension variants.
The adapter receives the untyped method, params, and connection-loop ownership
under both the foreground and idle branches so it can see:

- `_x.ai/session/update` `task_backgrounded`;
- `_x.ai/session/update` `task_completed`;
- hidden metadata on `user_message_chunk`; and
- both standard and namespaced `turn_completed`.

Task launch/settlement records update accounting regardless of ownership.
Hidden-trigger and content classification runs only for idle ownership. The
existing typed/extension paths may still process usage and other unrelated
state. Content claimed by the autonomous adapter is not sent to the ordinary
foreground streaming reducer.

### Task ledger

The Grok adapter maintains a bounded task map keyed by `task_id`:

```text
task_backgrounded(task_id, tool_call_id) -> insert if absent
task_completed(task_id)                  -> remove, remember as recently settled
expiry                                   -> remove stale entry
```

The map drives the existing `BackgroundActivity.outstanding` count so the
connection is exempt from idle sweeps while its background process is alive.
Repeated launch or completion events are idempotent. Unknown completion ids are
accepted as recently settled but never make the count negative.

V1 emits no Grok `BackgroundSettledInfo.result` from `task_snapshot`. That
snapshot can contain large command output and would conflict with the current
tool-card/poll rendering contract. The adapter may emit an empty `settled` list
while still updating `outstanding` and opening the follow-up episode.

### Hidden-trigger classification

A hidden chunk opens an episode only when all of these hold:

- the policy is `GrokIdleWire`;
- no Codeg prompt is active;
- `sessionUpdate == "user_message_chunk"`;
- `_meta.hideFromScrollback == true`;
- the record matches the verified background-task reminder shape; and
- it references a recently settled task id, or it immediately follows a
  `task_completed` record in the same ordered idle stream.

Structured ids are preferred. Parsing the reminder body is limited to the
existing exact task-id tag/shape and is used only for classification. The raw
body is discarded after classification and never enters a `MessageTurn`.

If several task completions are coalesced into one reminder, they open one
episode. A second hidden reminder before terminal completion joins the current
episode and cannot change its already-published id.

### Stable episode and turn identity

Once the tailer locates the persisted hidden-trigger line, it derives a stable
episode identity from:

```text
external session id
referenced task id set, when present
hidden-trigger complete-line byte offset
```

The task id is the semantic anchor; the transcript offset distinguishes a
resumed/repeated notification for the same task. When no task id is available,
the session id plus trigger offset is collision-safe. Raw ids may be represented
by a deterministic digest in the public turn id.

The assistant id is conceptually:

```text
grok-autonomous:<episode-key>:assistant:0
```

The live tail assembler and the cold Grok parser use the same derivation. Every
incremental event replaces the same turn in place. Normal Grok turns retain
their current positional `grok-turn-<index>` ids.

### Persisted tail assembly

The tailer begins at the last complete `updates.jsonl` offset established before
the hidden trigger. It reads newline-delimited records and advances its
committed cursor only through complete lines. Non-UTF-8 or malformed complete
lines follow the normal parser's skip policy but remain counted as consumed
bytes; a partial final line remains buffered until completed.

The tailer uses the same Grok normalization as history for:

- assistant text and thinking;
- tool calls and cumulative tool updates;
- Grok `use_tool` MCP unwrapping;
- result/error normalization and output caps;
- model, usage, duration, and completion metadata; and
- supported private output such as context compaction.

After any newly persisted visible record, the adapter emits the entire current
assistant turn with:

- the stable id;
- `role: assistant`;
- `autonomous_origin: background_task`;
- the first visible record's timestamp;
- all blocks assembled so far; and
- `watermark` equal to the committed `updates.jsonl` byte offset.

The hidden trigger contributes no block and no user turn.

### Terminal behavior

An idle `turn_completed` closes the autonomous episode. It does not call the
foreground `finalize_turn_terminal_with_permissions`, emit a foreground
`TurnComplete`, settle a prompt generation, or set connection status through
`prompting`.

Instead it:

1. marks the episode terminal in the adapter;
2. drains persisted records through the matching terminal line;
3. emits the final whole-turn upsert with `completed_at` and the terminal byte
   watermark;
4. updates agent/background activity timestamps;
5. releases the autonomous-busy prompt gate;
6. schedules a conversation detail refetch; and
7. retains a small terminal tombstone so duplicate terminal frames are no-ops.

The refetch folds the overlay into parser history. A non-`end_turn` reason may
be recorded in diagnostics, but V1 does not synthesize foreground interruption
metadata because this was not a user-owned prompt.

## Event and Data Contract Changes

`AcpEvent::BackgroundActivity` remains the sole frontend event. Its semantics
become provider-neutral:

```rust
BackgroundActivity {
    session_id: String,
    turns: Vec<MessageTurn>,
    outstanding: u32,
    settled: Vec<BackgroundSettledInfo>,
    watermark: u64,
}
```

The wire shape does not need a provider field. A conversation has one agent and
one authoritative parser, and the policy adapter guarantees that the watermark
and `ConversationDetail.transcript_watermark` refer to that agent's same
transcript source.

For Claude, `watermark` keeps its current meaning. For Grok, it is the complete
byte count consumed from `updates.jsonl`. ACP event sequence numbers, Grok
`eventId`, `promptIndex`, task ids, and episode counters are never written into
this field.

An accounting-only event (`turns` is empty) carries the adapter's latest
confirmed transcript offset. That offset is not attached to an overlay entry
and therefore cannot retire content. The Grok adapter establishes an initial
complete-line offset on session readiness; if the transcript is temporarily
unavailable, it delays the accounting event or uses a separate state update
instead of fabricating a content watermark.

`ConversationDetail.transcript_watermark` keeps its optional type but its
documentation changes from “Claude only” to “available for transcript-backed
autonomous overlay providers.” Grok begins returning `Some(consumed_bytes)`.

No database entity changes. `MessageTurn` remains parser/event data serialized
over the existing Tauri or HTTP transport.

## Grok Watermark and Overlay Retirement

This handoff is the correctness boundary.

The current Grok parser returns `transcript_watermark: None`. It will instead
return the exact committed byte count produced by the same complete-line scanner
used by the live tailer.

The handoff is:

```text
wire hidden trigger identifies an idle episode
  -> updates.jsonl tailer finds the persisted trigger
  -> tailer parses persisted assistant records through byte W
  -> BackgroundActivity(turn id=A, watermark=W)
  -> frontend upserts overlay A@W
  -> detail parser later consumes updates.jsonl through byte D
  -> retire A only when D >= W
```

The tailer must not emit `A@W` before it has actually consumed the records used
to build A through W. A file `metadata.len()`, mtime, outer event timestamp, or
wire sequence does not prove that.

The live tailer and full parser also derive the same autonomous turn id. During
a refetch race, timeline assembly shows at most one copy of a matching id,
preferring the newer overlay until watermark retirement. Id equality is only a
display dedupe; it is not proof that history has consumed the bytes. Retirement
still requires `D >= W`.

If the transcript cannot be found or lags the wire:

- the observer retains a bounded pending episode and retries;
- it does not publish an unwatermarked turn;
- terminal observation triggers a normal detail refetch as a recovery attempt;
- a later transcript append or reconnect can recover the episode; and
- the failure is diagnosed without logging content.

This may delay live display under filesystem failure, but it cannot cause the
overlay to disappear before history contains it.

## Detailed Data Flow

### Claude

```text
Claude transcript append
  -> existing watcher tails complete records
  -> PromptLedger classifies foreground vs out-of-turn
  -> Claude accumulator groups changed autonomous turn
  -> annotate autonomous_origin
  -> BackgroundActivity(turns, accounting, Claude byte watermark)
  -> frontend overlay whole-turn upsert
  -> detail refetch parses same Claude file
  -> transcript watermark retires covered overlay
```

### Grok

```text
Grok task_backgrounded while foreground turn is active
  -> Grok task ledger records task_id
  -> BackgroundActivity(outstanding=N, turns=[])
  -> SessionState idle-sweep exemption

Grok task_completed while connection is idle
  -> remove task_id from outstanding ledger
  -> cache recently settled task identity

Grok hidden user_message_chunk while idle
  -> classify exact background reminder
  -> open autonomous episode
  -> do not emit user content
  -> arm immediate updates.jsonl tail

Grok assistant/thinking/tool update while episode is open
  -> observer advances expected episode sequence
  -> tailer consumes persisted equivalent records
  -> shared Grok accumulator rebuilds current assistant turn
  -> BackgroundActivity(same turn id, whole-turn upsert, byte watermark)
  -> frontend overlay replaces the previous version in place

Grok turn_completed while episode is open
  -> tail through persisted terminal record
  -> emit final upsert
  -> close/tombstone episode
  -> release queued Codeg prompt
  -> refetch detail
  -> Grok parser returns same turn + covering transcript watermark
  -> overlay retires without flicker or duplication
```

## State Machines

### Grok task accounting

```text
Unknown
  -- task_backgrounded(task_id) --> Running

Running
  -- duplicate task_backgrounded --> Running
  -- task_completed -------------> SettledRecently
  -- max-age expiry -------------> Expired

Unknown
  -- task_completed -------------> SettledRecently

SettledRecently
  -- matching hidden trigger ----> consumed identity / episode opens
  -- duplicate task_completed ---> SettledRecently
  -- TTL expiry -----------------> removed
```

### Grok autonomous episode

```text
Dormant
  -- verified hidden trigger while idle --> Opening

Opening
  -- persisted trigger located ----------> Open
  -- foreground turn wins race ----------> SuppressedForeground
  -- stale timeout ----------------------> Abandoned

Open
  -- persisted visible update -----------> Open + whole-turn upsert
  -- duplicate/replay -------------------> Open, no duplicate block
  -- another hidden trigger -------------> Open, same episode
  -- turn_completed ---------------------> AwaitingPersistedTerminal
  -- stale timeout ----------------------> Abandoned

AwaitingPersistedTerminal
  -- persisted terminal consumed --------> Closed + final upsert/refetch
  -- duplicate terminal -----------------> unchanged
  -- stale timeout ----------------------> ClosedDegraded + refetch

Closed
  -- duplicate/replayed episode events --> ignored by tombstone
  -- tombstone TTL ----------------------> Dormant storage reclaimed
```

`SuppressedForeground` means a user prompt entered the connection loop before
the hidden trigger was classified. The active prompt path owns those updates;
the adapter does not create an overlay copy.

### Prompt concurrency

The connection loop is the serialization authority. Once an idle hidden trigger
opens an episode, the normal prompt receive branch is gated while
`autonomous_busy == true`; control-lane cancellation/disconnect and necessary
permission handling remain available. A queued prompt stays in the existing
bounded channel and is read only after the autonomous terminal or stale close.

This prevents two requests from sharing one Grok session without pretending the
autonomous episode is a Codeg prompt. It does not set `turn_in_flight`, allocate
a prompt generation, create an optimistic user turn, or emit
`StatusChanged(prompting)`.

If the prompt command wins the single-threaded select before the hidden trigger,
the normal prompt path owns the turn. Subsequent hidden task updates are treated
as in-prompt provider activity and are not duplicated into the autonomous
overlay.

## Ordering, Deduplication, and Races

### Duplicate wire notifications

Use provider `eventId` when present. For standard records without an event id,
the adapter uses the ordered transcript occurrence after the episode's trigger
offset. A bounded LRU remembers processed identities. Replaying the same hidden
trigger, content record, tool update, or terminal cannot append twice.

### Incremental tool updates

`tool_call_id` is the identity for a tool block. A later cumulative
`tool_call_update` patches the existing result/status in the episode
accumulator. The adapter re-emits the whole `MessageTurn`; the frontend replaces
the overlay entry by turn id.

### Transcript before wire

The tailer does not classify arbitrary transcript records on timing alone. It
waits for the verified idle trigger, then can consume records already appended
after its baseline. This handles file writes that beat ACP delivery.

### Wire before transcript

The episode remains pending and the active retry consumes the file when it
catches up. No unsafe watermark or partial overlay is emitted.

### Detail refetch during an open episode

The parser may return a partial version of the autonomous turn. Because the
parser and overlay use the same autonomous id, timeline assembly renders one
copy and prefers the newer overlay. Retirement still uses the transcript
watermark, so a detail parse that covered only an earlier version cannot discard
a later overlay upsert.

### Foreground transition race

Foreground ownership is decided in the connection loop, not by wall-clock
timestamps. Whichever event is processed first establishes ownership. The
prompt ledger/generation and autonomous episode cannot both own the same record.

### Multiple task completions

Recently settled task ids are a set, not a single “last task.” A reminder that
names several ids creates one episode keyed by that set and trigger offset. A
reminder without an exact id may use ordered adjacency only inside the same
already-verified Grok idle stream; it never searches an arbitrary old task.

### Transcript truncation or replacement

If `updates.jsonl` shrinks below the committed cursor or its file identity
changes, the tailer discards partial state, runs a detail refetch, and re-arms
from the new parser watermark. It does not compare offsets across two file
generations.

## Disconnect, Reattach, and Cold-Load Recovery

The authoritative recovery source is Grok's transcript, not the in-memory wire
observer.

The Grok full parse tracks enough non-rendered control state to identify:

- `task_backgrounded` minus `task_completed` for live-attach accounting;
- hidden background-task triggers;
- the assistant span following such a trigger; and
- the terminal boundary when present.

On a cold detail load, the parser:

1. skips the hidden user record;
2. marks the following independently-opened assistant turn as
   `background_task`;
3. assigns the canonical autonomous id derived from the trigger record;
4. returns partial content even if no terminal exists yet; and
5. returns the complete-byte Grok transcript watermark.

If a hidden reminder arrives while a foreground assistant accumulator is still
open, the cold parser keeps the existing behavior: it suppresses the reminder
without cutting or relabeling that foreground assistant. Only a hidden trigger
seen at an idle turn boundary, paired with the verified task-completion context,
opens a cold autonomous turn.

On reattach to a still-running connection, the adapter initializes from the
parser's watermark and recovered task/episode index. If an autonomous episode
is already open, subsequent upserts reuse its canonical id. It does not replay
the whole session into the overlay.

An explicit disconnect retains current semantics: the CLI may terminate and
background work may die. A window detach/reload that leaves the backend
connection alive does not discard task accounting. If the process itself
restarts, persisted completed or partial content remains visible through the
normal parser even though no live task can be kept alive.

## Error Handling and Bounds

The adapter fails closed for classification and fails recoverably for display.

These conditions create no autonomous overlay until a transcript parse can
prove content:

- missing or unreadable Grok session directory;
- missing `updates.jsonl`;
- hidden chunk without the verified background-task shape;
- hidden chunk while a foreground prompt owns the session;
- unknown provider method or update variant;
- malformed structured task identity;
- transcript/wire order that cannot be reconciled safely; or
- transcript replacement while an episode is open.

The normal connection remains usable. A terminal observation schedules a detail
refetch, and a later reconnect/cold load can recover persisted content.

Bounds for V1:

- at most one open Grok autonomous episode per connection;
- at most 16 awaiting-persistence/tombstoned episodes;
- at most 64 running or recently settled task ids;
- at most 1,024 remembered provider record identities;
- rotate at 512 accumulated episode records and force-rotate at 1,024, matching
  the existing Claude watcher's safety model;
- use the existing per-block text/tool-output truncation limits;
- cap total retained episode payload to 2 MiB after normalization;
- retry immediately on a wire update and at one-second cadence while active;
- expire inactive task/episode state using
  `background_keepalive_max_age()` (default 3,600 seconds, existing environment
  override); and
- retain terminal tombstones for ten minutes or until their bounded LRU evicts
  them.

Force rotation may split a pathological autonomous response into two marked
assistant messages. It must preserve order and origin and must not merge either
piece with a foreground reply.

Warnings are rate-limited. Diagnostics include provider, connection id, opaque
session/episode id, state, failure class, offsets, record counts, and elapsed
time. They never include reminder text, prompt text, task command, tool input,
tool output, or assistant content.

## UI and UX

### Message presentation

Only an assistant `MessageTurn` with `autonomous_origin` renders the marker.
The marker is a compact icon-plus-label above the assistant content, aligned
with the existing message column. V1 labels all three origins as
`后台续写`; future copy may distinguish scheduled automation after product
review.

It is metadata chrome, not a card and not part of the message bubble. It is
excluded from copy text, artifacts, token counts, and model output.

### Independent grouping boundary

`mergeConsecutiveAssistantTurns` currently merges adjacent assistant turns.
Autonomous origin becomes a hard grouping boundary:

- a foreground assistant cannot merge into an autonomous assistant;
- an autonomous assistant cannot merge into the next foreground assistant;
- autonomous turns from distinct episode ids do not merge; and
- force-rotated pieces may render consecutively but retain their own marked
  boundaries.

`ResolvedMessageGroup` carries the optional origin so memoization and rendering
invalidate when metadata is added by an overlay or parser refresh. The canonical
turn id remains the virtualized row key input, preventing layout churn across
whole-turn upserts.

### Localization

Add one message-list key to all ten locale files. The Simplified Chinese value
is `后台续写`; the English value is `Background continuation`; other locales
receive reviewed literal translations consistent with existing message-list
terminology.

No raw reminder, task id, provider method, or internal origin enum is shown.

### Notifications and activity chips

Claude settlement notifications and background-task cards stay unchanged.
Grok V1 updates the existing outstanding count for keepalive/accounting but does
not create a new task-result card from `task_snapshot`. The visible assistant
continuation is the primary user-facing completion signal. Adding Grok OS
settlement notifications is a separate product decision.

## Compatibility and Migration

- `autonomous_origin` is optional and omitted for all existing/unclassified
  turns.
- No SQLite migration or stored-row rewrite is required.
- Normal Grok positional ids remain unchanged; only newly recognized autonomous
  turns use canonical episode ids.
- The Tauri and server transports serialize the same added optional field.
- Claude's event type and frontend overlay action remain compatible.
- Grok starts supplying `ConversationDetail.transcript_watermark`; consumers
  already accept the field as optional.
- Older session transcripts with no recognizable hidden trigger render exactly
  as before.
- Unsupported agents continue to have idle streaming dropped by the frontend
  guard. This is an explicit limitation, not an implicit promise of support.

## Observability

Add structured, low-cardinality diagnostics and counters where the existing
telemetry surface permits:

```text
autonomous_policy_selected{provider,policy}
autonomous_task_accounting{provider,result=opened|settled|expired|duplicate}
autonomous_episode_started{provider,origin}
autonomous_episode_upserted{provider}
autonomous_episode_completed{provider,reason}
autonomous_episode_recovered{provider,source=cold_parse|reattach}
autonomous_transcript_wait{provider,result=caught_up|timeout|reset}
autonomous_overlay_refetch{provider,result=scheduled|failed}
autonomous_unsupported_shape{provider,kind}
```

Useful numeric fields are committed byte offset, pending-record count, episode
record count, retry count, and elapsed milliseconds. Metric labels never contain
session ids, connection ids, task ids, paths, prompts, commands, tool names, or
content.

The existing sampled frontend out-of-turn drop log should change its wording
from “the transcript overlay renders them” to “provider policy owns
out-of-turn rendering.” This avoids claiming unsupported providers are covered.

## Security and Privacy

- Hidden reminder text is used only for exact local classification and is
  discarded before event construction.
- The reminder is not rendered, copied, logged, persisted by Codeg, or sent in
  metrics.
- Grok transcripts are read locally with the same user permissions as existing
  conversation parsing.
- No transcript file is modified, copied, checkpointed, or migrated.
- Task snapshot command/output is not promoted into `BackgroundSettledInfo` in
  V1.
- Existing block truncation and event-size limits apply to autonomous content.
- Origin metadata is backend-derived; model text cannot set or spoof it.
- Session and task ids are used only inside the owning connection/session
  scope. They never authorize cross-session correlation.
- A malformed or unexpected provider sequence reduces live availability but
  never causes content to be attached to an unrelated turn.

## Testing Matrix

### Policy tests

- Claude maps to `ClaudeTranscript`.
- Grok maps to `GrokIdleWire`.
- Every other built-in and a custom agent map to `Unsupported`.
- No unsupported agent starts a watcher or observer.

### Grok parser/scanner tests

- returns the exact complete-byte `updates.jsonl` watermark;
- does not count a trailing partial line until it is completed;
- safely skips malformed/non-UTF-8 complete records while advancing according
  to the documented parser policy;
- recognizes `task_backgrounded` and `task_completed` identities;
- suppresses hidden reminder content;
- a hidden trigger at an idle boundary marks the following assistant turn
  `background_task`;
- a hidden reminder inside an already-open foreground assistant does not split
  or relabel it;
- autonomous id derivation is stable across repeated parses;
- the same task id notified twice receives distinct ids by trigger offset;
- no-task-id fallback is collision-safe inside a session;
- an incomplete autonomous turn is returned on cold load; and
- normal Grok turn ids/content snapshots remain unchanged.

### Grok observer/state-machine tests

- `task_backgrounded` increments outstanding once;
- duplicate launches and completions are idempotent;
- an unknown completion never underflows outstanding;
- `task_completed` without a hidden trigger creates no message;
- the exact hidden trigger opens an idle episode;
- the hidden trigger itself produces no user turn or content block;
- visible or malformed hidden user chunks do not open an episode;
- assistant text/thinking/tool updates upsert one stable turn;
- cumulative tool updates replace the matching result rather than append a
  duplicate;
- standard and namespaced `turn_completed` close the episode;
- duplicate terminals hit the tombstone and do nothing;
- foreground ownership suppresses the autonomous copy;
- a queued Codeg prompt is not sent until autonomous close;
- control-lane disconnect still wins while autonomous work is open;
- task/episode bounds evict or rotate deterministically; and
- stale state releases prompt gating and keepalive.

### Watermark/race tests

- wire-before-file waits and then emits the persisted turn;
- file-before-wire consumes already-appended records after the baseline;
- a detail watermark behind an overlay keeps the overlay;
- an equal or greater Grok watermark retires it;
- an unrelated ACP sequence value can never retire an overlay;
- detail and overlay versions with the same autonomous id render once, with the
  newer overlay version winning until retirement;
- a refetch between two incremental upserts cannot erase the later content;
- transcript truncation resets the cursor and schedules recovery; and
- a missed wire terminal is recovered by cold detail parse/reconnect.

### Claude regression tests

- current prompt-ledger foreground exclusion remains green;
- task outstanding/settled accounting remains green;
- held-turn wire-visible suppression remains green;
- episode rotation and watermark tests remain green;
- a task-notification continuation carries `background_task` through overlay
  and detail parse; and
- existing OS notifications and card settlement do not duplicate.

### Frontend tests

- out-of-turn streaming still does not mutate `liveMessage`;
- `background_activity` upserts by stable turn id;
- Grok overlay retirement uses its detail watermark;
- an autonomous assistant renders `后台续写` in Chinese and the localized
  equivalent in another locale;
- the hidden reminder text is absent from DOM and copy output;
- origin-bearing assistants are hard boundaries in
  `mergeConsecutiveAssistantTurns`;
- foreground assistants retain existing merging behavior;
- origin-only metadata changes invalidate cached groups/rows;
- detail/overlay same-id display dedupe renders one message; and
- historical turns without origin have no marker.

### Integration fixtures

Add a captured, redacted Grok sequence matching session 3806:

```text
task_completed
hidden user_message_chunk
agent thought/message/tool updates
turn_completed
```

Drive it through the idle connection branch and a temporary `updates.jsonl`.
Assert that no foreground `TurnComplete` is emitted, one marked assistant turn
is incrementally upserted, the final refetch is requested, and the parser
watermark retires the overlay.

Run the narrow Rust and Vitest targets first, then the repository-prescribed
desktop/server checks and frontend lint/build appropriate to the final changed
surface.

## Rollout

1. Land the normalized origin type, policy selector, and parser watermark tests
   without changing runtime behavior.
2. Move Claude startup behind `ClaudeTranscript` and run the existing Claude
   regression suite.
3. Land Grok cold-parse origin recovery and watermark support.
4. Enable the Grok idle observer/tailer for the exact verified sequence.
5. Land frontend marker, grouping boundary, same-id display dedupe, and
   localized strings.
6. Exercise the redacted session-3806 integration fixture and a real local
   background task on Windows.
7. Watch failure-class counters and sampled logs for unsupported Grok shapes
   before considering more origins or providers.

There is no broad “all ACP agents” switch. The capability policy is the rollout
gate. A future provider is added only after its fixtures and retirement authority
are known.

## Acceptance Criteria

- In the session-3806 sequence, Grok's follow-up appears without a new prompt or
  manual reload.
- It appears as one independent assistant message with the localized
  `后台续写` marker.
- No raw `<system-reminder>` or hidden user bubble appears anywhere in the UI or
  copy output.
- Incremental Grok updates replace one stable turn instead of creating multiple
  messages.
- Idle `turn_completed` closes the autonomous episode and triggers parser
  reconciliation without emitting a foreground turn completion.
- A prompt cannot be sent concurrently on the same Grok session while the
  autonomous episode is open.
- Overlay content remains until the Grok detail parser has consumed the same
  `updates.jsonl` bytes.
- Reload/reconnect/cold load recovers the completed continuation and its marker
  from Grok's transcript.
- A missed, duplicate, replayed, reordered, partial, or malformed event cannot
  attach content to the preceding foreground turn.
- Grok background task accounting exempts genuinely running work from idle
  sweeps and expires stale work within the configured bound.
- Claude's existing background tasks, cron/loop turns, settlement cards,
  notifications, and watermark handoff do not regress.
- Codex, Cursor, and every other ACP agent remain explicitly unsupported rather
  than being handled by unverified heuristics.
- No Codeg database migration is introduced.

## File-Level Impact

Expected implementation surface:

- `src-tauri/src/acp/autonomous_activity.rs` (new): policy and common adapter
  lifecycle contract.
- `src-tauri/src/acp/grok_autonomous.rs` (new): Grok observer, task ledger,
  episode state, transcript-tail reconciliation, and tests.
- `src-tauri/src/acp/mod.rs`: register the new modules.
- `src-tauri/src/acp/background_watch.rs`: expose the Claude adapter through
  the normalized lifecycle and annotate proven origins.
- `src-tauri/src/acp/connection.rs`: choose policy, feed raw dispatches with
  foreground/idle ownership, gate prompts during a Grok autonomous episode,
  and handle idle terminal closure.
- `src-tauri/src/acp/types.rs`: provider-neutral `BackgroundActivity`
  documentation; event shape otherwise unchanged.
- `src-tauri/src/acp/session_state.rs`: provider-neutral accounting comments
  and autonomous activity assertions.
- `src-tauri/src/models/message.rs`: `AutonomousTurnOrigin` and optional
  `MessageTurn.autonomous_origin`; update Rust struct literals mechanically.
- `src-tauri/src/parsers/grok.rs`: shared complete-line scanner, transcript
  watermark, canonical autonomous ids, hidden-trigger cold recovery, and tests.
- `src-tauri/src/parsers/claude.rs`: cold-parse origin annotation for proven
  task-notification/automation shapes.
- `src/lib/types.ts`: TypeScript origin type and updated watermark docs.
- `src/contexts/acp-connections-context.tsx`: provider-neutral event wording,
  refetch scheduling, and unchanged foreground guard.
- `src/stores/conversation-runtime-store.ts`: same-id display dedupe and origin
  preservation through overlay/detail reconciliation.
- `src/stores/background-overlay.test.ts` and related window/timeline tests:
  Grok watermark, race, and dedupe coverage.
- `src/components/message/message-list-view.tsx`: grouping boundary, group
  metadata, and marker rendering.
- `src/components/message/message-list-view.test.tsx`: presentation and merge
  tests.
- `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`: localized
  marker.

Exact test files may be split to keep focused modules readable. No unrelated
frontend redesign or ACP connection refactor is part of this change.

## Rejected Alternatives

### Remove the frontend `status !== "prompting"` guard

Rejected. Idle deltas would mutate or visually merge with the last foreground
`liveMessage`, recreating the original garbled/incomplete background-result bug.

### Temporarily set the connection to `prompting`

Rejected. The user did not initiate a prompt. Faking foreground state would
corrupt prompt generations, cancellation ownership, optimistic turns,
permissions, watchdog behavior, and concurrent-send semantics.

### Add Grok directly to the Claude transcript watcher

Rejected. Claude needs permanent polling because some autonomous turns produce
no wire events. Grok has a verified idle wire trigger and a different transcript
format. Combining provider heuristics in one watcher would make both harder to
reason about and test.

### Buffer all idle ACP deltas only in the frontend

Rejected. The frontend lacks the provider's terminal semantics, stable replay
identity, transcript byte watermark, reconnect recovery, and safe distinction
between a hidden trigger and an external visible user message.

### Emit wire content immediately with a synthetic watermark

Rejected. An ACP sequence, timestamp, event id, or observed file length does not
prove the history parser contains the emitted blocks. The overlay could retire
early and make content disappear.

### Use turn ids alone to retire the overlay

Rejected. Stable ids prevent duplicate display but do not prove the parser
consumed the latest incremental version. Byte-watermark coverage remains the
retirement authority.

### Poll every provider transcript forever

Rejected. Transcript formats and autonomous semantics are provider-specific,
and several agents do not expose a suitable local authority. V1 keeps Claude's
required permanent poll and arms Grok tailing only around a verified idle
episode.

### Support every ACP agent with matching event names

Rejected. Private method names, hidden-message meaning, terminal ownership, and
history persistence differ across hosts. Superficial JSON similarity is not a
safe capability contract.

### Render the hidden reminder as the user turn

Rejected. Grok explicitly marks it out of scrollback; exposing internal system
instructions is confusing, leaks implementation detail, and splits the response
into a false user/assistant exchange.

### Persist synthetic autonomous turns in Codeg's database

Rejected for V1. Claude and Grok already have authoritative local transcripts,
and the overlay/watermark handoff provides live continuity without a second
durable message store or schema migration.
