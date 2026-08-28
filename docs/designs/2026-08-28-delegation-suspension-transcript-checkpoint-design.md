# Delegation Suspension Transcript Checkpoint Design

## Status

Revised draft for review on 2026-08-28. This document defines the repair only;
no implementation has started.

## Executive Decision

Codeg will treat an accepted frontend transition from `prompting` to
`connected` that still owns a `liveMessage` as a transcript checkpoint when no
accepted `turn_complete` ended that live turn.

The checkpoint will publish the stage through the existing canonical mirror,
honor the mirror's owner filter and stale-publication rejection, then reuse
`completeLiveTranscriptTurn()` to promote the exact accepted message into
`localTurns`. A later hidden continuation may then create a new `liveMessage`
without overwriting the prior stage.

No new backend event will be added. A successful suspension already emits the
ordered signal needed by continuously attached desktop and Web clients and by
trusted replay:

```text
prompting
  -> assistant/thinking/tool events
  -> StatusChanged(Connected)       # suspension checkpoint
  -> ContinuationWaitingChanged
  -> StatusChanged(Prompting)       # hidden continuation starts
```

`TurnComplete` remains reserved for a real terminal turn. The checkpoint has a
separate admission and promotion path and does not acquire terminal semantics.

## Problem

Delegation continuation intentionally suspends a Codex provider turn without
emitting `TurnComplete`:

1. The provider writes `turn_aborted` for the suspended stage.
2. `finalize_turn_terminal()` classifies it as `DelegationSuspended`.
3. The backend clears the active generation and emits
   `StatusChanged(Connected)`.
4. The frontend keeps the stage only in the temporary `liveMessage` because
   promotion currently runs only for `turn_complete`.
5. The hidden continuation emits `StatusChanged(Prompting)`, whose reducer
   creates a fresh `liveMessage` and overwrites the prior stage.

This explains the observed shape: the last normally completed reply remains,
every suspended middle stage disappears when the next stage starts, and the
latest stage remains visible until it is replaced.

The provider rollout still contains the missing content. This is a live
transcript lifecycle bug, not source transcript data loss.

## Goals

- Preserve every visible continuation stage before the next hidden prompt.
- Keep the hidden continuation prompt out of the rendered transcript.
- Reuse the existing canonical mirror and live-to-local Markdown handoff.
- Work when `connected` and the next `prompting` arrive in one frontend frame.
- Work for desktop delivery, Web live delivery, and trusted Web replay.
- Preserve owner/viewer, conversation alias, session, and canonical rejection
  fences.
- Keep ordinary `TurnComplete` behavior unchanged and exactly once.
- Add a Rust test for the backend event contract without changing behavior.

## Non-Goals

- Emitting a synthetic or weakened `TurnComplete` for suspension.
- Changing delegation continuation persistence, wake-up, or child ownership.
- Adding a database table, migration, overlay type, or transcript format.
- Rendering the server-authored hidden continuation prompt.
- Recovering a stage from a Web snapshot that contains
  `status=connected, live_message=null`. That snapshot has no stage payload to
  promote; recovery requires normal cold transcript parsing or a separate
  backend snapshot/history contract.
- Automatically reconstructing stages already overwritten before this fix was
  installed.
- Changing the existing cold-history window.

## Existing Contract

The repair relies on these existing invariants:

1. A live provider turn is represented by `status === "prompting"` and a
   non-null `liveMessage`.
2. A normal turn applies `turn_complete` before the backend's trailing
   `StatusChanged(Connected)`. The `turn_complete` mapping itself pushes
   `STATUS_CHANGED(connected)` and `prepareEventFrame()` immediately closes
   that step. The trailing backend event therefore sees an already connected
   snapshot and cannot satisfy the checkpoint edge.
3. Among connections retained for continued use, successful
   `DelegationSuspended` is the active-turn path that reaches
   `prompting -> connected` without a preceding `TurnComplete`.
4. Suspension fence mismatch and suspension drain timeout do not create a
   retained checkpoint path. Both produce `SuspensionFailed`; cancelled
   response handling or the timeout branch sets `disconnect_requested`, and
   the outer loop breaks before its trailing `StatusChanged(Connected)`.
   Other upstream suspension failures use ordinary terminal handling.
5. Streaming actions are already rejected while the connection is not
   `prompting`. A late `content_delta`, thinking update, tool update, or plan
   update therefore cannot append to the retained checkpoint message while
   the connection is `connected`.

