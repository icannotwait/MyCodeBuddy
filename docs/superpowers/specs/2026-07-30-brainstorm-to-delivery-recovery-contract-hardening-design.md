# Brainstorm-to-Delivery Recovery Contract Hardening Design

## Status and Relationship to Approved Designs

Approved by the user on 2026-07-30 as a supplemental Skill-contract Design.

This Design supplements, and does not supersede or alter, either approved
recovery Design:

- `2026-07-30-delegation-recovery-authorization-design.md`; and
- `2026-07-30-workflow-blocked-recovery-design.md`.

Those Designs remain authoritative for backend policy, persistence, wire
contracts, authorization, and transactional behavior. This Design closes the
orchestration gap between those platform contracts and
`.agents/skills/brainstorm-to-delivery/SKILL.md`, then defines deterministic
validator and behavior-test coverage for that guidance. Implementation is
folded into Task 11 of the existing combined implementation plan; Tasks 1-12
remain numbered and ordered as approved.

## Problem

The current Brainstorm-to-Delivery (B2D) Skill predates the approved recovery
policies. Its recovery paragraph currently permits
`replacement_reason=unresumable` for cancellation, parent-end, and stall
codes. That shortcut conflicts with resume-first platform policy, changes the
meaning of `unresumable`, spends replacement budget early, and encourages a
caller to bypass durable lineage by changing profile or key.

The surrounding guidance has other recovery ambiguities:

- it does not state the typed confirmation challenge and exact replay order;
- it can suggest that `recover_workflow` creates its own challenge;
- it treats a harvested terminal card as weaker than chat emission even after
  platform validation;
- it allows post-admission profile escalation despite frozen Task identity;
- it does not require independent risk recomputation by normal-route review;
- its two route tables can drift because only the numbered table is parsed;
- its ledger fields are described without write-ahead ordering; and
- its validator mostly checks token presence and raw forbidden literals, so
  negated safety prose can be mistaken for a violation while negated required
  prose can be mistaken for compliance.

These are orchestration-contract defects. The Skill cannot change backend
policy, and its validator cannot prove arbitrary natural-language intent, but
both must make the safe platform path unambiguous and detect known dangerous
mutations.

## Goals

- Make B2D recovery index/status-first and resume-first.
- Make delegation and workflow confirmation sequences exact and typed.
- Preserve admitted Task key, role, agent, profile, lineage, and recovery
  consumption through every legal recovery.
- Keep cancellation-family and stall evidence out of `unresumable` aliases.
- Make workflow recovery and Plan-lineage reset follow the approved durable
  authorization contracts.
- Treat platform-harvested, platform-validated cards as settlement evidence.
- Cross-check both authoritative route-table reading surfaces.
- Require independent Task-risk review and deterministic Design-review hard
  triggers.
- Make the delegation ledger write-ahead and recovery-reconcilable.
- Add stable validator rule IDs, semantic negation-aware recovery checks, and
  positive and negative mutation fixtures.
- Prove judgment-shaping prose with baseline and post-change behavior tests,
  while keeping mechanical invariants in deterministic tests.
- Keep the production Skill below 500 lines.

## Non-Goals

- Redefining either approved backend recovery policy.
- Making Skill prose or validator output authoritative for admission.
- Adding a second recovery tool, authorization store, or workflow repair path.
- Accepting caller-authored authorization, target state, risk, route, or
  resumability claims.
- Solving blocked recovery by replacing a workflow, changing a Plan path, or
  changing an admitted work-unit key or profile.
- Building a general natural-language theorem prover. The parser is bounded,
  deterministic defense in depth against specified mutation families.
- Storing generated pressure-test transcripts in the repository.

## Authority Boundaries

Recovery has three deliberately separate enforcement layers.

### Platform-Enforced Invariants

The backend policies and transactions from the two approved Designs decide:

- whether a delegation can continue, dispatch fresh, replace, or must stop;
- whether confirmation is required and which exact action it authorizes;
- whether evidence is structurally or durably `unresumable`;
- ownership, role, route, agent/profile identity, latest-run, active-run,
  frozen-cohort, and budget validity;
- whether a workflow may recover, its derived lifecycle target, and whether
  `reset_plan_lineage` is required;
