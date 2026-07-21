# Grok Retry Stream Reconciliation Design

**Date:** 2026-07-21

**Status:** Approved

**Scope:** Grok ACP updates during an active prompt, including normal chat,
automatic titles, document translation, delegation result assembly, and chat
channel buffering. Other agents and historical transcript parsing are unchanged.

## Problem

Grok can retry a model call after a provider failure. The private
`_x.ai/session/update` stream reports `retry_state`, while standard
`session/update` messages may still deliver output from the failed call. The two
channels are not ordered by file/arrival position, but their `_meta.eventId`
values retain causal order.

The automatic-title failure for conversation 800 had this wire sequence:

```text
agent_thought_chunk  event ...-21  stream A
retry_state          event ...-32  type=retrying, attempt=1
agent_message_chunk  event ...-31  stream A (failed candidate, delivered late)
agent_thought_chunk  event ...-51  stream B
agent_message_chunk  event ...-61  stream B (accepted candidate)
turn_completed       event ...-63
```

Grok's finalized `chat_history.jsonl` contains only stream B. Codeg currently
discards the outer `SessionNotification._meta` and concatenates both standard
message chunks into generic `ContentDelta` events. The title normalizer then
sees two adjacent candidates as one title.

Local evidence establishes this as a protocol-reconciliation issue rather than
a title-similarity issue:

- 10 of 10 retrying hidden title runs contained one failed candidate followed
  by one accepted candidate.
- Only 3 candidate pairs were exact duplicates; 7 differed.
- All 10 finalized Grok histories contained only the later candidate.
- Across 53 Grok sessions, 14 retry markers causally followed an
  `agent_message_chunk`; all 14 retry markers were delivered before that older
  message chunk, including four ordinary non-title sessions.

## Alternatives

### 1. Fuzzy title collapse

Rejected. Edit distance cannot identify which candidate is authoritative, can
drop intentional repetition, and does nothing for normal chat or translation.

### 2. Hidden-run-only last-chunk selection

Rejected as the primary fix. It would repair titles and translations but leave
the same invalid failed-attempt output in normal ACP consumers.

### 3. Reconcile Grok attempts before generic ACP events

Selected. Treat retry metadata as control-plane information, roll back the
current speculative model-call tail, and prevent causally older failed-stream
updates from becoming `AcpEvent`s.

## Invariants

1. A `retry_state` is never user content and never counts as agent output.
2. Only `retry_state.type == "retrying"` starts a replacement attempt.
   `failed`, `exhausted`, unknown, malformed, idle, and load-replay notices do
   not trigger rollback.
3. A retry rolls back only the current model-call tail. Content and tools from
   earlier accepted model calls in the same user turn remain intact.
4. An update is dropped only when wire metadata proves it belongs to a failed
   attempt. Ambiguous metadata fails open and emits a diagnostic; Codeg never
   guesses from text similarity.
5. All active-prompt dispatch paths, including the pre-finalization drain, use
   the same reconciler instance.
6. Non-Grok agents never enter this path.

## Architecture

### Grok retry reconciler

Add a small pure state machine beside the existing xAI private notification
adapter. One instance lives for one active prompt and is reset on terminal,
cancel, disconnect, or the next prompt.

It tracks a bounded set of failed-attempt windows (Grok currently advertises at
most 15 retries):

```text
prompt_id
stream_start_ms
retry_event_sequence
retry_agent_timestamp_ms
attempt
```

Standard Grok notifications are observed before `SessionNotification.meta` is
discarded. `promptId` and `streamStartMs` identify the current model call;
`eventId` supplies the causal numeric sequence.

When an in-prompt private `retry_state(type=retrying)` arrives, the reconciler:

1. records the current stream as failed;
2. records the retry event sequence/timestamp as a stale-event ceiling;
3. emits one `TurnAttemptRollback { attempt }` when speculative output exists;
4. leaves accepted tool-call state and earlier model calls untouched.

For later standard updates, the reconciler drops only rollbackable output
(`AgentMessageChunk`, `AgentThoughtChunk`, `Plan`, or an initial `ToolCall`)
whose event sequence is causally older than the retry marker and which passes
one of two mutually exclusive proof paths:

- Primary: its `promptId` and `streamStartMs` match the recorded failed stream.
- Pre-stream fallback: no failed `streamStartMs` was observable, its `promptId`
  matches the active prompt, and its `agentTimestampMs` is older than or equal
  to the retry marker timestamp.

