# Task 5 Final Fix Round 9 Primary Re-review

## Verdict And Counts

**NOT APPROVED.** Scoped counts: **Critical 0, Important 7, Minor 0**.

All 38 unique prose probes from the Round-8 primary and auxiliary reports now
classify correctly, and the post-admission dirty pending suffix is closed.
Fresh neighboring controls still expose seven ordinary-language gaps in the
same bounded contradiction classifier. Several are direct false negatives for
the generic Task Agent, exact reviewer set, or boundary-only Agent-change
invariants; the remaining false positives reject compliant document
coordination. The structured contract, routing JSON, progress agreement, and
lineage paths produced no new finding in this range.

The two retained non-blocking branch Minors remain outside this range's scoped
count: the CommonMark-invalid backtick-info fence behavior and combined rather
than isolated failed/canceled projection-locality coverage.

## Findings

### Critical

None.

### Important

1. **Routing prose still recognizes Grok as the only concrete Task Agent and
   silently ignores other supported Agent identities.**

   `directiveActors` maps only the literal `grok` and the generic phrase
   `Task Agent` to `task_agent`; every other built-in and every custom Agent is
   omitted (`validate-contract.lib.mjs:947-964`). The implementation-route
   check can reject only actors returned by that whitelist
   (`validate-contract.lib.mjs:1994-2000`). Fresh probes accepted all six of
   these contradictions without `B2D-SKILL-005`:

   ```text
   High Tasks are implemented by Gemini.
   High Tasks are implemented by Cline.
   High Tasks are implemented by Claude.
   High Tasks are implemented by custom Acme Agent.
   Always use Gemini as the implementer.
   Use Gemini as the implementer for all Tasks.
   ```

   The same active-switch parser already recognizes concrete Agent names, so
   this is specific to ownership/routing actors rather than tokenization. It
   directly undercuts Task 5's generic Task Agent goal: the validator catches a
   Grok-only high implementer but accepts the equivalent Gemini, Cline, Claude,
   or custom-Agent contract.

   Qualified generic actors can disappear as well. `actorsAfterLink` rejects
   the whole relation when the prefix contains a modifier outside a fixed
   whitelist (`validate-contract.lib.mjs:1229-1245`). Both of these invalid
   high routes were accepted:

   ```text
   Route high Tasks to the currently selected Task Agent and Codex.
   High Tasks are implemented by the user-selected Task Agent and Codex.
   ```

2. **A deferred Agent change is rejected when active state is carried from the
   prior clause and completion is expressed by pronoun.**

   `conflictsWithActiveTaskSwitch` returns `true` immediately for any carried
   active Task before inspecting completion timing in the current clause
   (`validate-contract.lib.mjs:2244-2246`). Consequently this legal boundary
   directive returns `B2D-SKILL-005`:

   ```text
   The current Task is running. Change the Task Agent after it completes.
   ```

   The illegal `Change the Task Agent now` control rejects, while spelling the
   noun again in `after the current Task completes` accepts. Pronoun choice
   must not change the approved behavior: an active-Task request is deferred
   until completion, while an immediate same-Task handoff remains forbidden.

3. **Reviewer replacement antecedents still fail open for `that role`.**

   `reviewTargetForBypass` recognizes `that reviewer`, `this reviewer`, and
   `this role`, but not the equivalent `that role`
   (`validate-contract.lib.mjs:2349-2374`). This forbidden replacement was
   accepted:

   ```text
   The Codex reviewer is mandatory. User-named Design reviewers replace that role.
   ```

   The negative control `must not replace that role` remains accepted. The
   positive sentence replaces the mandatory Codex reviewer with optional
   Design reviewers and therefore contradicts the document-role contract just
   as directly as the newly covered `this role` form.

4. **Reviewer cardinality still misses ordinary `another` and `one more`
   surplus forms.**

   `explicitReviewerCardinality` handles numeric/ordinal counts and only the
   extra markers `additional`, `extra`, and `surplus`
   (`validate-contract.lib.mjs:1734-1760`). It accepted these explicit
   contradictions:

   ```text
   Normal Tasks have another reviewer.
   Normal Tasks have one more reviewer.
   High Tasks have another reviewer.
   ```

   `High Tasks have one more reviewer` happens to reject because `one` is
   interpreted as total cardinality one, while the same wording on a normal
   Task is interpreted as its allowed total of one. The modifier is semantic:
   `one more` and `another` both add a reviewer to the required set. This keeps
   the exact normal/high reviewer-set invariant dependent on a narrow synonym
   list.

5. **Common actor alternatives are treated as additive actors, rejecting
   compliant routes.**

   Actor-local polarity handles explicit `not`, but neither `rather than` nor
   `instead of` marks the excluded actor as negative
   (`validate-contract.lib.mjs:1249-1271`, `:1369-1391`). These legal
   statements all returned `B2D-SKILL-005`:

   ```text
   High Tasks are implemented by Codex rather than Grok.
   Route high Tasks to Codex instead of Grok.
   Normal Tasks are implemented by the Task Agent rather than Codex.
   ```

   The inverse illegal controls with Grok selected over Codex still reject.
   The current behavior therefore does not merely ignore unfamiliar prose; it
   reverses the meaning of common exclusion forms and blocks compliant Skill
   wording.