- source fingerprints, ten-minute authorization validity, one-use
  consumption, replay, and immutable provenance; and
- platform validation or harvesting of terminal settlement cards.

The Skill never overrides these decisions. A stale projection, ledger entry,
report, or prose claim is not write evidence.

### Skill Guidance

The B2D Skill tells the Parent which read and mutation to attempt, how to react
to typed results, which evidence to persist before and after admission, and
when to stop. It shapes orchestration judgment for recovery, risk review,
settlement evidence, and pressure conditions that are not fully mechanical.

### Deterministic Validator

The validator enforces bounded textual and structural invariants: required
tools and sequences, forbidden affirmative mappings, exact route tables,
known Parent-ownership verb evasions, fixed identity/budget language, card and
ledger contracts, and the line limit. It rejects known unsafe mutations but is
not proof of every possible natural-language judgment. Platform checks remain
authoritative even when the Skill passes validation.

## Recovery Contract

Recovery starts with the durable index or status projection and prefers the
existing work unit. The projection selects the next attempted operation; the
mutation re-evaluates current platform state and is authoritative.

### Decision Table

| Durable recovery source | Confirmation | Rail | Next call |
| --- | --- | --- | --- |
| Genuine unexpected transport loss with resume identity and continue budget | No only when central policy permits automatic recovery | Continue | `continue_delegation` |
| `tool_stalled_timeout` with resume identity and continue budget | Yes | Continue | Attempt `continue_delegation`; after typed challenge, authorize and replay it |
| `parent_canceled`, `parent_turn_failed`, `join_abandoned`, or `user_cancelled` with resume identity and continue budget | Yes | Continue | Attempt `continue_delegation`; after typed challenge, authorize and replay it |
| Current structural resume-identity absence, or a durable attempted-resume failure persisted as `unresumable` | As derived by central policy; valid provenance can avoid a second card | Replacement | `delegate_to_agent` with exact `replacement_reason=unresumable` |
| Continue rail exhausted and replacement rail remains | Inherit the cause-derived confirmation requirement | Same-key replacement | `delegate_to_agent` with exact `replacement_reason=budget_exhausted_continue` |
| Continue rail exhausted and replacement rail already consumed | Not applicable | Stop | Emit a blocking report; do not change key or profile |
| Workflow projection derives `recover_workflow` | Yes | Workflow state transition | Authorize, then call `recover_workflow` with the receipt |
| Workflow projection derives `reset_plan_lineage` | Yes, bound to displayed reason hash | Plan lineage settlement | Authorize the exact reason, then submit the receipt with the matching Plan settlement |
| Missing or invalid terminal card and platform harvest cannot validate one | No new recovery authorization | Existing child continue rail | `continue_delegation` to re-emit the card |

Cancellation-family evidence is never aliased to
`replacement_reason=unresumable`. `tool_stalled_timeout` is also not an
`unresumable` or replacement source. A cancellation or stall may coexist with
independent current structural unresumability, but only that platform-derived
evidence selects the replacement rail.

### Delegation Confirmation Sequence

The exact sequence is:

1. Read `get_delegation_status` and reconcile the ledger with current platform
   state.
2. Attempt the projected exact `continue_delegation` or replacement
   `delegate_to_agent` call without inventing an authorization.
3. Receive typed `recovery_confirmation_required`. No run or budget is minted.
4. Call `request_recovery_authorization` for the same owned subject.
5. On approval, replay the exact rejected call with only
   `recovery_authorization_id` added.
6. Persist the admitted `latest_task_id`, then reconcile platform state again.

The Parent does not substitute a different action, key, profile, replacement
reason, or payload between rejection and replay. Authorization IDs are replay
inputs only. They never appear in status projections, the B2D progress ledger,
workspace reports, card summaries, or generated pressure-test records.

`tool_stalled_timeout` always reaches step 3 before continuing. Genuine typed
unexpected transport loss may skip steps 3-5 only when central policy admits
an unconfirmed continue.

### Blocked Workflow Sequence

The exact sequence is:

