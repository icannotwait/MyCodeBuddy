# Task1 store review — reply preservation

Date: 2026-09-24. Baseline: `093f5a4a`. Verdict: **request changes**.

**Spec compliance: incomplete. Quality: not ready to merge.** No Critical finding. Four Important findings remain open: two new regressions versus HEAD and two remaining acceptance gaps, supported by five failing variants. A fifth finding, R5, was fixed by concurrent store edits during final verification; its regression now passes. The original four F1–F4 repros pass, but that is narrower than the audit's acceptance contract.

## Scope and method

Reviewed only the working-tree diff for `src/stores/conversation-runtime-store.ts`, `src/stores/conversation-runtime-store.test.ts`, the new `src/stores/conversation-reply-preservation.test.ts`, and their store-local call paths. Read F1–F4 and the acceptance requirements in `docs/reviews/2026-09-24-reply-loss-audit.md`. The existing in-flight-marker fix is part of the dirty baseline and was not changed.

No separate Task1 plan document was found in the repository searches. Spec assessment uses the delegated Task1 description and audit section 5, item 1; it does not claim verification against an unavailable standalone plan.

No production or test source was edited, no subagents were started, and no views/provider/backend changes were reviewed. This requested report is the only authored file. Tests were injected through an in-memory Vite transform; baseline comparison substituted `git show 093f5a4a:src/stores/conversation-runtime-store.ts` in memory, without checking out or overwriting files.

## Important findings

### R1 — New regression: real finals are rejected when parser and client timestamps differ

**Location:** `src/stores/conversation-runtime-store.ts:1139` and `:1146`; observable through `:6040` and the hydrate predicate at `:7183`.

**Trigger:** Start with empty loaded history, send local prompt `client` at `10:00:00.000Z`, and complete a short `Checking.` reply. Persist the same prompt as parser ID `parser` at `10:00:00.010Z`, with `Checking. FINAL COMPLETE` at `10:00:01.000Z`. This is a single new round with an available send-time alignment boundary.

**Observed:** After the full 30-second hydration window and an explicit preserving refetch, the timeline is still `check / Checking.`; the final is hidden. This variant **passes on HEAD and fails on Task1**.

**Cause:** The new user check requires either matching raw IDs or an exactly matching timestamp/content key. It returns before considering any tool evidence and does not accept the existing verified alignment boundary. Hydration consequently rejects the full result. When explicit refetch installs it, alignment still lets the short local group replace that full persisted group. Different client/parser IDs and slightly different clocks are already anticipated by the store's alignment code.

**Minimal change:** Feed the existing verified same-round alignment evidence into coverage decisions, including the hydration path. Preserve strict rejection of ambiguous repeated prompts, but do not demand timestamp equality when the verified send boundary identifies the new round. Do not solve this by restoring a content-only identity check.

**Required regression:** Distinct parser/client IDs and nonidentical timestamps, with a verified boundary; assert that the final becomes visible both when persisted detail arrives before completion and when it arrives afterward.

### R2 — New regression: equal timestamps make an old repeated prompt retire a new reply

**Location:** `src/stores/conversation-runtime-store.ts:1143`, `:1518`, and `:3140`; timestamp key implementation at `:893`.

**Trigger:** Loaded history contains old prompt `repeat` and `OK, old full reply`. Send a distinct prompt ID with identical text and timestamp, complete the new reply `OK`, then refetch the **unchanged old detail**.

**Observed:** Task1 removes `live-909-new` from `localTurns`; only the old reply remains visible. The baseline preserves the new local reply and passes the same assertions.

**Cause:** Role + timestamp + prompt text is not unique for a repeated prompt. The new same-turn gate treats the collision as identity, and removing the persisted-growth guard exposes unconditional richer retirement against the old tail. The last-group shortcut does not reject a persisted group known to predate the send boundary. Assistant-only text identity has the analogous timestamp-only weakness; the executed regression here is the repeated-user case.

