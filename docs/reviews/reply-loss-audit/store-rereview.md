# Store R1–R5 re-review

Date: 2026-09-24. Baseline: `093f5a4a`. Scope: store fixes by child5108 and the subsequent controller reasoning-poll follow-up visible in the same working tree.

**Verdict: request changes.** Two Important findings remain: RR1 is a reply-loss interaction between R2 and R3; RR2 is duplicate final text introduced by the richer-reply merge. No Critical finding.

**Spec compliance:** incomplete because a later local reply can still be retired against the preceding round. **Quality:** the shared merge and existing alignment reuse are appropriate, but the two cases below need correction before approval.

## Scope and verification

Read `store-fix-results.md`, the latest diff of `conversation-runtime-store.ts` and its test file, the complete new reply-preservation test file, and the relevant alignment/retirement/selector/hydration code. No provider, view or backend review; no subagents; no edits to production or test source.

The implementer records 550 store tests, four audit repros, and seven original review variants passing. The controller follow-up separately records 115 focused tests passing after adding the reasoning-before-final polling regression. I did not rerun these full suites.

Independent **narrow** verification injected the original seven reviewer variants plus four new cases into the existing fixture module through Vite, entirely in memory:

- **7/7 original reviewer variants pass** against the reviewed source.
- **2/4 new variants pass:** cached complete text with uncovered thinking survives completion and later hydration without repeating the final; cached assistant-only reasoning continuation also retains one final.
- **2/4 new variants fail:** RR1 and RR2 below.
- Combined execution: **9 passed, 2 failed, 23 permanent tests filtered out**, approximately 1.66 seconds. The two new failures were also observed in an earlier run containing only the four new probes.

## R1–R5 disposition

| Original finding | Re-review |
| --- | --- |
| R1: client/parser timestamp skew hides real final | Addressed for the reviewed cases. Verified alignment can establish the same user round; before/after completion permanent cases exist, and the original reviewer variant passes. |
| R2: repeated timestamp retires a new reply against old history | Only partially addressed. Pre-boundary rejection fixes the empty-local-buffer case, but RR1 defeats it when an earlier round is intentionally retained. |
| R3: longer final deletes thinking/tool output | The original missing/different-block cases are addressed. Merge retains uncovered blocks, and retirement requires their coverage. RR2 is a new display regression in text coverage during that merge. |
| R4: assistant-only thinking never hydrates its final | Addressed for the reviewed identity cases. Reasoning must belong to a matching assistant; the controller follow-up also requires actual text after the reasoning/tool evidence, so old text cannot stop polling prematurely. Original reviewer variant and cached-before-completion control pass. |
| R5: queued activation during reload is missed | Remains addressed. Queue/active-token freshness checks are preserved; the original activation and metadata/no-op controls pass. |

No general persisted-tail-growth prerequisite was restored. The seven pre-read snapshot captures, scoped cleanup, and the previous in-flight-marker fix remain intact.

## Important RR1 / P1 — retained old blocks let the old round retire the new one

**Locations:** `src/stores/conversation-runtime-store.ts:1153` (`usersMatch`), `:1591` (`dropLastCovered`), and `:1615` (`batchStartCapture`).

**Concrete sequence:**

1. Seed empty detail. Send user `old-p`, text `repeat`, and complete `OK` with accepted reasoning `unpersisted prior reasoning`.
2. Persist `old-p / OK, old full reply`, omitting the reasoning. R3 correctly retains the first local round to preserve that reasoning. Its batch boundary remains 0.
3. Send distinct user ID `new-p` with the same text and timestamp, and complete a distinct live reply `new / OK`. Both local rounds now exist. The probe explicitly verifies `live-909-new` is in `localTurns` before the next read.
4. Refetch the unchanged first-round detail.
5. `live-909-new` is removed. The second user/reply round disappears instead of remaining separate from the first.

**Why the fix misses this:** Retaining the old group is an intended new R3 behavior. `batchStartCapture` preserves the existing batch boundary while local turns remain, so the first persisted group is no longer wholly before that boundary. The normal alignment already associates it with the first local round through `old-p`. Nevertheless, the separate last-group retirement shortcut feeds that same persisted group to the later local round. Timestamp/text equality makes `usersMatch` true without requiring the range to belong to that later local group or rejecting a range already owned by a distinct earlier round.

