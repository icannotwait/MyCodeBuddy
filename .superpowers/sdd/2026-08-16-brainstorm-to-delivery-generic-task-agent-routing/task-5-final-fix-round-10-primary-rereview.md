# Task 5 Final Fix Round 10 Primary Re-review

## Finding Verdicts

### 1. Concrete and qualified generic Task Agent routing: NOT ADDRESSED

The eight exact Round-9 probes now classify correctly. Concrete built-ins and
custom names are normalized by `directiveActors`
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:982`),
and `current`, `currently`, and `user-selected` were added to the relation
modifier whitelist (`validate-contract.lib.mjs:468`).

The underlying qualified-actor gap remains. `actorsAfterLink` still discards
the entire actor relation when an otherwise ordinary modifier is outside that
finite whitelist (`validate-contract.lib.mjs:1317`). These direct
contradictions are accepted without `B2D-SKILL-005`:

```text
High Tasks are implemented by the chosen Task Agent.
High Tasks are implemented by the resolved Task Agent.
High Tasks are implemented by the invocation-selected Task Agent.
```

The legal normal-route control with the same `chosen Task Agent` qualifier is
accepted. This remains a spec false negative for generic invocation-selected
Task Agents and a code-quality failure in the same prefix-binding mechanism.

### 2. Carried active Task with completion timing: NOT ADDRESSED

The exact legal `after it completes` probe and its immediate-switch control
now classify correctly. `timingReferencesCompletion` can bind a current-clause
Task or a prior Task referenced specifically by `it`/`its`
(`validate-contract.lib.mjs:2473`), and the carried active state is consulted
after completion timing (`validate-contract.lib.mjs:2543`).

The typed antecedent remains dependent on two literal pronouns. Both legal
boundary directives still return `B2D-SKILL-005`:

```text
The current Task is running. Change the Task Agent after this completes.
The current Task is running. When complete, change the Task Agent.
```

The current implementation reaches the carried-active rejection at
`validate-contract.lib.mjs:2548` because neither completion form satisfies the
literal `it`/`its` check at `validate-contract.lib.mjs:2488`.

### 3. Reviewer replacement antecedents for `that role`: NOT ADDRESSED

The exact positive and negated `that role` probes now classify correctly; the
new literal branch is at `validate-contract.lib.mjs:2665`.

The same replacement antecedent still fails open for equally direct noun
phrases:

```text
The Codex reviewer is mandatory. User-named Design reviewers replace the role.
The Codex reviewer is mandatory. User-named Design reviewers replace the same role.
```

`reviewTargetForBypass` enumerates selected pronoun phrases
(`validate-contract.lib.mjs:2639`) and then prefers the local optional Design
reviewer at `validate-contract.lib.mjs:2684`; it never reaches the prior
mandatory Codex reviewer. Both contradictions are accepted, so the required
reviewer replacement invariant remains phrase-list dependent.

### 4. Surplus reviewer cardinality: NOT ADDRESSED

The four exact `another`/`one more` contradictions now reject. The added extra
markers are at `validate-contract.lib.mjs:2011`.

The same exact-set invariant still accepts ordinary surplus wording:

```text
High Tasks have two more reviewers.
High Tasks have a further reviewer.
```

`two more` bypasses the special-case `one more` check and is interpreted as a
total count of two, which matches the required high-route total at
`validate-contract.lib.mjs:2063`. `further` is not recognized at all. Negated
and `two additional reviewers` controls classify correctly, isolating the
surplus-modifier gap.

### 5. Actor alternatives using `rather than` / `instead of`: NOT ADDRESSED

All three exact legal alternatives and the inverse illegal control classify
correctly. The second actor is marked excluded when the alternative phrase
falls between it and the immediately preceding actor
(`validate-contract.lib.mjs:1345`).

Exclusion does not propagate through an ordinary coordinated alternative:

```text
High Tasks are implemented by Codex rather than Grok or Gemini.
```

The parser excludes Grok but restores Gemini as a positive implementer because
the `rather than` tokens are not between Grok and Gemini. The legal Codex-only
route therefore returns `B2D-SKILL-005` through the multiple/wrong implementer
checks at `validate-contract.lib.mjs:2271`. The polarity binding remains
structurally incomplete.

### 6. Delegated producer predicates across `afterward`: ADDRESSED

The exact compliant delegation and explicit finite-parent control both
classify correctly. `afterward` is no longer itself a finite-parent marker
(`validate-contract.lib.mjs:395`), while subsequent producer actions must stay
bare, coordinated, and free of a new actor or finite-parent marker
(`validate-contract.lib.mjs:1744`). Parent ownership uses that result at
`validate-contract.lib.mjs:2325`.

An independent six-case neighboring matrix also classified correctly:
delegated `later update`, `afterward also update`, and `afterward patch` were
accepted; inflected parent `later updates`, modal parent `afterward will
update`, and reflexive parent `afterward itself patches` were rejected.

### 7. Document targets versus role recipients and people pronouns: NOT ADDRESSED

The two exact legal coordination probes and the direct Plan-edit control now
classify correctly. Actor spans are removed from document targets at
`validate-contract.lib.mjs:1101`, and `them` is treated as a document only when
no prior recognized actor or reviewer exists (`validate-contract.lib.mjs:1535`).

That heuristic still misclassifies both directions:

```text
The developers discuss the Plan. The parent updates them with review findings.
The Plan Author lists the Plan and Design. The parent updates them.
```

The first legal coordination is rejected because `developers` is not a
recognized actor. The second direct parent edit is accepted because the mere
presence of `Plan Author` suppresses the explicit plural document antecedents.
The latter is also a base-to-HEAD regression. This violates the parent
coordination-only contract and does not provide defensible typed antecedent
resolution.

## New Breakage In The Fix Diff

### Important: bare `Codex Agent` is forced into the selected Task Agent role

`agentActorEnd` extends any concrete name followed by `Agent`, and the Codex
branch maps every extended span to `task_agent`
(`validate-contract.lib.mjs:975`, `validate-contract.lib.mjs:990`). This
rejects all of these compliant statements at HEAD although the fix base
accepted them:

```text
High Tasks are implemented by the Codex Agent.
Normal Tasks are reviewed by the Codex Agent.
High Tasks are reviewed by the primary Codex Agent and the auxiliary Grok Agent.
```

Only explicit `Codex Task Agent` should select the Task Agent role. Bare Codex
is the required high implementer and primary reviewer. The false positives
reach the high-production and normal-review checks at
`validate-contract.lib.mjs:2367` and `validate-contract.lib.mjs:2380`.

### Important: generic `take*` tokens reject ordinary reviewer actions

Round 10 adds every `take*` form to both review-bypass and replacement action
sets (`validate-contract.lib.mjs:547`, `validate-contract.lib.mjs:566`) without
requiring `over`, `the place of`, or another replacement construction. The
fallback then attaches the verb to the preceding required reviewer
(`validate-contract.lib.mjs:2684`). These compliant statements changed from
accepted at the fix base to rejected at HEAD:

```text
The primary Codex reviewer takes notes.
The Codex reviewer is mandatory. It takes notes for the Plan Author.
```

This is a newly introduced false-positive class, not a residual synonym gap.

### Important worsening counted with Finding 7

The plural-artifact parent-edit false negative shown in Finding 7 changed from
rejected at `2d7467ab` to accepted at `943bfc29`. It is new fix-range breakage,
but is counted once under the unresolved document-target finding rather than
as a duplicate finding.

No new Critical or Minor breakage was found in the scoped diff.

## Out-of-Scope Observations

- The retained CommonMark backtick-info-string Minor is unchanged and is not
  counted in this fix-range review.
- The retained combined rather than isolated failed/canceled route-locality
  coverage Minor is unchanged and is not counted in this fix-range review.
- The diff changes only the validator library and Node tests. Structured
  contract, routing, progress, and lineage behavior was not changed in this
  range and the existing structured tests remain green.
- No Rust command was run, as required.

## Verification Commands And Results

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 226 tests, 4 suites, 226 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: 0 failures, 1 check; Skill line count 418.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: both files use Prettier style.
- `git diff --check 2d7467ab8c578a917d5ecfbc1d496cb0f3a48abf..943bfc291e7fa30d49c94b845e3528ba415a85a3`
  - PASS: exit 0, no output.
- Independent prior exact/control matrix
  - PASS: 25/25 classifications correct across all seven prior findings.
- Independent neighboring matrix
  - FAIL: 0/12 classifications correct; Findings 1-5 and 7 list the scoped
    misclassifications and their controls.
- Independent delegated-producer neighboring matrix
  - PASS: 6/6 classifications correct.
- Independent base-to-HEAD regression matrix
  - Six classification changes confirmed: three bare-Codex-Agent false
    positives, two `takes notes` false positives, and one plural-artifact
    parent-edit false negative.

## Counts

**Critical 0 / Important 8 / Minor 0**

The count consists of six unresolved prior Important invariant classes plus
two distinct newly introduced Important false-positive classes. The new
plural-artifact regression is counted once within prior Finding 7.

## Final Approval

**NOT APPROVED**