1. Call `get_workflow_state` with omitted detail or `detail=index`.
2. If the enabled catalog promises workflow recovery but omits
   `recover_workflow`, hard-block. Do not use legacy mode, publish around the
   block, replace the workflow, or edit Plan identity.
3. When the projection derives `recover_workflow`, call
   `request_recovery_authorization` for that workflow.
4. On approval, call `recover_workflow` with the approved
   `recovery_authorization_id`, current expected manifest revision, and
   correlation ID.
5. Re-read `get_workflow_state` and reconcile the ledger.

`recover_workflow` always requires the receipt. It is never called first as a
challenge generator and never accepts a caller-chosen target state.

### Plan `user_decision_required`

When durable workflow policy derives `reset_plan_lineage`, the Parent presents
one exact bounded reason. The authorization source fingerprint includes the
hash of that displayed reason. The resulting receipt must allow exactly
`reset_plan_lineage` and is submitted with the byte-identical reason through
the approved Plan settlement path.

That consumed receipt is the durable persisted reason required by the existing
"user-approved requirements change" lineage rule. Successful settlement
starts a new stagnation baseline and resets the counter. A model claim, free
text without the matching receipt, changed reason, stale Plan/reviewer state,
or generic `recover_workflow` receipt cannot reset lineage.

## Frozen Identity, Risk, and Budgets

### Post-Admission Identity

First admission freezes the complete Task identity: Task index,
`work_unit_key`, role, agent type, profile, route/cohort identities, and
lineage. A profile change is an identity change; it cannot mint a new key,
lineage, continue budget, or replacement budget.

Every legal continue or replacement preserves the admitted key and profile
and inherits all consumed recovery counters. If a different route or profile
is needed before first admission, the Plan Author makes a material Plan
revision and the full Plan group reviews it. After admission, a discovered
identity/risk mismatch blocks and escalates without mutating the cohort.

### Risk Review

The Plan Author records `b2d_task_risk_v1`, but the normal-route Codex Task
reviewer independently recomputes the classification from the approved Plan
row and changed files. A mismatch is a finding and cannot be waived by copying
the Author's value. High-route reviewers still verify the same invariant.

External Design review has deterministic hard triggers. Any migration,
security or authorization boundary, concurrency behavior, persistence or
state-machine change, or externally visible contract-compatibility change
requires external review. Real ambiguity is an additional trigger, not a
replacement for those hard triggers.

### Budget Exhaustion

When the unexpected-continue budget is exhausted, the only replacement reason
for that fact is `budget_exhausted_continue`, using the same admitted key,
role, agent, and profile. It consumes the existing replacement rail if that
rail remains. If replacement is already consumed, the Parent stops with a
blocking report. Changing key/profile or labeling cancellation/stall as
`unresumable` to obtain more budget is forbidden.

## Settlement Evidence

A terminal `codeg-card-summary-v1` that the platform harvested from an allowed
report and platform-validated is valid settlement evidence. Chat emission
remains the expected child behavior, but a validated harvest does not require
an otherwise pointless continuation.

If harvest is unavailable or validation fails, the child is degraded. The
Parent preserves the report and uses `continue_delegation` on the same child
to re-emit a valid card. The Parent never advances a gate, dispatches a Final
fixer, or settles from prose, an unvalidated report block, or an inline verdict
alone.

## Write-Ahead Delegation Ledger

Before every `delegate_to_agent` or `continue_delegation`, persist the intended
`work_unit_key`, role, agent type, profile, and action in the B2D progress
ledger. Include replacement reason and source `latest_task_id` when applicable,
but never an authorization ID.

After platform admission, fill the returned `latest_task_id` and state. If the
Parent recovers after a disconnect, timeout, compaction, or ambiguous tool
result, read platform status first and reconcile the intended row rather than
issuing a blind duplicate. This ordering makes the ledger a recovery aid, not
an alternative authority.

## Route-Table Contract

Both Skill surfaces are authoritative for readers:

- the top `## Codeg roles and tools` table; and
- the `### Normal route` and `### High route` tables inside numbered
  `## 4. Task route`.

After canonicalizing harmless annotations, they must encode exactly:

| Route | Implementer | Required reviewers |
| --- | --- | --- |
| Normal | Grok | Codex |
| High | Codex | Codex and Grok |

