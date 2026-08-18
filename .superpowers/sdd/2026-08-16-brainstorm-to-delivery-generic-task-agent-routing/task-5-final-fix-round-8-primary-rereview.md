# Task 5 Final Fix Round 8 Primary Re-review

## Verdict And Counts

**NOT APPROVED.** Scoped counts: **Critical 0, Important 6, Minor 0**.

The two previously retained non-blocking branch Minors remain outside this
range's scoped count: the CommonMark-invalid backtick-info fence behavior and
combined rather than isolated failed/canceled projection-locality coverage.

## Findings

### Critical

None.

### Important

1. **Delegated-producer ownership still fails in both directions for ordinary
   coordination and finite parent predicates.**

   `actionDelegatesToProducer` requires every producer action after the first
   to be a bare production verb separated from the previous action by `and`,
   with no `but` or `then`
   (`validate-contract.lib.mjs:1380-1437`). This rejects valid producer-owned
   alternatives and Oxford-comma infinitives, but a modal finite parent action
   can still inherit the producer subject. Fresh production-Skill probes gave:

   - false positive: `The parent directs the Plan Author to revise, update, and edit the Plan.`
   - false positive: `The parent asks the Plan Author to revise or update the Plan.`
   - false negative: `The parent asks the Plan Author to revise the Plan and afterward will edit the Design.`
   - false negative: `The parent asks the Plan Author to revise the Plan and will itself edit the Design.`

   The first two returned `B2D-SKILL-005`; the latter two returned no failure.
   Thus the Round-5 finite-parent class is not closed by the exact Round-6
   controls.

2. **A legal completed-Task boundary is rejected when it also bounds the next
   Task.**

   `hasPreCompletionTaskTiming` associates any nearby `before` marker with any
   nearby completion token and Task, then treats `change < marker` as an
   in-Task handoff (`validate-contract.lib.mjs:1916-1941`). It does not bind the
   completion to the Task on the same side of `before`. Both legal directives
   returned `B2D-SKILL-005`:

   - `Switch the Task Agent after the current Task completes but before the next Task starts.`
   - `After the current Task completes, switch the Task Agent before the next Task begins.`

   A switch after the completed current Task and before admission of the next
   Task is exactly the approved boundary. The earlier active/pre-completion
   rejection controls still pass, so this is a false-positive attachment gap.

3. **Codex-reviewer replacement antecedents remain incomplete.**

   `reviewTargetForBypass` carries only `it`, `them`, and the exact phrase
   `that reviewer` to the prior required reviewer
   (`validate-contract.lib.mjs:2043-2084`). With a local optional document
   reviewer, equivalent demonstratives fall back to that optional subject.
   Both forbidden replacements returned no `B2D-SKILL-005`:

   - `The Codex reviewer remains required; optional user-named Design reviewers may replace the former.`
   - `The Codex reviewer remains required; optional user-named Design reviewers may replace this reviewer.`

   This leaves the prior reviewer replacement/antecedent Important class open.

4. **Coordinated passive/direct-route actor lists are still order-, modifier-,
   and negation-sensitive.**

   `actorsAfterLink` collects all recognized actors after one `by`/`to` link
   only when the first actor begins within three tokens, while
   `relationTargetIsNegated` evaluates negation once before the link rather
   than per actor (`validate-contract.lib.mjs:1141-1148`, `:1245-1267`). Fresh
   probes showed all of these wrong classifications:

   - false positive: `High Tasks are implemented by Codex, not Grok.`
   - false positive: `Normal Tasks are reviewed by Codex, not the Task Agent.`
   - false positive: `Route high Tasks to Codex, not Grok.`
   - false negative: `High Tasks are implemented not by Codex but Grok.`
   - false negative: `Route high Tasks not to Codex but Grok.`
   - false negative: `Route high Tasks to the selected auxiliary Task Agent and Codex.`
   - false negative: `High Tasks are reviewed by the independent primary Codex reviewer and Grok.`

   The first three were rejected and the remaining four accepted. The new
   comma-list tests cover unqualified affirmative actors, but do not make the
   complete relation actor-local or preserve common omitted-link negation.