The initial R2 regression starts with no retained local round and captures a boundary after the old detail. It therefore cannot exercise this interaction. Dropping the first local group to refresh the boundary would lose the content R3 was added to preserve.

**Minimal change:** Respect existing alignment ownership in the last-group merge/retirement shortcut. A persisted range already associated with a different local user round cannot cover this one. Reject conflicting range ownership rather than allowing timestamp equality to override it. Keep retained old blocks and the cached-final improvement; no global “persist must grow” guard is needed.

**Acceptance:** After the sequence, both distinct prompt/reply rounds and the older uncovered reasoning remain visible. The supplied probe currently fails its post-refetch local-retention assertion.

## Important RR2 / P2 — different text segmentation duplicates the final

**Locations:** `src/stores/conversation-runtime-store.ts:1190`, `:1195`, `:1260`, and `:1275`.

**Concrete sequence:** Complete one user round with live content blocks:

```text
text:     Checking.
thinking: unpersisted reasoning
text:     FINAL COMPLETE
```

Then refetch the same user round as one persisted text block:

```text
Checking.

FINAL COMPLETE
```

The visible final occurs **twice** after the merge. The exact same final text was already accepted and is present in persistence; the only difference is that persistence includes a blank-line separator between text fragments.

**Cause:** `localText` concatenates fragments with no separator, producing `Checking.FINAL COMPLETE`. The persisted text is longer, but it does not start with that concatenation. Therefore the single `textCovered` flag is false. The uncovered-block filter only checks exact whole-block equivalence against the single persisted text block, so it treats both already-present text fragments as missing and prepends them to the full persisted answer. The final remains duplicated on repeated reads while those text blocks remain in the local overlay.

This is new behavior in `mergeRicherPersistedReply`: retaining genuinely absent reasoning is correct, but it must not replay final text solely because block boundaries differ. The existing R3 tests use a short single text block that is a direct prefix of the persisted text, so they do not cover it.

**Minimal change:** Determine text coverage across the already verified same-round text sequence, independently of whole-block segmentation. Recognize the ordered local text fragments already present in persistence and preserve only genuinely uncovered text and nontext blocks. Do not broadly normalize code/whitespace or treat out-of-order substring matches as turn identity. Keep the strict round-identity checks; this change belongs in content coverage after identity is established.

**Acceptance:** The full persisted final appears once and `unpersisted reasoning` remains visible. The probe counts `FINAL COMPLETE` occurrences and currently observes two.

## Positive cases and remaining limits

The two additional cached-before-completion probes pass with the new merge, including a missing reasoning block and an assistant-only reasoning continuation. This is direct evidence that the F1 rendering fix does not require waiting for a later persisted-tail increase in those cases.

The controller's concurrent follow-up is present in this reviewed snapshot: reasoning-only hydration now checks for text after the reply's own reasoning/tool evidence. Its permanent regression reads reasoning first and the final later. The match remains conservative when identity is ambiguous; no additional finding is raised merely because an identity-free continuation cannot be safely matched.

This re-review does not claim exhaustive parser-format coverage, actual browser rendering, or broader frontend/backend integration results.

## Reproduce without editing source

The four cases below use the existing new test file's real-store fixtures and cleanup. This PowerShell command injects them in memory. It runs only `REREVIEW` tests; expected current result is **2 failed, 2 passed**, with permanent tests excluded. Injected stack lines are virtual, not physical lines in the unchanged test file.

```powershell
@'
import { readFileSync } from "node:fs"
import { startVitest } from "vitest/node"
const report = readFileSync("docs/reviews/reply-loss-audit/store-rereview.md", "utf8")
const marker = "<!-- readonly-rereview-tests:" + "start -->"
const extra = report.split(marker)[1].split("\x60\x60\x60typescript\n")[1].split("\n\x60\x60\x60")[0]
const context = await startVitest(
  "test",
  ["src/stores/conversation-reply-preservation.test.ts"],
  { watch: false, testNamePattern: "REREVIEW" },
  { plugins: [{
    name: "readonly-store-rereview",
    enforce: "pre",
    transform(code, id) {
      if (id.replaceAll("\\", "/").endsWith("/src/stores/conversation-reply-preservation.test.ts"))
        return code + extra
    }
  }] }
)
await context?.close()
'@ | node --input-type=module
```

