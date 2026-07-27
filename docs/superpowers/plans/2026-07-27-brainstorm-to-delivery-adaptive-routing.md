# Brainstorm-to-Delivery Adaptive Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every B2D Plan come from an independent Codex Plan Author,
reduce repeated Plan review to owners of unresolved high-severity findings,
and route each implementation Task through a pre-recorded normal or high-risk
cohort with strict latest-artifact review coverage.

**Architecture:** Replace the unused manifest v1 contract atomically with v2.
Keep policy and route declarations in immutable manifest revisions, put Plan
finding/stagnation derivation in a small pure module, persist each derived Plan
review round on the immutable gate settlement, enforce work-unit independence
and cohort freezing at admission, and project route-dependent reviewer fan-out
to the existing deterministic workflow UI. The B2D Skill remains the
orchestrator; generic `writing-plans` and `subagent-driven-development` Skills
are invoked rather than copied.

**Tech Stack:** Rust 2021, SeaORM + SQLite, serde/serde_json, Axum/Tauri shared
backend, stdio MCP JSON-RPC, React 19, TypeScript strict, Zustand, next-intl,
Vitest, Tailwind CSS v4.

**Design baseline:**
`docs/superpowers/specs/2026-07-27-brainstorm-to-delivery-adaptive-routing-design.md`

## Global Constraints

- Implement in an isolated worktree and keep Task execution serial. Do not
  parallelize production edits that share workflow manifest, admission, or
  projection state.
- Follow RED-GREEN-REFACTOR. Every production behavior change starts with a
  focused test that is observed failing for the intended reason.
- Manifest and capability are v2-only: `schema_version = 2`,
  `workflow_manifest_v2`, and feature token `workflow_v2`. Do not retain a v1
  parser, feature alias, capability fallback, dual-write path, or legacy mode.
- Preserve the existing final whole-branch reviewer/fixer rules. Adaptive
  routing applies to Plan review and Task cohorts only.
- The parent coordinates, adjudicates, settles, and recovers. It must never
  write/revise the Plan or implement Task code.
- A Task route is immutable after any cohort member is admitted. Newly
  discovered evidence that makes the recorded risk wrong blocks and escalates;
  it never mutates or downgrades the admitted cohort.
- Keep detailed paths/evidence out of the redacted frontend graph. The agent
  recovery DTO retains them.
- Do not add a cost database table, cost API, or cost UI. Measurement reuses
  conversation usage, run timestamps, manifest revisions, and gate cycles.
- Grok's serialized `tools/list` JSONL frame must remain `<= 7,680` bytes. The
  literal budget must not be raised to accommodate v2.
- Keep `SKILL.md` concise and imperative. Put machine-enforceable structure in
  Rust/JSON validation, not duplicated prose.
- Use one focused commit per Task. Do not push or open a PR unless separately
  requested.
- Every command block is executed from the directory named by its step. Do not
  rely on a working directory carried over from an earlier command block.
- Every filtered test command must report at least one matching test. A
  zero-test success is a verification failure, not GREEN evidence.

## Canonical V2 Decisions

- `plan_target_rel_path` and `risk_policy_version` are required on every v2
  manifest. The exact policy version is `b2d_task_risk_v1`.
- A skeleton contains exactly one Codex `author` work unit keyed by the target
  Plan path. It contains no Plan document, Task nodes, or Task policies.
- Once `plan` is present, its normalized path must equal the target path. Every
  Task node index has exactly one matching `task_policy`.
- A Plan gate stores both `reviewer_cohort_node_ids` (the complete configured
  group) and `required_reviewer_node_ids` (this cycle's full group or owner
  subset).
- Normal Task route: one Grok implementer and one Codex reviewer.
- High Task route: one Codex implementer and exactly one Codex plus one Grok
  reviewer. Both reviewers must approve the same latest producer `task_id` and
  non-empty artifact digest.
- Card-summary marker version remains independent of manifest capability. Keep
  `codeg-card-summary-v1`, add bounded role-specific fields, and enforce their
  presence by workflow role. Do not make unrelated delegation summaries
  incompatible merely to rename the marker.
- Rename runtime/storage vocabulary from `pair_frozen` to `cohort_frozen`.
  Historical migration source may still mention the old column it originally
  created; active entities, code, DTOs, errors, tests, and Skill text may not.

## Task Routing Matrix

Policy version for every row: `b2d_task_risk_v1`.

