# Final Whole-Branch Review

## Findings

### Critical

None.

### Important

1. **A missing routing block is accepted as legacy without any legacy
   provenance, so the authoritative pre-dispatch validator fails open.**
   `parseSimplePlan` only parses routing when it has already found a marker at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1197`;
   `validateSimpleDocuments` then skips routing and Plan/progress agreement at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1827`.
   The unconditional legacy fixture at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:820`
   locks in acceptance of a markerless Plan. A focused probe confirmed that a
   markerless new Plan paired with route-shaped progress returns no failures.
   This contradicts the requirement that every new Plan contain exactly one
   routing block and that an absent block block dispatch at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:311`
   and
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:491`.
   Make adaptive/new validation require routing by default and permit the
   compatibility exception only through an explicit, verified legacy mode.

2. **Generation-change validation checks the boundary Task but not the rest of
   the affected pending suffix.** The loop at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1682`
   verifies the completed prefix and the generation boundary only; it never
   requires every later Task to remain `pending` with `runs: []`. The serial
   check at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1517`
   does not close this gap. A focused probe with an empty pending boundary and
   a later pending Task containing a reserving run returned no failures. That
   violates the Skill's own boundary rule at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/SKILL.md:127`
   and the approved change procedure at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:221`.
   Validate the entire suffix affected by each newly adopted generation and
   add a regression with a clean boundary but a dirty later Task.

3. **Contradiction resistance recognizes a small list of canned sentences, not
   the ownership and routing rules it claims to enforce.** The seven literal
   regular expressions at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:254`
   are the only explicit contradiction patterns applied at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:734`;
   the tests at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:390`
   repeat those exact phrases. A focused probe confirmed that clear
   paraphrases such as "The parent writes and revises every Plan. Grok
   implements every Task, including high Tasks." pass validation. The approved
   contract explicitly requires contradictory prose to be rejected at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:306`.
   Enforce normalized positive ownership/route clauses structurally, or use a
   bounded directive grammar, instead of treating seven sentence literals as
   semantic validation.

4. **The operational Skill omits binding policy needed to construct the
   documents it requires.** It names `b2d_task_risk_v1` but does not define the
   six hard triggers, six weighted soft signals, evidence object shape, or the
   threshold/arithmetic at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/SKILL.md:155`.
   It says only "When the Design needs review" at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/SKILL.md:135`
   without the mandatory conditional triggers. Its progress instructions at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/SKILL.md:169`
   list additive fields but not the complete top-level schema or the 2 MiB /
   512 KiB / 64 KiB / 256 KiB document and block limits. The missing decisions
   are defined only in the branch-specific design at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:146`,
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:242`,
   and
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:380`.
   A fresh coordinator or Plan Author using the installed Skill cannot derive a
   deterministic valid contract without out-of-band knowledge. Put the
   decision tables, exact JSON shape, and numeric bounds in the Skill or in a
   required bundled reference that the Skill explicitly loads and passes to
   the Plan Author.

### Minor

1. **Malformed multi-generation progress is fail-closed only by exception.**
   The generation pass dereferences `task.index` at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1687`
   after structural validation has retained raw entries. A focused probe with
   `tasks: [null]` produced `TypeError: Cannot read properties of null (reading
   'index')`. The CLI catches exceptions and returns nonzero at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs:101`,
   so dispatch remains fail-closed; the exported validator nevertheless breaks
   its deterministic rule-ID result contract. Keep this as Minor, but make the
   library total before it is used directly by another caller.

2. **The shared fence detector accepts a CommonMark-invalid backtick opener.**
   The Rust detector at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/src-tauri/src/acp/delegation/workflow/simple_parse.rs:331`
   and the JavaScript detector at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:747`
   treat a backtick opener whose info string contains a backtick as a fence,
   although CommonMark does not. This can hide a later live marker. Keep this
   as Minor because it requires malformed Markdown and the platform parser is
   non-authoritative; fixing Important finding 1 also makes the Skill side
   fail closed rather than silently accepting the hidden route.

3. **Failed/canceled route-locality coverage remains combined rather than
   isolated.** The production branch is route-local at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/src-tauri/src/acp/delegation/workflow/project.rs:2499`,
   but the test at
   `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/src-tauri/src/acp/delegation/workflow/project.rs:4738`
   injects a canceled implementer and failed reviewer together, then asserts
   that every node is blocked. Keep this as Minor: the implementation groups
   and derives status per exact key, so no defect is demonstrated, but
   one-at-a-time tests are still needed to prove that one failed/canceled route
   node does not contaminate a sibling.

## Deferred-Minor Triage

- **Task 2 CommonMark backtick opener:** remains Minor for the reasons in Minor
  finding 2; it is not an independent delivery blocker.
- **Task 4 isolated failed/canceled locality coverage:** remains Minor for the
  reasons in Minor finding 3; this is a coverage gap, not a demonstrated
  projection bug.
- **Task 5 malformed `tasks: [null]`:** remains Minor for the reasons in Minor
  finding 1; the CLI exits nonzero, but deterministic library behavior should
  be restored.

## Spec Compliance

**Verdict: Needs fixes.** The branch implements the exact v2 positive contract,
Grok-only defaulting with generic built-in/custom Task Agent identities,
deterministic normal/high route shapes, distinct producer/reviewer keys,
legacy five-part reviewer readability, bounded Rust parsing, warning-only
Simple projection, and owning-producer final fixes. Those are substantial
compliant parts of the approved design.

The branch does not yet satisfy the binding fail-closed Plan requirement,
generation-boundary invariant, contradiction-resistance requirement, or
complete operational Skill requirement described in the Important findings.
Passing the production Skill fixture and its 32-test Node suite therefore does
not establish whole-contract compliance.

## Verification Basis

- Reviewed `896be5f8..caaae2fe` as a whole: 12 commits, 10 changed files,
  `+6220/-1675`.
- Did not rerun the already recorded broad suites, per review scope. The final
  Task report records 32/32 Node tests, focused Rust tests, server/test-utils
  check and clippy, formatting, and diff checks as passing.
- Ran focused read-only probes only. They reproduced the markerless-route
  acceptance, paraphrased contradiction acceptance, dirty generation suffix
  acceptance, and malformed multi-generation throw described above.
- The recorded Rust verification used only
  `--no-default-features --features server,test-utils`; no default
  `tauri-runtime` check or clippy was recorded, so the existing evidence is not
  complete desktop-release verification.

## Delivery Readiness

**Ready to deliver: With fixes.** Resolve the four Important findings and add
focused regressions before delivery. The three retained Minors may remain
deferred if they are explicitly carried as known debt.

**Finding count:** Critical 0, Important 4, Minor 3.
