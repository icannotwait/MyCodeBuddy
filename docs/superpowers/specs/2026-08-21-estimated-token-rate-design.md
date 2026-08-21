# Estimated Token Rate for ACP Requests Design

## Status

Direction approved in conversation on 2026-08-21. This document is the
implementation specification.

No implementation plan has been approved yet. This design does not change
production code.

## Executive Decision

Codeg will add a request-scoped output-token estimator for ACP agents that
report an LLM-completion boundary but do not report exact output tokens for
that request.

For the official Codex ACP adapter, each usable `usage_update` settles the
current request. Codeg will count observable root-model output produced since
the previous boundary, expand that count by a fixed hidden-reasoning ratio,
and divide it by the time from the first observable output to the boundary.
The estimator publishes only when the request completes. It does not publish a
continuously changing token rate while the request is streaming.

The resulting request sample flows through the existing request-usage
accumulator and turn-generation persistence path. Exact usage always takes
precedence over an estimate for the same request.

The live UI will not render zero-valued token-rate or generation-share fields.
Each positive target will animate over five seconds at 33 ms intervals. A new
target cancels the previous transition and starts from the value currently on
screen.

## Problem

The live turn footer currently renders request-level output speed and
generation share from `RequestUsageSnapshot`. An empty snapshot is converted
to zero, so a Codex session shows `0.0 tok/s` and `0 (0%)` throughout a turn.

The official `@agentclientprotocol/codex-acp` packages do not provide the
request usage expected by Codeg's current parser:

- In both 1.4.0 and 1.6.2, `CodexEventHandler.createUsageUpdate` emits only
  context-window `used` and `size` values.
- `PromptResponse.usage` is built from `lastTokenUsage`, so it describes only
  the final upstream request rather than the complete tool-using turn.
- Codex app-server exposes exact per-response usage internally through
  `rawResponse/completed`, but codex-acp 1.6.2 explicitly ignores that
  notification instead of forwarding it through ACP.
- Codeg's `_meta.codeg.outputTokens` parser supports an older patched adapter,
  but that field is absent from the official npm package used in production.

Therefore upgrading from codex-acp 1.4.0 to 1.6.2 does not repair this display.
Using `PromptResponse.usage` alone would also be incorrect for a turn that
contains multiple model requests separated by tool calls.

## Goals

- Show a useful token rate after each completed LLM request when exact output
  usage is unavailable.
- Exclude time-to-first-token (TTFT) from generation duration.
- Exclude tool execution time between model requests.
- Include observable output that the root model generated, including tool-call
  arguments.
- Estimate hidden reasoning without counting it as an additional token class
  on top of output tokens.
- Preserve exact Claude, Grok, generic `request_usage`, and patched Codex
  samples.
- Keep the existing aggregate definition:

  ```text
  turnTps = sum(outputTokens) / sum(generationDurationSeconds)
  ```

- Persist positive aggregate generation duration and output tokens through the
  existing turn-generation storage path.
- Hide unavailable or zero statistics instead of presenting them as measured
  zeroes.
- Smooth each newly settled display target without running overlapping
  transition timers.

## Non-Goals

- Recovering exact hidden reasoning tokens from encrypted reasoning content.
- Patching or redistributing the official codex-acp npm package.
- Treating context-window `used` or `size` as request output usage.
- Treating the final `PromptResponse.usage` as whole-turn usage.
- Including prompt processing, TTFT, tool execution, permission waits, user
  question waits, or subagent work in generation duration.
- Estimating input tokens, context-window consumption, cost, or provider
  billing.
- Continuously recalculating or publishing tok/s while an LLM request is still
  streaming.
- Online calibration, per-user learned ratios, or an adaptive model in this
  version.
- Adding a database migration solely to record whether a persisted sample was
  estimated.

## Accuracy Hierarchy

Codeg will choose one token count for each settled request in this order:

1. An exact usage sample emitted by the agent, such as Claude SDK request
   usage, Grok exact turn usage, a generic `request_usage`, or patched Codex
   metadata.