Missing or unparsable metadata that satisfies neither complete proof path is
passed through with a bounded warning rather than risking loss of valid output.

`ToolCallUpdate`, permission, usage, user echo, and terminal updates are never
dropped by retry reconciliation.

### Rollback event

Add an agent-neutral wire event:

```rust
AcpEvent::TurnAttemptRollback { attempt: u32 }
```

The event is a stream-ordering barrier. It is not rendered and carries no
provider error text. Existing consumers apply the same rollback rule:

- Find the final accepted tool-call boundary in the current live turn.
- Retain that tool call and everything before it.
- Remove trailing text, thinking, and plan blocks after that boundary.
- If no tool-call boundary exists, clear the speculative live content.
- Preserve active/completed tool state, permissions, usage, delegation state,
  and prior finalized turns.

An initial `ToolCall` that passes stale-event filtering commits its model-call
prefix and becomes the next rollback boundary. `TurnComplete` commits the
remaining tail through the existing completion path.

The event must be flush-sensitive in desktop batching and a merge barrier in
the frontend event ingestor so deltas on opposite sides cannot be coalesced.

### Consumer behavior

- `SessionState` truncates its `live_message` speculative tail and recomputes
  whether the turn still has accepted agent output.
- The frontend context reducer and incremental live-transcript projector apply
  the identical truncation rule.
- Automatic-title and document-translation collectors clear their private text
  buffer on rollback. Their hidden profiles prohibit tools, so their accepted
  checkpoint is empty.
- Chat-channel sessions remember the content-buffer length at each accepted
  tool-call boundary and truncate to that checkpoint on rollback.
- Delegation result assembly inherits the corrected `SessionState` final text.

The existing exact-AA title collapse remains as defense in depth. No fuzzy
near-duplicate logic is added.

## Data Flow

For the conversation-800 sequence:

```text
stream A thought    -> speculative state
retry marker        -> rollback speculative state; mark stream A failed
late stream A title -> dropped before generic AcpEvent conversion
stream B thought    -> new speculative state
stream B title      -> one ContentDelta
turn complete       -> commit stream B
```

The title collector therefore receives only the accepted title followed by
`TurnComplete`; the normalizer handles formatting and length, not retry
recovery.

## Error Handling And Observability

- Log retry observation at debug level with connection/session, attempt, and
  parsed causal sequence; do not log generated text.
- Log malformed metadata at warning level with rate/burst bounding per turn.
- Count dropped stale updates by update kind in tracing fields.
- A rollback with no speculative content is an idempotent no-op.
- Repeated retry markers extend the failed-window set without emitting duplicate
  rollback events when no new speculative output has appeared.
- Terminal cleanup always clears reconciler state so late updates cannot affect
  the next turn.

## Compatibility And Scope

- No database migration is required.
- Persisted Grok history is already correct and remains parser-authoritative.
- Existing malformed titles are not rewritten automatically.
- Other agents keep their current standard ACP behavior.
- The private xAI compact mapper remains unchanged; retry reconciliation runs
  before its existing fallback handling.
- Fixing Grok's upstream event ordering remains desirable, but Codeg must not
  depend on it because current released clients already produce these traces.

## Verification

Regression coverage must include:

1. A raw fixture matching conversation 800 emits only the accepted title.
2. An exact-duplicate retry also emits one copy without relying on title
   normalization.
3. Retry after already-emitted speculative text rolls back that text.
4. Retry-before-late-output drops the delayed failed message.
5. A stale initial tool call is dropped, while tool-call updates are retained.
6. A retry after an accepted tool call preserves that tool and earlier content
   while removing only the trailing model-call tail.
7. Multiple retries are bounded, idempotent, and accept the final stream.
8. Missing/malformed xAI metadata fails open without panic.
9. Pre-finalization drain and the main in-prompt loop produce identical results.
10. Auto-title, document-translation, chat-channel, backend snapshot, context
    reducer, and incremental projector rollback semantics agree.
11. Non-Grok standard streams and xAI compact notifications are unchanged.
12. `turn_had_agent_output` is false after rolling back the only output, but
    remains true when an earlier accepted tool/model stage exists.

Focused Rust and frontend tests run first, followed by the repository-required
Rust checks and `pnpm test`/`pnpm eslint .` in proportion to touched surfaces.
