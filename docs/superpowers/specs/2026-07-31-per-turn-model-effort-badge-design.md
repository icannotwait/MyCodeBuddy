# Per-turn model and reasoning effort badges

**Date:** 2026-07-31
**Status:** Approved for implementation planning
**Approach:** Extend the existing turn metadata pipeline and remove the session-level status chip

## Problem

The active session's model and reasoning effort are currently shown in the
application status bar. That presentation is session-level even though the
archive records these values on individual assistant turns. It becomes
ambiguous when a session changes model or effort, and it makes the metadata
hard to associate with the response that produced it.

The assistant turn footer already contains the compact metadata row for token
usage, duration, previous-message navigation, and completion time. It is the
natural location for model and effort as well.

## Goals

| Goal | Success criteria |
|------|------------------|
| Per-turn attribution | Each assistant turn shows its archived model and reasoning effort in its own `TurnStats` row when the fields exist |
| Correct merged-turn behavior | Consecutive assistant sub-turns preserve all distinct model and effort values in encounter order |
| No session-level duplication | The bottom status bar no longer renders the active session model or effort chip |
| Live/history parity | Historical, replayed, and promoted live turns use the same metadata pipeline and rendering rules |
| Missing metadata is quiet | Null, undefined, and blank effort values produce no effort control; no current session config is used as a fallback |
| Existing footer behavior remains stable | Copy, token usage, duration, jump, completion time, interruption, and artifact controls keep their current behavior |

## Non-goals

- Changing Rust parsers, ACP frames, database schemas, or backend APIs
- Showing the current session configuration on every turn when the archive did not record it
- Changing the conversation details panel's model or effort display
- Changing the status bar's workspace statistics, task, update, command, or alert controls
- Adding visible model/effort text to the message body or changing the existing compact footer layout

## Design

### Metadata pipeline

The existing `MessageTurn.model` path is extended with
`MessageTurn.reasoning_effort` at each frontend boundary:

```text
MessageTurn
  -> AdaptedMessage
  -> ResolvedMessageGroup
  -> HistoricalMessageGroup
  -> TurnStats
```

`AdaptedMessage` receives an optional `reasoning_effort` field, and
`adaptMessageTurn` copies it from the source turn. The `MessageTurnAdapter`
cache fingerprint includes the field, so a post-turn metadata patch that adds
or changes effort invalidates the cached adapted message and reaches the UI.

`ResolvedMessageGroup` receives the corresponding single-value and
multi-value fields. `HistoricalMessageGroup` passes them to `TurnStats` next
to the existing model props.

The frontend reads only archived turn metadata. The existing live session
configuration remains useful to selectors, but it is not consulted for this
display.

### Consecutive assistant turns

`mergeConsecutiveAssistantTurns` already aggregates usage, duration, model,
completion time, and outcomes. Its metadata aggregation is extended as
follows:

- collect non-empty model values in first-seen order and remove duplicates;
- collect non-empty, trimmed reasoning-effort values in first-seen order and
  remove duplicates;
- pass a single value through the singular prop when only one distinct value
  exists;
- pass all distinct values through the plural prop when more than one exists;
- keep completion time semantics unchanged: the latest non-null completion
  value wins.

This keeps a merged visual response faithful when an agent emits multiple
assistant sub-turns with different metadata.

### Turn footer UI

`TurnStats` keeps the existing model control and adds a compact reasoning
effort control in the same metadata row. The model uses its existing
`BrainCog` icon. The effort control uses a `Gauge` icon and the same tooltip
and focus treatment as the other read-only metadata controls.

- Model tooltip: the existing localized model label and one or more model IDs.
- Effort tooltip: a new localized reasoning-effort label and one or more
  archived effort values.
- Multiple values are joined with `, ` inside the tooltip only; the footer
  remains compact.
- The row's early-return condition includes model and effort, so a turn with
  only metadata still renders its metadata controls.
- Missing model or effort values hide only that control.
- Existing copy and action controls keep their current ordering and behavior;
  model remains before token usage, and effort is adjacent to model.

### Status bar removal

`StatusBar` stops importing and rendering `StatusBarSessionModel` on desktop
and mobile. The status-bar-only component, its display resolver, and their
dedicated tests are removed when no remaining consumer exists. Shared
`active-session-details` helpers remain because conversation detail surfaces
still use them.

The status bar retains its current height, spacing, and remaining controls;
removing the chip must not leave a dead placeholder or alter the right-side
controls.

### Internationalization and accessibility

Every locale message file receives the same `messageList.reasoningEffort`
label. The new control has an accessible label and a tooltip, matching the
existing model and duration controls. Model IDs and effort values are marked
as non-translatable display values, while labels use `next-intl`.

## Testing

The implementation follows a red-green-refactor cycle with focused tests
before production changes:

1. Adapter tests verify effort pass-through and cache invalidation when effort
   changes without any content change.
2. Message-list merge tests verify single-value preservation, distinct
   multi-value ordering, duplicate removal, and blank-effort filtering.
3. `TurnStats` tests verify model and effort controls render together, effort
   is hidden when absent, and metadata-only turns still render.
4. A status-bar integration test or equivalent focused render assertion verifies
   the removed session chip is not rendered while the remaining status-bar
   controls stay mounted.
5. Locale shape checks and the normal frontend checks cover all ten message
   files.

## Acceptance criteria

- The bottom-left session model/effort text is absent on desktop and mobile.
- A completed assistant turn with archived model and effort exposes both values
  from its turn footer controls.
- A turn without archived effort exposes no effort control, even if the live
  selector currently has an effort configured.
- Model/effort metadata survives adapter cache reuse boundaries and merged
  assistant sub-turn rendering.
- Existing turn footer actions and all unrelated status-bar controls continue
  to work.
- Focused tests, the full Vitest suite, ESLint, and the static export build
  pass.