**Minimal change:** Use the captured boundary and unambiguous group identity to reject candidates from before the current batch. Treat timestamp equality as alignment evidence, not sufficient permission to retire a distinct new turn when the boundary contradicts it. Keep the F1 improvement: reintroducing global “detail must grow” would merely restore the original failure.

**Required regression:** Same prompt text and same timestamp, distinct IDs, unchanged old history. Assert both local retention and two distinct visible rounds. Existing repeated-prompt tests deliberately use different timestamps and cannot catch this collision.

### R3 — Remaining acceptance gap: longer text still deletes accepted thinking and tool output

**Location:** `src/stores/conversation-runtime-store.ts:1172` and `:1207`; newly reused for display at `:6040`.

**Trigger:** Complete a local reply containing `Checking.` plus either thinking `UNIQUE CONTENT` or a completed tool call whose output is `UNIQUE CONTENT`. Refetch the same user round with the longer text `Checking. FINAL COMPLETE`, but without that thinking/tool content.

**Observed:** Both executed variants lose `UNIQUE CONTENT` from the timeline. They fail on both HEAD and Task1. The reproduction uses public store actions and the normal live-to-local conversion, not direct state seeding.

**Cause:** The prefix early return succeeds before checking non-text coverage. For a matching user, `toolsMatch` is not required at all. Matching tool IDs alone would also establish identity, not preservation of tool results, images or other accepted content. The final summary fallback likewise does not prove content coverage. Task1 now invokes this incomplete predicate in the selector as well as retirement, so preserving the local array during an in-flight read does not itself guarantee visibility.

**Minimal change:** Reuse block comparison for accepted non-text content before treating a group as fully covered. If persisted final text and local non-text content are complementary, display the final while retaining the uncovered local blocks; simply refusing retirement and allowing the short local overlay to hide the final would reintroduce F1. Keep this logic shared between display selection and retirement.

**Required regression:** Text plus thinking, text plus completed tool output, and a tool-ID match with missing/different output. Assert final visibility and retention of every uncovered accepted block. This is explicitly required by F3's tools/thinking paragraph, so it is an unresolved Task1 requirement rather than an unrelated historical defect.

### R4 — Remaining acceptance gap: thinking-only assistant continuations never hydrate the final

**Location:** `src/stores/conversation-runtime-store.ts:1179`, reached from `:7183`.

**Trigger:** After older history, an internal continuation has no optimistic user prompt and streams only thinking. Complete it. Every subsequent settled transcript read contains that same reasoning record at the matching assistant timestamp and a new final answer.

**Observed:** After 30 seconds, the final is absent and detail remains the older history. This variant fails on both HEAD and Task1.

**Cause:** The assistant identity gate can succeed, but the empty-text/thinking branch still requires `usersMatch`. That is necessarily false for a local assistant-only continuation. The hydrate loop rejects the result without committing its final. Existing thinking-only tests append a user prompt; existing assistant-only continuation tests use tools, leaving this intersection uncovered.

**Minimal change:** Allow the already established, unambiguous assistant-round identity in the thinking-only branch, while still requiring actual thought coverage and a trailing final belonging to that continuation. Apply the identity safeguards from R2; do not make identical reasoning text alone sufficient identity.

**Required regression:** Assistant-only thinking with covered reasoning plus a final, and an unrelated later answer with similar reasoning that must not retire the continuation.

### R5 — Fixed during review: queued activation was treated as unchanged content

**Final status:** Concurrent edits added queue-reference and active-token comparisons to `contentAdvanced`, plus a permanent queued-activation regression. The report's queued-activation test and metadata/no-op control both pass against that update. The description below records the initially reproduced defect, not an outstanding blocker.

**Location:** `src/stores/conversation-runtime-store.ts:3067`; activating reducer at `:3673`.

**Trigger:** Queue a prompt, start manual reload, activate that same queued prompt before the read returns, then resolve the old empty detail. There is not yet a new live message.

