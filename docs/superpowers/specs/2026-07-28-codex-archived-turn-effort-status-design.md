# Codex Archived Turn Effort Status Design

Date: 2026-07-28
Status: Approved for implementation planning

## Problem

The bottom status bar already renders an active conversation as
`model · reasoning effort`. The model appears for
Codex, but the reasoning effort can remain absent after a live turn completes.

Codex rollout parsing already reads the real per-turn effort from
`turn_context.payload.effort`, with
`turn_context.payload.collaboration_mode.settings.reasoning_effort` as the
archive-format fallback. The parser stamps that value onto the parsed assistant
turn's `MessageTurn.reasoning_effort`.

The value is lost later in the frontend. A completed streaming reply is kept in
`ConversationRuntimeSession.localTurns`. The post-turn archive reparse aligns
parsed assistant turns with those local turns and currently backfills usage,
duration, model, and completion time, but not `reasoning_effort`. The cold
detail may predate the completed reply, so the status bar has no archived effort
to display until a full reload replaces the local runtime state.

## Requirements

- Display only an effort persisted on a real archived assistant turn.
- Never infer or fall back to the live ACP `reasoning_effort` selector.
- Update the status bar after the existing post-turn archive reparse observes
  the completed Codex turn.
- Preserve the existing history boundary so an older archived turn's effort is
  never assigned to a newly completed reply.
- Preserve first-write-wins behavior: archive metadata fills a missing local
  value but does not overwrite an already populated value.
- Continue showing only the model when the archived turn has no effort.

## Design

Extend `TurnMetadataPatch` with an optional `reasoning_effort` field. During
`computeTurnMetadataPatches`, copy the effort from the same parsed assistant
turn already selected for model, usage, duration, and completion time.

For the existing merged-sub-turn case, use the matched (latest aligned) parsed
turn's effort. If it has no effort, use the same folded-turn fallback policy as
the model metadata. This keeps the aggregate local turn representative of the
latest archived metadata without consulting session configuration.

In the `PATCH_TURN_METADATA` reducer, apply the effort with the same
first-write-wins rule as model:

```text
next effort = local effort when present, otherwise archived patch effort
```

Include effort in the reducer's change detection and in the updated local turn.
No status-bar component change is required: `resolveActiveSessionDetails`
already scans `localTurns` before cold detail and
`resolveSessionModelDisplay` already refuses live-config effort fallback.

## Data Flow

```text
Codex rollout turn_context
  -> CodexParser MessageTurn.reasoning_effort
  -> getFolderConversation post-turn reparse
  -> computeTurnMetadataPatches
  -> PATCH_TURN_METADATA
  -> ConversationRuntimeSession.localTurns
  -> resolveActiveSessionDetails
  -> StatusBarSessionModel
```

## Error And Retry Behavior

The existing metadata synchronization retry behavior remains authoritative. If
the rollout has not flushed yet, no effort patch is produced and a later retry
can observe it. If all retries finish without an archived effort, the status bar
does not display one. No default value, selector value, or stale historical
value is substituted.

## Tests

- `computeTurnMetadataPatches` carries the matched parsed turn's archived
  effort into the patch.
- A resumed conversation excludes historical efforts before the captured
  `persistedAssistantCount` boundary.
- A parser/local sub-turn count mismatch follows the documented merged-sub-turn
  effort rule.
- `PATCH_TURN_METADATA` fills a missing local effort.
- `PATCH_TURN_METADATA` does not overwrite an existing local effort.
- Existing status-bar tests continue to prove that live config is not used when
  archive history has no effort.

## Non-Goals

- Showing the currently selected effort before it appears in an archived turn.
- Adding a session-level effort field to conversation summaries.
- Changing Codex rollout parsing or ACP selector behavior.
- Changing the status bar's visual layout or localization.

## Acceptance Criteria

After a Codex assistant turn with archived effort `high` completes and the
existing post-turn reparse succeeds, the bottom status bar displays
`<model> · high`. A turn whose archive has
no effort continues to display only `<model>`.