Initial readiness and idle cancellation may also emit `connected`, but neither
owns a prompting `liveMessage`, so they are checkpoint no-ops.

## Checkpoint Predicate

The predicate is evaluated immediately before applying a typed envelope:

```text
event.type == status_changed
and event.status == connected
and snapshot.status == prompting
and snapshot.liveMessage != null
```

The predicate uses reducer state and typed event fields only. It must not
inspect continuation marker text, agent output, `turn_aborted`, or timing.
An accepted `turn_complete` cannot satisfy this edge: its mapped reducer
actions first move the snapshot to `connected`, and `prepareEventFrame()`
closes that step immediately. No additional accepted-completion marker check
is needed.

The later `ContinuationWaitingChanged` event cannot be the trigger because it
is published only after the suspension acknowledgement; the `connected` event
has already crossed the frontend boundary by then.

## Frontend Design

### 1. Add checkpoint-specific admission

Add `admitSuspensionCheckpoint()` in
`src/contexts/acp-connections-context.tsx`. It returns the admitted runtime
conversation IDs directly; an empty array means the status edge is not a
checkpoint. Keep `admitTurnComplete()` unchanged; its authoritative-delivery
fallback, `projectedInFrame` behavior, empty-owner acceptance, and user-stop
fences are terminal-only semantics.

Checkpoint admission examines only runtime entries that already exist. For
each runtime it will:

- derive the connection's known session through
  `resolveKnownConnectionSessionId()`;
- accept a candidate only when it maps through the known session or
  conversation aliases, or already owns the exact connection
  `liveMessage.id`;
- reject a conflicting non-null runtime session;
- reject a runtime that owns a different non-null live message; and
- never materialize a runtime or accept an ownerless checkpoint.

A mapped runtime with no current live message may be a candidate so the mirror
can publish the checkpoint object to it. Exact ownership is checked again after
canonical publication before promotion.

### 2. Split the accepted frame at the checkpoint

`prepareEventFrame()` already splits a connection frame after every accepted
`turn_complete`. It will also split immediately after an admitted suspension
checkpoint.

This ordering is required for a coalesced delivery such as:

```text
prompting -> content A -> connected -> prompting -> content B
```

The first prepared step owns A and promotes it. The second step owns a new
`liveMessage` containing B. Without the split, the final frame snapshot would
expose only B.

The prepared step carries `checkpointRuntimeConversationIds` separately from
`completionRuntimeConversationIds`. The fields remain separate so a checkpoint
cannot enter terminal-only branches. A checkpoint step also carries
`liveMessageIsLive=true`, derived from its pre-boundary `prompting` state, for
desktop, Web live, and replay delivery.

### 3. Preserve the existing virtual-step identity rule

`prepareEventFrame()` creates virtual reducer snapshots for intermediate
boundaries. Reducer-generated message UUIDs in those snapshots are intentional:

- for the last step of a context, `commitEventFrame()` substitutes the actual
  reducer object already published in the connection map;
- for an earlier step, canonical mirroring and checkpoint promotion both use
  the same `step.nextConnection.liveMessage` object; and
- only the final continuation message must share identity with the final
  published connection state.

Therefore an intermediate A UUID is not an orphaned duplicate identity. It is
the identity published to canonical runtime state and immediately used to form
the completed local turn. There is no second published A object whose UUID
must match it.

Do not move UUID allocation into `FrameAction` as part of this fix. Instead,
the coalesced-frame regression must record the exact A object accepted by the
canonical sink and assert that the promoted local turn derives from that ID,
while B matches the final connection-map live message.

### 4. Reuse mirror owner filtering and canonical rejection

The mirror's current completion parameters perform two jobs: they limit
publication to admitted runtime owners and collect runtime IDs whose canonical
sink returns `false`.

Generalize those parameter names to boundary-neutral owner and rejection sets,
or equivalently pass the active checkpoint sets through the same code path.
Do not bypass either behavior for checkpoints.

For a checkpoint step, commit ordering is:

```text
candidate checkpoint owners
  -> canonical/transcript mirror filtered to those owners
  -> remove owners rejected by canonical
  -> exact live-id ownership check
  -> local turn promotion
```

Passing `liveMessageIsLive=true` is required even though the post-step
connection status is `connected`. In Web replay, falling back to
`nextConnection.status === "prompting"` would publish A as non-live and can
cause the runtime store to reject a later hidden continuation stage.

### 5. Promote before the non-completion early return