The validator parses both surfaces independently and compares their canonical
role/agent multisets. A mutation in either surface fails even when the other
surface remains correct. Alternate agents, duplicate same-agent high
reviewers, missing rows, and extra route identities fail closed.

## Validator Architecture

### Stable Diagnostics

Every validator failure is prefixed by one exact stable rule ID. Multiple
fixtures may exercise the same rule, but every mutation test asserts the exact
expected ID rather than matching arbitrary prose.

| Rule ID | Contract |
| --- | --- |
| `B2D-001` | Legacy forbidden literals remain absent |
| `B2D-002` | Baseline required terms are present affirmatively |
| `B2D-003` | Index-first recovery evidence terms are present affirmatively |
| `B2D-004` | Frontmatter is valid and trigger-only |
| `B2D-005` | Production Skill is below 500 lines |
| `B2D-006` | Parent does not author Plan/Task code, including known verb evasions |
| `B2D-007` | Numbered Task-route tables have the exact route shape |
| `B2D-008` | High gate never passes with one reviewer |
| `B2D-009` | Reviews cover the latest task ID and artifact digest |
| `B2D-010` | Plan review, stagnation, and frozen-cohort contracts are present |
| `B2D-011` | Required `subagent-driven-development` invocation is present |
| `B2D-012` | Approved-gate continuation and pause conditions remain exact |
| `B2D-013` | Top Codeg role table has the exact route shape |
| `B2D-014` | Top and numbered route surfaces agree exactly |
| `B2D-R001` | Recovery tools/tokens occur in affirmative required guidance |
| `B2D-R002` | Delegation challenge, authorization, and exact replay order is present |
| `B2D-R003` | No affirmative cancellation/stall-to-`unresumable` mapping exists |
| `B2D-R004` | Stall requires confirmation on the continue rail |
| `B2D-R005` | Workflow status, authorization, then recovery order and hard catalog gate are present |
| `B2D-R006` | Exact reason-hash `reset_plan_lineage` receipt resets the baseline |
| `B2D-R007` | Post-admission key/profile and inherited budgets are frozen |
| `B2D-R008` | Validated harvest and degraded-card re-emission rules are present |
| `B2D-R009` | Independent risk recomputation and deterministic Design triggers are present |
| `B2D-R010` | Continue exhaustion uses same-key `budget_exhausted_continue` or blocks |
| `B2D-R011` | Ledger writes intent before dispatch/continue and reconciles afterward |

The existing passing fixture is extended to satisfy all rules. Every existing
mutation fixture is updated to assert its exact applicable ID, and every new
mutation names and asserts one exact recovery ID.

### Negation-Aware Recovery Checks

The recovery checker normalizes Markdown, splits bounded sentences/clauses,
and evaluates polarity against the matched mapping or requirement. It must:

- reject affirmative mappings such as "use
  `replacement_reason=unresumable` for `parent_canceled`" and "cancellation or
  stall is an unresumable replacement source" with `B2D-R003`;
- accept explicit prohibitions such as "never map cancellation to
  `unresumable`" and "`tool_stalled_timeout` is not a replacement source";
- reject unrelated negation followed by an affirmative mapping; and
- refuse to satisfy `request_recovery_authorization`,
  `recovery_authorization_id`, `recovery_confirmation_required`, or
  `recover_workflow` merely because the token appears inside negated prose.

This is semantic within the bounded recovery grammar, not an unrestricted
language judgment. Positive and negative controls lock the supported scope.

### Parent-Ownership Parser Hardening

Extend the existing action grammar with `draft/drafts/drafting/drafted`,
`compose/composes/composing/composed`, and
`generate/generates/generating/generated`. The Chinese protected-action set is
exactly `起草`, `拟写`, `编写`, `撰写`, `创作`, `生成`, `改写`, `重写`, `编辑`, and
`修改` when the Parent/`父会话` acts on a Plan or Task code. Positive mutations
granting the Parent those actions fail `B2D-006`. Negative controls whose
negation governs the protected action pass. Legitimate Parent coordination
artifacts, such as a Task brief or review findings, remain allowed.

## Skill Content Placement

Pressure-critical behavior appears in three scanning surfaces without copying
long explanations:

