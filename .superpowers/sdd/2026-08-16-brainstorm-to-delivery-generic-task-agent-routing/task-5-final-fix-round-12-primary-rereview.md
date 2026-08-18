# Task 5 Final Fix Round 12 Primary Re-review

## Scope

Reviewed the scoped fix
`94ba94f92b914b1dec4b5eb7833146bea28d1c33..e72a5f8345d238ad30ed4f7d966c18a9c868bc17`
at exact HEAD `e72a5f8345d238ad30ed4f7d966c18a9c868bc17`.
The fix changes only
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
and `validate-contract.test.mjs`. I read the complete Task brief, both complete
Round-11 reports, the complete producer report including Final Fix Round 12,
and the supplied diff package once. I inspected only the fix diff for new
Critical/Important breakage.

The two Round-11 reports contain no Critical finding. Their counted Important
findings deduplicate to eight relation classes: passive actor ownership,
Task-completion timing, reviewer replacement, reviewer surplus, alternative
polarity, plural pronoun antecedents, reviewer-set exclusivity, and postposed
reviewer absence.

## Deduplicated Round-11 Finding Union

| Union finding | Round-11 source mapping | Verdict | HEAD evidence |
| --- | --- | --- | --- |
| 1. Passive actors attach only to their governing production relation | Primary new actor-prefix finding; auxiliary scoped #7 | **ADDRESSED** | Nested actor links and relation boundaries now limit `actorsAfterLink` at `validate-contract.lib.mjs:1502` and `:1518`. Both reported approval/instruction-source controls independently accept. |
| 2. Completion must belong to the Task whose boundary permits an Agent change | Primary new completion finding; auxiliary scoped #2 | **ADDRESSED** | `completionBelongsToTask` binds explicit Task noun phrases at `validate-contract.lib.mjs:2795`; `timingReferencesCompletion` uses it and narrows carried implicit completion at `:2895`. All reported component-completion forms reject and the reported preposed active-Task completion accepts. |
| 3. Reviewer replacement must bind the construction and its reviewer object | Primary new replacement finding; auxiliary scoped #5 | **ADDRESSED** | `takeRoleObjectLink` recognizes `take (on) the role of` at `validate-contract.lib.mjs:3007`; substitution targeting and qualified-role resolution are at `:3046` and `:3106`. Both reported replacements reject, while notes-over-time and advisory-role controls accept. |
| 4. Qualified same-slot reviewers remain surplus | Primary new surplus finding; auxiliary scoped #6 duplicate-primary half | **ADDRESSED** | `markerIntroducesComplementarySlot` permits an `another` marker only when it supplies the other explicit slot at `validate-contract.lib.mjs:2316`; the reported high/normal duplicate-primary assertions reject. |
| 5. Alternative polarity covers repeated predicates and stops at positive contrast | Primary new alternative finding; auxiliary scoped #3 | **ADDRESSED** | Actor and action alternative polarity is evaluated at `validate-contract.lib.mjs:1545` and `:1557`; the two reported post-`but` Gemini routes reject and the repeated-predicate Codex-over-Grok route accepts. |
| 6. Plural people and artifact pronouns resolve by grammatical role | Primary new explicit-recipient finding; auxiliary scoped #4 | **ADDRESSED** | `directivePronounAntecedent` separates document objects, people subjects, and people recipients at `validate-contract.lib.mjs:1257`; both reported people-antecedent forms accept. |
| 7. Reviewer exclusivity is bound to the reviewer relation | Primary new scoped-`solely` finding; auxiliary scoped #1 | **ADDRESSED** | Exhaustive quantifiers are evaluated against the review action and actor relation at `validate-contract.lib.mjs:2418`; `exclusively by Codex` rejects and `solely about correctness` accepts. |
| 8. Postposed reviewer absence is bound through the reviewer predicate | Primary new modifier finding; auxiliary scoped #6 absence half | **ADDRESSED** | `reviewerRoleIsExplicitlyAbsent` now finds a subject link and rejects intervening relation boundaries/actors at `validate-contract.lib.mjs:2183`; both reported adverb-qualified omissions reject. |

Independent classification reproduced the complete 23-sentence deduplicated
report union: **23/23 correct at HEAD**. This establishes that the exact
Round-11 counted findings are repaired; it does not establish that the fix is
regression-free.

## Numbered Finding Ledger

### Round-11 Primary Numbered Findings

1. Concrete and qualified generic Task Agent routing: **ADDRESSED**.
   Qualified Task Agent spans remain recognized by `directiveActors` and the
   bounded prefix logic (`validate-contract.lib.mjs:1087`, `:1502`).
2. Carried active Task with completion timing: **ADDRESSED** for the reported
   `after this completes` / `When complete` forms through
   `timingReferencesCompletion` (`validate-contract.lib.mjs:2895`).