2. Exact `PromptResponse.usage.outputTokens` used only to replace the final
   request's estimate when correlation is unambiguous.
3. The fixed-ratio estimate described in this document.

An exact sample and an estimate for the same request must never both be added
to the turn total. A late exact value corrects the latest eligible estimated
sample; it is not appended as another request.

If the final exact value cannot be tied unambiguously to the latest estimated
request, Codeg keeps the estimate and ignores the exact value. It must not risk
double-counting the turn.

## Observable Token Scope

The estimator counts only output attributable to the current connection's
root model.

Included:

- assistant text deltas;
- visible reasoning or thinking-summary deltas;
- model-generated plan entry text; and
- model-generated tool-call arguments or raw input.

Excluded:

- tool stdout, stderr, results, file contents returned by tools, and images;
- status-only tool updates and tool-generated titles;
- UI-generated labels, summaries, and synthetic content;
- user prompts, permission answers, and question answers; and
- content carrying a parent tool-use id, because that output belongs to a
  subagent rather than the root request.

The existing dense-script and other-character estimator in
`src/lib/token-speed.ts` remains the character-to-token primitive. It measures
only newly observed content. Request-level rounding happens once at settlement
so repeated fractional rounding does not bias long turns.

### Snapshot and duplicate handling

Text and thinking events are wire deltas and rely on the existing envelope
sequence deduplication.

Plan updates and tool inputs can be repeated snapshots. The estimator keeps a
turn-local baseline for each plan snapshot and tool-call id:

- unchanged content contributes zero;
- appended content contributes only the appended estimate;
- a replacement is remeasured and changes the active request total by the
  token-count delta, floored so the active request cannot become negative; and
- status-only updates after a request boundary do not re-add the previous
  request's arguments.

These baselines survive request settlement for the life of the turn. Only the
active request's token total and clock reset at a boundary.

## Hidden Reasoning Estimate

Provider output-token accounting already includes reasoning tokens. Reasoning
is a subset of output, not an additional amount to add after output has been
estimated.

Let:

```text
q = reasoningTokens / outputTokens
visibleTokens = estimated tokens in the observable output scope
estimatedOutputTokens = visibleTokens / (1 - q)
```

For example, `q = 40%` uses a multiplier of `1 / 0.6 = 1.667`. It does not use
`visibleTokens * 1.4`.

The initial fixed profiles are:

| Provider profile | Effort | q |
| --- | --- | ---: |
| GPT | xhigh | 47.2% |
| GPT | max | 48.5% |
| GPT | high | 41.0% |
| GPT | medium | 40.0% |
| GPT | low | 40.0% |
| GPT | unknown or unavailable | 46.7% |
| Grok | xhigh | 55.6% |
| Grok | high | 63.0% |
| Grok | medium | 40.0% |
| Grok | low | 40.0% |
| Grok | unknown or unavailable | 57.0% |

Effort values are trimmed and normalized case-insensitively. Unsupported effort
names use the provider's unknown-effort ratio. A Codex connection uses the GPT
profile; a Grok connection uses the Grok profile. Other agents do not opt into
this fallback until they define both a reliable request boundary and a profile.

The effective profile is frozen when the first eligible output of a request is
observed. A settings change cannot alter the multiplier midway through that
request.

At settlement, Codeg computes the expanded token count and rounds it to the
nearest positive integer. A result that is non-finite or not positive is not a
sample.

## TTFT-Free Timing Contract

All estimator timestamps use frontend `performance.now()`. Unix timestamps
such as an envelope's `received_at` must not be mixed into this clock.

For one request:

```text
start = arrival of the first eligible root-model output
end = arrival of the LLM-completion boundary
generationDuration = end - start
```

Eligible start events are the first root text delta, thinking-summary delta,
plan text, or nonempty tool-call input. A hidden reasoning phase before any of
those signals is intentionally treated as TTFT and excluded. Once a visible
reasoning summary or other eligible output arrives, generation has started.

The primary Codex fallback boundary is `usage_update`. It is usable only when
the active request has a start timestamp and positive newly observed output.
This guard filters session hydration updates, context refreshes, local commands,
and duplicate boundaries.