| Task | Planned production surface | Hard triggers | Soft signals | Risk and recorded reason | Route |
| --- | --- | --- | --- | --- | --- |
| 1 | Manifest types, keys, validator | `public_compatibility`: replaces the serialized manifest/key grammar | `broad_production_surface` +1, `shared_interface` +1 | **high**: a hard trigger changes the public workflow schema and canonical work-unit keys | Codex implementer; independent Codex + Grok reviewers |
| 2 | Pure Plan finding/stagnation state machine | `concurrency_lifecycle`: changes ordering and lifecycle transitions across review cycles | none | **high**: a hard trigger controls rewrite and user-escalation lifecycle decisions | Codex implementer; independent Codex + Grok reviewers |
| 3 | SQLite migration, entities, card-summary wire | `migration_destructive_persistence`; `public_compatibility` | `cross_runtime_or_process` +2, `broad_production_surface` +1, `multiple_ownership_modules` +1, `shared_interface` +1 | **high**: hard triggers change durable schema and serialized Rust/TypeScript evidence | Codex implementer; independent Codex + Grok reviewers |
| 4 | Publish/settle/recovery store | `concurrency_lifecycle`: CAS and gate-cycle ordering; `migration_destructive_persistence`: durable settlement semantics | `shared_interface` +1 | **high**: hard triggers change authoritative workflow transitions and recovery records | Codex implementer; independent Codex + Grok reviewers |
| 5 | Admission, independence, Author coverage, cohort freeze | `concurrency_lifecycle`: first-admission and continuation ordering | `multiple_ownership_modules` +1, `shared_interface` +1 | **high**: a hard trigger changes admission/freeze lifecycle and reviewer identity fences | Codex implementer; independent Codex + Grok reviewers |
| 6 | Multi-reviewer execution gates and backend projection | `concurrency_lifecycle`: concurrent reviewer join and latest-artifact invalidation | `shared_interface` +1 | **high**: a hard trigger changes Task completion from one reviewer to strict cohort AND | Codex implementer; independent Codex + Grok reviewers |
| 7 | MCP feature/capability/transport/schema | `public_compatibility` | `cross_runtime_or_process` +2, `broad_production_surface` +1, `multiple_ownership_modules` +1, `shared_interface` +1 | **high**: a hard trigger atomically replaces a root MCP protocol across process boundaries | Codex implementer; independent Codex + Grok reviewers |
| 8 | Frontend graph types, store, UI, translations | none | `broad_production_surface` +1, `multiple_ownership_modules` +1, `shared_interface` +1 | **high** (score 3): graph DTO, state derivation, UI, and i18n must agree on route fan-out | Codex implementer; independent Codex + Grok reviewers |
| 9 | B2D Skill and deterministic Skill contract validator | none | none (documentation/scripts only, no production surface) | **normal** (score 0): behavior is constrained by already-tested v2 platform contracts | Grok implementer; independent Codex reviewer |
| 10 | Full verification and measurement report | none | none (verification/report only) | **normal** (score 0): runs existing checks and records evidence without changing runtime behavior | Grok verifier; independent Codex evidence reviewer |

If a Task's actual file/interface evidence exceeds its row before first
admission, revise this Plan materially and run the complete Plan reviewer
cohort. Do not silently reclassify it in the implementation prompt.

---

### Task 1: Define Manifest V2, Risk Policy, Routes, and Plan Author Keys

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/key.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/validate.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs` (exhaustive
  key/role plumbing only; Task 5 owns Author admission behavior)
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (exhaustive
  key/role plumbing only)
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs` (exhaustive role
  serialization only)
- Test: the same three Rust modules

**Interfaces:**

```rust
pub const MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const TASK_RISK_POLICY_VERSION: &str = "b2d_task_risk_v1";

pub enum ManifestNodeRole {
    Author,
    Reviewer,
    Implementer,
    Fixer,
}

pub enum WorkUnitKeyParts<'a> {
    Design {
        rel_doc_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    PlanAuthor {
        rel_plan_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    PlanReviewer {
        rel_plan_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskImplementer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskReviewer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalReviewer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalFixer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
}

pub enum TaskRiskLevel { Normal, High }
pub enum TaskHardTriggerKind {
    ConcurrencyLifecycle,
    SecurityTrustBoundary,
    MigrationDestructivePersistence,
    PublicCompatibility,
    UnsafeFfi,
    UpdateRollback,
}
pub enum TaskSoftSignalKind {
    CrossRuntimeOrProcess,
    BroadProductionSurface,
    MultipleOwnershipModules,
    SharedInterface,
    DependencyOrBuild,
    MultiLayerWithoutTestSeam,
}

pub struct ManifestTaskPolicy {
    pub task_index: u32,
    pub risk: ManifestTaskRisk,
    pub route: ManifestTaskRoute,
}

pub struct ManifestTaskHardTrigger {
    pub kind: TaskHardTriggerKind,
    pub evidence: Vec<String>,
}

pub struct ManifestTaskSoftSignal {
    pub kind: TaskSoftSignalKind,
    pub score: u32,
    pub evidence: Vec<String>,
}

pub struct ManifestTaskRisk {
    pub level: TaskRiskLevel,
    pub hard_triggers: Vec<ManifestTaskHardTrigger>,
    pub soft_signals: Vec<ManifestTaskSoftSignal>,
    pub score: u32,
    pub reason: String,
}

pub struct ManifestTaskRoute {
    pub implementer_node_id: String,
    pub reviewer_node_ids: Vec<String>,
}
```

Add required `plan_target_rel_path`, `risk_policy_version`, and `task_policies`
fields to `ManifestDocument`. Add required `reviewer_cohort_node_ids` alongside
`required_reviewer_node_ids` on `ManifestGate`; retain the remaining graph and
gate fields unchanged.

Replace the old role-less `WorkUnitKeyParts::Plan` and
`ParsedWorkUnitKey::Plan` variants with explicit `PlanAuthor` and
`PlanReviewer` variants. The old Plan key grammar must fail parsing; it is not
an alias. Update exhaustive matches in admission, store, and projection so this
Task compiles, but keep Author admission fail-closed until Task 5 adds its
durable behavior.

Canonical keys added by this Task:

```text
plan|{normalized-relative-plan-path}|author|codex|{profile-or-none}
plan|{normalized-relative-plan-path}|reviewer|{agent}|{profile-or-none}
```

- [ ] **Step 1: Add failing key/serde tests**

Add `plan_author_key_round_trips`, `v1_manifest_is_rejected`, and a serde
fixture proving all new required v2 fields are required rather than defaulted.

- [ ] **Step 2: Add failing validator table tests**

Cover at least:

- skeleton requires one Codex Author and no Plan/Task policies;
- eventual Plan path must equal `plan_target_rel_path`;
- unknown/duplicate/evidence-free risk signals fail;
- every hard trigger forces `high`;
- soft totals 0, 1, and 2 are normal; 3 and above are high;
- submitted soft score must equal the unique signal weights;
- every estimated/approved Task index has exactly one policy;
- normal route is Grok + one Codex reviewer;
- high route is Codex + distinct Codex/Grok reviewers;
- Plan `required_reviewer_node_ids` is non-empty and a subset of the complete
  cohort; Design self-review keeps both sets empty;
- no route node can be omitted, duplicated, or point at the wrong Task index.