5. **Exact Task reviewer slots and cardinality remain unenforced for direct
   review routes and role-qualified counts.**

   The high-review branch of `conflictsWithDirectRoute` rejects surplus,
   duplicate, or wrongly slotted bound targets but never requires the complete
   Codex-primary plus Task-Agent-auxiliary set for an explicitly exhaustive
   direct route (`validate-contract.lib.mjs:1671-1714`). Separately,
   `explicitReviewerCount` deliberately ignores a number when a role/slot word
   occurs before `reviewer`, so the later exact-count comparison never runs
   (`validate-contract.lib.mjs:1555-1582`). These explicit contradictions all
   returned no `B2D-SKILL-005`:

   - `Route high Tasks only to Codex for review.`
   - `Route high Tasks to Codex and no other Agent for review.`
   - `High Tasks have two primary reviewers.`
   - `High Tasks have two Codex reviewers.`
   - `High Tasks have two auxiliary reviewers.`
   - `Normal Tasks have two primary reviewers.`

   The Round-8 tests prove unqualified `one/two/three reviewers` and several
   absence forms, but not the required slot cardinality once the count is
   qualified or expressed on a direct review route.

6. **A dirty future pending suffix is accepted once its generation boundary
   implementer is admitted.**

   `pendingSuffixIsClean` is consulted only by `emptyPendingBoundary`; the
   `historicalAdoptedBoundary` branch checks only the boundary Task and ignores
   later Tasks in the same generation
   (`validate-contract.lib.mjs:3246-3302`). A read-only three-Task document
   probe used:

   - Task 1 generation 1: `completed`, both required lineages completed;
   - Task 2 generation 2 boundary: `in_progress`, admitted implementer run;
   - Task 3 generation 2: `pending`, but already containing a `reserving`
     implementer run.

   `validateSimpleDocuments` returned exactly `[]`. This violates serial Task
   execution and means the complete pending suffix is protected only before
   the first boundary admission, not throughout the adopted generation. The
   existing dirty-suffix test covers only the pre-admission state.

### Minor

None newly found.

## Prior Important Disposition

- **Markerless routing:** closed for the tested compatibility boundary. The
  lower Plan parser accepts a markerless legacy Plan, while authoritative
  `validateSimpleDocuments` returns `B2D-ROUTING-001`.
- **Malformed totality:** closed for the reported null-Task case. In addition
  to the automated regression, a deterministic 20,000-case randomized nested
  malformed-input probe of routing/progress validation completed with no
  throw.
- **Operational Skill policy:** closed. The production Skill contains the full
  policy and document shapes, remains below 500 lines, and passes production
  validation.
- **Entire pending generation suffix:** the pre-admission exact case is fixed,
  but the post-admission serial suffix hole is Important finding 6.
- **Finite parent ownership, Task switch boundaries, reviewer antecedents,
  coordinated passive/direct actor lists, and exact reviewer cardinality:**
  exact prior report controls pass, but neighboring controls remain open as
  Important findings 1-5.
- **Exact structured normal/high routing, risk arithmetic, progress route
  agreement, and lineage identity:** no separate defect was found in the JSON
  validation paths during this range review.

## Verification Evidence

Reviewed `7173d031..e660f404cef1ab4d0fd552eb24df75cdad821fb2`
at exact HEAD `e660f404cef1ab4d0fd552eb24df75cdad821fb2`, including all six
commits, AGENTS.md, the approved Design and Plan, production Skill, validator
CLI/library/tests, Task 5 brief/report, and the complete available Task 5
review history. The tracked range changes only the validator library and Node
tests.

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 202 tests, 4 suites, 202 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: 0 failures, 1 check; reported Skill line count 418.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: both files use Prettier style.
- `git diff --check 7173d031..e660f404cef1ab4d0fd552eb24df75cdad821fb2`
  - PASS: exit 0, no output.
- `git diff --check`
  - PASS: exit 0, no output.
- Independent bounded Skill pressure matrix
  - 29 cases, 8 expected classifications and 21 misclassifications. Findings
    1-5 list the blocking cases; correctly classified controls included the
    prior comma/then passive-scope violations and exact legal reviewer set.
- Adopted-generation dirty-suffix document probe
  - `validateSimpleDocuments` returned exactly `[]` for the invalid state
    described in finding 6.
- Randomized malformed-input pressure
  - 20,000 nested routing/progress cases, no throws.

No Rust command was run. No production, test, Skill, Design, Plan, progress,
or existing report was edited by this review. Only this ignored report was
created.
