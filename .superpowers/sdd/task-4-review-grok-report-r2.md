# Task 4 Review R2 — Grok (HIGH dual reviewer, scoped re-review)

- **Work unit:** Independent Task 4 HIGH re-review (Grok) after fix
- **reviewed_task_id / implementer task:** `aac0bcf4-09ac-482e-af17-9d2a2f28a960`
- **Prior producer (R1):** `7b826557fe38fca115dfadd65c10b2eb0da54abf`
- **New producer (this review):** `3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c`
- **Prior Grok report:** `.superpowers/sdd/task-4-review-grok-report.md` (`request_changes`)
- **Grok finding closed:** `T4-GROK-I1` (MCP recover auto-restart before fenced core)
- **Also closed in same fix (Codex Important):** `T4-CODEX-I1` multi-association root loader; `T4-CODEX-I2` Design preflight typed header + stable nested protocol mapping
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 4
- **Design:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md`
- **Implementer report:** `.superpowers/sdd/task-4-report.md` § Fix Round 1
- **Reviewer:** Grok
- **Mode:** scoped re-review only (no implementation)

## Verdict

**`approve_with_minors`**

Fix producer `3f0fb8f4` closes the blocking Grok Important finding: production MCP `recover_workflow` no longer auto-restarts historical v1 and reaches the fenced store core with zero successor growth. The same commit also hardens the root protocol loader across durable bindings/generations and the Design self-review preflight corrupt-header path. Prior non-blocking minors remain open and do not block Task 5 dual-review gate closure for Task 4.

## T4-GROK-I1 fix verification

### Original defect

`process_recover_workflow` called `restart_legacy_if_required` before `recover_workflow_core`. Under enforce rollout with no successor, a historical v1 parent created a successor projection instead of returning `legacy_completion_protocol_read_only`. The Task 4 matrix only exercised the store core, so the production MCP boundary was unfenced.

### Fix evidence (`3f0fb8f4`)

| Check | Status | Evidence |
| --- | --- | --- |
| Pre-recover auto-restart removed | Pass | `process_recover_workflow` now enters `recover_workflow_core` only |
| Historical MCP recover returns stable read-only code | Pass | Listener regression asserts `recovery["error"]["code"] == "legacy_completion_protocol_read_only"` |
| No successor workflow created | Pass | Workflow row count remains 1 after MCP recover rejection |
| Rollout enforce does not divert recover | Pass | Regression installs `default_mode: V2Enforce` and still hits the store fence |
| Store core fence still required | Pass | Core still runs `require_owned_stored_v2_header` + in-txn recheck |
| Regression lives at production boundary | Pass | Calls `process_recover_workflow` on `DelegationListener`, not only `recover_workflow_core` |

Post-fix recover path:

```text
process_recover_workflow
  auth + store availability
  recover_workflow_core(...)          // no restart_legacy_if_required
    require_owned_stored_v2_header
    in-txn require_owned_stored_v2_header + require_v2_mutation
    ... recovery mutation only if (2, v2_enforce)