- [ ] **Step 3: Run focused tests and verify RED**

From `src-tauri/`:

```powershell
cargo test --features test-utils workflow::key -- --nocapture
cargo test --features test-utils workflow::validate -- --nocapture
```

Expected: FAIL because schema v2 types, Author keys, risk arithmetic, and
route-dependent validation do not exist.

- [ ] **Step 4: Implement the minimal v2 types, key grammar, and validation**

Use typed serde enums for known signals, a single weight function for soft
signals, and one route validator keyed by the derived risk. Normalize evidence
strings as bounded non-empty text; do not attempt repository semantic analysis
in the backend.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run the two Step 3 commands again.

Expected: PASS, including explicit rejection of schema 1.

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/key.rs src-tauri/src/acp/delegation/workflow/validate.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/store.rs
git commit -m "feat(workflow): define manifest v2 task routing"
```

---

### Task 2: Derive Plan Finding Ownership and Stagnation in Pure Logic

**Files:**

- Create: `src-tauri/src/acp/delegation/workflow/plan_review.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Test: `src-tauri/src/acp/delegation/workflow/plan_review.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewScope { Full, Scoped }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRevisionKind { Initial, Localized, Material, HolisticRewrite }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity { Critical, Important, Minor }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus { Open, Resolved, New, Reopened }

pub struct PlanFindingUpdate {
    pub finding_id: String,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub owner_reviewer_node_ids: Vec<String>,
    pub summary: String,
    pub evidence_ref: String,
    pub report_file: String,
}

pub struct PlanReviewRoundSubmission {
    pub scope: PlanReviewScope,
    pub revision_kind: PlanRevisionKind,
    pub scope_reason: String,
    pub covered_author_task_id: String,
    pub covered_plan_digest: String,
    pub required_reviewer_node_ids: Vec<String>,
    pub finding_updates: Vec<PlanFindingUpdate>,
    pub lineage_reset_reason: Option<String>,
}

pub enum PlanReviewNextAction {
    ContinueReview,
    HolisticRewriteRequired,
    UserDecisionRequired,
    Approved,
}

pub fn derive_plan_review_round(
    prior: Option<&PlanReviewRoundState>,
    reviewer_cohort_node_ids: &[String],
    submission: &PlanReviewRoundSubmission,
) -> Result<PlanReviewRoundState, PlanReviewError>;
```

Bound IDs, summaries, evidence references, owner sets, finding count, and JSON
size with constants local to this module. Treat `new` and `reopened` as open.

- [ ] **Step 1: Write failing finding-ledger tests**

Add tests for stable ID reuse, duplicate owner union, illegal severity mutation,
unknown owners, owner-subset derivation, all-owner resolution, new findings on a
scoped round, minor-only approval, and material/ambiguous revisions restoring
the full cohort.

- [ ] **Step 2: Write failing stagnation boundary tests**

Add tests named at least:

- `first_full_round_establishes_baseline_without_stagnation`
- `lower_blocking_total_without_critical_increase_is_improvement`
- `new_critical_is_not_improvement_even_when_total_falls`
- `two_non_improving_rounds_require_one_holistic_rewrite`
- `post_rewrite_round_compares_with_pre_rewrite_counts`
- `second_stagnation_pair_requires_user_decision`
- `requirements_lineage_reset_requires_reason_and_resets_baseline`

Use the approved exact rule: relative to the preceding completed round, net
improvement is true only when open Critical count `C` does not increase and
`C + I` strictly decreases. Minor findings are excluded from convergence.
Incomplete or infrastructure-failed rounds do not change the counter.

- [ ] **Step 3: Run the focused module and verify RED**

```powershell
cargo test --features test-utils workflow::plan_review -- --nocapture
```

Run from `src-tauri/`.

Expected: FAIL because the module and transition function are missing.

- [ ] **Step 4: Implement deterministic merge and transition logic**

