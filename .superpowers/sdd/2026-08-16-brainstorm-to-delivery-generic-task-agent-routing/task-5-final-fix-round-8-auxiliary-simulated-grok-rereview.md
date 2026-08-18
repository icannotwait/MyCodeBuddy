# SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: Task 5 Final Fix Round 8 Auxiliary Re-review

This is an independent Codex simulation of the auxiliary Grok workflow test
double. It is not a real Grok verdict.

Critical: none.

Important:

1. A complete legal high-route directive is rejected when the same Codex
   identity is named separately as implementer and primary reviewer. The
   matcher assigns one review purpose to the whole route action, then treats
   the two role-distinct Codex bindings as a duplicate actor
   (`validate-contract.lib.mjs:1281`, `:1671`). This exact approved route
   returns `B2D-SKILL-005`:

   ```text
   Route high Tasks to Codex for implementation and to Codex for primary review and to Grok for auxiliary review.
   ```

   The Design explicitly requires distinct Codex implementer and Codex primary
   reviewer work units, so repeating the Agent identity for different roles is
   necessary and legal. Separate semicolon/sentence high implementation and
   review clauses, and the existing two-link reviewer orders, remain accepted.

2. Exact reviewer-set enforcement still fails open for ordinary cardinality,
   missing, and extra-reviewer wording. The count parser recognizes only
   `one`/`two`/`three` numerals and skips qualified counts; exhaustive review
   statements with no recognized actor return non-conflicting
   (`validate-contract.lib.mjs:1555`, `:1585`, `:1661`). Fresh probes accepted
   all of these contradictions:

   ```text
   High Tasks have a single reviewer.
   High Tasks have only a primary reviewer.
   High Tasks are missing an auxiliary reviewer.
   High Tasks have two primary reviewers.
   High Tasks have two Codex reviewers.
   Normal Tasks have a second reviewer.
   Normal Tasks have an extra reviewer.
   ```

   This leaves direct `single`/`sole`, qualified-count, `missing`/`lack`, and
   ordinal/extra forms outside the exact normal/high reviewer contract despite
   the new `only`/`no` tests.

3. The new explicit-absence checks run before statement negation, producing
   false positives for requirements phrased as prohibitions against incomplete
   review. `reviewerSetIsExplicitlyEmpty` and slot absence return a conflict at
   lines 1613-1628 before the negated-statement exit at line 1630. These legal
   requirements all returned `B2D-SKILL-005`:

   ```text
   Normal Tasks cannot complete without a reviewer.
   High Tasks cannot complete without both reviewers.
   High Tasks must not proceed with no auxiliary reviewer.
   ```

   Existing `Never skip` and `Do not omit` controls pass, but the broader
   negated-prohibition invariant requested by the Design is not preserved.

4. Task scope, document targets, and active-state relations are discarded at
   semicolon and sentence boundaries. `directiveWindows` splits on
   `[.!?;]` and carries only prior reviewer mentions into the next clause
   (`validate-contract.lib.mjs:820`). It does not carry a prior Task, document
   antecedent, or active Task state. Consequently all of these explicit
   contradictions were accepted:

   ```text
   High Tasks are reviewed by Codex; they are implemented by Grok.
   High Tasks are reviewed by Codex. Implementation is by Grok.
   The Plan Author writes the Plan; the parent revises it.
   The current Task is running. Change the Task Agent now.
   ```

   The corresponding conjunction and contrast Task-scope probes reject as
   required, so this is specifically a clause-boundary relation loss rather
   than an unknown route.

5. Active-switch detection requires the literal token `agent`/`agents` and
   ignores concrete discovered Agent identities (`validate-contract.lib.mjs:1944`).
   Both direct active-Task handoffs were accepted:

   ```text
   Switch from Grok to Gemini during the current Task.
   Replace Grok while the current Task is active.
   ```

   The Skill resolves and records concrete Agent identities, so naming those
   identities instead of the generic role must not bypass the active-Task
   switch prohibition.

6. Reviewer replacement still loses common possessive and role antecedents.
   `substitutionReviewTarget` requires `place of`, while the pronoun path only
   recognizes `it`, `them`, or `that reviewer`
   (`validate-contract.lib.mjs:2001`, `:2043`). These clear replacements of
   the mandatory Codex reviewer were accepted:

   ```text
   The Codex reviewer is required. User-named Design reviewers can take its place.
   The Codex reviewer is mandatory. User-named Design reviewers replace this role.
   ```

   The equivalent negative prohibition using `cannot take its place` remains
   accepted, as required.

Verification and non-findings:

- Reviewed the complete `7173d031..e660f404cef1ab4d0fd552eb24df75cdad821fb2`
  range at exact HEAD `e660f404cef1ab4d0fd552eb24df75cdad821fb2`, including
  the approved Design and Plan, production Skill, complete validator and
  tests, Task 5 report, and prior review history. The tracked range changes
  only the validator library and its Node tests.
- Coordinated passive/direct-route lists now reject extra recognized actors in
  both orders and comma/`and` forms. Legal high implementer/reviewer clauses
  and both reviewer orders remain accepted except for finding 1's single-action
  role conflation.
- Markerless lower-level Plan/progress parsing remains compatible while the
  authoritative validator returns `B2D-ROUTING-001`.
- An independent three-generation probe accepted an admitted historical
  generation plus a clean pending suffix, rejected a dirty suffix with
  `B2D-ROUTING-007`, and rejected reviewer-only historical adoption.
- Null routing Tasks, null generations, malformed risk arrays, null progress
  Tasks/runs, and malformed expected keys returned deterministic failures and
  did not throw.
- The production Skill contains the full trigger/risk/shape policy and passes
  its validator. No complete-Skill policy omission was found outside the
  contradiction-resistance findings above.
- Full Node validator suite: 202 passed, 0 failed.
- Production validator: PASS, 0 failures; Skill line count 418.
- Prettier check on both changed JavaScript files: PASS.
- `git diff --check 7173d031..e660f404cef1ab4d0fd552eb24df75cdad821fb2`
  and worktree `git diff --check`: PASS with no output.
- No Rust command was run, as required.

SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: NOT APPROVED. Critical: 0;
Important: 6; Minor: 0.
