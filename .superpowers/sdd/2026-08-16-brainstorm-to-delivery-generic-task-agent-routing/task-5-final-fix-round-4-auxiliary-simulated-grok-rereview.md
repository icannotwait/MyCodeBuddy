SIMULATED GROK WORKFLOW TEST DOUBLE ONLY:

# SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: Task 5 Final Fix Round 4 Auxiliary Scoped Re-review

This is a simulated workflow test double, not a real Grok model verdict.

## Scope

Reviewed `7173d031..20f19035` in the assigned worktree. The range contains
the clause-local directive parser refactor and the reviewer-attachment fix.
This review did not edit production files, tests, Design, Plan, progress, or
other reports. The only new file is this ignored review report.

## Findings

### Critical

None.

### Important

1. **The ownership/routing grammar still accepts clear inverse and
   co-ownership contradictions.** `conflictsWithTaskAgentRoute` only checks
   `production` actions in its actor-first branch
   ([`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1099)),
   so active direct route verbs are ignored. `activeActorForAction` also picks
   only the last actor before a predicate
   ([`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:978)),
   which hides an earlier Task Agent in a coordinated subject. The checker
   does not validate the inverse normal route or the normal reviewer slot at
   all. These isolated probes all returned `ACCEPT` rather than
   `B2D-SKILL-005`:

   ```text
   The Task Agent routes high Tasks to itself.
   The Task Agent and Codex implement high Tasks.
   Codex implements normal Tasks.
   The Task Agent reviews normal Tasks.
   ```

   Each sentence contradicts the exact normal/high route contract: high
   implementation belongs to Codex, normal implementation belongs to the
   selected Task Agent, normal review has only the Codex primary slot, and a
   high Task has one implementer rather than co-ownership. Bind every actor
   and action, including coordinated subjects and direct route actions, to the
   normal/high route table before accepting the Skill.

2. **Valid parent orchestration is rejected for direct delegation verbs.**
   `actionDelegatesToProducer` recognizes only `ORCHESTRATION_ACTIONS`
   ([`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:332))
   even though `delegate*` and `route*` are separate direct-action tokens.
   Consequently these coordinator-only directives return `B2D-SKILL-005`:

   ```text
   The parent delegates the Plan Author to write the Plan.
   The parent routes the Design Fixer to fix the Design.
   ```

   The parent is explicitly allowed to coordinate independent document
   producers; these sentences do not assign document ownership to the parent.
   Include direct delegation/route verbs in the producer-delegation relation
   while retaining the existing coordinated-subject rejection controls.

3. **Review requirement polarity and replacement vocabulary remain
   incomplete.** `REVIEW_BYPASS_ACTIONS` omits negated requirement predicates
   such as `not required`/`not mandatory`, and `reviewBypassIsNegated` only
   recognizes the fixed `NEGATION_TERMS` set
   ([`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:368),
   [`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1242)).
   It therefore accepts these forbidden bypasses:

   ```text
   Primary review is not required.
   No primary review is required.
   The Codex reviewer is not required.
   ```

   Conversely, `avoid`/`refuse` are not treated as negation, so this compliant
   prohibition is rejected:

   ```text
   Avoid skipping primary review.
   ```

   Finally, `in place of` is not a recognized replacement action, so this
   forbidden reviewer substitution is accepted:

   ```text
   Use optional user-named Design reviewers in place of the Codex reviewer.
   ```

   Attach required/optional predicates and common replacement relations to
   the same reviewer subject model used by the current four exact controls;
   do not rely on the current narrow token list.

4. **A completed-boundary exemption can still erase an explicit active-Task
   switch.** `conflictsWithActiveTaskSwitch` returns early when any completed
   timing is found ([`validate-contract.lib.mjs`](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1192)).
   This accepts the contradictory directive below:

   ```text
   Switch the Task Agent after the current Task completes, but the current Task is active.
   ```

   The simpler required boundary forms (`after completion`, `once ...
   finishes`, and `following completion`) classify as accepted, while the
   requested `while`, `during`, and `inside` active forms classify as rejected.
   The later completed clause must not suppress a separate active-state
   predicate in the same coordinated directive.

## Verification

- Focused reviewer-attachment suite:
  `node --test --test-name-pattern='round-3 reviewer attachment' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - `tests 4`, `pass 4`, `fail 0`, exit 0.
- Full Node suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - `tests 82`, `pass 82`, `fail 0`, exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - `PASS: brainstorm-to-delivery Simple contract`; `0 failures, 1 checks completed`; exit 0.
- Prettier check for both changed JavaScript files passed.
- `git diff --check 7173d031..20f19035` passed.
- Read-only pressure probes reproduced all four Important findings above.
- Boundary probes accepted `after completion`, `once ... finishes`, and
  `following completion`, and rejected the `while`, `during`, and `inside`
  active forms.
- No Rust command was run.

## Verdict

**SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: NOT APPROVED.** Critical: 0;
Important: 4; Minor: 0 newly introduced.

The exact 82-test suite and production fixture pass, and the four reviewer
attachment regressions are fixed. The broader pressure probes still show
route/ownership false negatives, parent-orchestration false positives,
review polarity/replacement gaps, and a completion-boundary attachment gap.