3. Reviewer replacement antecedents for `the role` / `same role`:
   **ADDRESSED** by the qualified role-reference path
   (`validate-contract.lib.mjs:3117`).
4. Surplus reviewer cardinality: **ADDRESSED** by the extra-marker and
   complementary-slot checks (`validate-contract.lib.mjs:2292`).
5. Actor alternatives using `rather than` / `instead of`: **ADDRESSED** for
   the reported coordinated/repeated forms (`validate-contract.lib.mjs:1537`,
   `:1557`).
6. Delegated producer predicates across `afterward`: **ADDRESSED**;
   producer infinitives remain attached by `actionDelegatesToProducer`
   (`validate-contract.lib.mjs:1961`).
7. Document targets versus role recipients and people pronouns:
   **ADDRESSED** for the reported controls by typed antecedent resolution
   (`validate-contract.lib.mjs:1257`).
8. Bare `Codex Agent` forced into the Task Agent role: **ADDRESSED**; only the
   explicit `Codex Task Agent` span maps to `task_agent`
   (`validate-contract.lib.mjs:1095`).
9. Generic `take*` tokens reject ordinary reviewer actions: **ADDRESSED** for
   the reported notes/take-over controls by `reviewActionIsReplacement`
   (`validate-contract.lib.mjs:3020`).

### Round-11 Auxiliary Previous-Finding Disposition

1. High-review exclusivity: **ADDRESSED** for the reported
   `exclusively by Codex` omission (`validate-contract.lib.mjs:2418`).
2. Completed-boundary timing: **ADDRESSED** for the reported preposed
   `completion of the active Task` form (`validate-contract.lib.mjs:2795`).
3. Repeated-predicate alternative: **ADDRESSED** by action-level alternative
   exclusion (`validate-contract.lib.mjs:1557`).
4. Document targets and role recipients: **ADDRESSED** for the reported
   Plan/Plans controls (`validate-contract.lib.mjs:1257`).
5. Bare Codex Agent identity: **ADDRESSED** (`validate-contract.lib.mjs:1095`).
6. Notes containing nearby `over`: **ADDRESSED** because only immediate
   `take over` or a parsed role object is replacement
   (`validate-contract.lib.mjs:3020`).
7. Qualified duplicate/missing reviewers: **ADDRESSED** for both reported
   controls (`validate-contract.lib.mjs:2183`, `:2292`).

### Round-11 Auxiliary Scoped Important Findings

1. High-review exclusivity: **ADDRESSED** for the exact reported omission.
2. Completed-boundary timing: **ADDRESSED** for both reported directions.
3. Alternative polarity: **ADDRESSED** for both reported directions.
4. Plural artifact/people precedence: **ADDRESSED** for the reported people
   subject form.
5. Narrowed `take` replacement grammar: **ADDRESSED** for both reported
   directions.
6. Reviewer modifier binding: **ADDRESSED** for the reported missing and
   duplicate reviewer forms.
7. Arbitrary actor-link filler: **ADDRESSED** for the reported implementation
   guidance form.

## New Fix-Diff Breakage

### Important 1: Sequential passive implementers after an adverb fail open

`directPassiveActorsForAction` considers a second `by` relation only when the
text between the previous actor and that link contains `and`, `but`, `or`,
`yet`, `rather than`, or `instead of`
(`validate-contract.lib.mjs:1617`, `:1624`). A clear sequential relation is
silently discarded:

```text
High Tasks are implemented by Codex, then by Grok.
High Tasks are implemented by Codex, subsequently by Grok.
```

Both sentences were rejected at the fix base and are accepted at HEAD. Grok
is an explicit second high-Task implementer, so ignoring that relation defeats
the exact Codex-only high route.

### Important 2: A modifier breaks legal carried-Task completion

For a carried prior Task, `timingReferencesCompletion` accepts an implicit
Task subject only when the completion token is exactly one position after the
boundary marker (`validate-contract.lib.mjs:2910`, `:2917`). These ordinary
completed-boundary forms changed from accepted at the fix base to rejected at
HEAD:

```text
The current Task is running. When finally complete, change the Task Agent.
The current Task is running. Once already complete, change the Task Agent.
```

The adverbs modify `complete`; they do not change its carried Task subject.
The fix closes unrelated review/test completion but regresses valid boundary
language that the prior carried-Task finding requires.

### Important 3: `take the role of` falls back to an unrelated required reviewer