```

### Related Important closes in the same producer (audited, not Grok-owned)

| Finding | Status | Evidence |
| --- | --- | --- |
| `T4-CODEX-I1` multi-association root loader | Pass | `load_completion_protocol_for_conversation` unions owned + all bound workflow ids; rejection order unsupported → legacy → allowed; regressions for unbound latest gen and owned-v2 vs bound-v1 conflict |
| `T4-CODEX-I2` Design preflight corrupt header | Pass | Preflight txn starts with `require_owned_stored_v2_header`; nested completion protocol errors mapped to non-retryable stable codes; race asserts zero semantic writes |

## No-regression matrix (Task 4 baseline + fix)

| Requirement | Status after `3f0fb8f4` |
| --- | --- |
| Five-pair core mutation matrix + cross-parent unauthorized | Pass |
| Corrupt header non-terminal fences | Pass |
| Manager linked-root prompt fence | Pass |
| Recovery-authorization prepare fence | Pass |
| Completion evidence / `complete_work` historical matrices | Pass |
| MCP recover fail-closed (production boundary) | **Pass** (was Fail in R1) |
| Design preflight concurrent corrupt mode | Pass |
| Root multi-generation / multi-association loader | Pass |
| Explicit restart tools still present until Task 6 | Pass (expected) |
| No Task 5 admission/terminal rewrite | Pass |

## Independent verification (this re-review)

| Command | Result |
| --- | --- |
| `cargo test ... --lib ... workflow_mutations_reach_v2_store_guards_without_rollout_restart` | 1 passed |
| `cargo test ... --test completion_protocol_v2 ... root_protocol_loader` | 2 passed |
| `cargo test ... --test completion_protocol_v2 ... historical_protocol` | 2 passed |
| `cargo test ... --test completion_protocol_v2 ... root_prompt_protocol_fence` | 1 passed |
| `cargo test ... --test completion_protocol_v2 ... corrupt_header` | 1 passed |
| `cargo test ... --lib ... historical_protocol_mutation_matrix` | 2 passed |
| `cargo test ... --lib ... design_self_review_preflight_maps_concurrent_corrupt` | 1 passed |
| `cargo test ... --lib ... design_preflight_completion_protocol` | 1 passed |
| `cargo test ... --lib ... recovery_authorization_protocol_fence` | 1 passed |
| Static: `process_recover_workflow` has no `restart_legacy_if_required` | Pass |
| Static: Root companion `process()` still has auto-restart | Present (prior minor T4-GROK-M1) |

## Findings

| id | severity | title | status after R2 | notes |
| --- | --- | --- | --- | --- |
| T4-GROK-I1 | Important | MCP recover auto-restart before fenced core | **Closed** | Listener-level regression + zero successor growth |
| T4-GROK-M1 | Minor | Root companion `process()` still auto-restarts historical parents | Open | Deferred to Task 5/6 admission + restart removal; non-blocking |
| T4-GROK-M2 | Minor | `mutation_snapshot` omits some plan-listed counters | Open | Non-blocking |
| T4-GROK-M3 | Minor | Unlinked `send_prompt` APIs lack root fence | Open | Production linked paths covered; non-blocking |
| T4-GROK-M4 | Minor | Broader lib fixture debt (Task 2/3) | Open | Outside Task 4 producer scope |

No Critical findings.  
No open Important findings.

## Scope notes

- Fix round is three files (`listener.rs`, `store.rs`, `completion_protocol_v2.rs`) and stays within Task 4 fence surfaces.
- Explicit `restart_legacy_workflow` catalog/tools remain until Task 6 (plan-correct). Only the automatic recover diversion required for Task 4 was removed.
- Root companion auto-restart on `process()` remains intentional deferral (Task 5/6), tracked as non-blocking M1.

## Review card

```json
{
  "kind": "task_review",
  "task": 4,
  "reviewer": "grok",
  "round": 2,
  "reviewed_task_id": "aac0bcf4-09ac-482e-af17-9d2a2f28a960",
  "prior_producer_commit": "7b826557fe38fca115dfadd65c10b2eb0da54abf",
  "producer_commit": "3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c",
  "verdict": "approve_with_minors",
  "critical": [],
  "important": [],
  "important_closed": [
    {
      "id": "T4-GROK-I1",
      "title": "MCP process_recover_workflow no longer auto-restarts; reaches fenced recover_workflow_core",
      "blocking": false
    }
  ],
  "minor": [
    {
      "id": "T4-GROK-M1",
      "title": "Root companion process() still auto-restarts historical parents",
      "blocking": false
    },
    {
      "id": "T4-GROK-M2",
      "title": "mutation_snapshot omits some plan-listed side-effect counters",
      "blocking": false
    },
    {
      "id": "T4-GROK-M3",
      "title": "Unlinked send_prompt APIs do not apply root protocol fence",
      "blocking": false
    },
    {
      "id": "T4-GROK-M4",
      "title": "Pre-existing broader lib fixture debt remains outside Task 4 scope",
      "blocking": false
    }
  ],
  "verification": {
    "mcp_recover_workflow_fail_closed": "pass",
    "historical_protocol_mutation_matrix": "pass",
    "corrupt_header_nonterminal_fences": "pass",
    "root_prompt_protocol_fence": "pass",
    "root_protocol_loader_multi_association": "pass",
    "design_preflight_corrupt_header_race": "pass",
    "recovery_authorization_protocol_fence": "pass",
    "completion_and_complete_work_matrices": "pass"
  },
  "scope_notes": [
    "T4-GROK-I1 closed with listener-level regression and no successor growth",
    "Same producer also closes Codex multi-association loader and Design preflight typed-header findings",
    "Remaining minors are non-blocking for Task 5"
  ]
}
```

## Conclusion

**approve_with_minors** — Fix `3f0fb8f4` closes **T4-GROK-I1**: MCP `recover_workflow` is fail-closed through the fenced store core with no auto-restart or successor creation, proven by an independent listener regression plus the original Task 4 matrices still green. Four non-blocking minors remain (root companion auto-restart, snapshot breadth, unlinked prompt APIs, pre-existing fixture debt). Task 4 is clear for dual-review gate purposes pending Codex concurrence on this producer.

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"aac0bcf4-09ac-482e-af17-9d2a2f28a960","producer_commit":"3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c","verdict":"approve_with_minors","critical":0,"important":0,"minor":4,"summary":"Fix 3f0fb8f4 closes T4-GROK-I1: MCP recover_workflow no longer auto-restarts historical v1 and reaches the fenced core with zero successor growth; multi-association root loader and Design preflight corrupt-header races also green. Four non-blocking minors remain.","report_file":".superpowers/sdd/task-4-review-grok-report-r2.md"}
-->
