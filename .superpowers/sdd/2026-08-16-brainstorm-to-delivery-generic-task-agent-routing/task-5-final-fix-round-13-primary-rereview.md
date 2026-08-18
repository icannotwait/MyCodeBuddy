# Task 5 Final Fix Round 13 Primary Re-review

## Scope

Reviewed the scoped fix
`e72a5f8345d238ad30ed4f7d966c18a9c868bc17..698c98bc916e40b3891c17a1515b1e7ac375f3e1`
at exact HEAD `698c98bc916e40b3891c17a1515b1e7ac375f3e1`.
The fix changes only
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
and `validate-contract.test.mjs`.

I read `AGENTS.md`, the complete Task 5 brief, the complete producer report
through Final Fix Round 13, both complete Round-12 reports, and the supplied
diff package once. Producer test results were treated as claims. I inspected
the entire fix diff for new Critical/Important breakage and ran only focused
Node classification probes for concrete doubts.

The two Round-12 reports contain no Critical finding. Their seven primary and
three auxiliary Important findings deduplicate to eight invariant groups:
the auxiliary sequential-passive finding overlaps primary finding 1, and the
auxiliary mixed plural subject/object finding overlaps primary finding 5.
The auxiliary Task-review-completion finding is distinct from the primary
carried-completion-modifier finding.

## Deduplicated Round-12 Finding Union

| Union finding | Round-12 source mapping | Verdict | HEAD evidence |
| --- | --- | --- | --- |
| 1. Sequential passive implementer ellipsis must retain every actor | Primary 1; auxiliary 1 | **ADDRESSED** | `directPassiveActorsForAction` recognizes the reported `then`, `subsequently`, and `then also` continuations through `SEQUENTIAL_PASSIVE_RELATION_FILLERS` and the bounded continuation check (`validate-contract.lib.mjs:555`, `:1669`). All three distinct reported illegal routes reject at HEAD. |
| 2. Carried completed-Task timing must admit ordinary reported modifiers | Primary 2 | **ADDRESSED** | `finally` joins the bounded completion bridges and `timingReferencesCompletion` accepts only bridge tokens between the boundary marker and completion (`validate-contract.lib.mjs:585`, `:3004`). Both reported legal carried-Task forms accept. |
| 3. Unrelated explicit take-role objects must not fall back to the required reviewer | Primary 3 | **ADDRESSED** | A parsed `take (on) the role of` object without a reviewer/pronoun target returns no required-reviewer fallback (`validate-contract.lib.mjs:3219`, `:3232`). Both reported observer/note-taker controls accept. |
| 4. Positive subordinate clauses must reset alternative exclusion | Primary 4 | **ADDRESSED** | `while`, `although`, and `whereas` are explicit alternative reset boundaries (`validate-contract.lib.mjs:548`). The two exact reported positive additions and the added `whereas` neighbor reject. |
| 5. A transitive predicate's plural document object must beat a plural people subject | Primary 5; auxiliary 3 | **ADDRESSED** | Production/list predicates prefer their plural document object after first honoring recognized people recipients (`validate-contract.lib.mjs:1339`). All three distinct reported parent-edit forms reject. |
| 6. Temporal complements of reviewer exclusivity must not exhaust the reviewer set | Primary 6 | **ADDRESSED** | Timing links are quantifier complements and the immediate-modifier path preserves that relation (`validate-contract.lib.mjs:714`, `:2503`). Both reported `exclusively after ...` routes accept. |
| 7. Missing evidence must not become postposed reviewer absence | Primary 7 | **ADDRESSED** | The tokens between the reviewer's copula and an absence term must now be bounded absence modifiers (`validate-contract.lib.mjs:2285`, `:2295`). Both reported embedded input/evidence sentences accept. |
| 8. Completion of a Task review must not authorize a Task Agent switch | Auxiliary 2 | **ADDRESSED** | The new Task-component suffix check prevents `completion of the active Task review` from counting as Task completion (`validate-contract.lib.mjs:2901`). The exact reported sentence rejects. |

The focused prior-report matrix reproduced every 17 distinct reported prose
probe at HEAD: **17/17 correct**. Thus every exact Round-12 counted finding is
repaired. This does not establish that the repair is regression-free.

## Producer Independent-review Regressions

The six independent-review regression groups described in the producer
report are all addressed for their reported cases:

1. Former-role anaphora rejects while an unrelated explicit role accepts.
2. A post-comma review with an explicit object remains after Task completion,
   while `supply is complete` does not become Task completion.
3. `now missing` and `again missing` remain reviewer absence, while embedded
   missing evidence does not.
4. `then also by Gemini` remains a sequential passive implementer.
5. `revise` and `edit` establish transitive plural document antecedents.
6. `exclusively immediately after` remains a temporal complement.

The focused matrix for these reported controls was **9/9 correct at HEAD**.
The broader changes implementing them nevertheless introduce the four new
regression groups below.

## New Fix-Diff Breakage

### Important 1: A bare object after a post-completion review is mistaken for a Task component