Every syntactically valid `take (on) the role of` is classified as a
replacement (`validate-contract.lib.mjs:3007`, `:3020`). When its object is
not itself a reviewer, the generic `the role` reference falls back to the
prior required reviewer (`validate-contract.lib.mjs:3117`, `:3180`):

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of observers.
The Codex reviewer is mandatory. Optional Design reviewers take the role of note takers.
```

Both compliant statements were accepted at the fix base and reject at HEAD.
The optional reviewers are taking unrelated roles, not replacing the mandatory
Codex reviewer.

### Important 4: Alternative exclusion leaks across `while` and `although`

Alternative scope resets only on `but` or `yet`
(`validate-contract.lib.mjs:538`, `:1545`). The whole later action is then
skipped before route validation (`validate-contract.lib.mjs:1557`, `:2714`):

```text
High Tasks are implemented by Codex rather than by Grok, while Gemini also implements them.
High Tasks are implemented by Codex rather than by Grok, although Gemini also implements them.
```

Both explicit Gemini additions were rejected at the fix base and are accepted
at HEAD. `while` and `although` introduce positive relations here; they are not
part of the excluded Grok alternative.

### Important 5: People precedence hides a nearer plural document object

`directivePronounAntecedent` returns `people` whenever any plural people
subject or recipient exists, before considering plural document objects
(`validate-contract.lib.mjs:1272`, `:1294`). This reverses the Round-11
precedence bug instead of resolving mixed clauses structurally:

```text
The developers list the Plan and Design. The parent updates them.
The reviewers list the Plan and Design. The parent revises them.
```

In both cases `them` is the coordinated direct object `Plan and Design`, not
the subject. The fix base rejected both parent edits; HEAD accepts both.

### Important 6: `exclusively` is still treated as a global reviewer quantifier

The new exhaustive vocabulary includes `exclusively`, but the scope exemption
covers only `about`, `for`, `on`, and `regarding`
(`validate-contract.lib.mjs:657`, `:676`). An `after` timing modifier therefore
exhausts the reviewer set at `validate-contract.lib.mjs:2418`:

```text
High Tasks are reviewed by Codex exclusively after implementation. The Task Agent provides the auxiliary review.
High Tasks are reviewed by Codex exclusively after testing. The Task Agent provides the auxiliary review.
```

Both complete legal high-review routes were accepted at the fix base and
reject at HEAD. `Exclusively` limits when Codex reviews, not who reviews.

### Important 7: Embedded missing evidence is attached as reviewer absence

Postposed absence now requires a reviewer-side copular link but does not prove
that the absence term is the complement of that link. Any later `missing`
within six tokens passes when there is no intervening coordinator or actor
(`validate-contract.lib.mjs:2223`, `:2230`, `:2253`):

```text
High Tasks are reviewed by Codex and the Task Agent reviewer is aware input is missing.
High Tasks are reviewed by Codex and the Task Agent reviewer is told evidence is missing.
```

Both complete legal routes were accepted at the fix base and reject at HEAD.
The input/evidence is missing; the Task Agent reviewer is present.

## Out-of-Scope Observations

- The retained Task 2 CommonMark backtick-info-string Minor is unchanged and
  is not counted.
- The retained Task 4 combined failed/canceled route-locality coverage Minor
  is unchanged and is not counted.
- The fix changes only contradiction prose classification and its Node tests;
  structured routing, risk, generation, progress, lineage, Skill prose, and
  Rust are outside the fix diff and were not reopened.
- The validator remains a bounded English classifier. Pre-existing gaps
  entirely outside this fix are not counted here.

## Verification

- Confirmed exact HEAD:
  `e72a5f8345d238ad30ed4f7d966c18a9c868bc17`.
- Confirmed the scoped diff contains only the two validator JavaScript files.
- `git diff --check 94ba94f92b914b1dec4b5eb7833146bea28d1c33..e72a5f8345d238ad30ed4f7d966c18a9c868bc17`
  passed with zero diagnostics.
- Independent prior-report exact matrix: **23/23 correct at HEAD**.
- Independent scoped differential matrix: **14/14 correct at the fix base and
  14/14 wrong at HEAD**, two concrete probes for each of the seven new
  Important regression groups above.
- The producer's full Node suite, production validator, and Prettier results
  were treated as claims and were not rerun.
- No Rust command was run. No command enabled default `tauri-runtime`.
- No production, test, Skill prose, Design, Plan, progress, prior report,
  index, HEAD, or branch state was modified. Only this assigned ignored report
  was created.

## Severity Counts

Prior deduplicated union: **Critical 0 / Important 8**, with all eight exact
reported finding classes addressed.

New scoped breakage: **Critical 0 / Important 7 / Minor 0**.

Final counted severity: **Critical 0 / Important 7 / Minor 0**.

## Final Verdict

**NOT APPROVED**

Every exact Round-11 report probe is repaired, but the fix introduces seven
new Important contradiction-classifier regression groups.