**Observed:** The optimistic array remains reference-identical across activation, as intended by the existing reducer. The old reload then removes `NEW PROMPT` from the timeline and clears the active interaction state. This fails on HEAD and Task1. The executed failure is loss of the newly activated prompt; it does not claim an already completed reply was erased in this particular sequence.

**Cause:** Activation changes `queuedOptimisticTurnIds`, `activeTurnToken`, and `syncState` but reuses all four references checked by `contentAdvanced`. Therefore the result remains authoritative and clears the new interaction. The pre-existing in-flight-marker tests already demonstrate that queue activation can reuse the optimistic array; Task1's freshness check does not include that transition.

**Minimal change:** Include the transition to a new active turn in read freshness, preferably via the existing active token and actual queue membership transition. Avoid treating unrelated session-object changes or exact duplicate events as new content. A blanket session-reference comparison would defeat legitimate reload cleanup.

**Required regression:** Queue before read, activate during read, return old detail; assert prompt, active token and awaiting state survive. Retain the metadata-only/no-op control described below.

## What is correct in this patch

- All seven `FETCH_DETAIL_SUCCESS` dispatches capture the **pre-read** session, rather than looking up a new session at commit. The three direct loads use `session`; viewer/hydrate polling use per-attempt `cur`; delegate seed uses `initial`; delegate polling uses per-attempt `current`. No late-capture mistake was found.
- An absent session is explicitly represented by `null` in cold fetch/refetch paths, so newly created live/local/optimistic/background buffers are recognized.
- Store-local live replacement and promotion produce changed buffer references. The executed original F2/F4 cases and continuation-during-read cases confirm that these references protect newly received/completed replies.
- Reference comparison deliberately ignores session-only bookkeeping. A control test changes pending cleanup, republishes the same live object and repeats the external ID during reload; the reload still clears the pre-existing stale live object and displays the persisted final. **This control passes.**
- Scoped `dropLiveTurnIds` cleanup remains in place. The patch does not blanket-preserve pre-existing dead live buffers. No additional scope changes are proposed.
- Removing the global persisted-growth requirement and preferring covered persisted content in the selector are appropriate F1 directions. Their safety depends on fixing the identity and block-coverage problems above.

The seven captures were checked by inspection; the additional async race executions cover direct fetch/refetch/reload, not every delegate/viewer polling interleaving. Caller-side object immutability outside the store was not audited because provider work is explicitly out of scope.

## Spec and test assessment

| Requirement | Verdict |
| --- | --- |
| Original F1 cached final before short completion | Original repro passes; R1 still suppresses legitimate finals with distinct timestamps |
| Original F2 stale reload versus newly completed reply | Original repro passes; R5 queued-activation gap is fixed in the concurrent follow-up |
| F3 same-round identity before coverage | Original different-time continuation repro passes; R2 violates identity for repeated timestamps |
| F3 tools/thinking coverage | Incomplete: R3 and R4 |
| Original F4 stale ordinary refetch versus new viewer live | Original repro passes |
| Keep scoped fork cleanup and dead-live cleanup | Preserved in code; no-op/manual-reload control passes |
| Preserve the prior dirty in-flight-marker fix | Preserved; existing store suite passes |

Independent execution results:

- Current store test file: **92 passed**; new reply-preservation file: **8 passed**. They were executed together with the first in-memory review variants; the five added variants in that exploratory run failed.
- Original audit configuration: **4 passed**, command `pnpm exec vitest run --config docs/reviews/reply-loss-audit/vitest.config.ts`.
- Final six failure variants below on current Task1: **6 failed** (the original eight cases were deliberately filtered out).
- The same six variants against HEAD substituted in memory: **2 passed, 4 failed**. Timestamp skew and repeated-timestamp retirement are the two new regressions.
- Separate metadata/no-op freshness control: **1 passed**.
- Final execution of this report's own runnable appendix detected concurrent store changes: **5 failed, 2 passed, 9 original tests filtered out**. R1–R4 still fail; R5 and the no-op control pass. This is the latest review-variant result and supersedes the earlier six-failure result for the final verdict.
- Fresh ordinary-suite verification after that follow-up: `pnpm exec vitest run src/stores/conversation-runtime-store.test.ts src/stores/conversation-reply-preservation.test.ts` — **101 passed** (92 store + 9 preservation).
- No full-suite, build, lint, view/provider/backend or desktop-runtime verification is claimed. Other workers' reported counts are not substituted for these executions.