In `commitEventFrame()`, place checkpoint promotion after
`mirrorLiveMessageForCanonical()` and before the existing
`if (!completion) continue`.

Filter `checkpointRuntimeConversationIds` through the same canonical rejection
set. For each remaining candidate, promote only when:

```text
runtime.liveMessage?.id === finalLiveMessage.id
```

Checkpoint ownership must not use the completion fallback for
`syncState === "awaiting_persist"` or an in-flight optimistic turn. That
fallback exists for terminal delivery after a live message has already been
consumed and is not valid for a suspension boundary.

For exact owners, call
`completeLiveTranscriptTurn(runtimeConversationId, finalLiveMessage)`. This
reuses the existing atomic handoff, Markdown partition preservation, turn-ID
deduplication, settlement-patch draining, and matching live projection removal.

Do not set `acceptedCompletionMessageId` or dispatch
`TURN_COMPLETION_ACCEPTED`. Those markers authorize terminal replay, while a
suspended stage has only been transcript-checkpointed.

### 6. Keep terminal effects isolated

The checkpoint path bypasses behavior owned by `turn_complete`:

- user-stop completion coordination;
- system notifications;
- pending-question extraction at terminal completion;
- `SETTLE_SESSION_FAILURES` at clean end;
- awaiting-reply generation;
- lifecycle and broker settlement; and
- conversation `PendingReview` transitions.

This does not mean the `connected` transition has no other effects. Existing
leave-prompting reducer behavior remains unchanged: it clears
`pendingUserMessage`, resets request estimation and the generation clock,
clears `claudeApiRetry`, `pendingAskQuestion`, and `pendingPlanApproval`, and
keeps the checkpoint `liveMessage` long enough for ordered publication.

The backend continues to own status restoration, permission draining, tool
watchdog completion, suspension acknowledgement, and continuation waiting
projection.

## State Transitions

```text
Before suspension
  connection.status = prompting
  connection.liveMessage = A
  runtime.liveMessage = A

Checkpoint event
  connection.status = connected
  connection.liveMessage = A          # retained for ordered publication
  runtime.localTurns += completed(A)
  runtime.liveMessage = null

Hidden continuation starts
  connection.status = prompting
  connection.liveMessage = B          # fresh id
  runtime.liveMessage = B
  runtime.localTurns still contains A
```

Repeated delivery is fenced first by accepted event sequencing and then by
canonical rejection and exact runtime ownership. A checkpoint with no exact
owner after mirroring changes no runtime.

## Web And Snapshot Scope

The checkpoint applies to accepted desktop events, Web live events, and Web
resume replay. Add a replay regression because mapped/replay delivery currently
does not preserve the pre-boundary `isLive` bit automatically.

Snapshot fallback is intentionally different. `ws_attach` may return an
authoritative snapshot instead of replay, and a completed suspension snapshot
contains `status=connected, live_message=null`. `HYDRATE_FROM_SNAPSHOT` replaces
the mutable live state with that payload. The frontend cannot infer the lost
stage or synthesize a checkpoint from it.

This repair therefore guarantees prevention while the boundary is delivered
live or replayed. Snapshot-only recovery continues through normal cold
conversation parsing and remains subject to its history coverage. Full
snapshot reconstruction would require a separate backend payload/history
design.

## Alternatives

### Rejected: Refactor `admitTurnComplete()` into a shared admission helper

The existing function contains terminal-only authority, projection, ownerless,
and user-stop rules. Sharing it would couple checkpoint correctness to terminal
fallbacks. A small dedicated checkpoint admission is narrower and leaves the
well-covered terminal path unchanged.

### Rejected: Allocate message UUIDs in frame actions

The existing virtual-step object is already the single identity mirrored and
promoted for an intermediate boundary. Moving UUID creation into actions would
broaden reducer contracts without fixing a demonstrated mismatch.

### Rejected: Emit `TurnComplete` with a suspension flag

`TurnComplete` has many backend and frontend consumers. Even with a flag, every
consumer would need auditing to suppress notifications, awaiting-reply,
conversation status, broker settlement, retry settlement, and user-stop logic.

### Deferred: Add a new `TurnSuspended` ACP event

A dedicated event is explicit, but it adds a Rust event variant, TypeScript
wire type, snapshot/replay handling, subscriber audits, and tests in both
runtimes. The existing successful-suspension status edge is already ordered and
replayed. Add a dedicated event only if another retained active-turn path can
legitimately emit the same edge without representing a transcript boundary.