Samples with duration below 1 ms, non-finite duration, or non-positive output
are discarded. The request estimator then resets its active clock and token
total. Tool execution occurs while no request clock is active, so it does not
enter generation duration. The next model request starts only when its first
eligible output arrives.

## Request Estimation State Machine

Each live connection owns one turn-local estimator. It is independent of the
React display component.

Conceptually, the state contains:

```text
active request:
  startedAt: performance timestamp or null
  visibleTokens: fractional count
  frozenProfile: provider + effort or null

turn ledger:
  ordered settled samples
  plan/tool snapshot baselines
  latest estimated sample eligible for exact correction
```

The state transitions are:

```text
no active output
  -- first eligible output --> start clock, freeze profile, add output

active request
  -- more eligible output --> increment or reconcile visible count
  -- exact request usage --> append exact sample, reset active request
  -- usage_update without exact usage --> append estimated sample, reset active
  -- rollback/cancel/turn reset --> discard active request

settled estimated final request
  -- exact PromptResponse usage --> replace its token count, preserve duration
```

`usage_update` always continues to update context-window `used/size`; its new
boundary role is an additional Codex-only behavior. If an exact
`request_usage` is emitted for the same underlying notification, exact usage is
processed first. It settles and clears the active request, so the following
plain `usage_update` has no active output and cannot append an estimate.

Turn rollback, reconnect without a live clock, and snapshot hydration do not
reconstruct elapsed generation time. Codeg resets the active estimator and
waits for new observable output. A later boundary with no post-attach start is
ignored. This undercounts an interrupted request but avoids fabricating a rate
from incompatible or unknown clocks.

## Exact Final-Request Correction

The backend currently discards `PromptResponse.usage` while finalizing the
prompt. It will expose positive `outputTokens` as a final-request correction
before completing the turn.

The existing `request_usage` wire event gains an optional
`replace_latest_estimate` boolean. Missing or false retains today's append
behavior. The prompt finalizer emits true; ordinary exact request usage emits
false. The frontend carries the flag unchanged into the reducer, so correction
semantics are explicit rather than inferred from event order or agent type.

The correction contract is intentionally narrow:

- it applies only to the latest settled sample;
- that sample must be estimated;
- no newer request may have started;
- the sample's measured duration is retained; and
- only its output-token count is replaced.

If no estimated sample satisfies those conditions, the correction is ignored.
It is never appended and never used as a whole-turn replacement. This preserves
all earlier per-request estimates in a tool-using turn while improving the
final request when the official adapter reports it exactly.

## Integration With Existing Usage State

The implementation is split by responsibility:

- a new pure frontend module, `src/lib/estimated-request-usage.ts`, owns ratio
  lookup, observable-token reconciliation, request state, and settlement;
- `acp-connections-context.tsx` maps ordered wire events into that pure state
  and publishes settled ledger snapshots;
- `request-usage-speed.ts` owns exact/estimated ledger aggregation and the
  replace-latest-estimate operation;
- Rust ACP prompt finalization exposes the optional exact final correction;
  and
- `live-turn-stats.tsx` owns only visibility and target interpolation.

The end-to-end flow is:

```text
root text/thinking/plan/tool input
  -> request-local observable token state + first-output clock

Codex usage_update
  -> settle exact sample when present, otherwise settle estimate
  -> request usage ledger
  -> RequestUsageSnapshot
  -> live publication + existing turn persistence

PromptResponse final usage
  -> replace_latest_estimate
  -> correct latest eligible ledger entry
  -> republish corrected snapshot before TurnComplete
```

The estimator and correction ledger derive the same aggregate fields already
consumed by the application:

```text
sampleCount
outputTokens
generationMs
tps = outputTokens / (generationMs / 1000)
estimatedSampleCount
```

`estimatedSampleCount` is live, non-persisted provenance. Appending an estimate
increments it; replacing an estimate with exact final usage decrements it. The
live UI marks the aggregate as approximate whenever it is greater than zero.

`request-usage-speed.ts` remains the owner of aggregate validation and rate
calculation. It will gain a replace-latest-estimate operation rather than
letting callers subtract totals manually.

