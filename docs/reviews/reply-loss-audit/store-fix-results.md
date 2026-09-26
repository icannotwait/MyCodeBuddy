# Task 1 store review fixes — R1–R4

Date: 2026-09-24. Scope: runtime store, reply-preservation regressions, one obsolete runtime-test expectation, and this report. No commits or nested delegation. Provider, views, and backend remain owned by their workers.

## Investigation and red evidence

Read the complete store review, including its executable appendix; approved audit F1–F4; and the implementation plan. Traced alignment, completion promotion, retirement, historical selection, and settled hydration. The initial dirty in-flight-marker fix, all read snapshots, and the queued-activation R5 fix were retained.

Permanent regressions initially produced **8 failures / 13 passes** in `conversation-reply-preservation.test.ts`:

- Parser/client timestamp skew with final persistence both before and after completion.
- Identical prompt text and timestamps across two different rounds, with unchanged old detail.
- A richer final omitting thinking, all tool content, a matching tool's output, or replacing that output with different content.
- Assistant-only reasoning followed by matching persisted reasoning and a final.

Negative reasoning cases and the metadata/no-op reload control already passed. An additional adversarial test then reproduced a separate ambiguity: old identical reasoning plus a final whose timestamp matched the continuation falsely passed identity. That case failed before the follow-up identity fix and now passes.

## Implementation

- **R1:** Shared reconciliation accepts the existing `alignTurnGroups` result when its batch boundary is verified. Client/parser IDs and timestamps may differ without hiding the final.
- **R2:** Both alignment and the final-group reconciliation shortcut reject persisted ranges wholly preceding the verified boundary. The test asserts two separate visible rounds as well as local retention. No generic detail-tail-growth prerequisite was restored.
- **R3:** `mergeRicherPersistedReply` serves display, hydration, and retirement. It uses existing block-equivalence helpers, retains uncovered accepted blocks alongside the richer final, and permits retirement only once those blocks are covered. Distinct same-round summary text also retains uncovered local text. The tests separately assert final visibility, nontext retention, and eventual retirement after actual coverage.
- **R4:** Assistant-only thinking requires an unambiguous matching assistant record containing the accepted reasoning; a timestamp match on the final alone is insufficient. Unrelated timestamps, unrelated prompts, different reasoning, and old reasoning with a timestamp-matching final do not hydrate or retire the continuation.
- **R5 and existing protections:** Read snapshot reference checks, queued IDs, active token, in-flight marker handling, cancel fences, and public APIs remain intact.

The existing tool-hydration test expected the local tool result to disappear when the parser returned text only. That expectation contradicted accepted-content preservation. It now expects the local copy to remain, asserts the tool result in the timeline, and independently asserts the complete final text in the timeline; its persisted-detail assertion remains.

## Verification

- First complete store run: `pnpm test src/stores` — **31 files, 549 tests passed**.
- After adding the adversarial reasoning case: focused runtime/preservation run — **114 passed** (92 runtime + 22 preservation).
- Original audit: `pnpm exec vitest run --config docs/reviews/reply-loss-audit/vitest.config.ts` — **4 passed**.
- Review appendix executed via the report's in-memory transform — **7 passed**; permanent tests excluded by its `REVIEW` filter.
- Changed-file ESLint — exit 0. Prettier run only on the three owned TypeScript files.

Final verification after the last reasoning change: **31 store files / 550 tests passed; all 4 original audit repros passed**. Lint caught six formatting errors in the follow-up patch; those were corrected, and the final changed-file lint rerun and `git diff --check` passed.

## Remaining concerns and limits

### Controller integration follow-up

- Added a real-store regression where an assistant-only reasoning continuation is first read with reasoning only, then with its final. It failed (old text was counted as the continuation's final and stopped polling), then passed after requiring text following the current reply's reasoning/tool evidence. Focused store/preservation validation: 115 passed (92 + 23). No provider/backend changes in this follow-up.

- If the parser never persists an accepted block, the local copy deliberately remains. No retention timeout or new persistence layer was added.
- Ambiguous assistant-only identity is conservative: content equality alone cannot authorize replacement. Missing identity evidence may delay adopting a final.
- This work validates the real store and timeline behavior. Browser rendering, provider/view/backend integration, full frontend tests, build, and Cargo checks belong to controller integration and were not run here.
- Store-suite stderr includes existing intentional failure-path logs and unrelated tab-store mock warnings; the tests themselves pass. The Vite CJS deprecation warning remains.

## RR1 / RR2 follow-up — 2026-09-24

Read `store-rereview.md` in full, including all four runnable probes. This follow-up changes only `conversation-runtime-store.ts`, `conversation-reply-preservation.test.ts`, and this report; the existing runtime-test corrections and controller changes remain intact.

### Red → green evidence

- Added six permanent, strongly typed real-store cases. The first run of `pnpm test src/stores/conversation-reply-preservation.test.ts` produced **6 failed / 23 passed**. RR1 removed the newer prompt/reply despite an older retained overlay; RR2 and its four coverage controls duplicated already-persisted text.
- RR1 now checks existing alignment ownership inside the shared merge. An overlapping persisted range owned by a local group outside the candidate reply cannot cover that reply. The regression asserts both complete local rounds before and after refetch and hydration, both visible prompts/replies, and the older uncovered reasoning.
- RR2 now scans local text fragments against persisted text in order, advancing a character cursor only on an exact match. Already-covered text is omitted from the complementary merge while thinking and genuinely uncovered text remain. This runs after the existing identity checks. No whitespace normalization, unordered text matching, dependency, or generic detail-growth guard was introduced.
- Coverage controls exercise a missing middle fragment, reversed fragment order, significant whitespace changes, and a repeated fragment with only one persisted occurrence. Repeated reads stay stable; adding the missing thinking retires the overlay only when all text is also covered.
- Narrow green run: `pnpm test src/stores/conversation-reply-preservation.test.ts src/stores/conversation-runtime-store.test.ts` — **121 passed** (29 preservation + 92 runtime). The controller's 23rd regression and its requirement for text after reasoning/tool evidence remain unchanged and pass.

### Final validation

- `pnpm test src/stores` — **31 files / 561 tests passed**, exit 0. This shared-workspace total includes the other worker's current projection tests; this worker did not edit them.
- `pnpm exec vitest run --config docs/reviews/reply-loss-audit/vitest.config.ts` — **4 original audit repros passed**, exit 0.
- Original review and rereview appendices injected together through the documented in-memory Vite transform — **11 passed / 29 permanent tests filtered out**, exit 0: all seven original probes and all four rereview probes.
- `pnpm lint src/stores/conversation-runtime-store.ts src/stores/conversation-reply-preservation.test.ts src/stores/conversation-runtime-store.test.ts` — exit 0. Prettier formatted only the two TypeScript files changed in this follow-up.

### Remaining limits

Neither RR1 nor RR2 remains reproducible in the permanent regressions or independent reviewer probes. Ordered coverage preserves character-level differences rather than inferring semantic equivalence, so rewritten text or changed whitespace may intentionally retain a local fragment. Unpersisted nontext content still retains its local overlay. Browser rendering and broader provider/projection/backend integration remain outside this store-only validation. No commits, resets, subagents, Cargo commands, build, or full frontend suite were run.