The new Task-component check treats `review`, `test`, `testing`, or
`validation` immediately after `Task` as a component unless the following
token is one of seven determiners/pronouns (`validate-contract.lib.mjs:606`,
`:2903`). Tokenization has already discarded the comma, so an ordinary action
with a bare plural object is indistinguishable under this heuristic:

```text
The current Task is running. After completion of the active Task, review findings and switch the Task Agent.
The current Task is running. After completion of the active Task, test results and switch the Task Agent.
```

Both switches occur after Task completion and were accepted at the fix base.
HEAD rejects both because `review findings` and `test results` are treated as
`Task review` / `Task test`. The added test covers only `review the report`,
whose determiner happens to bypass the heuristic. This is a new false positive
on valid completed-boundary instructions.

### Important 2: A possessive unrelated reviewer object falls back to the required Codex reviewer

`substitutionReviewTarget` now redirects any non-required reviewer object to
the previous required reviewer when `its`, `same`, `that`, `their`, or `this`
occurs before the object (`validate-contract.lib.mjs:3192`, `:3202`). This
mistakes an explicit possessive relationship for anaphora to the mandatory
reviewer:

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of their advisory reviewer.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of their optional advisory reviewer.
```

Both statements concern an explicitly unrelated advisory reviewer. The fix
base accepts both; HEAD rejects both as replacement of the mandatory Codex
reviewer. The new `former` control does not cover this possessive-object
regression.

### Important 3: Common multiword absence modifiers make a required reviewer disappear undetected

The postposed-absence repair requires every token between the copula and
`missing` to end in `ly` or be exactly `again`, `now`, or `still`
(`validate-contract.lib.mjs:522`, `:2295`). Common multiword adverbials fall
through:

```text
High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing.
High Tasks are reviewed by Codex and the Task Agent reviewer is for now missing.
```

Both sentences explicitly omit the mandatory high-route auxiliary reviewer.
They rejected at the fix base but are accepted at HEAD. The `now` and `again`
tests exercise only each isolated token, not their ordinary multiword forms.

### Important 4: Document-object precedence overrides a nearer people recipient not introduced by `to`/`by`

`directivePronounAntecedent` recognizes a post-predicate people recipient only
when a `to` or `by` token precedes it (`validate-contract.lib.mjs:1326`). The
new production-predicate priority then selects the earlier Plan/Design object
before considering any other people mention (`validate-contract.lib.mjs:1343`):

```text
The developers revise the Plan and Design on behalf of the reviewers. The parent updates them.
The developers edit the Plan and Design together with the reviewers. The parent updates them.
```

In both statements `them` refers to the later reviewers, so the parent's
communication is allowed. The fix base accepts both. HEAD rejects both as
parent edits to Plan/Design. The new tests cover people subjects and `to`/`by`
recipients but not other explicit people-recipient/participant relations.

## Out-of-Scope Observations

- The retained Task 2 CommonMark backtick-info-string Minor is unchanged and
  is not counted.
- The retained Task 4 combined failed/canceled route-locality coverage Minor
  is unchanged and is not counted.
- Focused probes also reproduced pre-existing vocabulary gaps at both base and
  HEAD: `afterward by Gemini` / `then later by Gemini` fail open, while
  `When fully complete` and `exclusively shortly after` fail closed. Because
  those classifications did not change in this fix diff, they are not counted
  here.
- Structured routing, risk, generation, progress, lineage, Skill prose, and
  Rust are outside this fix diff and were not reopened as a whole-branch
  review.

## Verification

- Confirmed exact HEAD
  `698c98bc916e40b3891c17a1515b1e7ac375f3e1` and supplied fix base
  `e72a5f8345d238ad30ed4f7d966c18a9c868bc17`.
- Confirmed the scoped diff contains only the two validator JavaScript files.
- Read the supplied `review-e72a5f83..698c98bc.diff` package once.
- `git diff --check e72a5f8345d238ad30ed4f7d966c18a9c868bc17..698c98bc916e40b3891c17a1515b1e7ac375f3e1`
  passed with zero diagnostics.
- Focused prior-report matrix: **17/17 distinct reported probes correct at
  HEAD**.
- Focused producer-independent matrix: **9/9 reported controls correct at
  HEAD**.
- Focused base-to-HEAD doubt matrix: **8/8 concrete probes correct at the fix
  base and wrong at HEAD**, two probes for each new Important regression group.
- Producer full-suite, production-validator, and Prettier results were not
  rerun and remain producer claims.
- No Rust command was run. No command enabled default `tauri-runtime`.
- No production, test, Skill prose, Design, Plan, progress, prior report,
  index, HEAD, or branch state was modified. Only this assigned ignored report
  was created.

## Severity Counts

Prior deduplicated union: **Critical 0 / Important 8**, all eight addressed.

New scoped breakage: **Critical 0 / Important 4 / Minor 0**.

Final counted severity: **Critical 0 / Important 4 / Minor 0**.

## Final Verdict

**NOT APPROVED**

All prior counted findings and all six reported independent-review regression
groups are addressed for their supplied cases, but the Round-13 fix diff
introduces four new Important contradiction-classifier regression groups.
