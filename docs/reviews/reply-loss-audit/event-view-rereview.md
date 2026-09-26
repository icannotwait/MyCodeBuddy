# Event/view reply-loss fix re-review

**Verdict: approved within the requested scope. Specification and code quality
both pass. R1 and R2 are addressed; no remaining Important or Critical issue
was established.**

Scope: the two findings in `event-view-review.md`, the follow-up recorded in
`acp-fix-results.md`, and the latest ACP provider and live-transcript-store
production/test diffs against `093f5a4a`. Existing view fixes were considered
only as callers of the shared projection. This is not a new general audit or
approval of the separately owned history-store/backend changes.

## Specification assessment

### R1: session-specific same-frame ownership — pass

`src/contexts/acp-connections-context.tsx:5825` and `:5853` now consult a
runtime-to-session map instead of only recording that a runtime completed.
The map is scoped to one connection's frame preparation (`:6005`) and records
the actual admitted terminal session (`:6071`). Reusing ownership requires
both the recorded session and the connection's known session to equal the
incoming terminal session. Existing connection mapping, wrong-session, live
ownership, and stale user-stop guards remain in the admission path.

The permanent paired regression checks real virtual-runtime history for both
A and B, with stale persisted identity and a DB runtime alongside the virtual
runtime. Both same-frame and split-frame delivery pass.

An independent in-memory variant changes the connection to `sess-2` between
A's admitted completion and B, while B's terminal also names `sess-2`.
The virtual owner still carries its stale persisted identity. A remains in
local history, B is not falsely promoted through A's `sess-1` ownership, and
the connection remains prompting in `sess-2`. This passes and exercises the
new session-specific condition rather than only the earlier top-level
wrong-session guard.

### R2: shared projection idempotence — pass

`src/stores/live-transcript-store.ts:552` rejects duplicate/older publication
cursors only when connection and message identities both match. Therefore:

- Multiple registrations sharing a runtime do not reapply the same delta.
- A resumed connection with a lower cursor rebuilds instead of being dropped.
- A new message at the same cursor rebuilds under its own identity.
- Overlapping compacted replay rebuilds from canonical using the raw-event
  overlap check, avoiding partial application of a combined old/new delta.
- Explicit snapshot `rebuild` remains available at an equal cursor. A later
  duplicate publication cannot undo that corrected snapshot, and subsequent
  new deltas append normally.

The provider's permanent regressions use real canonical/runtime/projection
stores for same-key and canonical/alias registrations and both disposal orders.
They verify continued delivery once while both sinks exist and after either
registration is disposed twice. The focused projection regressions cover the
cursor/identity/rebuild cases above, including terminal publication.

An independent three-sink probe combines overlapping replay, an equal-cursor
explicit correction, repeated publication of the old frame, the next delta,
a resumed connection with a lower cursor, an older replay, and a new message
at the same cursor. Every sink observes the expected canonical text; duplicate
publications preserve the already-applied snapshot object. The combined probe
passes.

## Quality assessment

The follow-up makes small changes at the ownership and shared-projection
boundaries that caused the findings. It retains independent subscription
disposal and the existing sink API, uses existing canonical rebuild behavior,
and adds no dependency or unnecessary abstraction. The added permanent tests
exercise the real shared state that was missing from the original coverage.
No additional refactor is requested.

## Independent evidence

One narrow run selected 14 permanent cases and two in-memory probes:

| Selection | Result |
| --- | --- |
| Provider: paired stale-ID ownership, four shared-projection/disposal cases, four wrong-session/stale-stop guards, one session-transition probe | 11 passed; 421 skipped |
| Projection store: four follow-up regressions, one combined three-sink probe | 5 passed; 14 skipped |
| Total | **16 passed; 435 skipped; exit 0** |

The run used `node --input-type=module`, `startVitest` from `vitest/node`, and
an in-memory Vite pre-transform to insert the two probes into existing fixtures.
Only the named follow-up cases, four ownership guards, and `rereview:` cases
were selected. The independent session probe reused the permanent paired
fixture with same-frame delivery, replaced B's user-message envelope with a
`session_started(sess-2)` envelope, changed B's terminal session to `sess-2`,
and asserted no cross-session promotion.

Scoped `git diff --check` for the four production/test files also passed.
The implementer's recorded 487-suite pass and three original audit variants
were read but not rerun or presented as independent evidence here.

No production/test file was edited. No full suite, build, Cargo invocation,
agent dispatch, commit, browser run, or deployment was performed. The only
file written by this re-review is this report.
