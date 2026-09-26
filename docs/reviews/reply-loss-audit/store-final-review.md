# Final scoped store review

Date: 2026-09-24. Baseline: `093f5a4a`. Reviewed the frozen store revision after the RR1/RR2 follow-up.

**Verdict: approve within the assigned store scope. No remaining concrete Critical or Important findings.** R1–R5, RR1 and RR2 are addressed by the inspected code and the targeted executions below. **Spec compliance:** satisfied for these store findings and their reviewed acceptance cases. **Quality:** the fix reuses alignment ownership and a shared merge, preserves uncovered content, and adds no dependency or separate lifecycle mechanism.

This is not approval of the entire multi-worker patch or a claim that the parent's concurrent full frontend regression/build has completed.

## What was reviewed

Read the latest RR1/RR2 section of `store-fix-results.md`, the final `mergeRicherPersistedReply` implementation, its alignment/retirement context, the six new permanent regression cases, and the controller's reasoning-only polling check. Relevant production call sites still share the merge for retirement, historical display and settled hydration.

No production/test edits, Cargo commands, full suites, subagents, commits or resets. No backend/provider/view audit. This report is the only authored file in this final review task.

## Findings closed

### RR1: conflicting alignment ownership

`src/stores/conversation-runtime-store.ts:1148` checks every existing persisted-range owner before accepting same-round content evidence. A range overlapping the candidate persisted group is rejected if its owning local group lies outside the candidate reply. Local groups wholly contained within the candidate remain eligible, allowing a candidate that contains several assistant fragments.

This closes the previous last-group shortcut: the old persisted reply cannot be reused to retire the newer repeated-prompt round when an older local overlay remains for missing reasoning. Timestamp/text equality no longer overrides the earlier round's established range ownership. The retained batch boundary and uncovered old content do not need to be discarded.

The permanent regression checks both local rounds before and after refetch/hydration, both visible prompts/replies, and the original missing reasoning. It passes, as does the original RR1 reviewer probe.

### RR2: ordered text-fragment coverage

`src/stores/conversation-runtime-store.ts:1269` scans exact local text fragments against the persisted text using a forward-only cursor. Matching consumes a distinct ordered span; an absent fragment is retained and does not advance the cursor. This runs after round identity and range ownership have been checked.

Already-persisted final fragments are therefore not prepended merely because the parser joined them with separators. The code does not normalize significant whitespace, accept reversed matches as ordered coverage, or reuse one occurrence for two repeated fragments. Nontext blocks still use the existing block-equivalence checks, and `coversLocal` is true only when no uncovered blocks remain.

The five parameterized permanent cases pass: segmented final, missing middle fragment, reversed fragment order, significant whitespace difference, and a repeated fragment with only one persisted occurrence. They also check repeated-read stability, retained thinking, and retirement only after missing thinking is persisted and no uncovered text remains. The original RR2 reviewer probe passes.

### Earlier findings and controller follow-up

| Item | Final scoped disposition |
| --- | --- |
| R1: client/parser timestamp skew | Verified boundary alignment still permits the real final; original reviewer probe passes. |
| R2: equal-timestamp repeated prompts | Pre-boundary rejection remains, and RR1 now closes the retained-old-round interaction. Both reviewer cases pass. |
| R3: missing thinking/tool output | Uncovered blocks remain alongside persisted final text; original thinking/tool probes and new missing-text/thinking controls pass. |
| R4: assistant-only reasoning continuation | Matching assistant/reasoning evidence remains required; original reviewer probe and cached-before-completion control pass. |
| R5: queued activation during reload | Queue/active-token freshness remains; original activation and metadata/no-op controls pass. |
| Cached full reply before completion | Both prior re-review controls pass: complete text with missing thinking, and an assistant-only reasoning continuation. Each asserts one visible final. |
| Reasoning first, final persisted later | The controller's `lastEvidence` check at `conversation-runtime-store.ts:1234` still requires text after the current reasoning/tool evidence. Its permanent polling regression passes. |

The previous in-flight-marker fix and pre-read snapshot handling remain in the reviewed implementation. No global persisted-tail-growth prerequisite was restored.

## Independent targeted verification

Executed one selected Vitest run against `src/stores/conversation-reply-preservation.test.ts`:

- Seven original reviewer probes from `store-review.md`, injected in memory.
- Four previous re-review probes from `store-rereview.md`, injected in memory.
- Six new permanent RR1/RR2 cases.
- The controller's permanent reasoning-before-final polling case.

**Result: 18 passed, 22 unrelated permanent cases filtered out; exit 0.** The registered test count was 40 (29 permanent plus 11 injected). No source files were changed for injection.

An initial reviewer harness attempt omitted a newline between the two appended snippets and failed transformation before running any tests. The in-memory harness separator was corrected; the successful result above is the completed verification. That setup failure was not a product/test-source failure.

For reproducibility, the selection pattern was:

```text
REVIEW|retained reasoning in an earlier round|merges ordered text coverage|keeps polling a thinking-only continuation
```

The injected snippets are unchanged from the two previous reports and were joined with a newline using a Vite `enforce: "pre"` transform. The existing test module supplies its real-store fixtures, API mocks and cleanup.

The implementer separately reports **561 store tests, four original audit repros, 11 reviewer probes, and changed-file lint passing**. Those broader results were read from `store-fix-results.md`, not rerun or independently claimed here. Full frontend regression/build remains with the parent task.

## Remaining limits

Unpersisted blocks intentionally keep a local overlay. Changed text or significant whitespace may also keep a local fragment because coverage uses exact characters, not semantic equivalence. Ambiguous assistant-only identity remains conservative. These are explicit preservation choices, not additional findings in this scoped review.

No claim is made about browser rendering, durable recovery after process loss, every parser representation, or other workers' changes. No further source change is requested for RR1/RR2.

## Reviewed fingerprints

SHA-256 of the frozen files used for verification:

- `src/stores/conversation-runtime-store.ts`: `E3FB5309DEE5FEE6539E9EC013BF00E6593276C1D4E6AAA6EE43C373E047FBC6`
- `src/stores/conversation-runtime-store.test.ts`: `1849EB2EB07B993B820BFFBD6A5796FFE96D61FA8AE6534A2B6BC5A7727F090F`
- `src/stores/conversation-reply-preservation.test.ts`: `C0AC348B627BB656816EFC7AC19D10F7E14D73A54CF4CE53D371CFCF2C11DAB6`
