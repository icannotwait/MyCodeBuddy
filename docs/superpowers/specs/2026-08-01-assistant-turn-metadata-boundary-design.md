# Assistant Turn Metadata Boundary Design

## Context

The conversation renderer collapses consecutive assistant archive records into
one synthetic turn. This is necessary for tool calls and polling updates that
span parser-level records, but the current merge also aggregates every distinct
model and reasoning effort into arrays. `TurnStats` then renders those arrays in
one footer, so a model or effort switch appears as multiple values on one turn.

A visible turn must represent one model and one reasoning effort. A concrete
metadata change is therefore a turn boundary, even when there is no intervening
user message.

## Goals

- Split consecutive assistant records when a concrete model or reasoning effort
  changes.
- Keep records with the same metadata merged so existing tool-card, usage,
  duration, and artifact aggregation continues to work.
- Treat missing metadata as unknown rather than as a boundary.
- Render at most one model and one reasoning effort in each turn footer.
- Apply the rule to existing archived conversations without a migration.

## Non-Goals

- Changing backend archive formats or parser output.
- Treating every assistant archive record as a visible turn.
- Inferring missing effort from session configuration.
- Changing live-turn statistics or composer model selection behavior.

## Merge Boundary

`mergeConsecutiveAssistantTurns` will track the current buffer's known metadata
identity:

- `model`: trimmed non-empty model value, or unknown.
- `reasoning_effort`: trimmed non-empty effort value, or unknown.

Before appending an assistant item, compare each concrete value with the known
value for the current buffer:

1. If either concrete value conflicts with an already-known value, flush the
   current buffer and start a new one with the incoming item.
2. If a value is missing, it neither conflicts nor clears the known value.
3. If the buffer does not yet know a field and the incoming item provides it,
   adopt that value for the buffer.
4. Equal concrete values remain in the same buffer.

Examples:

| Records | Visible turns |
| --- | --- |
| `A/high`, `A/high` | one `A/high` turn |
| `A/high`, `B/high` | `A/high`, then `B/high` |
| `A/high`, `A/low` | `A/high`, then `A/low` |
| `A/high`, `unknown/unknown`, `B/low` | `A/high`, then `B/low` |
| `unknown/unknown`, `A/high` | one `A/high` turn |

The transparent unknown record in the fourth example remains attached to the
preceding buffer. It does not create a third footer or bridge the later concrete
metadata conflict.

## Merged Output

Within one metadata-compatible buffer, the existing merge behavior remains:

- concatenate and regroup tool parts;
- aggregate usage and duration;
- retain completion time and terminal outcome;
- union source turns and artifact inputs;
- preserve merged-run cache reuse when membership is unchanged.

The merged group exposes only singular `model` and `reasoning_effort` values.
The plural `models` and `reasoning_efforts` fields and corresponding `TurnStats`
props are removed. This makes the one-footer/one-metadata invariant explicit and
prevents a future fallback from rendering multiple values in one footer.

Each metadata-compatible buffer is cached independently. A boundary flush means
a cache entry can never span two concrete model or effort identities.

## Rendering

`HistoricalMessageGroup` passes the singular metadata fields to `TurnStats`.
`TurnStats` keeps its current metadata-only rendering behavior, accessible labels,
and tooltips, but builds at most one model control and one effort control.

Missing archived effort remains hidden. No session-level fallback is introduced.

## Testing

Focused tests will verify:

- different concrete models produce two assistant turn items with the correct
  singular metadata;
- different concrete efforts produce two items even when the model is equal;
- missing metadata does not split a compatible run;
- equal metadata still merges and preserves aggregate usage, duration, tools,
  and source turns;
- a missing record between two conflicting concrete identities does not bridge
  the boundary;
- `TurnStats` continues to render singular metadata and metadata-only footers;
- plural metadata fields and props are absent from the rendering path.

The focused message-list and turn-stats suites run first, followed by the full
frontend test suite and static export build.

## Compatibility And Rollout

This is a deterministic frontend projection change. No database migration or
archive rewrite is required. Historical sessions adopt the corrected turn
boundaries the next time they render.
