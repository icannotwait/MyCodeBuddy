# Event/view reply-loss fix review

Reviewed the working diff against `093f5a4a`, restricted to the ACP provider,
`LiveTranscriptView`, `ConversationSessionSurface`, and their requested tests.
Production and test files were not edited; no agents were dispatched. Store
implementation review and backend validation remain separately owned.

**Verdict: changes required — two Important findings, no Critical findings.**

## Requirements assessment

| Requirement | Assessment |
| --- | --- |
| F5: frame-independent completion ownership | Incomplete. The normal two-terminal case is covered, but a supported stale-external-ID virtual owner still loses the second reply when terminals share a frame (R1). |
| F6: flush before gap hydration, including completed snapshots | The new flush precedes hydration. The completed-snapshot regression checks real runtime history and one canonical publication, beyond publication-only coverage. No additional blocking defect established in this path. |
| F7: readonly unmount preserves shared runtime | Addressed for the requested readonly bridge: unmount releases its registration and metadata-sync cancellation handle without removing the runtime. Streaming and completed sibling-state assertions cover the intended boundary. |
| F8: bound-draft runtime identity survives remount | Addressed: initialization prefers the tab's retained runtime ID. The remount regression reads the retained reply from the real runtime store. |
| F9: standalone readonly incremental projection | The missing sink is supplied using the existing implementation. Combined with F10, however, multiple viewers can corrupt the shared projection (R2). |
| F10: independent subscriptions and symmetric disposal | Registration/disposal is independent, including merged rekeys, but publication is not safe when registrations target the same runtime projection (R2). |
| F12: transient snapshot failure preserves state and buffers | Retry retains state and uses ingestor pause/resume; completion re-resolves the key and checks connection identity. Unmount clears retry timers and late responses check ingestor identity. No additional blocking defect established; the narrow unmount probe passed. |

The requested fixes therefore do **not** yet satisfy all acceptance conditions.

## Important findings

### R1 — P1: a previously admitted virtual owner loses the next completion when its persisted external ID is stale

Location: `src/contexts/acp-connections-context.tsx:5825`, with rejection at
`:5857` and frame bookkeeping at `:6063`.

Trigger: use the already-supported fixture in
`src/contexts/acp-connections-context.test.tsx:9823`: virtual runtime
`-1049581191` is bound to DB conversation 7, its persisted external ID is
`stale-persisted-sess`, and the connection's known active session is `sess-1`.
Another runtime for DB conversation 7 has the current external ID. Publish A's
text in frame 1. In frame 2, deliver A complete, B user message, B prompting,
B text, and B complete.

A is admitted through its live-message ownership. For B, the new
`completedRuntimeIds` branch replaces that owner's runtime live message with
`null`. This removes the live-ownership exception to the stale persisted
external ID, and `!sessionMatches` rejects the virtual runtime. B can still be
admitted for the DB runtime, so the boundary filter skips the virtual runtime's
sink. Its local history retains A but contains no `answer B`; its live message
is also null. The connection has consumed B's terminal sequence.

**Observed, using the real provider/runtime in an in-memory test variant:**
the same-frame assertion that the virtual runtime contains `answer B` failed.
Moving only B's completion into the next frame made that assertion pass. This
is a remaining F5 defect, not a claim that the original normal-case fix has no
effect. It also bypasses the stale-external-ID compatibility explicitly
documented beside the admission code.

Required correction: preserve the already-proven ownership of this connection
and session as frame preparation advances to subsequent turns, while retaining
wrong-session and stale-stop rejection. Add the combined stale-ID/multi-terminal
case to the permanent coverage; the two existing regressions in isolation do
not exercise this interaction.

### R2 — P2: multiple subscriptions append the same delta twice to one runtime projection

Location: `src/contexts/acp-connections-context.tsx:7488`; the readonly caller
now creates the shared projection sink at
`src/components/message/live-transcript-view.tsx:131`.

Trigger: two independent viewers subscribe to the same connection and runtime
ID, each using `createLiveTranscriptFrameSink(runtimeId, connectionId)`. Deliver
a desktop frame containing prompting and a content delta. The new fan-out
invokes both sinks, but both mutate the process-wide projection keyed by that
same runtime ID. This also applies across canonical/alias registrations that
share a projection.

The first sink rebuilds or appends the canonical content. The second applies
the identical frame again. `liveTranscriptStore.publish` at
`src/stores/live-transcript-store.ts:549` and
`applyLiveTranscriptEvents` at `src/lib/acp/live-transcript-projector.ts:659`
do not reject an already-applied sequence: the latter advances a cursor but
unconditionally applies each delta. Those unchanged implementations were safe
only under the previous single-publication assumption.

**Observed, using two real projection sinks in an in-memory provider variant:**
after the first frame the projection text was `answer Aanswer A`, expected
`answer A`. Canonical runtime text need not be duplicated for the incremental
view to show the corruption. The visible impact concerns the opt-in incremental
rendering path, not default legacy rendering or transcript-file deletion.

Required correction: deliver each accepted frame once per shared projection,
or make its publication idempotent for that connection/message/cursor, while
preserving independent subscriber disposal. The new provider test at
`src/contexts/acp-connections-context.test.tsx:7687` uses separate arrays for
each fake transcript sink; it proves callback delivery, not shared projection
correctness. The standalone view test also has only one sink.

## Quality assessment

The changes are focused and reuse the existing transcript sink, runtime ID,
flush, and ingestor machinery. The ownership/disposal implementation is
understandable; no speculative refactor is requested. The blocking quality gap
is integration coverage: both findings cross conditions that the added tests
exercise separately. The stale-ID case requires actual runtime ownership, and
the multiple-subscriber case requires the actual shared projection store.

Additional narrow probes passed:

- Dispose the source registration twice after rekey merges it with an existing
  destination registration: only the destination receives the next frame.
- Reject a gap snapshot, unmount the provider, then advance fake time five
  seconds: no further snapshot request is made.
- Split-frame stale-ID control: the virtual runtime receives B's completed reply.

## Evidence and limits

No full suite was rerun. Implementer records report 425 provider + 23 ingestor
tests, three original ACP audit variants, and 119 component tests passing;
these are cited as recorded results, not independent runs in this review.

Review probes used `node --input-type=module` with `startVitest` from
`vitest/node` and an in-memory Vite `enforce: "pre"` transform of the existing
provider fixture. Only `review:` test names ran; the original 425 tests were
skipped. No probe file or source/test modification was written. The final
ownership/control selection reported **one failed, three passed, 425 skipped**;
the separate shared-projection probe failed with the exact doubled text above.
Transformed stack line numbers are shifted; the finding locations above refer
to the unmodified working source.

The initial broad stale-ID diagnostic also expected B's user turn in a split
frame and failed for that separate expectation. The final control deliberately
checks the requested reply-preservation invariant only; no additional finding
is inferred from that exploratory assertion. No browser rendering, full build,
store review, or backend validation is claimed.
