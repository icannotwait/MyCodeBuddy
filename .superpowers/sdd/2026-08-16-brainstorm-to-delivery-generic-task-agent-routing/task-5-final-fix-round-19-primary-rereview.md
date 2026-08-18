# Task 5 Final Fix Round 19 Primary Re-review

Fix base: `a778e592e41c2b45bc7e0489140e4b31a9fac6cd`  
Reviewed head: `ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2`

## Finding Verdicts

1. **Important 1: ADDRESSED.**
   `completionHasDirectObject` now checks whether the first lexical token after
   the completion predicate owns a possessive boundary before applying the
   temporal/adjunct exemptions
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4460`).
   The focused expectations cover all four possessive temporal artifacts, all
   seven required genuine adjuncts, a plain transitive artifact, an
   article-qualified migration, and later reactivation
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4910`).

2. **Important 2: ADDRESSED.**
   `changeHasExplicitNonAgentObject` now rejects the unrelated-object exemption
   only when every material token between the change predicate and the first
   identity/profile term is a route-identity prefix
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4724`,
   especially lines 4732-4747). The expectations include the four required
   unrelated profile-bearing actions, the three direct identity/profile
   controls, and the existing unrelated-object controls
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4951`).

3. **Important 3: ADDRESSED.**
   The qualified non-Task antecedent path now derives explicit Task mentions
   from the clause tokens and suppresses the non-Task fallback when one occurs
   after the qualified subject and before the restart boundary
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3908`).
   Both required closer-Task forms and all specified restart-pronoun neighbors
   are asserted
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4982`).

4. **Important 4: ADDRESSED.**
   A Task governed by a parenthetical reactivation predicate is no longer
   classified as merely nested under the outer non-Task subject
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3981`,
   especially lines 3985-3988). The exact source, monitoring control,
   preposed-gerund Task restarts, and explicit-server controls are asserted
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5016`).

## New Breakage in the Fix Diff

### Important 1: Singular-they selected-profile objects now evade the active-Task switch ban

The new prefix allowlist includes `its` but omits `their`
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:914`).
Because lines 4742-4747 treat any unlisted material prefix as evidence of an
unrelated object, both of these direct selected-Agent profile changes are now
accepted while the Task is running:

```text
The active Task is running. The Task Agent will change their selected profile immediately.
The active Task is running. The Task Agent will change their current profile immediately.
```

An in-memory base/head differential probe returned `base=reject, head=accept`
for both. Here singular `their` refers directly to the immediately preceding
Task Agent, so these are the same forbidden route-profile change as the
covered `its selected profile` form. This is a new fail-open active-route
regression introduced by the Round 19 prefix filter.

### Important 2: Any later `Task` token defeats a qualified non-Task antecedent

The new check at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3908`
uses `directiveTasks`, which recognizes every `Task` token other than
`Task Agent` without distinguishing a Task noun from an attributive modifier
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2059`).
Consequently this compliant completed-Task statement now rejects:

```text
The Task is completed but a separate service starts a Task worker and the server restarts it and it is still running. Then switch the Task Agent.
```

The closer noun phrase is `a Task worker`, not the Task itself, and restarting
`it` refers to that worker. The differential probe
returned `base=accept, head=reject`. The fix therefore closes the reported
explicit-Task cases by introducing a new fail-closed compound-modifier case.

### Important 3: Negated parenthetical reactivation is treated as affirmative

The new early return at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3985`
checks only whether the governing token belongs to
`TASK_REACTIVATION_PREDICATES`; it does not check that predicate's polarity.
Both compliant controls below now reject:

```text
The active Task is completed but the server, not restarting the Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, without restarting the Task, restarts and it is still running. Then switch the Task Agent.
```

The differential probe returned `base=accept, head=reject` for both, while the
affirmative source changed as intended from `base=accept` to `head=reject`.
Explicitly denying Task reactivation must not make the Task active, so this is
a new polarity regression in the Round 19 parenthetical repair.

No new Critical or Minor breakage was found in the scoped fix diff.

## Out-of-Scope Observations

No new untouched-code observation was used to block this scoped verdict. The
producer's two carried whole-branch Minors remain outside this diff: the Task 2
CommonMark backtick-info-string fence case and the Task 4 combined rather than
isolated failed/canceled projection-locality coverage. They are not included
in the scoped severity counts below.

## Verification Performed

- Read the Task 5 brief, complete Round 19 findings, complete producer report
  through Round 19, and the supplied scoped diff package.
- Confirmed `HEAD` is exactly `ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2`
  and `base..head` contains exactly the single advertised commit.
- `git diff --name-status base..head` lists only the two permitted validator
  files. Numstat is `51/17` for the library and `135/0` for the test file.
- Reviewed the zero-context test diff: it contains additions only. No existing
  test expectation was deleted, relabeled, or weakened; the added Round 18
  neighbor controls and all four Round 19 groups are additive.
- `git diff --check base..head` produced no diagnostics. The tracked worktree
  and index were clean before writing this ignored report.
- Ran focused, read-only/in-memory differential probes that loaded
  `validateSkillMarkdown` directly from the base and head Git objects. They
  confirmed the new adjacent regressions described above and retained the
  affirmative parenthetical source as a control. No repository file was used
  for probe state.
- Did not rerun the producer's focused or full Node suites. Their results were
  treated as claims, as required. Ran no Rust command and did not enable
  `tauri-runtime`.

## Severity Counts

- Binding Round 19 findings: **4 ADDRESSED, 0 NOT ADDRESSED**.
- New scoped breakage: **Critical 0, Important 3, Minor 0**.
- Approval-blocking open findings: **Critical 0, Important 3**.
- Out-of-scope carried observations: **2 Minor**, nonblocking and not counted
  above.

## Final Verdict

**NOT APPROVED.** All four binding Round 19 groups are addressed, but the fix
introduces three new Important contradiction-classification regressions. The
approval condition therefore is not met at
`ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2`.