Derive open counts, net improvement, stagnation count, `rewrite_used`, required
owner subset, and next action from prior immutable state plus the submitted
finding updates. Never accept parent-supplied counts/counters/booleans.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run the Step 3 command again. Expected: PASS.

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/acp/delegation/workflow/plan_review.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "feat(workflow): derive adaptive plan review state"
```

---

### Task 3: Persist V2 Review Evidence and Add Role-Specific Card Summaries

**Files:**

- Create: `src-tauri/src/db/migration/m20260727_000003_workflow_manifest_v2.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/delegation_workflow_node_binding.rs`
- Modify: `src-tauri/src/db/entities/delegation_workflow_gate_settlement.rs`
- Test: `src-tauri/tests/delegation_workflows_migration.rs`
- Modify: `src-tauri/src/acp/delegation/card_summary.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/delegation-run-snapshot.ts`
- Modify: `src/lib/delegation-run-snapshot.test.ts`
- Modify: `src/components/message/delegation-run-summary.tsx`
- Create: `src/components/message/delegation-run-summary.test.tsx`
- Modify: all ten `src/i18n/messages/*.json` locale files

**Persistence shape:**

```sql
ALTER TABLE delegation_workflow_node_bindings
  RENAME COLUMN pair_frozen TO cohort_frozen;

ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN review_scope TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN revision_kind TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN scope_reason TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN required_reviewer_node_ids_json TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN covered_author_task_id TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN covered_plan_digest TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN finding_ledger_json TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN net_improvement INTEGER NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN stagnation_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN rewrite_used INTEGER NOT NULL DEFAULT 0;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN next_action TEXT NULL;
ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN report_files_json TEXT NULL;
```

Use CHECK constraints where SQLite can express the finite enum/boolean domain.
Design settlement rows leave Plan-only columns null/defaulted. Do not transform
v1 manifest JSON into v2.

**Card-summary shape:**

```rust
pub enum CardSummary {
    Review {
        verdict: ReviewVerdict,
        critical: u32,
        important: u32,
        minor: u32,
        summary: String,
        report_file: Option<String>,
    },
    Author {
        status: WorkStatus,
        summary: String,
        plan_digest: String,
        report_file: String,
    },
    Implementation {
        phase: ImplementationPhase,
        status: WorkStatus,
        summary: String,
        commits: Vec<CommitEntry>,
        tests: Option<TestsSummary>,
        concerns: Vec<String>,
        report_file: Option<String>,
    },
}
```

The generic parser accepts an optional review report path for non-B2D callers;
workflow role validation in Task 5 requires it for Plan/Task reviewers. Author
digest/report path are always required and bounded.

- [ ] **Step 1: Write the migration RED test**

Apply migrations only through `m20260727_000002`, seed a binding with
`pair_frozen = 1`, apply the new migration, then assert:

- `cohort_frozen` exists and preserves `1`;
- `pair_frozen` no longer exists on the migrated schema;
- all Plan settlement evidence columns exist with safe defaults;
- a complete fresh migration exposes the same schema.

- [ ] **Step 2: Write card-summary and frontend normalization RED tests**

Test valid Author evidence, missing/empty digest, absolute/parent report paths,
review report paths, and frontend preservation of Author/Review summaries.
Render an Author summary once to ensure the union does not fall through to the
implementation branch.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test delegation_workflows_migration -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --features test-utils card_summary -- --nocapture
pnpm test -- src/lib/delegation-run-snapshot.test.ts src/components/message/delegation-run-summary.test.tsx
```

Run from the repository root.

Expected: FAIL on the missing migration, fields, and Author summary variant.

- [ ] **Step 4: Implement migration, entities, parsers, and compact display**

Keep path validation shared on the Rust side by exposing the existing bounded
workspace-relative validator as `pub(crate)`. Mirror the exact bounds in the
frontend defense-in-depth normalizer. Add matching translation keys to all ten
locales and rely on `src/i18n/messages.test.ts` for key parity.

- [ ] **Step 5: Run focused tests and verify GREEN**

From the repository root, run all Step 3 commands plus:

```powershell
pnpm test -- src/i18n/messages.test.ts
```

Expected: PASS.

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/db/migration src-tauri/src/db/entities/delegation_workflow_node_binding.rs src-tauri/src/db/entities/delegation_workflow_gate_settlement.rs src-tauri/tests/delegation_workflows_migration.rs src-tauri/src/acp/delegation/card_summary.rs src-tauri/src/acp/delegation/workflow/project.rs src/lib/types.ts src/lib/delegation-run-snapshot.ts src/lib/delegation-run-snapshot.test.ts src/components/message/delegation-run-summary.tsx src/components/message/delegation-run-summary.test.tsx src/i18n/messages
git commit -m "feat(workflow): persist v2 review evidence"
```

---

### Task 4: Integrate V2 Publish, Plan Settlement, and Recovery State

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/state_dto.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Test: `src-tauri/src/acp/delegation/workflow/store.rs`

**Interfaces:**

```rust
pub enum SettleGateEvidence {
    Design {
        critical_count: i64,
        important_count: i64,
        minor_count: i64,
    },
    Plan(PlanReviewRoundSubmission),
}

pub struct SettleWorkflowRequest {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub gate_id: String,
    pub expected_graph_revision: u64,
    pub gate_cycle: u64,
    pub outcome: GateSettlementOutcome,
    pub evidence: SettleGateEvidence,
    pub summary: String,
}

pub struct SettleResult {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_cycle: u64,
    pub graph_revision: u64,
    pub outcome: GateSettlementOutcome,
    pub idempotent_replay: bool,
    pub plan_next_action: Option<PlanReviewNextAction>,
    pub critical_count: i64,
    pub important_count: i64,
    pub minor_count: i64,
    pub stagnation_count: u32,
    pub rewrite_used: bool,
}
```

Extend `WorkflowStateDto` with normalized target path, policy version, complete
Task policies/routes, and latest Plan review state. Extend node evidence with
`cohort_frozen`, child conversation ID, reviewed task ID, verdict, digest, and
report path. Extend gate evidence with complete cohort plus current required
subset. Current open findings and stagnation state are never truncated.

- [ ] **Step 1: Write failing publish/fingerprint tests**

Cover v2 skeleton publish, estimated publish after Author, policy/route changes
changing the Plan fingerprint, required-subset changes invalidating stale gate
runs, complete cohort persistence, v1 rejection, and idempotent CAS replay.

- [ ] **Step 2: Write failing Plan settlement tests**

Cover:

- full initial round derives counts and baseline;
- scoped round requires the exact owner subset from the active manifest;
- material/holistic revisions require the complete cohort;
- all required reviewer bindings cover the same Author task/digest;
- parent-supplied aggregate counts are absent from the Plan branch;
- two stagnant rounds return `holistic_rewrite_required` once;
- the second stagnant pair returns `user_decision_required` and blocks;
- same-cycle same-payload replay is idempotent, while any structured evidence
  difference conflicts;
- approval with any open Critical/Important finding fails.

- [ ] **Step 3: Write failing recovery tests**

Assert that `get_workflow_state` returns Author evidence, full/required Plan
reviewer sets, finding owners/statuses, stagnation/rewrite state, every risk
reason/signal/evidence/score, complete Task routes, reviewer coverage, report
paths, and `cohort_frozen` without leaking these details into the graph DTO.

- [ ] **Step 4: Run the store module and verify RED**

```powershell
cargo test --features test-utils workflow::store -- --nocapture
```

Run from `src-tauri/`.

Expected: FAIL because store requests, fingerprints, settlements, persistence,
and recovery still encode v1 aggregate/pair behavior.

- [ ] **Step 5: Implement v2 store integration**

Call `derive_plan_review_round` inside the settlement transaction after fresh
reviewer evidence validation and before inserting the immutable row. Serialize
only bounded validated structures. Include target, Author, complete cohort,
Task policy, and route material in the Plan fingerprint. Keep Design settlement
behavior and final execution projection unchanged.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run the Step 4 command again. Expected: PASS.

- [ ] **Step 7: Commit**

```powershell
git add src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/state_dto.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "feat(workflow): integrate v2 plan review state"
```

---

### Task 5: Enforce Independent Author/Reviewer Sessions and Frozen Task Cohorts

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Test: the same Rust modules

**Interfaces:**

```rust
pub struct WorkflowAdmitInput<'a> {
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub task_id: &'a str,
    pub work_unit_key: Option<&'a str>,
    pub agent_type: &'a str,
    pub profile_id: Option<&'a str>,
    pub lineage_root_task_id: &'a str,
    pub generation: i64,
    pub kind: AdmissionDispatchKind,
    pub admission_class: AdmissionClass,
    pub workspace_path: Option<&'a str>,
}

async fn ensure_child_conversation_independent<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    node_id: &str,
    child_conversation_id: i32,
) -> Result<(), TaskStoreError>;

async fn mark_observed_and_freeze_cohort<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    task_index: i64,
    route_node_ids: &[String],
) -> Result<(), TaskStoreError>;
```

Admission behavior:

```text
PlanAuthor: skeleton/estimated allowed -> terminal Author summary stamps Plan digest
PlanReviewer: Plan exists -> stamp latest Author task_id + exact Plan digest
TaskImplementer: approved manifest -> enforce recorded risk route
TaskReviewer: stamp latest implementer task_id + non-empty artifact digest
Any first cohort admission: freeze policy + all route node identities
Any reused child conversation on another work unit: reject reviewer_not_independent
```

Continue/replacement may reuse or replace its own work unit conversation under
existing lineage budgets, but the chosen conversation may not belong to any
other work unit in the workflow.

- [ ] **Step 1: Write failing Plan Author admission tests**

Cover skeleton Author admission before a Plan digest exists, rejection of a
non-Codex Author, Author continuation for revisions, reviewer-before-Author
rejection, and Plan reviewer bindings stamped with the newest Author task ID
and digest.

- [ ] **Step 2: Write failing independence tests**

Use real persisted run/binding rows to prove Author/reviewer, two Plan
reviewers, implementer/reviewer, two high-risk reviewers, Task/Final, and two
different Task work units cannot share a child conversation. Prove a legal
continuation of the same work unit still succeeds.

- [ ] **Step 3: Write failing route/freeze tests**

Cover normal/high admission, exact agent/profile matching, two- and three-node
`cohort_frozen`, route/policy removal after any member admission, pre-admission
material risk revision, and post-admission mutation returning typed
`cohort_frozen` without changing persisted bindings.

- [ ] **Step 4: Write failing role-summary tests**

Require Author summaries on Author nodes, Review summaries with report paths on
Plan/Task reviewer nodes, Implementation summaries on Task implementer/fixer
nodes, and non-empty producer digests before reviewer admission.

- [ ] **Step 5: Run admission/run-store tests and verify RED**

```powershell
cargo test --features test-utils workflow::admission -- --nocapture
cargo test --features test-utils run_store -- --nocapture
```

Run from `src-tauri/`.

Expected: FAIL on missing Author behavior, child-conversation fencing,
role-aware summaries, and cohort freezing.

- [ ] **Step 6: Implement admission and publication fences**

Query run bindings joined to durable delegation runs inside the same admission
transaction for conversation independence. Resolve Task cohort node IDs from
the active validated policy, not all nodes that happen to share an index.
Generalize store mutation protection from pair membership to the frozen policy
and complete route.

- [ ] **Step 7: Run focused tests and verify GREEN**

Run both Step 5 commands again. Expected: PASS.

- [ ] **Step 8: Commit**

```powershell
git add src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/workflow/error.rs
git commit -m "feat(workflow): enforce independent routed cohorts"
```

---

### Task 6: Require Every Task Reviewer and Project Route Fan-Out

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/gates.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/dto.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Test: `src-tauri/src/acp/delegation/workflow/gates.rs`
- Test: `src-tauri/src/acp/delegation/workflow/project.rs`

**Gate interface:**

```rust
pub struct RequiredReviewerEvidence {
    pub node_id: String,
    pub evidence: Option<ExecutionGateRunEvidence>,
}

pub struct ExecutionGateInput {
    pub kind: ExecutionGateKind,
    pub implementer_or_fixer: Option<ExecutionGateRunEvidence>,
    pub required_reviewers: Vec<RequiredReviewerEvidence>,
    pub branch_tip_digest: Option<String>,
}

fn evaluate_task_gate(input: &ExecutionGateInput) -> ExecutionGateEval {
    let producer = require_passing_producer(input)?;
    require_non_empty_digest(producer)?;
    for required in &input.required_reviewers {
        let reviewer = required.evidence.as_ref()
            .ok_or_else(|| missing(&required.node_id))?;
        require_passing_review(reviewer)?;
        reviewer_covers_producer(reviewer, producer)?;
    }
    pass()
}
```

Keep Final's required set at exactly one reviewer and preserve its first-pass
branch-tip behavior.

**Redacted graph additions on Task nodes:**

```rust
pub task_risk_level: Option<String>,
pub task_risk_reason_codes: Vec<String>,
pub required_reviewer_count: Option<u64>,
pub returned_reviewer_count: Option<u64>,
```

Reason codes are hard/soft signal names only. Do not expose evidence paths or
free-form risk reasons in `WorkflowGraphSnapshot`.

- [ ] **Step 1: Write failing pure gate tests**

Cover normal one-reviewer pass and high two-reviewer pass, then reject each of:
missing second reviewer, failed/canceled/non-terminal reviewer, stale task ID,
empty/mismatched digest, one approval plus one request-changes, and an approval
for the prior fixer artifact. Prove a new producer artifact invalidates both
prior approvals.

- [ ] **Step 2: Run gate tests and verify RED**

```powershell
cargo test --features test-utils workflow::gates -- --nocapture
```

Run from `src-tauri/`.

Expected: FAIL because the evaluator accepts only one reviewer and permits the
old empty-digest fallback.

- [ ] **Step 3: Implement the required-reviewer collection**

Return reviewer-specific failure details for diagnostics while preserving a
stable coarse reason code for projection.

- [ ] **Step 4: Write failing projection tests**

Build normal and high manifest fixtures. Assert reviewer fan-out follows the
policy node IDs, both high reviewers point at the same producer, returned /
required counts are 0/2, 1/2, and 2/2, stale reviewers are demoted, overall
completion waits for strict AND, Author appears before Plan reviewers, and
Final projection remains unchanged.

- [ ] **Step 5: Run projection tests and verify RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils workflow::project -- --nocapture
```

Expected: FAIL because projection still builds `(implementer, reviewer)` pairs.

- [ ] **Step 6: Project by normalized policy routes and verify GREEN**

Use node IDs from each `ManifestTaskPolicy` as authority. Never select an
arbitrary `.one()` reviewer by role/task index. Run both Step 2 and Step 5
commands; expected PASS.

- [ ] **Step 7: Commit**

```powershell
git add src-tauri/src/acp/delegation/workflow/gates.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/dto.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "feat(workflow): require all task reviewers"
```

---

### Task 7: Atomically Replace the Root MCP Workflow Capability with V2

**Files:**

- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/bin/codeg_mcp.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Test: the same Rust modules

**Capability contract:**

```rust
pub const WORKFLOW_V2_TOOLS: &[&str] = &[
    "get_workflow_capabilities",
    "get_workflow_state",
    "publish_workflow_manifest",
    "settle_workflow_gate",
];

pub enum WorkflowCapabilityMode {
    Unavailable,
    WorkflowManifestV2,
    Inconsistent,
}
```

Rename the existing `CompanionFeatures::workflow_v1` field to `workflow_v2`;
retain the other feature-group fields unchanged. Set
`WORKFLOW_CAPABILITY_VERSION` in workflow store to `workflow_manifest_v2` in
this same atomic change.

`workflow_v1` is an unknown/ignored feature token and never enables workflow
tools. Catalog absence is unavailable and causes the B2D Skill to block; it is
not a legacy execution mode. Tool names remain stable.

**Compact schema direction:**

```json
{
  "name": "publish_workflow_manifest",
  "inputSchema": {
    "type": "object",
    "required": [
      "schema_version", "workflow_kind", "publication_token",
      "workflow_state", "plan_target_rel_path", "risk_policy_version",
      "task_policies", "phases", "nodes", "edges", "gates"
    ],
    "properties": {
      "schema_version": { "type": "integer", "const": 2 },
      "plan_target_rel_path": { "type": "string" },
      "risk_policy_version": { "type": "string", "const": "b2d_task_risk_v1" },
      "task_policies": { "type": "array" }
    }
  }
}
```

Keep nested manifest and Plan finding validation in Rust rather than expanding
every nested property in `tools/list`. The settle tool must still name the
full/scoped, revision-kind, Author coverage, reviewer-set, finding-update, and
report-path fields in a compact description/schema so an agent can construct a
valid request.

- [ ] **Step 1: Write failing feature/capability tests**

Assert root `workflow_v2` exposes exactly all four operations and reports only
`workflow_manifest_v2`; children expose none; missing or partial catalogs are
not usable; `workflow_v1` does not enable anything; persisted headers stamp v2.

- [ ] **Step 2: Write failing publish/settle round-trip tests**

Send a minimal v2 skeleton and a structured Plan settlement through companion
-> transport -> listener -> store. Assert v1 documents and omitted/contradictory
Plan evidence return stable typed errors such as `risk_assessment_invalid`,
`task_route_mismatch`, `reviewer_set_mismatch`, `reviewed_task_stale`,
`artifact_digest_mismatch`, and `cohort_frozen`.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --features test-utils workflow_v2 -- --nocapture
cargo test --features test-utils workflow_manifest_v2 -- --nocapture
```

Run from `src-tauri/`.

Expected: FAIL because feature parsing, capability payload, broker request, and
listener/store mapping still use v1.

- [ ] **Step 4: Rename and wire the capability atomically**

Update CLI help, launch feature assembly, role gating, local discovery,
transport structs, listener parsing/error mapping, comments, and tests in one
commit. Do not accept both feature tokens during transition.

- [ ] **Step 5: Preserve the fixed Grok catalog budget**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --nocapture
cargo test --features test-utils tool_schema_retains_essential_agent_guidance -- --nocapture
```

Expected: PASS with the existing literal `7_680`. If the first test fails,
compact duplicated descriptions/schema text; do not raise the limit or remove
required v2 fields.

- [ ] **Step 6: Verify the codeg-mcp target**

Run from `src-tauri/`:

```powershell
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: both pass without warnings.

- [ ] **Step 7: Re-run the v2 protocol tests and verify GREEN**

Run both Step 3 commands again from `src-tauri/`.

Expected: PASS through companion, transport, listener, and store with v1
documents and partial catalogs still rejected.

- [ ] **Step 8: Commit**

```powershell
git add src-tauri/src/acp/connection.rs src-tauri/src/bin/codeg_mcp.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/transport.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/workflow/store.rs
git commit -m "feat(mcp): expose workflow manifest v2"
```

---

### Task 8: Render Plan Author and Adaptive Task Reviewer Fan-Out

**Files:**

- Modify: `src/lib/types.ts`
- Modify: `src/lib/workflow-graph-store.ts`
- Modify: `src/lib/workflow-graph-store.test.ts`
- Modify: `src/components/chat/workflow-graph-panel.tsx`
- Modify: `src/components/chat/workflow-node-detail.tsx`
- Modify: `src/components/chat/workflow-overlay.test.tsx`
- Modify: all ten `src/i18n/messages/*.json` locale files

**Frontend contract:**

Add `task_risk_level?: "normal" | "high" | null`,
`task_risk_reason_codes: string[]`,
`required_reviewer_count?: number | null`, and
`returned_reviewer_count?: number | null` to the existing
`WorkflowNodeSnapshot` interface.

Task lane rendering groups nodes by `task_index` into one stable row. Render
the implementer first, then a visible reviewer fan-out sorted by policy order,
then a compact returned/required count. The Plan lane renders Author before its
reviewer cohort. Node detail shows a localized risk level and safe reason-code
labels; it never receives free-form evidence/path text.

- [ ] **Step 1: Write failing graph-store tests**

Cover Task progress with one and two reviewers, an active earlier reviewer
preventing the current Task from advancing, returned/required counts, Author
ordering, and safe default handling for older/observed-only snapshots without
the new optional counters.

- [ ] **Step 2: Write failing component tests**

Render normal and high snapshots. Assert one Task row, two reviewer branches on
high, `1 / 2` then `2 / 2`, high-risk badge and reason codes in detail, no
evidence paths, keyboard activation, and no text overflow at narrow layout.

- [ ] **Step 3: Run focused frontend tests and verify RED**

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts src/components/chat/workflow-overlay.test.tsx
```

Run from the repository root.

Expected: FAIL because current UI renders a flat list and lacks v2 risk/count
fields.

- [ ] **Step 4: Implement deterministic grouped rendering**

Reuse existing badges, buttons, spacing, and phase lanes. Do not add nested
cards, a force-directed graph, decorative visuals, or a new cost panel. Keep
fixed row/button dimensions so count and badge changes do not shift the lane.

- [ ] **Step 5: Add all locale keys and verify GREEN**

Run from the repository root:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts src/components/chat/workflow-overlay.test.tsx src/i18n/messages.test.ts
pnpm eslint src/lib/types.ts src/lib/workflow-graph-store.ts src/components/chat/workflow-graph-panel.tsx src/components/chat/workflow-node-detail.tsx
```

Expected: PASS with no missing translations or lint errors.

- [ ] **Step 6: Commit**

```powershell
git add src/lib/types.ts src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts src/components/chat/workflow-graph-panel.tsx src/components/chat/workflow-node-detail.tsx src/components/chat/workflow-overlay.test.tsx src/i18n/messages
git commit -m "feat(workflow): render adaptive task cohorts"
```

---

### Task 9: Rewrite and Pressure-Test the B2D Skill

**Required Skills:**

- `superpowers:writing-skills`
- `superpowers:test-driven-development`
- system `skill-creator`

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Create: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- Verify/update only if stale: `.agents/skills/brainstorm-to-delivery/agents/openai.yaml`

**Target Skill shape:**

```markdown
## Plan production

1. Publish a v2 skeleton with the target path and fresh Codex Author work unit.
2. Dispatch that Author and require complete `writing-plans` behavior.
3. Publish the resulting Plan digest, Task Routing Matrix, policies, routes,
   complete Plan reviewer cohort, and full initial required set.
4. Re-review localized revisions with owners of open Critical/Important
   findings; restore the complete cohort for material/ambiguous changes.
5. On two non-improving rounds, request one holistic Author rewrite. On the
   second stagnant pair after it, block and ask the user.

## Task route

- normal: Grok implementer/fixer -> independent Codex reviewer
- high: Codex implementer/fixer -> independent Codex AND Grok reviewers
- every review covers the latest producer task ID and non-empty digest
```

Keep the body below 500 lines, imperative, and free of repeated full JSON
schemas already exposed by tools/validation. Retain one excellent end-to-end
example and one quick-reference/rationalization table. Keep the frontmatter
description trigger-only.

- [ ] **Step 1: Create a deterministic contract test before editing the Skill**

The validator must assert required v2 terms/sections and reject at least:

```js
const forbidden = [
  /workflow_manifest_v1/,
  /schema_version\s*[=:]\s*1/,
  /pair_frozen/,
  /mode\s*=\s*legacy/i,
]

const required = [
  /Codex Plan Author/,
  /writing-plans/,
  /b2d_task_risk_v1/,
  /reviewer_cohort_node_ids/,
  /cohort_frozen/,
  /holistic rewrite/i,
  /user-approved requirements change/i,
  /reviewed_task_id/,
  /artifact_digest/,
]
```

Also parse the route tables and fail if high can pass with one reviewer, normal
does not use Grok/Codex, or the parent is allowed to write the Plan/Task code.

- [ ] **Step 2: Run the validator against the current Skill and verify RED**

Run from the repository root:

```powershell
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
```

Expected: FAIL and specifically identify v1/legacy/pair language plus missing
Author, risk, owner-review, and high dual-review contracts.

- [ ] **Step 3: Capture behavior RED from the old Skill**

Use session 2070 as the raw historical baseline, then run fresh-context
decision scenarios against the unmodified Skill for:

1. one remaining Important finding after a localized Plan revision under time
   pressure;
2. a migration/updater Task where a cheaper Grok route is requested;
3. a high Task where one reviewer approved and the other is unavailable.

Record exact choices/rationalizations in temporary
`.superpowers/sdd/skill-tests/` artifacts. Do not put intended answers in the
prompts and do not commit the temporary outputs.

- [ ] **Step 4: Rewrite the Skill minimally to satisfy observed failures**

Make the Plan Author, risk matrix, scoped owner review, full-group reset,
stagnation, normal/high Task cohorts, exact artifact coverage, consolidated
fix request, both-reviewers-after-fix rule, and recovery state explicit. State
that only a user-approved requirements change with a persisted reason resets
the Plan lineage. Require a pre-admission risk correction to use material Plan
revision/full review, while a post-admission invalidation blocks without
mutating `cohort_frozen`. Invoke generic Skills by name instead of reproducing
their full procedures.

- [ ] **Step 5: Run structural validation and verify GREEN**

Run from the repository root:

```powershell
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
python C:\Users\drawpeng\.codex\skills\.system\skill-creator\scripts\quick_validate.py .agents\skills\brainstorm-to-delivery
(Get-Content .agents/skills/brainstorm-to-delivery/SKILL.md).Count
```

Expected: both validators PASS and line count is below 500. Confirm
`agents/openai.yaml` still matches the Skill; regenerate only if its interface
metadata is stale.

- [ ] **Step 6: Micro-test wording and pressure-test the rewritten Skill**

For the key routing decision, run at least five fresh-context samples for a
no-guidance control and five with the rewritten Skill; manually inspect every
result and require convergence. Re-run all three Step 3 pressure scenarios with
the new Skill using independent conversations. Capture any new rationalization,
tighten only the relevant rule/table, and re-test until all scenarios preserve
the approved routes under pressure.

- [ ] **Step 7: Commit**

```powershell
git add .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/agents/openai.yaml
git commit -m "feat(skills): route B2D plans and tasks adaptively"
```

If `openai.yaml` is unchanged, omit it from the commit rather than touching its
metadata.

---

### Task 10: Verify All Runtimes and Measure the Adaptive Workflow

**Files:**

- Create: `docs/superpowers/performance/b2d-adaptive-routing-evaluation.md`
- Do not modify runtime, frontend, or Skill files in this normal-risk Task. A
  verification failure reopens the owning Task and its frozen risk route.

**Verification/output contract:**

This Task changes no runtime contract. Treat any mandatory command, structural
scenario, or route assertion failure as RED. Diagnose it in the owning Task's
file surface, reopen that Task, and dispatch the fix plus review through its
recorded cohort. After that Task returns GREEN, restart the affected Task 10
matrix before writing the evidence report. The report must map every acceptance
criterion to command output or recorded workflow evidence and must distinguish
external measurement blockers from product test failures.

- [ ] **Step 1: Run formatting and static contract scans**

From the repository root:

```powershell
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
python C:\Users\drawpeng\.codex\skills\.system\skill-creator\scripts\quick_validate.py .agents\skills\brainstorm-to-delivery
rg -n "workflow_v1|workflow_manifest_v1|pair_frozen" src-tauri/src/acp src-tauri/src/bin src-tauri/src/db/entities src .agents/skills/brainstorm-to-delivery
git diff --check
```

Expected: formatting/validators/diff check pass and the runtime/Skill scan has
no matches. Historical migration source and approved design/history documents
are intentionally outside this scan.

- [ ] **Step 2: Run the full Rust desktop matrix**

From `src-tauri/`:

```powershell
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all pass. The known future-incompatibility note from third-party
`proc-macro-error2` is not a project clippy warning.

- [ ] **Step 3: Run server and MCP matrices**

From `src-tauri/`:

```powershell
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
cargo test --features test-utils grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --nocapture
```

Expected: all pass and the MCP budget remains at 7,680 bytes.

- [ ] **Step 4: Run the full frontend matrix**

From the repository root:

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: lint, all Vitest suites, static export, TypeScript strict checks, and
all locale-key parity checks pass.

- [ ] **Step 5: Exercise ten deterministic workflow scenarios**

Use backend integration fixtures or a local Codeg workflow, not mocked gate
booleans, for the ten scenarios in the approved design: normal; hard-high;
score-3-high; scoped owner review; material full reset; one rewrite then user
block; split high reviewer verdicts then both re-review; recovery; v1 rejection;
pre-admission revision versus post-admission freeze. Record task IDs, digests,
routes, gate cycles, and typed failures in the implementation report.

- [ ] **Step 6: Run comparative measurements without adding product code**

Run representative normal, hard-trigger high, and score-trigger high workflows
at least three times each when configured Codex/Grok agents are available.
Create a comparison table with rows for the session 2070 baseline, normal
median, hard-trigger-high median, and score-trigger-high median. Required
columns are run count, total tokens, elapsed time, Plan reviewer calls, Task
reviewer calls, and gate cycles. Populate every cell from existing
usage/timestamps; when an external agent is unavailable, replace the affected
row with the exact blocker instead of leaving a marker value.

Also report scoped fan-out versus complete cohort size, high-risk signal
distribution, holistic rewrite count, and implementation/fix rounds. If a live
agent is unavailable, report the exact external blocker; never fabricate
measurements or weaken route assertions.

- [ ] **Step 7: Review scope and commit the evidence report**

Confirm the diff contains no v1 fallback, generic Skill edits, final-route
change, cost API/schema/UI, or unrelated refactor. Then commit:

```powershell
git add docs/superpowers/performance/b2d-adaptive-routing-evaluation.md
git commit -m "test(workflow): verify adaptive routing end to end"
```

If verification required code fixes, keep them within the owning Task's file
surface and commit them from the reopened owning Task after its required
reviewers approve. Task 10 resumes only after that route settles; its own commit
contains the evidence report only.

## Completion Evidence

Before calling the implementation complete, the final reviewer must be able to
trace each acceptance criterion to concrete evidence:

- one Codex Author node and exact Plan digest/task ID coverage;
- full initial Plan review, owner-only localized re-review, full material reset;
- stable findings and deterministic one-rewrite/user-block boundaries;
- a complete versioned risk record and reason for every Task;
- exact normal/high implementer and reviewer agent sets;
- strict AND over the same latest non-empty producer artifact;
- persisted/recovered cohort, owner, counter, report-path, and route evidence;
- graph Author ordering, reviewer fan-out, counts, and redacted risk codes;
- v2-only capability with fixed Grok catalog budget;
- unchanged final whole-branch review behavior;
- Skill pressure tests and all Rust/frontend/static checks passing;
- measured fan-out/token/time results recorded without new cost product code.