`acp-connections-context.tsx` owns request boundaries and estimator lifecycle
because it sees ordered ACP events, tool inputs, connection identity, turn
reset, rollback, and exact request usage. The UI reads only the published
snapshot and cannot create or settle samples.

`request-usage-live.ts` continues publishing snapshots by canonical
conversation id. Positive settled aggregates continue through
`conversation-runtime-store.ts` and the existing `turn_generation_stat`
columns. No database schema change is required.

Because persistence currently stores only aggregate duration and token count,
the estimated/exact sample mix is transient metadata. The live fallback is
visually marked as approximate. Persisted historical values retain the current
numeric schema; adding durable provenance is a separate feature, not part of
this fix.

## Live Display Transition

The display consumes settled aggregate targets. It does not drive estimation.

### Visibility

- Null, non-finite, or non-positive tok/s is hidden.
- A value that formats to `0.0 tok/s` is hidden.
- `generationMs <= 0` hides both generation duration and percentage.
- Resetting a turn to an empty snapshot cancels the active transition and hides
  the fields immediately.
- At an enabled responsive breakpoint, each usage metric keeps a fixed-width
  slot. Its contents and adjacent separator use `visibility: hidden` until the
  metric is valid, so no literal zero is rendered and the footer does not shift
  when the first sample appears.

An estimated live rate uses an approximation marker or localized tooltip so it
cannot be mistaken for provider-reported billing usage.

### Transition

Each `LiveTurnStats` instance owns at most one transition interval:

```text
duration: 5000 ms
tick: 33 ms
clock: performance.now()
easing: easeOutCubic
```

On a new positive target:

1. Compute the current interpolated on-screen values.
2. Cancel the previous transition interval, if any.
3. Use the current values as the new start and the new snapshot as the target.
4. Tick every 33 ms for at most five seconds.
5. On completion, assign the exact target and clear the interval.

For the first positive target, the transition starts at zero but suppresses
rendering until the formatted value is positive. The scheduler interpolates
rate and generation duration. Generation percentage is derived on each render
from the interpolated generation duration and the current wall elapsed time;
it is not a third independently animated target. A new sample therefore cannot
create overlapping animation timers.

This behavior is deliberately independent of the reduced-motion preference,
as requested. The interval is still cleared on unmount, conversation change,
turn reset, or transition completion.

The existing low-frequency wall-elapsed clock is not a transition timer and
may remain separate. The uniqueness invariant applies to the 33 ms target
transition: only one such timer may exist per component instance.

## Failure and Recovery Rules

- A `usage_update` before observable output updates context usage but creates no
  request sample.
- A duplicate `usage_update` after settlement creates no sample because the
  active request is empty.
- A status-only tool update contributes no tokens and cannot start a clock.
- Tool-result streaming never contributes tokens or duration.
- A reconnect or hydration discards unknown pre-attach timing and waits for new
  output.
- Envelope replay is filtered by the existing sequence watermark before it can
  affect the estimator.
- A turn rollback or cancellation discards an unsettled estimate. Already
  settled samples follow the existing turn-reset semantics.
- Non-finite token estimates, invalid ratios, and durations below 1 ms fail
  closed by producing no sample.
- Missing model or effort metadata uses the provider-global ratio; it does not
  prevent the request from settling.
- Exact usage remains authoritative even if it differs substantially from the
  configured estimate.

## Testing Strategy

### Pure estimator tests

- dense-script and other-character increments remain additive;
- root text, thinking summary, plan text, and tool input are included;
- tool results, UI content, and parented subagent output are excluded;
- repeated plan and tool snapshots contribute zero;
- growing tool input contributes only its delta;
- replacement input reconciles without making the active total negative;
- profile lookup normalizes effort names and uses the documented fallbacks;
- `q = 40%` produces the `1 / 0.6` multiplier;
- expansion is rounded once at request settlement; and
- invalid ratios or non-positive results do not produce samples.

### Reducer and boundary tests