<!-- readonly-rereview-tests:start -->
```typescript
it("REREVIEW retained earlier overlay cannot authorize retiring a repeated later round", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const oldPrompt=turn("old-p","user","repeat")
  actions().appendOptimisticTurn(CID,oldPrompt,oldPrompt.id)
  actions().completeTurn(CID,{...live("old","OK"),content:[{type:"text",text:"OK"},{type:"thinking",text:"unpersisted prior reasoning"}]})
  const persisted=detail([oldPrompt,turn("old-a","assistant","OK, old full reply")])
  mockGet.mockResolvedValueOnce(persisted)
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns.length).toBeGreaterThan(0)
  const newPrompt=turn("new-p","user","repeat")
  actions().appendOptimisticTurn(CID,newPrompt,newPrompt.id)
  actions().completeTurn(CID,live("new","OK"))
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns.some(entry=>entry.id==="live-909-new")).toBe(true)
  mockGet.mockResolvedValueOnce(persisted)
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns.some(entry=>entry.id==="live-909-new")).toBe(true)
  expect(texts().filter(text=>text==="repeat")).toHaveLength(2)
})
it("REREVIEW segmentation differences must not duplicate final text", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt=turn("p","user","check")
  actions().appendOptimisticTurn(CID,prompt,prompt.id)
  actions().completeTurn(CID,{...live("reply",""),content:[{type:"text",text:"Checking."},{type:"thinking",text:"unpersisted reasoning"},{type:"text",text:"FINAL COMPLETE"}]})
  mockGet.mockResolvedValueOnce(detail([prompt,turn("final","assistant","Checking.\n\nFINAL COMPLETE")]))
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  expect(texts().join("\n").split("FINAL COMPLETE")).toHaveLength(2)
  expect(JSON.stringify(getTimelineTurns(CID))).toContain("unpersisted reasoning")
})
it("REREVIEW cached full before completion preserves missing thinking without duplicate final", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt=turn("p","user","check")
  actions().appendOptimisticTurn(CID,prompt,prompt.id)
  const accepted={...live("reply",""),content:[{type:"thinking",text:"unpersisted reasoning"},{type:"text",text:"Checking."}]}
  actions().setLiveMessage(CID,accepted,true)
  const full=detail([prompt,turn("final","assistant","Checking. FINAL COMPLETE")],prompt.id)
  mockGet.mockResolvedValueOnce(full)
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  actions().completeTurn(CID)
  expect(texts().join("\n").split("FINAL COMPLETE")).toHaveLength(2)
  expect(JSON.stringify(getTimelineTurns(CID))).toContain("unpersisted reasoning")
  mockGet.mockResolvedValue({...full,in_flight_user_turn_id:null})
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n").split("FINAL COMPLETE")).toHaveLength(2)
  expect(JSON.stringify(getTimelineTurns(CID))).toContain("unpersisted reasoning")
})
it("REREVIEW cached thinking-only continuation retains a single persisted final", async () => {
  vi.useFakeTimers()
  const history=[turn("old-p","user","old","2026-09-24T09:00:00.000Z"),turn("old-a","assistant","old answer","2026-09-24T09:01:00.000Z")]
  await seed(detail(history))
  const thought={type:"thinking",text:"reasoning"}
  actions().setLiveMessage(CID,{...live("cont",""),content:[thought]},true)
  const full=detail([...history,{...turn("reason","assistant",""),blocks:[thought]},turn("final","assistant","FINAL COMPLETE","2026-09-24T10:01:00.000Z")],"old-p")
  mockGet.mockResolvedValueOnce(full)
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  actions().completeTurn(CID)
  expect(texts().join("\n").split("FINAL COMPLETE")).toHaveLength(2)
  mockGet.mockResolvedValue({...full,in_flight_user_turn_id:null})
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n").split("FINAL COMPLETE")).toHaveLength(2)
})
```

## Reviewed fingerprints

SHA-256 after the narrow verification, including the controller reasoning-poll follow-up:

- `conversation-runtime-store.ts`: `6494534B62D5263AE803D064AA6D9A36936FEAC7687881DB4373A3F5C26A736B`
- `conversation-runtime-store.test.ts`: `1849EB2EB07B993B820BFFBD6A5796FFE96D61FA8AE6534A2B6BC5A7727F090F`
- `conversation-reply-preservation.test.ts`: `FFC40B03367437B13FB00BDE4131B8A2EC2E4402E62C7D23BD3A55EFAD24C194`

Only this requested report was written during re-review.