### Rejected: Trigger from `ContinuationWaitingChanged`

The waiting projection is emitted after suspension acknowledgement and can
arrive after `connected`. It is row-level continuation state, not the wire turn
boundary.

### Rejected: Add another post-checkpoint message-ID guard

`applyStreamingAction()` already drops all streaming actions while status is
not `prompting`, and the runtime store rejects stale non-live publication after
the turn is drained. Add another guard only if a regression test identifies an
event type that bypasses both existing protections.

### Rejected: Refetch the provider transcript at every suspension

The synchronous live-to-local handoff already has the final rendered content.
Refetching introduces file-flush races and repeats a previously reverted source
of trailing-content loss.

## Tests

Add focused frontend tests to
`src/contexts/acp-connections-context.test.tsx` using the existing provider
harness.

### Primary regressions

1. Deliver `prompting -> content A -> connected` and then
   `prompting -> content B` in separate frames. Assert A is in `localTurns`, B
   is the only current live message, no hidden user turn appears, and later
   frames do not change A.
2. Deliver the same sequence in one accepted frame. Assert the frame split
   preserves A before B is created.
3. Record the A object accepted by canonical in the coalesced test. Assert the
   completed A turn ID derives from that exact live-message ID, and B's object
   is the final connection-map live message.
4. Through `handlers.onReplay(...)`, deliver at least two hidden continuation
   stages and their checkpoints. Assert both completed stages remain in order
   and the current stage remains live. This proves checkpoint
   `liveMessageIsLive=true` is transport-neutral.

### Ownership and rejection regressions

- A canonical sink returning `false` for a checkpoint owner prevents that
  runtime from being promoted.
- A mapped runtime with a conflicting live ID is not mirrored or promoted.
- Checkpoint promotion requires exact post-mirror live ID and does not use the
  `awaiting_persist` or optimistic-turn completion fallback.
- Two aliases that own the same canonical live object each preserve their own
  runtime turn without duplicating either turn.
- An ownerless checkpoint does not create a runtime session.

### Safety regressions

- `turn_complete -> status_changed(connected)` promotes exactly once and keeps
  all existing terminal effects.
- Initial or idle `status_changed(connected)` with no prompting live message is
  a no-op.
- A late content delta while connected does not mutate or republish completed
  A.
- Checkpoint promotion sends no completion notification and creates no
  awaiting-reply or `PendingReview` transition.

Extend the existing Rust test
`delegation_suspend_cancelled_response_clears_turn_without_tree_cancel` to read
the recorded session events and assert:

- exactly one `AcpEvent::StatusChanged { Connected }` is emitted for successful
  suspension; and
- no `AcpEvent::TurnComplete` is emitted.

`EventEmitter::Noop` is sufficient because `emit_with_state()` still records
the event in `SessionState`; no production Rust behavior changes are needed.

## Files

Expected implementation scope:

- Modify `src/contexts/acp-connections-context.tsx` for checkpoint admission,
  frame splitting, canonical mirroring, and exact-owner promotion.
- Modify `src/contexts/acp-connections-context.test.tsx` for desktop, replay,
  identity, ownership, rejection, and safety regressions.
- Modify only the existing test in `src-tauri/src/acp/connection.rs` to pin the
  successful suspension event contract.

No database, transport protocol, parser, internationalization, dependency, or
production Rust changes are planned.

## Verification

Run the narrow frontend and Rust targets first:

```bash
pnpm test -- src/contexts/acp-connections-context.test.tsx
cd src-tauri
cargo test --lib --features test-utils \
  acp::connection::tests::delegation_suspend_cancelled_response_clears_turn_without_tree_cancel \
  -- --exact
```

Then run the required frontend checks for the touched surface:

```bash
pnpm eslint src/contexts/acp-connections-context.tsx \
  src/contexts/acp-connections-context.test.tsx
pnpm test
```

No full Rust regression is required for a test-only Rust edit unless the
focused test exposes a broader failure.

## Existing Affected Sessions

This fix prevents subsequently delivered continuation stages from overwriting
each other. It cannot recreate a stage that the running frontend already
removed, or one omitted by a snapshot fallback with no live payload.

Those bytes remain in the provider rollout and may reappear through a cold
conversation parse when they fall inside requested history coverage. If an
affected session still omits them after a cold reload, recovery belongs to the
separate history-window design rather than this live checkpoint repair.