- first eligible output starts a `performance.now()` request clock;
- prompt send and hidden pre-output reasoning do not start the clock;
- plain Codex `usage_update` settles one estimate after output;
- `usage_update` with no start or no new output settles nothing;
- tool execution between boundaries adds no generation time;
- a second request starts independently after the first boundary;
- exact request usage suppresses the estimate at the matching boundary;
- final `PromptResponse.usage` replaces only the latest estimated sample;
- an ambiguous or duplicate final correction is ignored;
- rollback, cancellation, reconnect, and hydration clear active timing; and
- no path mixes `received_at` with `performance.now()`.

### Aggregate and persistence tests

- turn tok/s is total tokens divided by total generation duration, not the
  arithmetic mean of request rates;
- replacing the final estimated token count updates totals without changing
  duration or sample count;
- positive mixed exact/estimated aggregates publish through the existing
  conversation-id aliasing path;
- empty aggregates are not persisted; and
- existing `generation_ms` and `generation_tokens` persistence and overlay
  behavior remain unchanged.

### UI timer tests

Using fake timers:

- empty, zero, non-finite, and formatted-zero values are absent;
- the first positive target becomes visible only above formatted zero;
- a target reaches its exact value after five seconds;
- ticks occur on the 33 ms schedule;
- a newer target cancels the old interval and continues from the current
  displayed value;
- rate and generation duration use one transition interval, while percentage
  is derived from the interpolated duration;
- reset and conversation change hide immediately and cancel the interval;
- unmount leaves no interval running; and
- reduced-motion preference does not disable the transition.

### Regression tests

- exact Claude request usage remains unchanged;
- exact Grok turn usage remains unchanged;
- patched Codex `_meta.codeg.outputTokens` remains exact and is not duplicated
  by the fallback;
- context-window `used/size` continues to update independently; and
- existing live edit, tool-call, elapsed-time, and persisted turn-stat displays
  remain functional.

## Alternatives Considered

### Upgrade codex-acp only

Rejected. Version 1.6.2 still omits output tokens from ACP `usage_update` and
still ignores the internal exact `rawResponse/completed` notification.

### Use only `PromptResponse.usage`

Rejected as the primary source. It reports the final upstream request, not all
requests in a tool-using turn. It remains useful only as a final-request
correction.

### Estimate continuously in the UI

Rejected. The user-facing metric should advance at completed LLM-request
boundaries. Keeping settlement in the event layer also makes tool arguments,
deduplication, exact precedence, persistence, and reconnection behavior
testable independently of React rendering.

### Add reasoning on top of visible output

Rejected. The supplied reasoning ratio uses reasoning divided by total output,
and provider output already includes reasoning. The correct inverse is
`visible / (1 - q)`.

### Include TTFT or tool time

Rejected. The selected metric measures observable generation throughput. Its
clock begins at first observable root-model output and ends at request
completion.

### Keep rendering zero until data arrives

Rejected. Zero means measured absence, while this state means no valid sample.
The UI must distinguish unavailable from zero.

### Run overlapping animations

Rejected. Multiple intervals can race, regress the displayed value, and leak
across conversation changes. One cancel-and-restart transition preserves a
single current value and target.

## Acceptance Criteria

- Official Codex ACP 1.4.0 and 1.6.2 sessions no longer show permanent zero
  token-rate and generation-share fields.
- Before the first valid completed request, those fields are absent rather than
  zero.
- Each usable Codex `usage_update` settles at most one estimated request.
- Estimated output uses the documented provider/effort ratio and includes
  reasoning as a subset of output.
- TTFT and tool execution time are excluded by the first-output-to-boundary
  clock.
- Exact per-request usage is never double-counted with an estimate.
- Final `PromptResponse.usage` can correct only the final estimated request and
  cannot replace a whole multi-request turn.
- Turn tok/s is token-weighted across settled requests.
- Every positive display target transitions for five seconds at 33 ms ticks;
  a newer target cancels and replaces the prior transition from its current
  displayed value.
- Zero or reset hides immediately, and unmount leaves no transition timer.
- Existing request-usage publication and turn-generation persistence continue
  without a database migration.