## Reproduction without editing source

Run the PowerShell block below from the repository root. It reads the test appendix in this report and injects it into the existing new test module through Vite. The existing module supplies its mocks, fixtures and cleanup. Set `baseline` to `true` to substitute the baseline store in memory. Injected stack line numbers do not correspond to physical lines in the unchanged test file.

```powershell
@'
import { readFileSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { startVitest } from "vitest/node"
const report = readFileSync("docs/reviews/reply-loss-audit/store-review.md", "utf8")
const marker = "<!-- readonly-review-tests:" + "start -->"
const extra = report.split(marker)[1].split("\x60\x60\x60typescript\n")[1].split("\n\x60\x60\x60")[0]
const baseline = false
const original = baseline
  ? execFileSync("git", ["show", "093f5a4a:src/stores/conversation-runtime-store.ts"], { encoding: "utf8" })
  : null
const context = await startVitest(
  "test",
  ["src/stores/conversation-reply-preservation.test.ts"],
  { watch: false, testNamePattern: "REVIEW" },
  { plugins: [{
    name: "readonly-store-review",
    enforce: "pre",
    transform(code, id) {
      const normalized = id.replaceAll("\\", "/")
      if (baseline && normalized.endsWith("/src/stores/conversation-runtime-store.ts")) return original
      if (normalized.endsWith("/src/stores/conversation-reply-preservation.test.ts")) return code + extra
    }
  }] }
)
await context?.close()
'@ | node --input-type=module
```