6. **The finite-parent marker `afterward` rejects an implied delegated
   producer subject.**

   Every delegated production action after the first is rejected when its
   intervening tokens include a finite-parent marker, and `afterward` is such
   a marker even without a modal, repeated parent, or reflexive subject
   (`validate-contract.lib.mjs:1571-1586`). This compliant coordination was
   rejected:

   ```text
   The parent asks the Plan Author to revise the Plan and afterward update the Design.
   ```

   The explicit finite-parent control `and afterward the parent updates the
   Design` correctly rejects. In the reported sentence, however, both bare
   infinitives remain complements of `asks the Plan Author to`; the parent is
   only coordinating delegated document work.

7. **Document-target detection confuses the `Plan Author` role and people
   pronouns with Plan content.**

   `actionHasDocumentTarget` treats any nearby `plan` token as document
   content and treats `it` or `them` as a carried document whenever the prior
   clause mentioned any document target (`validate-contract.lib.mjs:1394-1414`).
   It does not exclude the `Plan Author` actor span or resolve whether `them`
   refers to people. Both legal coordination statements returned
   `B2D-SKILL-005`:

   ```text
   The parent updates the Plan Author with review findings.
   The Plan Author and Codex reviewer discuss the Plan. The parent updates them with review findings.
   ```

   The control `The parent updates the Plan directly with review findings`
   also rejects, as required, while `sends the Plan Author review findings`
   accepts only because `send` is outside the production-action set. The
   parent is allowed to communicate adjudicated findings to the independent
   producer; choosing the common verb `update` must not be classified as the
   parent editing the artifact.

### Minor

None newly found.

## Round-8 Finding Disposition

The exact union of both Round-8 reports is **38/38 correct** at this HEAD. The
statuses below judge the underlying approved invariant separately from whether
the exact prior sentence is now covered. `NOT ADDRESSED` under code quality
means a neighboring ordinary form still demonstrates the same structural
weakness.

| Prior finding | Spec compliance | Code quality | Basis |
| --- | --- | --- | --- |
| Primary 1: delegated producer vs finite parent action | ADDRESSED | NOT ADDRESSED | All four exact probes pass; Important 6 rejects an implied delegated producer solely because of `afterward`. |
| Primary 2: completed-current/pre-next boundary | NOT ADDRESSED | NOT ADDRESSED | Both exact probes pass; Important 2 rejects the same legal completion boundary when the completed Task is a carried pronoun antecedent. |
| Primary 3: reviewer replacement antecedents | NOT ADDRESSED | NOT ADDRESSED | `former` and `this reviewer` now reject; Important 3 accepts the equivalent forbidden `that role`. |
| Primary 4: coordinated actor polarity/modifiers | NOT ADDRESSED | NOT ADDRESSED | All seven exact probes pass; Important 1 accepts supported concrete/qualified Task Agents and Important 5 rejects common exclusion polarity. |
| Primary 5: exact reviewer slots/cardinality | NOT ADDRESSED | NOT ADDRESSED | All six exact contradictions reject; Important 4 still accepts ordinary surplus-reviewer wording. |
| Primary 6: post-admission dirty pending suffix | ADDRESSED | ADDRESSED | The admitted boundary now also requires every future pending Task to retain `runs: []`; dirty and clean controls both pass. |
| Auxiliary 1: role-distinct Codex high route | ADDRESSED | ADDRESSED | The exact complete route and illegal swapped/missing controls pass. |
| Auxiliary 2: cardinality/missing/extra wording | NOT ADDRESSED | NOT ADDRESSED | All seven exact probes pass; Important 4 leaves `another` and normal `one more` outside exact-set enforcement. |
| Auxiliary 3: prohibitions against incomplete review | ADDRESSED | ADDRESSED | All exact legal prohibitions accept and positive incomplete-review controls reject. |
| Auxiliary 4: cross-clause Task/document/active antecedents | ADDRESSED | NOT ADDRESSED | The four exact contradictions reject; Important 7's untyped document carry now creates false positives for producer recipients and people antecedents. |
| Auxiliary 5: named-Agent active switches | ADDRESSED | NOT ADDRESSED | Concrete named active switches reject; Important 2 shows the carried-active early return still ignores a legal completion condition. |
| Auxiliary 6: possessive/role reviewer antecedents | NOT ADDRESSED | NOT ADDRESSED | `its place` and `this role` now reject; Important 3 accepts `that role`. |

## Verification Evidence

Reviewed `e660f404cef1ab4d0fd552eb24df75cdad821fb2..2d7467ab8c578a917d5ecfbc1d496cb0f3a48abf`
at exact HEAD `2d7467ab8c578a917d5ecfbc1d496cb0f3a48abf`.
The tracked range changes only the validator library and Node tests.

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 212 tests, 4 suites, 212 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: 0 failures, 1 check; reported Skill line count 418.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: both files use Prettier style.
- `git diff --check e660f404..2d7467ab`
  - PASS: exit 0, no output.
- Independent exact union of both Round-8 reports
  - PASS: `round8-union total=38 correct=38 wrong=0`.
- Independent 25-case neighboring prose matrix
  - 13 correct and 12 misclassified. Findings 1-7 list the failures; nearby
    legal/illegal controls establish the intended polarity and boundary.
- Independent concrete/qualified Agent matrix
  - Six direct generic-Task-Agent contradictions were accepted; active named
    Agent switching still rejected, isolating the routing actor gap.
- Independent parent/document-recipient matrix
  - Both legal producer-recipient/person-antecedent updates rejected; the
    illegal direct Plan update also rejected.

No Rust command was run. No production, test, Skill, Design, Plan, progress,
or existing report was edited by this review. Only this ignored report was
created.