1. the main recovery section contains the authoritative sequences and identity
   rules;
2. Quick reference contains terse pressure-to-action rows; and
3. Rationalizations closes the known key/profile, broad-`unresumable`, direct
   `recover_workflow`, and prose-settlement loopholes.

Supporting explanation stays in this Design. The production Skill must remain
below 500 lines after all changes.

## Compatibility and Rollout

The Task 11 delivery is atomic with the platform recovery tools it documents.
An enabled workflow catalog without `recover_workflow` is inconsistent and
hard-blocks. Older catalogs do not gain a Skill-only workaround.

Validator messages gain stable ID prefixes while retaining human-readable
detail. Direct consumers should key on IDs. The validator CLI continues to
exit nonzero on any failure. Existing role, ownership, index-first, phase,
line-limit, and production-Skill fixtures remain active and receive explicit
rule assertions.

No durable value, API action, budget, or authorization behavior is introduced
by this supplemental Design. Authorization IDs remain confined to replay
inputs and platform-owned provenance.

## Testing Strategy

### Deterministic RED-GREEN Tests

Before Skill or validator implementation, add mutation fixtures for every new
rule and update existing mutation fixtures with exact rule IDs. Run the Node
test file and prove a nonzero discovered test count plus the intended RED
failures. Positive and negative fixtures cover negation-aware
`unresumable`, required-token polarity, route-surface mutations, profile/key
budget minting, card harvest, risk recomputation, Design triggers, exhaustion,
write-ahead ordering, and English/Chinese ownership verbs.

After implementation, the same nonzero suite passes, the production Skill
passes validation, and its physical line count is below 500. Rust MCP contract
and frontend recovery-card tests retain their own focused RED-GREEN evidence
from Task 11.

### Behavior RED-GREEN Tests

Before changing Skill prose, run at least three fresh-context combined-pressure
scenarios without the new guidance. Each combines deadline/authority pressure
with sunk cost or budget temptation:

1. a cancellation-family Plan Author has valid resume identity while a caller
   urges immediate `unresumable` replacement;
2. a stalled Task has continue capacity while urgency and completed child work
   tempt an unconfirmed replacement or continue; and
3. a blocked workflow or `user_decision_required` Plan is tempting to repair
   by changing Plan/key/profile, using prose approval, or calling
   `recover_workflow` before authorization.

Record baseline choices and verbatim rationalizations in the Task report. Do
not commit generated transcripts.

For guidance that shapes judgment rather than a mechanical parser rule, run
fresh-context wording micro-tests with a no-guidance control and at least five
samples per variant. Read every flagged sample manually and select the minimum
wording that reduces both failures and variance. Then run the same three or
more combined-pressure scenarios with the complete revised Skill. Record
post-change decisions and convergence in the Task report. Mechanical parser
contracts do not need redundant behavior tests.

### Nonzero Gates

The Task 11 Node RED and GREEN wrappers must parse TAP output and require
`# tests N` with `N > 0`. A zero-match run is a hard failure even if Node exits
zero. The final Task 11 report records discovered, passed, failed, and skipped
counts alongside the Skill line count.

## Completion Criteria

This supplemental contract is complete when:

- cancellation-family and stall evidence are never affirmative
  `unresumable` aliases;
- delegation follows typed challenge, authorization, exact replay;
- workflow follows state, authorization, recovery and hard-blocks on a missing
  enabled tool;
- exact reason-hash lineage reset is the only authorized stagnation reset;
- admitted key/profile and inherited recovery budgets cannot be reminted;
- validated harvested cards settle, while invalid/missing cards re-emit on the
  same child;
- normal review recomputes risk and all deterministic Design triggers force
  external review;
- exhausted continue uses same-key `budget_exhausted_continue` or blocks;
- ledger intent is persisted before delegation mutation and reconciled after;
- both route surfaces agree and known ownership evasions are rejected;
- every validator mutation asserts an exact stable rule ID;
- baseline and revised pressure evidence is recorded without committed
  transcripts;
- focused Rust, frontend, and nonzero Node contract tests pass; and
- `.agents/skills/brainstorm-to-delivery/SKILL.md` remains below 500 lines.
