# SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: Task 5 Final Fix Round 9 Auxiliary Re-review

This is an independent Codex simulation of the auxiliary Grok workflow test
double. It is not a real Grok verdict.

## Findings

### Critical

None.

### Important

1. **High-review passive clauses still accept a missing auxiliary or duplicate
   Codex reviewer.** `conflictsWithReviewRoute` checks explicit absence words,
   rejects more than two bound actors, and enforces the exact set only for an
   exhaustive statement or an explicitly qualified slot
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1852`,
   `:1885`, `:1918`). Actor-local negation removes a required Task Agent
   before that check. These clear contradictions all return no
   `B2D-SKILL-005`:

   ```text
   High Tasks are reviewed by Codex, not the Task Agent.
   High Tasks are reviewed by Codex and Codex.
   High Tasks are reviewed by Codex and the Task Agent is omitted.
   Normal Tasks have another reviewer.
   ```

   High Tasks require exactly one Codex primary reviewer and one selected Task
   Agent auxiliary reviewer. The Round-9 tests cover explicit `missing`/`no`
   forms and qualified slots, but not the contrast, postposed omission, or
   unqualified duplicate forms that leave the route incomplete while still
   looking like a review clause.

2. **Required Codex reviewer replacement still loses possessive-role and
   replacement antecedents.** The replacement grammar recognizes only a small
   set of `for`/`of` objects and the pronouns `former`, `it`, `them`, `that
   reviewer`, `this reviewer`, and `this role`
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2321`,
   `:2349`). It does not bind `its role`, `take over for it`, or the
   repeated-role object `the mandatory reviewer`. All of these forbidden
   substitutions are accepted:

   ```text
   The Codex reviewer remains required; optional user-named Design reviewers may replace its role.
   The Codex reviewer remains required; optional user-named Design reviewers may take over for it.
   The Codex reviewer remains required; optional user-named Design reviewers may replace the mandatory reviewer.
   The Codex reviewer is mandatory. User-named Design reviewers replace that role.
   ```

   These are direct replacements of the required Codex reviewer, not optional
   document-reviewer participation. The latest possessive controls prove
   `its place` and `this role`, but not the equally ordinary role/verb forms
   above.

3. **Task-Agent change timing remains order- and clause-sensitive.**
   `hasPreCompletionTaskTiming` searches for completion only after each
   `before`/`prior` marker, while `conflictsWithActiveTaskSwitch` returns an
   active-state failure immediately for a carried active Task and otherwise
   uses a broad exemption from `hasCompletedTaskTiming`
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2194`,
   `:2244`, `:2258`). The legal boundary is rejected:

   ```text
   Before the next Task starts, after the current Task completes, switch the Task Agent.
   The current Task is running. Change the Task Agent after it completes.
   ```

   The first directive is the approved post-current/pre-next change, and the
   second defers an active Task change until the carried Task completes.
   Conversely, this explicit active-Task switch is accepted when the active
   state is carried in a prior clause and the switch clause names the next Task:

   ```text
   The current Task is running; switch the Task Agent before the next Task starts.
   ```

   A later or separately attached boundary must not erase an active handoff,
   and the direction of `before` must not make a legal completed boundary look
   pre-completion.

4. **Cross-clause document antecedents can bypass the parent-edit ban.**
   `directiveDocumentTargets` records only the narrow target vocabulary, and
   `actionHasDocumentTarget` carries only `it`/`them` from a previous clause
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1029`,
   `:1394`). Therefore both direct parent edits are accepted:

   ```text
   The Plan Author writes the Plan; the parent revises that document.
   The Plan Author writes the Plan. The parent revises.
   ```

   The Plan/Design antecedent makes `that document` and the elliptical second
   predicate unambiguously refer to the producer-owned artifact. This leaves
   the `parent_edits: false` policy dependent on an explicit local `Plan`,
   `Design`, `it`, or `them` token and is the remaining semicolon/sentence
   document-antecedent gap.

5. **Concrete non-Grok/custom Task-Agent identities bypass generic route and
   active-Task switch checks.** `directiveActors` recognizes only the literal
   `grok` plus the generic `Task Agent` phrase for the Task-Agent role, while
   `NAMED_AGENT_TERMS` contains only built-in names
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:229`,
   `:947`). The route checker therefore accepts this high-Task contradiction:

   ```text
   High Tasks are implemented by Gemini.
   ```

   The active-switch check also returns early when no generic `agent` token or
   built-in name appears (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2239`),
   so a custom identity explicitly supported by the routing contract bypasses
   the boundary check:

   ```text
   Switch from custom:foo to custom:bar while the current Task is active.
   ```

   The same active-switch wording with `Grok`/`Gemini` is rejected by the
   Round-9 control. Custom and non-Grok Agent IDs are valid
   invocation-selected identities, so their names must not provide a way to
   change the selected generation inside an active Task or bypass the generic
   high/normal route table.

6. **Actor/purpose binding rejects common legal route alternatives.**
   `relationBindingsAfterLink` assigns one review purpose to every actor in a
   coordinated list after the link; `conflictsWithDirectRoute` then sees the
   implementation actor as a review target and rejects the list. The actor
   polarity helper likewise does not treat `rather than`/`instead of` as an
   excluded actor
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1442`,
   `:1460`, `:1935`). These contract-compliant routes return
   `B2D-SKILL-005`:

   ```text
   Route high Tasks to Codex for implementation and the primary Codex reviewer and the auxiliary Grok reviewer.
   High Tasks are implemented by Codex rather than Grok.
   ```

   Repeating `to` before every role happens to pass, but ordinary coordinated
   English does not need that repetition. Likewise, excluding Grok with
   `rather than` does not add a second implementer. The actor/purpose binding
   should separate the implementation target from the two reviewer slots and
   preserve common exclusion polarity without requiring link duplication.

7. **Passive parent delegation is rejected even though the producer owns the
   action.** `conflictsWithParentOwnership` resolves the last actor before a
   production predicate as the subject, and `actionDelegatesToProducer` only
   recognizes a producer that occurs after the parent
   (`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1535`,
   `:2013`). These legal coordinator-only forms are rejected with
   `B2D-SKILL-005`:

   ```text
   The Plan Author is asked by the parent to revise the Plan.
   The Design Fixer is directed by the parent to fix the Design.
   ```

   The passive `by the parent` relation identifies the parent as coordinator,
   while the named producer is the object of the delegation. It should not be
   treated as a parent-owned document edit.

### Minor

None newly found in this range. The two retained branch Minors (the
CommonMark-invalid backtick fence info string and combined failed/canceled
projection-locality coverage) remain outside this scoped count.

## Verification

- Reviewed `e660f404..2d7467ab` at exact HEAD `2d7467ab`, including the
  approved Design, Plan, production Skill, Task-5 brief/report, complete
  review history, and the Round-9 implementation/test diff. No tracked files
  were changed by this review.
- Full Node validator suite: `212 passed, 0 failed`.
- Production validator: `PASS`, `0 failures, 1 check`; `SKILL.md` line count
  `418`.
- Prettier check for both changed JavaScript files: passed.
- `git diff --check e660f404..2d7467ab`: passed with no output.
- Independent 13-case production-Skill pressure matrix: all 13 expected
  classifications were misclassified (ten forbidden directives were accepted
  and three compliant directives were rejected); the exact cases are listed
  above.
- No Rust command was run.

## Verdict

**SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: NOT APPROVED.** This is not a real
Grok verdict. Scoped counts: **Critical 0, Important 7, Minor 0**.