<!-- readonly-review-tests:start -->
```typescript
it("REVIEW skewed timestamps must still display a boundary-aligned final", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt = turn("client", "user", "check")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  actions().completeTurn(CID, live("short", "Checking."))
  mockGet.mockResolvedValue(detail([
    turn("parser", "user", "check", "2026-09-24T10:00:00.010Z"),
    turn("final", "assistant", "Checking. FINAL COMPLETE", "2026-09-24T10:00:01.000Z")
  ]))
  await vi.advanceTimersByTimeAsync(30_000)
  actions().refetchDetail(CID, {preserveLive:true})
  await flush()
  expect(texts().join("\n")).toContain("FINAL COMPLETE")
})
it.each(["thinking", "tool"])("REVIEW text growth cannot delete %s", async (kind) => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt = turn("p", "user", "check")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  const extra = kind === "thinking" ? {type:"thinking",text:"UNIQUE CONTENT"} : {
    type:"tool_call", info:{tool_call_id:"check",title:"bash",kind:"execute",status:"completed",content:null,
    raw_input:"{}",raw_output_chunks:["UNIQUE CONTENT"],raw_output_total_bytes:14,locations:null,meta:null,images:[]}
  }
  actions().completeTurn(CID, {...live("short","Checking."),content:[{type:"text",text:"Checking."},extra]})
  expect(JSON.stringify(getTimelineTurns(CID))).toContain("UNIQUE CONTENT")
  mockGet.mockResolvedValueOnce(detail([prompt,turn("final","assistant","Checking. FINAL COMPLETE")]))
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  expect(JSON.stringify(getTimelineTurns(CID))).toContain("UNIQUE CONTENT")
})
it("REVIEW repeated prompt sharing timestamp cannot retire against unchanged old detail", async () => {
  vi.useFakeTimers()
  const old = detail([turn("old-p","user","repeat"),turn("old-a","assistant","OK, old full reply")])
  await seed(old)
  const prompt = turn("new-p","user","repeat")
  actions().appendOptimisticTurn(CID,prompt,prompt.id)
  actions().completeTurn(CID,live("new","OK"))
  mockGet.mockResolvedValueOnce(old)
  actions().refetchDetail(CID,{preserveLive:true})
  await flush()
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns.some(entry=>entry.id==="live-909-new")).toBe(true)
  expect(texts()).toContain("OK")
})
it("REVIEW thinking-only continuation must hydrate its final", async () => {
  vi.useFakeTimers()
  const history=[turn("old-p","user","old","2026-09-24T09:00:00.000Z"),turn("old-a","assistant","old answer","2026-09-24T09:01:00.000Z")]
  await seed(detail(history))
  const thought={type:"thinking",text:"Checking the final result"}
  actions().setLiveMessage(CID,{...live("continuation",""),content:[thought]},true)
  actions().completeTurn(CID)
  mockGet.mockResolvedValue(detail([...history,{...turn("reason","assistant",""),blocks:[thought]},turn("final","assistant","FINAL COMPLETE","2026-09-24T10:01:00.000Z")]))
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n")).toContain("FINAL COMPLETE")
})
it("REVIEW queued activation during reload is a new turn even if arrays are reused", async () => {
  vi.useFakeTimers()
  const old=detail([])
  await seed(old)
  const prompt=turn("queued","user","NEW PROMPT")
  actions().appendOptimisticTurn(CID,prompt,prompt.id,{queuePending:true})
  const before=useConversationRuntimeStore.getState().byConversationId.get(CID)!
  let resolve!:(value:DbConversationDetail)=>void
  mockGet.mockReturnValueOnce(new Promise(done=>{resolve=done}))
  actions().reloadDetail(CID,{reason:"manual_reload"})
  actions().appendOptimisticTurn(CID,prompt,prompt.id)
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.optimisticTurns).toBe(before.optimisticTurns)
  resolve(old)
  await flush()
  expect(texts()).toContain("NEW PROMPT")
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.syncState).toBe("awaiting_persist")
})

it("REVIEW metadata-only and identity no-ops do not defeat manual reload", async () => {
  const old=detail([turn("p","user","old"),turn("a","assistant","persisted final")])
  await seed(old)
  const stale=live("stale","old live")
  actions().setLiveMessage(CID,stale,true)
  let resolve!:(value:DbConversationDetail)=>void
  mockGet.mockReturnValueOnce(new Promise(done=>{resolve=done}))
  actions().reloadDetail(CID,{reason:"manual_reload"})
  actions().setPendingCleanup(CID,true)
  actions().setLiveMessage(CID,stale,true)
  actions().setExternalId(CID,"audit-session")
  resolve(old)
  await flush()
  expect(useConversationRuntimeStore.getState().byConversationId.get(CID)?.liveMessage).toBe(null)
  expect(texts()).toContain("persisted final")
})
```

## Reviewed source fingerprints

SHA-256 of the initial reviewed snapshot (before concurrent R5 changes):

- `conversation-runtime-store.ts`: `33F2DC7D3D14C0044E0FB24FAA96DEF14C5FC522E0FDD0F1F7BC8D8AD34C6026`
- `conversation-runtime-store.test.ts`: `0224B3D47D02C47BDAD5975CC541916C9754BFAA3176E0F17A7131F342A3A37E`
- `conversation-reply-preservation.test.ts`: `2EB40656506F0A8F8A1723CB92175598114278BF2707FC9CBA7CF74048F50EBF`

Final verification snapshot after concurrent R5 changes:

- `conversation-runtime-store.ts`: `27BEA2ED69B8FC5E2736DFA026956027B862323CBAC19617E62873A9148EC06C`
- `conversation-runtime-store.test.ts`: unchanged from the initial fingerprint.
- `conversation-reply-preservation.test.ts`: `17AA5AE004D47E0DA167993AD62CF3EA86533CC5CA8EE7AFEBF799E4FB7E2D96`

Locations after the `contentAdvanced` block shifted by two lines in that follow-up; finding locations above describe the initial reviewed snapshot.
