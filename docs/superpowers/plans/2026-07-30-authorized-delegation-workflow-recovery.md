# Authorized Delegation and Workflow Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover canceled or ambiguous delegation lineages and durably blocked brainstorm-to-delivery workflows in place through server-derived policy decisions and one-use user authorizations, without deleting evidence or bypassing existing ownership, route, gate, budget, and frozen-cohort constraints.

**Architecture:** Delegation and workflow recovery each receive a pure policy engine fed by transactionally loaded durable snapshots. Both policies issue challenges through one generic `recovery_authorizations` service, while delegation admissions consume receipts atomically with reserving-run insertion and workflow recovery consumes receipts atomically with immutable state-only manifest revisions. Typed ACP termination evidence and explicit disconnect origins remove unsafe inference; ordinary workflow publication remains structurally editable but cannot leave `blocked`. The B2D Skill then exposes an index-first, resume-first orchestration contract, while a stable-ID, negation-aware validator cross-checks both route-table surfaces and rejects known recovery and Parent-ownership evasions.

**Tech Stack:** Rust 2021, SeaORM + SQLite, Tokio, serde/serde_json, SHA-256, Axum/Tauri shared commands, length-prefixed broker transport, MCP JSON-RPC, Next.js 16, React 19, TypeScript strict mode, next-intl, Vitest, and Node.js Skill contract tests.

## Global Constraints

- Approved delegation Design baseline: `docs/superpowers/specs/2026-07-30-delegation-recovery-authorization-design.md`, SHA-256 `b8b04fb31daafd275d24fd8f712ff488d9a7429e7a3b9cd6a7f38c7b4cf0401d`. Do not modify it during implementation.
- Approved workflow Design baseline: `docs/superpowers/specs/2026-07-30-workflow-blocked-recovery-design.md`, SHA-256 `632d2c74a27fcff4c01b274b8b2bfb54fed35ea767cc2c39f5d0035352c87ce9`. Do not modify it during implementation.
- Approved supplemental B2D recovery-contract Design baseline: `docs/superpowers/specs/2026-07-30-brainstorm-to-delivery-recovery-contract-hardening-design.md`, SHA-256 `f1616f50352b1ce2b20b98fb098e65847068b627d28fe29acfb65fcc58716c93`. Do not modify it during implementation.
- Keep Task execution serial. Tasks 1-8 establish persistence, evidence, state authority, policies, and authorization interfaces consumed by Tasks 9-12.
- **Workspace Gate before Task 1:** run `git status --short` and require a clean implementation worktree. At plan-review time the main worktree contains unrelated edits in `src/components/message/live-transcript-row.test.tsx`, `src/components/message/message-list-view.test.tsx`, `src/hooks/use-delegation-card-model.test.ts`, `src/hooks/use-delegation-card-model.ts`, `src/lib/delegation-transcript-projection.test.ts`, and `src/lib/delegation-transcript-projection.ts`. Do not start Task 1 or run the final matrix over those edits. Their owner must first commit/stash them, or the executor must use a clean isolated worktree; never stage, stash, revert, or overwrite them implicitly.
- Follow RED-GREEN-REFACTOR for every Task. Observe each focused test fail for the intended missing behavior before production edits.
- **Focused Test Filter Gate:** every new Rust test group must be nested under the exact module name specified by its Task so the documented `cargo test <filter>` command matches its module path. Immediately before every filtered GREEN run, run the same command with `-- --list`; it must list at least one test. The GREEN output must report `N passed` with `N > 0`; `running 0 tests` is a hard failure even when Cargo exits 0. Record the listed and passed counts in the Task evidence.
- Do not delete or rewrite historical delegation runs, manifests, settlements, run bindings, or retired node bindings. The migration is additive and fabricates no historical provenance.
- Do not create a replacement workflow, new parent session, or cleanup command as the product fix. Session recreation remains only an old-binary operational workaround.
- Delegation recovery is resume-first. A valid established resume identity with available budget can authorize only `continue`; replacement needs current durable structural or attempted-resume `unresumable` evidence.
- Cancellation-family evidence (`parent_canceled`, `parent_turn_failed`, `join_abandoned`, `user_cancelled`) and `tool_stalled_timeout` never alias `replacement_reason=unresumable`. Stall stays on the confirmation-required continue rail. Genuine unexpected transport loss may continue without confirmation only when central policy permits it.
- When an authorized continue commits and then durably fails as a real `failed/unresumable`, its consumed authorization provenance permits the caller's separate replacement admission without a second card. The latest-run, exact `replacement_reason=unresumable`, and normal replacement-rail checks still apply.
- Reserving or running tasks always return `busy_thread`. Recovery never detaches, expires, supersedes, or cancels them, and the busy result keeps detachment unavailable.
- Failed and canceled lineage fences do not expire with wall-clock time. Only an admitted action derived by `DelegationRecoveryPolicy` can advance the lineage.
- Explicit cancel, parent-end ambiguity, stall, `admission_unknown`, malformed legacy audit, and legacy NULL parent disconnect require user authorization even if the final rail is replacement.
- Existing unexpected-continue and replacement counters remain unchanged and are charged only at the successful `running` promotion point, never when a challenge is created, approved, declined, or consumed into a reserving run.
- A recovery authorization is server-authored, parent-conversation scoped, action exact, fingerprint exact, approved for exactly ten minutes, and consumed at most once. It never overrides ownership, route, capability, latestness, active-run, frozen-cohort, or budget checks.
- Status projections never include `recovery_authorization_id` and are never accepted as write evidence.
- Authorization ids are exact replay inputs only; do not write them to B2D status projections, `.superpowers/sdd/progress.md`, workspace reports, card summaries, or pressure-test records.
- Every `blocked -> non-blocked` workflow transition requires an authorization. `publish_workflow_manifest` and ordinary Plan gate settlement have no force flag and cannot unblock.
- Workflow recovery derives its target from current durable evidence: exact current approved Plan evidence gives `approved`, a current unapproved Plan gives `estimated`, and no Plan gives `skeleton`; active, unresolved, corrupt, or contradictory evidence remains `blocked`.
- Normal Plan approval on a non-blocked workflow atomically creates an immutable `approved` state-only revision. Approval while already blocked records valid gate evidence but remains blocked until authorized recovery.
- Already-retired omitted bindings are stable no-ops: do not alter `retired_revision`, `retained_observed`, `node_outcome`, or timestamps. Exact-identity reactivation is the only retired-to-active path.
- Frozen Task implementer/reviewer cohorts remain immutable in blocked manifests. A generic blocked state is not permission to remove either side of a route.
- A Plan `user_decision_required` lineage reset requires an exact approved `reset_plan_lineage` receipt and exact displayed reason; the same transaction may perform the resulting authorized state transition.
- First Task admission freezes the complete key/role/agent/profile identity and inherited recovery consumption. Pre-admission profile or route correction is a material Plan revision; post-admission recovery cannot change key/profile to mint lineage or budget.
- Continue-budget exhaustion uses same-key, same-profile `budget_exhausted_continue` replacement only while replacement budget remains; otherwise stop with a blocking report.
- A platform-harvested and platform-validated card is settlement evidence. When harvest is unavailable or validation fails, treat the child as degraded and continue the same child to re-emit; never advance from prose alone.
- Before every `delegate_to_agent` or `continue_delegation`, write the intended key/role/agent/profile/action to the B2D ledger; fill `latest_task_id` after admission and reconcile from platform state after recovery.
- The top `Codeg roles and tools` route table and numbered `## 4. Task route` tables are equally authoritative and must agree exactly after canonicalization.
- Normal-route Task review independently recomputes `b2d_task_risk_v1`. Migration, security/authorization, concurrency, persistence/state-machine, and externally visible compatibility changes deterministically require external Design review; ambiguity remains an additional trigger.
- Persist the authorization consumer `correlation_id` with consumption provenance. Replay succeeds only for the same parent, subject, authorization, source revision/run, request payload, and correlation id.
- Preserve stable operation errors: delegation adds `recovery_confirmation_required`, `recovery_declined`, `recovery_authorization_expired`, `recovery_authorization_stale`, `recovery_authorization_consumed`, `recovery_authorization_action_mismatch`, and `inconsistent_durable_state`; workflow additionally adds `workflow_recovery_required`, `workflow_recovery_not_available`, `workflow_recovery_conflict`, `plan_lineage_reset_required`, and `plan_lineage_reset_authorization_required` while retaining existing admission errors.
- Keep `recovery_authorizations` as the only authorization table. Delegation and workflow policies must not call one another or share policy enums.
- Keep Next.js static export compatibility; do not introduce dynamic routes or server-only frontend code.
- During Tasks 1-11 run only focused tests. Run the full long-running repository validation matrix once in Task 12 after implementation, migration, docs, schema, Skill, and focused acceptance tests are complete.
- Use PowerShell syntax for commands. Run Rust commands from `src-tauri/`. Stage only Task-owned files, use local commits, and do not push, merge, or open a PR.
- Task 4's fail-closed adapter commit through Task 8 is an implementation-only transition and is not independently shippable. The Task 1-11 commit series is delivered atomically: do not expose either the old broad matcher as an alternative to the new policy or the temporary no-authorization adapter behavior to users.

## File Map

| File | Responsibility in this change |
| --- | --- |
| `src-tauri/src/db/migration/m20260730_000001_recovery_authorizations.rs` | Add the shared authorization table, active-challenge indexes, consumer provenance, and nullable delegation/workflow recovery columns. |
| `src-tauri/src/db/migration/mod.rs` | Register the additive migration after manifest v2. |
| `src-tauri/src/db/entities/recovery_authorization.rs` | SeaORM model and durable status/subject/consumer enums. |
| `src-tauri/src/db/entities/conversation.rs` | Expose the nullable latest typed termination-audit projection for delegation children. |
| `src-tauri/src/db/entities/delegation_task_run.rs` | Expose nullable delegation authorization provenance. |
| `src-tauri/src/db/entities/delegation_workflow.rs` | Expose typed active block provenance. |
| `src-tauri/src/db/entities/delegation_workflow_manifest_revision.rs` | Expose publication/state-only revision and transition provenance. |
| `src-tauri/src/db/entities/delegation_workflow_gate_settlement.rs` | Expose Plan lineage-reset authorization provenance. |
| `src-tauri/tests/delegation_recovery_migration.rs` | Prove additive migration preservation, defaults, uniqueness, and delete cleanup. |
| `src-tauri/src/acp/termination.rs` | Typed ACP termination summaries, frontend disconnect origins, delegation audit builders, and legacy parsing. |
| `src-tauri/src/acp/connection.rs` | Record observed/intentional connection exit evidence and pass typed parent-end context to cleanup. |
| `src-tauri/src/acp/manager.rs` | Record a typed disconnect origin before removing an owned connection. |
| `src-tauri/src/commands/acp.rs` | Accept desktop disconnect origin. |
| `src-tauri/src/web/handlers/acp.rs` | Accept server-mode disconnect origin. |
| `src/lib/api.ts`, `src/lib/tauri.ts` | Carry the frontend disconnect-origin union to both transports. |
| `src/contexts/acp-connections-context.tsx`, `src/stores/tab-store.ts` | Classify provider unmount, disconnect-all, idle, reconfiguration, retarget, abandon, and supersession call sites. |
| `src-tauri/src/acp/delegation/store.rs` | Require typed evidence for new canceled terminal writes and serialize it at the store boundary. |
| `src-tauri/src/acp/delegation/broker.rs` | Carry typed terminal evidence through all parent/child/cancel/handoff producers and expose recovery projections. |
| `src-tauri/src/acp/delegation/metrics.rs` | Count recovery decisions, confirmation requests, consumption, and rejection without recording prompts or session ids. |
| `src-tauri/src/acp/delegation/spawner.rs` | Classify broker-owned child teardown rather than losing disconnect provenance in the manager adapter. |
| `src-tauri/src/acp/delegation/run_store.rs` | Build durable recovery snapshots and atomically consume authorizations during fresh/continue/replacement reservation. |
| `src-tauri/src/acp/delegation/recovery_policy.rs` | Pure `DelegationRecoveryPolicy`, decision types, fingerprint input, and decision-table tests. |
| `src-tauri/src/acp/recovery_authorization/{mod,types,store,service}.rs` | Generic challenge preparation, fixed presentation data, ten-minute lifecycle, reconnect handling, validation, and one-use transactional consumption. |
| `src-tauri/src/acp/question.rs` | Carry a typed server-owned recovery presentation through the existing pending-question transport. |
| `src-tauri/src/acp/delegation/workflow/store.rs` | Correct binding lifecycle, enforce blocked state authority, append state-only revisions, settle Plan approval/reset, and implement authorized recovery. |
| `src-tauri/src/acp/delegation/workflow/recovery_policy.rs` | Pure `WorkflowRecoveryPolicy`, source snapshot, target derivation, blockers, and fingerprint input. |
| `src-tauri/src/acp/delegation/workflow/state_dto.rs` | Add read-only workflow recovery projection without authorization ids. |
| `src-tauri/src/acp/delegation/workflow/error.rs` | Add stable recovery, conflict, lineage-reset, and inconsistent-state errors. |
| `src-tauri/src/acp/delegation/workflow/events.rs` | Emit structured recovery decision, authorization, state-only revision, lineage-reset, and binding-reactivation events. |
| `src-tauri/src/acp/delegation/transport.rs` | Add authorization and workflow-recovery broker request variants and clients. |
| `src-tauri/src/acp/delegation/listener.rs` | Resolve direct parent ownership and dispatch authorization/recovery operations. |
| `src-tauri/src/acp/delegation/companion.rs` | Advertise, parse, dispatch, and render the new MCP contracts with root/feature gating. |
| `src-tauri/src/acp/delegation/tool_schema.json` | Publish exact authorization fields, recovery fields, and corrected replacement guidance. |
| `src/lib/types.ts`, `src/components/chat/ask-question-card.tsx` | Render localized fixed recovery challenges while submitting stable `approve`/`decline` values. |
| `src/components/chat/ask-question-card.test.tsx` | Prove fixed-copy, no-free-text, decline-on-dismiss, lock, and generic-card compatibility. |
| `src/i18n/messages/{en,zh-CN,zh-TW,ja,ko,es,de,fr,pt,ar}.json` | Localize recovery action/cause/risk copy in all supported locales. |
| `.agents/skills/brainstorm-to-delivery/SKILL.md` | Publish the index/status-first recovery sequence, frozen identity/budget rules, settlement-card behavior, independent risk review, Design triggers, and write-ahead ledger guidance below 500 lines. |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs` | Emit stable rule IDs; parse affirmative/negated recovery clauses, both route surfaces, and known English/Chinese Parent-ownership verb forms. |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs` | Keep existing fixtures nonzero and add exact-ID positive/negative mutations for every supplemental recovery contract. |
| `src-tauri/src/acp/delegation/workflow/recovery_tests.rs` | Reconstruct session-2566 durable evidence and prove in-place recovery through Task 1 admission. |

## Task Routing Matrix

| Task | Deliverable | Risk | Review focus |
| --- | --- | --- | --- |
| 1 | Add shared persistence and provenance | High | Migration preservation, uniqueness, logical references |
| 2 | Make terminal writes typed and auditable | High | First-terminal-wins, legacy compatibility, atomic projection |
| 3 | Propagate disconnect origins end to end | High | Intent/observation precedence, teardown races, desktop/server parity |
| 4 | Centralize delegation recovery policy | High | Full decision table, resume-first, no auto-expiry or broad replacement |
| 5 | Correct workflow binding lifecycle | High | Retired no-op, exact reactivation, frozen cohort protection |
| 6 | Establish workflow state authority | High | State-only revisions, sticky blocked, Plan approval atomicity |
| 7 | Centralize workflow recovery policy | High | Target derivation, evidence consistency, blockers, fingerprints |
| 8 | Build the shared one-use authorization service | High | Ten-minute approval, dedupe, abandon/reconnect, transactional consume |
| 9 | Integrate authorized delegation admission/status | High | Same policy on every entry, no synthetic failed run, provenance |
| 10 | Integrate workflow recovery and lineage reset | High | Root-only transaction, replay, receipt exactness, blocked invariants |
| 11 | Publish MCP, frontend, i18n, and Skill contracts | High | Role gating, fixed localized card, typed replay flow, stable-ID semantic validator, route parity, pressure convergence |
| 12 | Run acceptance fixtures and final verification | High | Session-2566, legacy delegation cases, complete matrix |

## Design Traceability

| Approved Design section | Implemented and verified by |
| --- | --- |
| Delegation Core Invariants, Recovery Policy, Decision Precedence/Table, Resume-First | Tasks 4, 9, 12 |
| Delegation Typed Parent-End Context, Typed Terminal Writes, Atomic Persistence, Legacy Compatibility | Tasks 2, 3, 4, 12 |
| Delegation Server-Owned Confirmation, Authorization Model/Fingerprint/Lifecycle/Consumption/Provenance | Tasks 1, 4, 8, 9 |
| Delegation Wire Contracts, Stable Errors, Replacement Schema | Tasks 9, 11 |
| Delegation end-to-end flows, concurrency, cold status, observability | Tasks 8, 9, 12 |
| Delegation migration, compatibility, implementation boundaries, testing, acceptance, rollout | Tasks 1-4, 8-9, 11-12 |
| Workflow relationship to delegation and shared authorization boundary | Tasks 1, 7, 8, 10 |
| Workflow Core Invariants, Recovery Policy, Precedence/Target Derivation, Typed Block Causes | Tasks 6, 7, 10 |
| Workflow State Authority, Ordinary Publication, State-Only Revision, Plan Approval | Tasks 6, 10, 12 |
| Workflow Binding Lifecycle and Frozen Task Cohorts | Tasks 5, 7, 12 |
| Workflow authorization, fingerprint, wire contracts, Plan lineage reset, transactional flow | Tasks 7, 8, 10, 11 |
| Workflow persistence, polluted-workflow compatibility, concurrency, projections/errors, observability | Tasks 1, 5-8, 10-12 |
| Workflow testing strategy, session-2566 fixture, rollout, completion criteria | Tasks 5-12 |
| Supplemental authority boundaries and recovery decision/sequencing contract | Tasks 9-12, chiefly Task 11 |
| Supplemental frozen identity, risk, budget, settlement-card, and write-ahead ledger semantics | Tasks 4, 7, 9-12, with Skill/validator proof in Task 11 |
| Supplemental route-surface parity, negation-aware validator, stable rule IDs, and ownership hardening | Task 11 |
| Supplemental RED-GREEN Skill behavior tests, compatibility, rollout, and completion criteria | Tasks 11-12 |

---

### Task 1: Add Shared Recovery Persistence and Provenance

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Create: `src-tauri/src/db/migration/m20260730_000001_recovery_authorizations.rs`
- Create: `src-tauri/src/db/entities/recovery_authorization.rs`
- Create: `src-tauri/tests/delegation_recovery_migration.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/mod.rs`
- Modify: `src-tauri/src/db/entities/prelude.rs`
- Modify: `src-tauri/src/db/entities/conversation.rs`
- Modify: `src-tauri/src/db/entities/delegation_task_run.rs`
- Modify: `src-tauri/src/db/entities/delegation_workflow.rs`
- Modify: `src-tauri/src/db/entities/delegation_workflow_manifest_revision.rs`
- Modify: `src-tauri/src/db/entities/delegation_workflow_gate_settlement.rs`

**Interfaces:**

- Produces `recovery_authorization::Model` with exact Design columns plus nullable `consumer_correlation_id` for exact post-consumption replay.
- Produces nullable `conversation.last_termination_audit_json` because the repository does not yet have the earlier termination-projection column.
- Produces nullable `delegation_task_runs.recovery_authorization_id`.
- Produces nullable manifest-revision fields `revision_kind`, `source_manifest_revision`, `recovery_authorization_id`, `transition_reason_code`, and `consumer_correlation_id`.
- Produces nullable workflow-header fields `block_cause_code` and `block_source_manifest_revision`.
- Produces nullable gate-settlement field `lineage_reset_authorization_id`.

- [ ] **Step 1: Write the migration preservation and constraint tests**

```rust
#[tokio::test]
async fn recovery_migration_preserves_existing_workflow_and_run_bytes() {
    // Migrate through the existing 42 migrations; seed historical NULL-audit
    // parent disconnect, established budgets, pure abort, admission_unknown,
    // replacement chain, revision-8 blocked workflow, retired observed rows,
    // frozen cohort, user_decision_required, and approved current Plan gate.
    // Apply migration 43 and assert every pre-existing selected value is equal.
}

#[tokio::test]
async fn recovery_migration_adds_one_active_challenge_and_provenance_columns() {
    // Insert one pending challenge, reject a second pending/approved row for the
    // same parent+subject+fingerprint, then permit declined and consumed history.
    // Assert every nullable consumer column accepts NULL on historical rows.
}

#[tokio::test]
async fn deleting_parent_conversation_removes_recovery_authorizations() {
    // Seed delegation_task and workflow authorizations owned by the same parent,
    // delete the conversation, and assert the authorization count becomes zero.
}
```

- [ ] **Step 2: Run the migration test and verify RED**

Run: `cargo test --test delegation_recovery_migration -- --nocapture`

Expected: FAIL because migration 43, `recovery_authorizations`, and the nullable provenance columns do not exist.

- [ ] **Step 3: Implement the additive migration and SeaORM entities**

```rust
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAuthorizationStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "declined")]
    Declined,
    #[sea_orm(string_value = "consumed")]
    Consumed,
    #[sea_orm(string_value = "expired")]
    Expired,
    #[sea_orm(string_value = "abandoned")]
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "recovery_authorizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub authorization_id: String,
    pub parent_conversation_id: i32,
    pub subject_kind: String,
    pub subject_id: String,
    pub source_task_id: Option<String>,
    pub child_conversation_id: Option<i32>,
    pub lineage_root_task_id: Option<String>,
    pub work_unit_key: Option<String>,
    pub source_state_fingerprint: String,
    pub allowed_action: String,
    #[sea_orm(column_type = "Text")]
    pub action_payload_json: String,
    pub cause_code: String,
    pub risk_class: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub display_reason: Option<String>,
    pub status: RecoveryAuthorizationStatus,
    pub question_id: Option<String>,
    pub requested_at: DateTimeUtc,
    pub approved_at: Option<DateTimeUtc>,
    pub expires_at: Option<DateTimeUtc>,
    pub consumed_at: Option<DateTimeUtc>,
    pub consumed_by_kind: Option<String>,
    pub consumed_by_id: Option<String>,
    pub consumer_correlation_id: Option<String>,
}
```

Use a partial unique SQLite index named `idx_ra_one_active_challenge` over `(parent_conversation_id, subject_kind, subject_id, source_state_fingerprint)` with `WHERE status IN ('pending','approved')`, plus lookup indexes `idx_ra_question_id`, `idx_ra_parent_status`, `idx_ra_status_expires_at`, and `idx_ra_consumed_by`. The expiry index covers `(status, expires_at)` for lazy approved-row expiry scans.

- [ ] **Step 4: Run focused migration and entity checks and verify GREEN**

Run: `cargo test --test delegation_recovery_migration -- --nocapture`

Expected: PASS; migration preserves seeded bytes, enforces one active challenge, accepts historical NULL provenance, and cascades parent cleanup.

Run: `cargo check --lib`

Expected: PASS with all new entity fields initialized at existing ActiveModel construction sites.

- [ ] **Step 5: Commit Task 1**

```powershell
git add src-tauri/src/db/migration/m20260730_000001_recovery_authorizations.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/recovery_authorization.rs src-tauri/src/db/entities/mod.rs src-tauri/src/db/entities/prelude.rs src-tauri/src/db/entities/conversation.rs src-tauri/src/db/entities/delegation_task_run.rs src-tauri/src/db/entities/delegation_workflow.rs src-tauri/src/db/entities/delegation_workflow_manifest_revision.rs src-tauri/src/db/entities/delegation_workflow_gate_settlement.rs src-tauri/tests/delegation_recovery_migration.rs
git commit -m "feat: add shared recovery authorization persistence"
```

### Task 2: Make Delegation Termination Evidence Typed and Atomic

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Create: `src-tauri/src/acp/termination.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`

**Interfaces:**

Task 2 defines `AcpDisconnectOrigin` together with the typed summaries so this Task compiles independently; Task 3 then propagates explicit values through every producer. Initially only `LegacyUnspecified` is used by unchanged call sites during RED.

```rust
pub const TERMINATION_AUDIT_VERSION: u8 = 1;

#[serde(rename_all = "snake_case")]
pub enum AcpTerminationSource {
    Transport,
    Process,
    Session,
    Frontend,
    HostRestart,
    ParentTurn,
    Watchdog,
    ChildConnection,
    Admission,
    Legacy,
}

#[serde(rename_all = "snake_case")]
pub enum AcpTerminationReason {
    TransportDisconnected,
    ProcessExited,
    SessionLost,
    FrontendDisconnected,
    HostRestarted,
    ParentCanceled,
    ParentTurnFailed,
    JoinAbandoned,
    UserCancelled,
    ToolStalledTimeout,
    SuspensionDrainTimeout,
    ChildTerminal,
    AdmissionFailed,
    AdmissionUnknown,
    LegacyUnspecified,
}

#[serde(rename_all = "snake_case")]
pub enum AcpTerminationClassification {
    Unexpected,
    Intentional,
    Explicit,
    AutomatedAmbiguous,
    LegacyUnknown,
}

pub struct AcpTerminationSummaryV1 {
    pub version: u8,
    pub source: AcpTerminationSource,
    pub reason: AcpTerminationReason,
    pub classification: AcpTerminationClassification,
    pub frontend_origin: Option<AcpDisconnectOrigin>,
    pub prompt_may_have_executed: bool,
    pub requested_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

pub struct DelegationTerminationAuditV1 {
    pub termination: AcpTerminationSummaryV1,
    pub prior_status: DelegationRunStatus,
    pub admission_class: AdmissionClass,
    pub parent_tool_use_id: Option<String>,
    pub child_connection_id: Option<String>,
}

pub enum ParsedDelegationTermination {
    Typed(DelegationTerminationAuditV1),
    LegacyParentDisconnect,
    LegacyUnspecified,
    Malformed { raw_sha256: String },
}

pub struct ParentEndContext {
    pub reason: ParentTurnEndReason,
    pub termination: AcpTerminationSummaryV1,
}
```

- [ ] **Step 1: Write typed audit, legacy parse, and first-terminal-wins tests**

Place the following tests under `#[cfg(test)] mod termination_audit`; the separate `parent_end` GREEN filter must also list at least the `later_parent_end_cannot_replace_winning_child_terminal_audit` regression.

```rust
#[test]
fn canceled_terminal_write_serializes_typed_evidence() {
    let evidence = cancellation_audit(AcpTerminationReason::UserCancelled);
    let write = TerminalTaskWrite::canceled("user_cancelled", now(), evidence.clone());
    assert_eq!(write.termination_evidence(), Some(&evidence));
}

#[test]
fn null_parent_disconnect_maps_to_legacy_confirmation_cause() {
    let parsed = parse_delegation_termination(
        DelegationRunStatus::Canceled,
        Some("parent_disconnected"),
        true,
        None,
    );
    assert_eq!(parsed, ParsedDelegationTermination::LegacyParentDisconnect);
}

#[test]
fn malformed_audit_hashes_raw_bytes_and_never_becomes_unexpected() {
    let parsed = parse_delegation_termination(
        DelegationRunStatus::Canceled,
        Some("parent_disconnected"),
        true,
        Some("{not-json"),
    );
    assert!(matches!(parsed, ParsedDelegationTermination::Malformed { .. }));
    assert!(!parsed.is_automatic_unexpected_termination());
}

#[tokio::test]
async fn later_parent_end_cannot_replace_winning_child_terminal_audit() {
    let fixture = seeded_running_delegation().await;
    fixture.settle_child_process_exit().await.unwrap();
    let winning = fixture.load_run().await.termination_audit_json;
    fixture.settle_parent_disconnect().await.unwrap();
    assert_eq!(fixture.load_run().await.termination_audit_json, winning);
}

#[tokio::test]
async fn terminal_cas_updates_run_and_child_projection_together() {
    let fixture = seeded_running_delegation().await;
    fixture.inject_terminal_transaction_failure(true);
    assert!(fixture.settle_child_process_exit().await.is_err());
    assert_eq!(fixture.load_run().await.status, DelegationRunStatus::Running);
    assert_eq!(fixture.load_child().await.delegation_task_status, DelegationTaskStatus::Running);
}
```

- [ ] **Step 2: Run focused termination tests and verify RED**

Run: `cargo test termination_audit --lib -- --nocapture`

Expected: FAIL because `acp::termination`, typed constructors, and legacy parsing do not exist; current code accepts raw JSON and NULL canceled writes.

- [ ] **Step 3: Implement typed evidence and remove production raw-audit construction**

```rust
impl TerminalTaskWrite {
    pub fn canceled(
        error_code: impl Into<String>,
        finished_at: DateTime<Utc>,
        evidence: DelegationTerminationAuditV1,
    ) -> Self;

    pub fn failed_with_evidence(
        error_code: impl Into<String>,
        finished_at: DateTime<Utc>,
        evidence: DelegationTerminationAuditV1,
    ) -> Self;

    #[cfg(test)]
    pub fn legacy_without_audit(status: TaskStatus, error_code: Option<String>) -> Self;
}
```

Serialize `DelegationTerminationAuditV1` only inside the run-store settlement transaction. Update drained running tasks, reserving handoffs, DB-only sweeps, setup/admission failures, explicit cancellation, child terminal handling, watchdog timeout, and host-restart reconciliation to construct typed evidence. Keep first-terminal-wins CAS authoritative.

- [ ] **Step 4: Run focused termination and settlement tests and verify GREEN**

Run: `cargo test termination_audit --lib -- --nocapture`

Expected: PASS for typed serialization, legacy classification, malformed fail-closed behavior, rollback, and first-terminal-wins.

Run: `cargo test parent_end --lib -- --nocapture`

Expected: PASS with every parent-end producer carrying `ParentEndContext`.

- [ ] **Step 5: Commit Task 2**

```powershell
git add src-tauri/src/acp/termination.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/types.rs
git commit -m "refactor: persist typed delegation termination evidence"
```

### Task 3: Propagate Connection Termination Origins End to End

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/termination.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/delegation/continuation/coordinator.rs`
- Modify: `src-tauri/src/acp/delegation/spawner.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/auto_title/runner.rs`
- Modify: `src-tauri/src/document_translate/runner.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/stores/tab-store.ts`
- Modify: `src/stores/tab-store-dispose-draft.test.ts`
- Modify: `src/stores/tab-store-delegation-route.test.ts`

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpDisconnectOrigin {
    ExplicitUser,
    ProviderUnmount,
    DisconnectAll,
    ApplicationShutdown,
    ConnectionSuperseded,
    IdleTimeout,
    ConfigReapply,
    DraftRetarget,
    AbandonedConnect,
    InternalJobComplete,
    LegacyUnspecified,
}

pub async fn disconnect_with_origin(
    &self,
    connection_id: &str,
    origin: AcpDisconnectOrigin,
) -> Result<(), AcpError>;

pub async fn disconnect_if_owner(
    &self,
    connection_id: &str,
    expected_owner_window: Option<&str>,
    expected_operation_id: Option<&str>,
    expected_generation: Option<u64>,
    origin: AcpDisconnectOrigin,
) -> Result<(), AcpError>;

pub async fn disconnect_all(&self, origin: AcpDisconnectOrigin) -> usize;

pub enum ParentConnectionExitCause {
    Disconnected { termination: AcpTerminationSummaryV1 },
    SuspensionDrainTimeout { termination: AcpTerminationSummaryV1 },
}
```

TypeScript mirrors the same snake-case union in `AcpDisconnectLease.origin` and sends `legacy_unspecified` only for old call sites during the Task's RED phase; the final implementation supplies an explicit origin at every production call.

- [ ] **Step 1: Write backend intent-registry and frontend call-site tests**

Place all new Rust tests in this Step under `#[cfg(test)] mod disconnect_origin`.

```rust
#[test]
fn observed_transport_loss_outranks_legacy_unspecified_but_not_recorded_user_intent() {
    let evidence = ParentConnectionExitEvidence::default();
    evidence.record_observation("c1", legacy_unspecified_summary());
    evidence.record_observation("c1", unexpected_transport_summary());
    assert_eq!(evidence.peek("c1").classification, AcpTerminationClassification::Unexpected);
    evidence.record_intent("c1", AcpDisconnectOrigin::ExplicitUser, now());
    assert_eq!(evidence.peek("c1").frontend_origin, Some(AcpDisconnectOrigin::ExplicitUser));
}

#[tokio::test]
async fn manager_records_origin_before_map_removal_and_disconnect_control() {
    let fixture = manager_with_recording_connection().await;
    fixture.manager.disconnect_with_origin("c1", AcpDisconnectOrigin::DisconnectAll).await.unwrap();
    assert_eq!(fixture.observed_order(), vec!["record_intent", "remove_connection", "send_disconnect"]);
}

#[tokio::test]
async fn cleanup_without_evidence_writes_legacy_unspecified_not_transport_loss() {
    let fixture = parent_cleanup_fixture_without_exit_evidence().await;
    cleanup_delegation_parent(&fixture.injection, "parent", &fixture.state).await;
    let audit = fixture.latest_termination().await;
    assert_eq!(audit.termination.classification, AcpTerminationClassification::LegacyUnknown);
    assert_eq!(audit.termination.reason, AcpTerminationReason::LegacyUnspecified);
}
```

```tsx
it("labels provider cleanup, disconnectAll, idle reap, supersession, and reapply", async () => {
  const h = renderConnectionsProvider()
  await h.exerciseEveryOwnerTeardown()
  expect(acpDisconnect).toHaveBeenNthCalledWith(1, expect.any(String), expect.objectContaining({ origin: "provider_unmount" }))
  expect(acpDisconnect).toHaveBeenNthCalledWith(2, expect.any(String), expect.objectContaining({ origin: "disconnect_all" }))
  expect(acpDisconnect).toHaveBeenNthCalledWith(3, expect.any(String), expect.objectContaining({ origin: "idle_timeout" }))
  expect(acpDisconnect).toHaveBeenNthCalledWith(4, expect.any(String), expect.objectContaining({ origin: "connection_superseded" }))
  expect(acpDisconnect).toHaveBeenNthCalledWith(5, expect.any(String), expect.objectContaining({ origin: "config_reapply" }))
})

it("labels draft disposal and route retarget teardown", async () => {
  await disposeAndRetargetDrafts()
  expect(runtime.acpDisconnect).toHaveBeenCalledWith(expect.any(String), { origin: "draft_retarget" })
})
```

- [ ] **Step 2: Run focused lifecycle tests and verify RED**

Run: `cargo test disconnect_origin --lib -- --nocapture`

Expected: FAIL because disconnect APIs and `ParentConnectionExitCauses` carry no typed origin.

Run: `pnpm test -- src/contexts/acp-connections-context.test.tsx src/stores/tab-store-dispose-draft.test.ts src/stores/tab-store-delegation-route.test.ts`

Expected: FAIL because frontend calls carry only ownership lease fields.

- [ ] **Step 3: Implement intent/observation precedence and classify every caller**

```rust
pub struct ParentConnectionExitEvidence {
    entries: Mutex<HashMap<String, AcpTerminationSummaryV1>>,
    suspension_drain_timeouts: Mutex<HashSet<String>>,
}

impl ParentConnectionExitEvidence {
    pub fn record_intent(&self, connection_id: &str, origin: AcpDisconnectOrigin, at: DateTime<Utc>);
    pub fn record_observation(&self, connection_id: &str, summary: AcpTerminationSummaryV1);
    pub fn take(&self, connection_id: &str) -> ParentConnectionExitCause;
}
```

Map unrequested wire EOF, process exit, and control/session loss from `run_connection` to unexpected observations. Map frontend and internal manager callers to the explicit enum before connection removal. Preserve `SuspensionDrainTimeout` as an automated ambiguous cause. `cleanup_delegation_parent` passes the complete termination summary to the coordinator and broker.

- [ ] **Step 4: Run focused backend/frontend lifecycle tests and verify GREEN**

Run: `cargo test disconnect_origin --lib -- --nocapture`

Expected: PASS for origin precedence, teardown ordering, unexpected observations, and legacy fallback.

Run: `pnpm test -- src/contexts/acp-connections-context.test.tsx src/stores/tab-store-dispose-draft.test.ts src/stores/tab-store-delegation-route.test.ts`

Expected: PASS with exact origin assertions and unchanged viewer/detach-only behavior.

- [ ] **Step 5: Commit Task 3**

```powershell
git add src-tauri/src/acp/termination.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/delegation/continuation/coordinator.rs src-tauri/src/acp/delegation/spawner.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/lib.rs src-tauri/src/auto_title/runner.rs src-tauri/src/document_translate/runner.rs src-tauri/src/automation/engine.rs src/lib/api.ts src/lib/tauri.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/stores/tab-store.ts src/stores/tab-store-dispose-draft.test.ts src/stores/tab-store-delegation-route.test.ts
git commit -m "feat: preserve acp disconnect provenance"
```

### Task 4: Centralize Delegation Recovery Policy

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Create: `src-tauri/src/acp/delegation/recovery_policy.rs`
- Modify: `src-tauri/src/acp/delegation/mod.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`

**Interfaces:**

```rust
pub fn decide_delegation_recovery(
    source: &RecoverySourceSnapshot,
    rails: &RecoveryRailSnapshot,
    operation: RequestedRecoveryOperation,
) -> RecoveryDecision;

pub struct RecoveryDecision {
    pub source_task_id: String,
    pub source_state_fingerprint: String,
    pub disposition: RecoveryDisposition,
    pub confirmation: RecoveryConfirmation,
    pub cause_code: RecoveryCauseCode,
    pub risk_class: RecoveryRiskClass,
}

pub enum RecoveryDisposition {
    Continue { admission_class: AdmissionClass },
    FreshDispatch,
    Replace { replacement_reason: ReplacementReason },
    Stop { code: RecoveryStopCode },
    InconsistentDurableState,
}

pub enum RequestedRecoveryOperation {
    Inspect,
    Continue,
    FreshDispatch,
    Replace { replacement_reason: ReplacementReason },
}

pub enum ReplacementReason {
    Unresumable,
    BudgetExhaustedContinue,
    NotSupported,
    AdmissionFailed,
    AdmissionUnknown,
}

pub enum RecoveryRiskClass {
    Normal,
    ExecutionMayHaveOccurred,
    ExplicitUserStop,
    LegacyUnknownOrigin,
}
```

`RecoverySourceSnapshot` contains only durable identity, latestness, active state, status/error/admission class, parsed typed termination, reached-running, launch/resume identity, and replacement/supersession facts. `RecoveryRailSnapshot` contains current reuse capability plus unexpected-continue and replacement budget availability.

Serialize the canonical fingerprint as `delegation_recovery_v1:<lowercase_sha256_hex>`. Hash external-session identity before it enters the canonical input; never include prompt text, task preview, display prose, a raw session id, or budget values. Budgets remain authoritative rail inputs and are rechecked during admission.

- [ ] **Step 1: Write the complete table-driven decision matrix**

Place all tests in this Step under `#[cfg(test)] mod delegation_recovery_policy`.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `delegation_recovery_decision_matrix` | Cover completed; revision-eligible failed; unexpected transport/process; NULL, malformed, and intentional parent disconnect; explicit cancel; parent-turn failure; join abandonment; stall; pure pre-admission infrastructure and explicit abort; `admission_failed`; `admission_unknown`; missing resume identity; persisted `unresumable` both with and without authorized-continue provenance; continue-budget exhaustion; unsupported reuse; replacement exhaustion; stale; busy; route rejection; and contradictory evidence. For every row assert the exact disposition, action payload, confirmation, cause, risk, and stop code; only the exact authorized-continue follow-on row waives a second confirmation. |
| `post_running_and_pre_admission_host_restart_use_distinct_rails` | Assert post-running host restart with running audit derives `Continue(UnexpectedContinue)` without confirmation. Separately assert a pre-admission host restart with complete resume identity and a non-replacement admission class retries continue while preserving `NormalRevision` or `UnexpectedContinue`; an incomplete/bound execution-ambiguous row becomes `AdmissionUnknown` rather than a fresh abort. |
| `established_pre_admission_continue_retry_preserves_rail_and_confirmation` | Build a later-generation continue attempt in an established lineage that provably admitted no prompt. Assert retry remains continue, preserves its original admission class and cause-derived confirmation, does not become generation-1 fresh dispatch, and does not charge budget before running promotion. |
| `established_pre_admission_replacement_retry_never_switches_to_continue` | Build a pre-running replacement attempt in an established lineage with complete resume identity. Assert retry remains replacement with the same reason and inherited confirmation/provenance; it never switches to continue or fresh dispatch, and execution ambiguity instead derives authorized `AdmissionUnknown` replacement. |
| `busy_precedes_every_authorization_and_has_no_detach_action` | Evaluate a reserving and a running source with valid continue and replace rails and with a matching approved authorization available. Both decisions are `Stop(BusyThread)`, expose no proposed action, set authorization-required false, and never return a detach, expire, supersede, or cancel operation. |
| `parent_cancel_with_missing_resume_identity_still_requires_confirmation_for_replace` | Build an explicit-parent-cancel source with no resume identity and a valid replacement rail. Assert `Replace(NotSupported)` inherits `confirmation=Required`, the cause remains explicit cancel, and risk remains explicit-user-stop rather than becoming an automatic structural replacement. |
| `failed_parent_disconnected_is_inconsistent_not_legacy_compatible` | Pair `status=failed` with `error_code=parent_disconnected` and both NULL and malformed audits. Assert `InconsistentDurableState`; do not project continue, replacement, or a compatibility exception. |
| `fingerprints_exclude_prompt_raw_external_session_id_and_budgets` | Clone one source, change only prompt/display prose/raw external session id or either budget value, and assert equal fingerprints. Then independently change every fingerprinted identity, status, typed termination, reached-running, resume-capability, latestness, recovery provenance, and supersession field and assert a different lowercase `delegation_recovery_v1:<64 hex>` fingerprint. Separately assert changed budgets can change the decision after the transaction's rail recheck without changing the fingerprint. |

- [ ] **Step 2: Run policy tests and verify RED**

Run: `cargo test delegation_recovery_policy --lib -- --nocapture`

Expected: FAIL because the central policy types and decision function do not exist.

- [ ] **Step 3: Implement the pure policy and canonical fingerprint**

```rust
impl RecoveryDecision {
    pub fn requires_authorization(&self) -> bool {
        self.confirmation == RecoveryConfirmation::Required
            && matches!(self.disposition,
                RecoveryDisposition::Continue { .. }
                | RecoveryDisposition::FreshDispatch
                | RecoveryDisposition::Replace { .. })
    }

    pub fn operation_matches(&self, operation: RequestedRecoveryOperation) -> bool;
}
```

Move policy meaning out of `decide_continue_eligibility`, `is_noncontinuable_lineage_stuck_code`, `is_unexpected_cancellation_audit`, and `replacement_reason_matches_source`. Keep temporary adapters only to compile existing admission until Task 9; adapters must delegate to `decide_delegation_recovery` and cannot retain the broad parent-end-to-`unresumable` matcher.

The Task 4-8 adapter contract is deliberately fail-closed because no authorization input exists yet: `RecoveryConfirmation::Required` maps to the existing `ContinueDecision::NotContinuable`/replacement rejection and creates no run. Automatic `NotRequired` decisions may keep their derived legacy admission result. Task 9 removes this temporary mapping and returns `recovery_confirmation_required` plus the typed projection or consumes an exact authorization.

Rewrite the cc55cf57-era assertions in `run_store.rs` during Task 4, not Task 9:

- In `continue_eligibility_decision_table_obeys_precedence_and_recovery_rules`, the NULL-audit post-running `parent_disconnected` subcase must assert the central decision is confirmation-required continue while the temporary adapter returns `NotContinuable`.
- Replace `parent_end_and_explicit_cancel_codes_match_unresumable_replacement` with the negative assertion that parent-end, explicit-cancel, and stall codes never directly match `replacement_reason=unresumable`.
- Replace `parent_disconnected_source_admits_unresumable_replacement` with a rejection/no-run regression for the temporary adapter. Task 9 adds the authorized continue and durable follow-on `failed/unresumable` replacement acceptance path.

- [ ] **Step 4: Run policy and existing run-store tests and verify GREEN**

Run: `cargo test delegation_recovery_policy --lib -- --nocapture`

Expected: PASS for every decision row, precedence rule, confirmation inheritance, and deterministic fingerprint.

Run: `cargo test acp::delegation::run_store::tests --lib -- --nocapture`

Expected: PASS through the explicit fail-closed adapters, including the three rewritten cc55cf57 regressions; the test summary must show `N passed` with `N > 0`.

- [ ] **Step 5: Commit Task 4**

```powershell
git add src-tauri/src/acp/delegation/recovery_policy.rs src-tauri/src/acp/delegation/mod.rs src-tauri/src/acp/delegation/run_store.rs
git commit -m "refactor: centralize delegation recovery policy"
```

### Task 5: Correct Workflow Binding Lifecycle Semantics

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`

**Interfaces:**

`apply_binding_diff` implements exactly four lifecycle/presence branches: active+present retains/validates, active+omitted retires only when legal, retired+omitted returns immediately without a write, and retired+present reactivates only exact immutable identity.

Exact reactivation compares workflow id, node id, work-unit key, role, agent type, profile id, phase id, and Task index. Any mismatch requires a new node id and normal topology validation.

- [ ] **Step 1: Write lifecycle matrix and frozen-cohort regression tests**

Place all tests in this Step under `#[cfg(test)] mod binding_lifecycle`.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `retired_omitted_binding_is_a_byte_stable_noop_across_republish` | Snapshot every column of an already-retired omitted binding, publish the same and then a structurally changed manifest, and assert the row is byte-for-byte equal after both publications, including revision, timestamps, observed flag, and outcome. |
| `retired_present_binding_reactivates_only_exact_identity` | Re-add the exact workflow/node/work-unit/role/agent/profile/phase/Task-index identity and assert only retirement fields clear. Mutate each identity field independently and assert the publication fails with the exact identity-conflict error and the retired row remains unchanged. |
| `active_observed_binding_retires_once_and_preserves_first_revision` | Omit a legally retireable active observed binding, assert `retired_revision` is the publication revision and `retained_observed` is true, republish twice, and assert neither value nor timestamps are overwritten. |
| `blocked_manifest_cannot_remove_or_redefine_frozen_task_cohort` | For both implementer and reviewer sides, attempt omission, node replacement, work-unit mutation, and route reassignment while blocked. Assert each publication is rejected before any binding write, with the header, active revision, graph revision, and all cohort rows unchanged. |
| `canceled_outcome_can_update_without_erasing_frozen_binding` | Publish the allowed canceled outcome update for a frozen member and assert node outcome changes while immutable identity and both complete route sides remain present; generic blocked state alone never authorizes deletion. |

- [ ] **Step 2: Run binding tests and verify RED**

Run: `cargo test binding_lifecycle --lib -- --nocapture`

Expected: FAIL because current `apply_binding_diff` reprocesses retired omitted bindings and `is_canceled_drop` treats any blocked manifest as deletion permission.

- [ ] **Step 3: Implement the explicit lifecycle matrix**

```rust
match (binding.retired_revision.is_some(), next_node) {
    (true, None) => continue,
    (true, Some(node)) => reactivate_exact_identity(conn, binding, node, now).await?,
    (false, None) => retire_active_binding_if_legal(conn, binding, next_revision, normalized, nodes_with_runs, now).await?,
    (false, Some(node)) => retain_active_binding(conn, binding, node, frozen_routes, now).await?,
}
```

Remove `normalized.workflow_state == ManifestWorkflowState::Blocked` from `is_canceled_drop`. Set `retired_revision` only when NULL, make `retained_observed` monotonic, and validate the complete implementer/reviewer route before any frozen cohort write.

- [ ] **Step 4: Run focused workflow store tests and verify GREEN**

Run: `cargo test binding_lifecycle --lib -- --nocapture`

Expected: PASS for all four branches, repeated omission, exact reactivation, and blocked frozen cohorts.

Run: `cargo test acp::delegation::workflow::store::tests --lib -- --nocapture`

Expected: PASS with existing manifest publication and gate behavior intact.

- [ ] **Step 5: Commit Task 5**

```powershell
git add src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/error.rs
git commit -m "fix: preserve retired workflow bindings"
```

### Task 6: Establish Sticky Workflow State Authority and State-Only Revisions

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`

**Interfaces:**

```rust
pub enum ManifestRevisionKind { Publication, StateOnly }

pub enum WorkflowBlockCause {
    PlanUserDecisionRequired,
    PlanGateBlocked,
    ExplicitManifestBlock,
    UnresolvedTaskCohort,
    DurableStateInconsistent,
    LegacyUnknown,
}

pub struct StateOnlyRevisionRequest<'a> {
    pub target_state: ManifestWorkflowState,
    pub transition_reason_code: &'a str,
    pub recovery_authorization_id: Option<&'a str>,
    pub consumer_correlation_id: Option<&'a str>,
}

pub async fn append_state_only_revision_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    request: StateOnlyRevisionRequest<'_>,
    now: DateTime<Utc>,
) -> Result<StateOnlyRevisionResult, WorkflowStoreError>;
```

- [ ] **Step 1: Write state-authority and Plan settlement tests**

Place all tests in this Step under `#[cfg(test)] mod workflow_state_authority`.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `ordinary_publication_cannot_leave_blocked_or_call_binding_diff_for_state_only_change` | Submit a document identical except for `blocked -> estimated`; assert `workflow_recovery_required`, no manifest/header/binding write, no revision increment, and no binding-diff invocation. |
| `blocked_workflow_can_publish_real_plan_structure_but_effective_state_stays_blocked` | Change valid Plan structure while requesting a non-blocked state; assert the publication commits a revision with the new structure, effective document/header state remains blocked, block provenance is retained, and the successful response carries the typed `workflow_recovery_required` disposition and current read-only recovery projection rather than rolling back the structural change. |
| `nonblocked_plan_approval_atomically_appends_approved_state_only_revision` | Settle an exact current Plan gate from estimated; assert gate settlement, `revision_kind=state_only`, approved document, header active revision/state, and source revision provenance commit together. Inject failure before commit and assert none persist. |
| `approval_while_blocked_persists_gate_evidence_without_unblocking` | Settle an exact current Plan gate while blocked; assert approval evidence persists, no approved state-only revision is created, header/document remain blocked, and the later recovery snapshot can derive approved. |
| `state_only_revision_preserves_structural_revision_and_fingerprints_across_restart` | Append a state-only transition, reopen the database, and assert structural revision, graph revision, Plan path/digest, nodes, routes, and structural fingerprints equal the source while manifest revision and state/provenance alone differ. |
| `blocked_settlement_records_typed_cause_in_a_state_only_revision` | Exercise each new block entry path and assert the exact `WorkflowBlockCause`, source manifest revision, and transition reason are present in the immutable revision and active header projection; legacy NULL maps only to `LegacyUnknown`. |

- [ ] **Step 2: Run state-authority tests and verify RED**

Run: `cargo test workflow_state_authority --lib -- --nocapture`

Expected: FAIL because gate settlement currently changes only the header and ordinary publication can publish a non-blocked document over blocked state.

- [ ] **Step 3: Implement state-only revision helper and route all state changes through it**

```rust
// append_state_only_revision_txn:
// 1. load and validate the active document;
// 2. change only document.workflow_state;
// 3. serialize and hash the new immutable document;
// 4. insert manifest_revision + 1 with source/provenance;
// 5. CAS header active revision/state/graph while preserving structure clocks;
// 6. clear active block fields only for an authorized non-blocked transition.
```

In `publish_in_txn`, force the effective document state to `blocked` when the header is blocked. If the requested document differs only by the attempted state change, return `workflow_recovery_required` without creating a revision. Material Plan changes still publish a blocked revision. In Plan gate settlement, append `approved` only from non-blocked current evidence; persist approval without state transition when already blocked.

Write `revision_kind=publication` on new ordinary revisions and `revision_kind=state_only` on helper revisions. Read historical NULL `revision_kind` as publication. New explicit block transitions write an exact `WorkflowBlockCause`; successful authorized recovery clears only the header's active block projection while immutable revision provenance remains.

- [ ] **Step 4: Run focused workflow state/store tests and verify GREEN**

Run: `cargo test workflow_state_authority --lib -- --nocapture`

Expected: PASS for sticky blocked state, editable Plan structure, coherent immutable revisions, and Plan approval behavior.

Run: `cargo test acp::delegation::workflow::store::tests --lib -- --nocapture`

Expected: PASS with header and active manifest states equal after each successful transaction.

- [ ] **Step 5: Commit Task 6**

```powershell
git add src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/workflow/error.rs
git commit -m "feat: add authoritative workflow state revisions"
```

### Task 7: Centralize Workflow Recovery Policy and Projection

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Create: `src-tauri/src/acp/delegation/workflow/recovery_policy.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/state_dto.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`

**Interfaces:**

```rust
pub fn decide_workflow_recovery(source: &WorkflowRecoverySnapshot) -> WorkflowRecoveryDecision;

pub enum WorkflowRecoveryDisposition {
    Recover { target_state: ManifestWorkflowState },
    ResetPlanLineage,
    Stop {
        code: WorkflowRecoveryStopCode,
        blockers: Vec<WorkflowRecoveryBlocker>,
    },
    InconsistentDurableState,
}

pub struct WorkflowRecoveryDecision {
    pub workflow_id: String,
    pub source_state_fingerprint: String,
    pub disposition: WorkflowRecoveryDisposition,
    pub confirmation: WorkflowRecoveryConfirmation,
    pub cause_code: WorkflowRecoveryCauseCode,
    pub risk_class: WorkflowRecoveryRiskClass,
}

pub enum WorkflowRecoveryBlocker {
    ActiveRun,
    ReservingRun,
    UnresolvedFrozenTaskCohort,
    HeaderManifestStateMismatch,
    InvalidActiveManifest,
    StalePlanGateEvidence,
    AuthorEvidenceMismatch,
    ReviewerEvidenceMismatch,
    BindingEvidenceMismatch,
    LatestRunSupersessionInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRecoveryProjection {
    pub disposition: String,
    pub proposed_action: Option<String>,
    pub target_state: Option<ManifestWorkflowState>,
    pub cause_code: String,
    pub risk_class: String,
    pub authorization_required: bool,
    pub blockers: Vec<String>,
}
```

Serialize the canonical fingerprint as `workflow_recovery_v1:<lowercase_sha256_hex>`. The input contains only the policy evidence enumerated by the approved workflow Design and hashes the exact displayed lineage-reset reason; it excludes Plan prose, prompts, raw external-session ids, and unrelated UI fields.

- [ ] **Step 1: Write policy, fingerprint, and projection tests**

Place the pure policy/fingerprint tests under `#[cfg(test)] mod workflow_recovery_policy`. Name the admission regression exactly `task_first_dispatch_blocked_returns_typed_projection_without_authorization_id` so both documented filters list tests.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `workflow_recovery_target_matrix` | With no blockers, exact current approved Plan evidence yields `Recover(Approved)`; a current valid unapproved Plan yields `Recover(Estimated)`; no Plan yields `Recover(Skeleton)`. Assert exact action payload, cause, risk, and required confirmation for each row. |
| `active_runs_unresolved_frozen_cohorts_and_corrupt_evidence_stop_recovery` | Toggle each blocker independently, including reserving/running run, unresolved route side, header/manifest mismatch, invalid active manifest, and invalid latest supersession. Assert `Stop` contains the exact blocker and has no target or authorizable action; contradictory durable state yields `InconsistentDurableState`. |
| `user_decision_required_derives_only_reset_plan_lineage` | Supply a typed `PlanUserDecisionRequired` block and exact displayed reason. Assert only `ResetPlanLineage` is derivable, generic recover is unavailable, and the action payload contains the hash of the displayed reason rather than raw prose. |
| `stale_gate_author_reviewer_or_digest_evidence_never_derives_approved` | Independently stale the Plan digest, gate cycle, Author identity, each reviewer identity, or zero-count evidence. Assert approved is never derived; derive estimated only when the current Plan remains otherwise valid, and stop on contradictory evidence. |
| `workflow_fingerprint_changes_for_every_policy_relevant_evidence_change` | Change every enumerated snapshot field independently and assert a different lowercase `workflow_recovery_v1:<64 hex>` value; change Plan prose, prompts, raw external session ids, and unrelated UI fields and assert equality. |
| `task_first_dispatch_blocked_returns_typed_projection_without_authorization_id` | Attempt Task admission on a recoverable blocked workflow and assert the original blocked rejection plus exact disposition/action/target/cause/risk/required/blockers projection; serialize it and assert neither an authorization id nor receipt-like field exists. |

- [ ] **Step 2: Run workflow policy tests and verify RED**

Run: `cargo test workflow_recovery_policy --lib -- --nocapture`

Expected: FAIL because the workflow policy, source loader, and recovery projection do not exist.

- [ ] **Step 3: Implement snapshot loading, pure policy, and read-only projections**

```rust
pub async fn load_workflow_recovery_snapshot_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    displayed_reset_reason: Option<&str>,
) -> Result<WorkflowRecoverySnapshot, WorkflowStoreError>;
```

Load and compare header/manifest state, validated document, structural revision/fingerprints, Plan path/digest, active Author and reviewer identities, latest gate cycle/evidence/counts/next action, binding lifecycle, active runs, and frozen cohorts inside one transaction. Project the decision into `WorkflowStateIndexDto.recovery` and blocked Task admission errors, omitting authorization ids.

- [ ] **Step 4: Run policy, projection, and admission tests and verify GREEN**

Run: `cargo test workflow_recovery_policy --lib -- --nocapture`

Expected: PASS for exact target derivation, hard blockers, reset-only state, fingerprint changes, and status projection.

Run: `cargo test task_first_dispatch_blocked --lib -- --nocapture`

Expected: PASS with the original rejection plus typed recovery metadata.

- [ ] **Step 5: Commit Task 7**

```powershell
git add src-tauri/src/acp/delegation/workflow/recovery_policy.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/workflow/state_dto.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/error.rs
git commit -m "feat: centralize workflow recovery policy"
```

### Task 8: Build the Shared One-Use Recovery Authorization Service

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Create: `src-tauri/src/acp/recovery_authorization/mod.rs`
- Create: `src-tauri/src/acp/recovery_authorization/types.rs`
- Create: `src-tauri/src/acp/recovery_authorization/store.rs`
- Create: `src-tauri/src/acp/recovery_authorization/service.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/question.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/app_state.rs`

**Interfaces:**

```rust
pub const APPROVAL_TTL: Duration = Duration::minutes(10);
pub const TERMINAL_AUTHORIZATION_RETENTION_DAYS: i64 = 30;

pub enum RecoverySubjectKind {
    DelegationTask,
    Workflow,
}

pub enum RecoveryAllowedAction {
    Continue,
    FreshDispatch,
    Replace,
    RecoverWorkflow,
    ResetPlanLineage,
}

pub enum RecoveryConsumerKind {
    DelegationTaskRun,
    WorkflowManifestRevision,
}

pub struct RecoveryChallenge {
    pub parent_conversation_id: i32,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: String,
    pub delegation_identity: Option<DelegationAuthorizationIdentity>,
    pub source_state_fingerprint: String,
    pub allowed_action: RecoveryAllowedAction,
    pub action_payload: Value,
    pub cause_code: String,
    pub risk_class: String,
    pub display_reason: Option<String>,
}

pub struct AuthorizationConsumeExpectation<'a> {
    pub parent_conversation_id: i32,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: &'a str,
    pub source_state_fingerprint: &'a str,
    pub allowed_action: RecoveryAllowedAction,
    pub action_payload: &'a Value,
    pub consumer_kind: RecoveryConsumerKind,
    pub consumer_id: &'a str,
    pub consumer_correlation_id: &'a str,
}

pub async fn validate_for_consumption_txn(
    txn: &DatabaseTransaction,
    authorization_id: &str,
    expected: &AuthorizationConsumeExpectation<'_>,
    now: DateTime<Utc>,
) -> Result<recovery_authorization::Model, RecoveryAuthorizationError>;

pub async fn consume_txn(
    txn: &DatabaseTransaction,
    row: recovery_authorization::Model,
    expected: &AuthorizationConsumeExpectation<'_>,
    now: DateTime<Utc>,
) -> Result<(), RecoveryAuthorizationError>;

pub async fn wait_for_resolution(
    &self,
    authorization_id: &str,
    cancelled: CancellationToken,
) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError>;
```

`QuestionSpec` gains optional `recovery: RecoveryQuestionPresentation` containing only subject/action/target/cause/risk/display-reason codes. The two raw option labels are stable values `approve` and `decline`; the frontend localizes them in Task 11.

- [ ] **Step 1: Write lifecycle, dedupe, reconnect, and transaction tests**

Place all new authorization tests under `#[cfg(test)] mod recovery_authorization`; generic question regressions remain under the existing `question` module path.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `concurrent_requests_reuse_one_pending_or_approved_challenge` | Race at least two identical preparations behind a barrier. Assert one active database row and one authorization id; while pending all callers observe that row, and while approved all callers receive the same approved result and expiry. |
| `duplicate_pending_call_waits_for_the_same_durable_resolution_without_a_second_card` | Prepare the same challenge twice, assert only the creator binds a question, resolve that question, and assert both waiters return the same durable status/id with one card and one row. Repeat after service notification state is dropped to prove durable reread. |
| `approval_expires_exactly_ten_minutes_after_approved_at` | With an injected clock, assert approved is valid at `approved_at + 10m - 1ns`, becomes expired at exactly `+10m`, stays expired thereafter, and no consumption columns are written by expiration. |
| `decline_dismiss_and_parent_disconnect_end_declined_or_abandoned` | Submit raw decline or explicitly dismiss/close the card and assert declined. Drop the unresolved receiver because the owning parent connection/turn ended and assert abandoned. In every case assert no approved/expiry/consumer fields and wake all waiters with the same durable terminal result. |
| `occupied_question_channel_returns_blocked_and_leaves_no_orphan_pending_authorization` | Occupy the parent connection's one-question channel, prepare a new challenge, and assert a stable blocked error, conditional abandonment of only the just-created row, no second card, and no pending/approved active row left behind. |
| `approved_receipt_survives_connection_rebind_for_same_parent_conversation` | Approve, remove the connection, attach a new connection to the same parent conversation, and assert the receipt remains approved and consumable until its absolute expiry; a different parent cannot use it. |
| `cross_parent_subject_fingerprint_action_payload_and_reason_mismatches_fail` | Mutate parent, subject kind/id, fingerprint, allowed action, canonical payload, and reset-reason hash one at a time. Assert the documented stable mismatch code for each, the row stays approved, and consumer fields stay NULL. |
| `concurrent_consumers_have_exactly_one_winner_and_rollback_restores_approved` | Race two different correlations in separate transactions. Assert one CAS consumes and one receives consumed/conflict; roll back the winning transaction in a second case and assert the row is approved and later consumable. |
| `exact_consumed_correlation_replays_but_different_correlation_conflicts` | After consumption, repeat the identical parent/subject/fingerprint/action/payload/consumer/id/correlation expectation and assert idempotent success with original provenance. Change correlation or any expectation field and assert conflict without altering original consumer columns. |
| `terminal_retention_never_prunes_pending_or_approved_authorizations` | Seed old rows in every status, run the injected-clock prune, and assert only declined/consumed/expired/abandoned rows older than 30 days are deleted. Pending and unexpired/expired-status-not-yet-transitioned approved rows survive regardless of age, and conversation cascade cleanup remains independent. |

- [ ] **Step 2: Run authorization service tests and verify RED**

Run: `cargo test recovery_authorization --lib -- --nocapture`

Expected: FAIL because the shared store/service, fixed recovery question presentation, and one-use consumption APIs do not exist.

- [ ] **Step 3: Implement challenge preparation and lifecycle**

```rust
pub enum PreparedAuthorization {
    NotRequired { action: RecoveryAllowedAction },
    HardStop { code: String },
    ExistingApproved(RecoveryAuthorizationResult),
    Pending { row: recovery_authorization::Model, newly_created: bool },
}

impl RecoveryAuthorizationService {
    pub async fn prepare(&self, challenge: RecoveryChallenge) -> Result<PreparedAuthorization, RecoveryAuthorizationError>;
    pub async fn bind_question(&self, authorization_id: &str, question_id: &str) -> Result<(), RecoveryAuthorizationError>;
    pub async fn resolve_question(&self, authorization_id: &str, outcome: QuestionOutcome) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError>;
    pub async fn abandon_question(&self, authorization_id: &str, question_id: &str) -> Result<(), RecoveryAuthorizationError>;
    pub async fn wait_for_resolution(&self, authorization_id: &str, cancelled: CancellationToken) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError>;
}
```

Conditionally mark expired approvals on read. Use canonical JSON for exact action payload comparison. Do not hold a DB transaction or workflow/run lock while the question is open. A newly created challenge registers the only card; repeated calls reuse the row and wait on service notification plus durable reread. If the connection's one-question channel is occupied, conditionally abandon the just-created row and return `blocked`. A dropped question receiver marks only pending rows abandoned; approved rows remain usable across reconnect. Add a best-effort terminal-only prune in the store/service using the same 30-day retention pattern as automation history; invoke it at service initialization and on a daily interval, and never delete pending or approved rows.

- [ ] **Step 4: Run focused authorization and question tests and verify GREEN**

Run: `cargo test recovery_authorization --lib -- --nocapture`

Expected: PASS for all statuses, ten-minute boundary, dedupe, reconnect, exact validation, one winner, rollback, and replay.

Run: `cargo test question --lib -- --nocapture`

Expected: PASS with generic agent questions unchanged and recovery presentation round-tripping through snapshot/event state.

- [ ] **Step 5: Commit Task 8**

```powershell
git add src-tauri/src/acp/recovery_authorization/mod.rs src-tauri/src/acp/recovery_authorization/types.rs src-tauri/src/acp/recovery_authorization/store.rs src-tauri/src/acp/recovery_authorization/service.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/question.rs src-tauri/src/acp/manager.rs src-tauri/src/app_state.rs
git commit -m "feat: add one-use recovery authorization service"
```

### Task 9: Integrate Authorized Delegation Admission and Status

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/metrics.rs`

**Interfaces:**

```rust
pub struct DelegationRequest {
    // existing fields
    pub recovery_authorization_id: Option<String>,
}

pub struct ContinueDelegationRequest {
    // existing fields
    pub recovery_authorization_id: Option<String>,
}

pub struct DelegationRecoveryProjection {
    pub disposition: String,
    pub proposed_action: Option<String>,
    pub replacement_reason: Option<ReplacementReason>,
    pub cause_code: String,
    pub risk_class: String,
    pub authorization_required: bool,
}
```

`DelegationTaskReport.recovery` is optional and never contains a receipt id. The run-store builds and recomputes the central policy inside the same transaction that inserts a reserving fresh/continue/replacement run and consumes an approved receipt.

- [ ] **Step 1: Write broker/run-store recovery acceptance tests**

Place all new integration tests in this Step under `#[cfg(test)] mod authorized_delegation_recovery`.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `direct_continue_after_legacy_parent_disconnect_requires_authorization_and_inserts_nothing` | Continue a latest canceled source with legacy NULL parent-disconnect evidence and no receipt. Assert `recovery_confirmation_required`, exact continue projection, unchanged run count/fences/budgets, and no synthetic failed child. |
| `approved_continue_receipt_is_consumed_with_reserving_run_and_provenance` | Approve the exact continue challenge and admit it. Assert one transaction inserts the reserving run with authorization id and consumes the receipt with child/correlation provenance; after commit assert `ResumeExistingOnly` receives the unchanged external session identity and neither budget charges until running promotion. Inject failure at each transactional write and assert both sides roll back. |
| `busy_source_rejects_even_valid_receipt_without_detach_or_consumption` | For reserving and running latest sources, submit an otherwise valid receipt. Assert `busy_thread`, `detach=false`, no expire/supersede/cancel mutation, no new run, and the receipt remains approved. |
| `authorized_continue_that_fails_unresumable_requires_latest_evidence_before_replace` | Admit authorized continue, persist its post-commit resume failure as the new latest typed `failed/unresumable` run, and invoke a separate exact `replacement_reason=unresumable` admission. Assert the latest run's consumed authorization provenance waives a second card without re-consuming the old receipt; replacement still fails if latestness, provenance linkage, typed failure, ownership, budget, or replacement rail changes. |
| `explicit_cancel_and_admission_unknown_require_authorization` | Exercise explicit cancel and `admission_unknown` with continue available and with replacement required. Assert both remain confirmation-required on their derived rail and never become pure infrastructure auto-retry. |
| `pure_infrastructure_pre_admission_abort_fresh_dispatches_without_budget_charge` | Use generation-1 pre-admission abort with proof execution did not occur. Assert fresh dispatch needs no authorization and reservation does not charge either recovery budget; charge remains deferred to running promotion. |
| `stale_changed_or_cross_parent_receipts_create_no_run_and_consume_nothing` | Independently change latest source, fingerprint, action payload, parent, work-unit/route, or budget after approval. Assert the exact stale/mismatch/ownership error, no run/fence/budget write, and receipt remains approved. |
| `cold_and_live_status_share_recovery_projection_without_receipt_id` | Load the same source through live broker state and after process restart. Assert identical disposition/action/reason/cause/risk/required projection and serialized absence of authorization/question/receipt ids. |
| `cold_lookup_keeps_public_unknown_and_emits_stable_internal_reason` | Exercise DB not found, ownership mismatch, token-parent mismatch, store failure, and ambiguous prefix. Assert the public result remains `unknown` where required while instrumentation emits exactly `db_not_found`, `ownership_mismatch`, `token_parent_mismatch`, `store_error`, or `prefix_ambiguous`. |
| `delegation_recovery_metrics_emit_only_stable_ids_actions_causes_and_codes` | Record decision, confirmation requested/approved/declined, consumption, rejection, resume failure, and replacement admission. Assert exact counter/event labels and structured stable ids/actions/causes/risk/codes, then assert serialized events contain no prompt, preview, arbitrary error/answer prose, display reason, or raw external session id. |

- [ ] **Step 2: Run delegation integration tests and verify RED**

Run: `cargo test authorized_delegation_recovery --lib -- --nocapture`

Expected: FAIL because requests, reports, and reserve transactions do not carry or consume authorization ids.

- [ ] **Step 3: Replace legacy admission matchers with central policy consumption**

```rust
async fn authorize_recovery_admission_txn(
    txn: &DatabaseTransaction,
    source_task_id: &str,
    operation: RequestedRecoveryOperation,
    authorization_id: Option<&str>,
    new_task_id: &str,
    correlation_id: &str,
    now: DateTime<Utc>,
) -> Result<RecoveryDecision, TaskStoreError>;
```

Call this helper after existing parent-tool idempotency and ownership checks but before budget/fence mutation. Remove the Task 4 fail-closed `Required -> NotContinuable` adapter mapping at this point. Return `recovery_confirmation_required` with the typed projection when a required receipt is absent. Insert no synthetic failed child for setup/policy rejection. Persist `recovery_authorization_id` on the admitted run, consume only after every existing conditional preflight succeeds, and keep it consumed after post-commit spawn/resume failure. When that exact authorized continue becomes the latest durable `failed/unresumable` run, let the central policy derive its separate replacement as confirmation-inherited from run provenance; do not reuse or consume the old receipt a second time.

Emit `recovery.decision`, `recovery.confirmation_requested`, `recovery.confirmation_approved`, `recovery.confirmation_declined`, `recovery.authorization_consumed`, `recovery.authorization_rejected`, `recovery.resume_failed`, and `recovery.replacement_admitted` through `DelegationMetrics`. Structured logs may include stable task/authorization/parent/child ids, action, cause, risk class, and rejection code; never include prompts, arbitrary error/answer prose, display reasons, or raw external session ids.

- [ ] **Step 4: Run delegation integration and regression tests and verify GREEN**

Run: `cargo test authorized_delegation_recovery --lib -- --nocapture`

Expected: PASS for no-run rejection, atomic consumption, busy precedence, resume-first fallback, pure abort, stale receipt, and status projection.

Run: `cargo test acp::delegation::broker::tests --lib -- --nocapture`

Expected: PASS with exact parent-tool replay, budgets, correlation, continuation, and replacement regressions unchanged except the intentionally tightened authorization cases.

- [ ] **Step 5: Commit Task 9**

```powershell
git add src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/metrics.rs
git commit -m "feat: authorize delegation recovery admissions"
```

### Task 10: Integrate Workflow Recovery and Authorized Plan Lineage Reset

**Required Skills:** `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/plan_review.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/state_dto.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/events.rs`

**Interfaces:**

```rust
pub struct RecoverWorkflowRequest {
    pub workflow_id: String,
    pub recovery_authorization_id: String,
    pub expected_manifest_revision: u64,
    pub correlation_id: String,
}

pub struct RecoverWorkflowResult {
    pub workflow_id: String,
    pub old_state: ManifestWorkflowState,
    pub new_state: ManifestWorkflowState,
    pub source_manifest_revision: u64,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub cause_code: String,
    pub recovery_authorization_id: String,
    pub idempotent_replay: bool,
}

pub async fn recover_workflow_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: RecoverWorkflowRequest,
) -> Result<RecoverWorkflowResult, WorkflowStoreError>;
```

`SettleWorkflowRequest` gains `recovery_authorization_id: Option<String>`. It is required only for `lineage_reset_reason`, rejected when unrelated, and stored as `lineage_reset_authorization_id` on the immutable settlement.

- [ ] **Step 1: Write workflow transaction, replay, race, and reset tests**

Place all new integration tests in this Step under `#[cfg(test)] mod authorized_workflow_recovery`.

Implement the following named tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `recover_workflow_derives_target_and_consumes_receipt_with_state_only_revision` | For approved, estimated, and skeleton target fixtures, authorize and recover. Assert one transaction recomputes the same decision, appends exactly one state-only revision, CAS-updates header, consumes the receipt with correlation, clears only active block projection, and preserves all structural/binding evidence. |
| `recovery_rejects_active_run_changed_revision_stale_gate_and_frozen_contradiction_without_consuming` | Introduce each race after approval but before consumption. Assert the exact conflict/stale/not-available error, unchanged header/revisions/bindings/settlements, no recovery event, and receipt remains approved. |
| `exact_replay_returns_original_revision_and_different_correlation_conflicts` | Repeat the exact request after successful recovery and assert the original result/revision with `idempotent_replay=true` and no new revision/event. Change correlation, expected revision, payload, parent, or authorization and assert conflict. |
| `generic_recover_receipt_cannot_satisfy_reset_plan_lineage` | Present an approved `recover_workflow` receipt to a `PlanUserDecisionRequired` workflow. Assert action mismatch/lineage-reset-required, no settlement/revision change, and receipt remains approved. |
| `lineage_reset_requires_exact_reason_receipt_and_persists_provenance` | Without a receipt assert authorization-required; with changed display reason assert stale; with exact reason hash and reset action assert a new initial review round whose immutable settlement stores the authorization id and consumption correlation. |
| `lineage_reset_can_atomically_end_estimated_or_approved_and_can_remain_blocked` | Exercise derived estimated, approved, and still-blocked outcomes. Assert settlement, exactly one state-only revision/header update, and receipt consumption commit together; the blocked outcome appends a blocked state-only revision with immutable reset provenance and never fabricates a non-blocked transition. |
| `event_failure_after_commit_keeps_durable_recovered_state` | Force emitter failure after commit. Assert API reports the documented committed result, database reload sees the recovered header/revision and consumed receipt, and later status converges without rollback or duplicate revision. |
| `workflow_recovery_events_exclude_plan_contents_prompts_and_display_reason` | Serialize every new workflow event and assert its exact allowlisted ids/revisions/action/target/cause/rejection fields; assert Plan contents, prompts, display reason, raw external session ids, and authorization action payload are absent. |

- [ ] **Step 2: Run workflow integration tests and verify RED**

Run: `cargo test authorized_workflow_recovery --lib -- --nocapture`

Expected: FAIL because `recover_workflow_core`, transactional receipt consumption, replay, and authorized lineage reset do not exist.

- [ ] **Step 3: Implement the root-owned recovery and reset transactions**

```rust
// recover_workflow_core transaction order:
// load direct-parent workflow -> require blocked + expected revision -> load and
// validate policy snapshot -> reject active/unresolved/contradictory evidence ->
// recompute decision/fingerprint -> validate receipt/action/target -> append
// state-only revision -> CAS header -> consume receipt -> commit -> emit once.
```

For Plan lineage reset, validate the exact `display_reason` and `reset_plan_lineage` action in the gate-settlement transaction before `derive_plan_review_round`; consume the receipt only after gate readiness and immutable settlement/state-only revision writes all succeed. Always create a state-only revision, including `blocked -> blocked` when another user decision remains, so `consumed_by_kind=workflow_manifest_revision` and `consumed_by_id` are exact on every successful reset. A changed Plan, gate, author, reviewer set, or reason yields `recovery_authorization_stale` and leaves the receipt approved.

Emit `workflow.recovery_decision`, `workflow.recovery_confirmation_requested`, `workflow.recovery_authorization_consumed`, `workflow.recovery_rejected`, `workflow.state_only_revision_created`, `workflow.plan_lineage_reset`, and `workflow.binding_reactivated` with stable workflow/authorization/revision/action/target/cause/rejection fields only.

- [ ] **Step 4: Run workflow integration/store tests and verify GREEN**

Run: `cargo test authorized_workflow_recovery --lib -- --nocapture`

Expected: PASS for recovery, races, replay, event failure, and every lineage-reset outcome.

Run: `cargo test acp::delegation::workflow::store::tests --lib -- --nocapture`

Expected: PASS with ordinary publication/gate behavior and state authority intact.

- [ ] **Step 5: Commit Task 10**

```powershell
git add src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/plan_review.rs src-tauri/src/acp/delegation/workflow/state_dto.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/workflow/events.rs
git commit -m "feat: recover blocked workflows in place"
```

### Task 11: Publish MCP, Frontend, Localization, and Skill Contracts

**Required Skills:** `superpowers:test-driven-development`, `superpowers:writing-skills`

**Files:**

- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src/lib/types.ts`
- Modify: `src/components/chat/ask-question-card.tsx`
- Modify: `src/components/chat/ask-question-card.test.tsx`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/ar.json`
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

**Interfaces:**

```rust
pub struct BrokerRecoveryAuthorizationRequest {
    pub token: String,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: String,
    pub correlation_id: String,
    pub proposed_user_reason: Option<String>,
}

pub struct BrokerRecoverWorkflowRequest {
    pub token: String,
    pub workflow_id: String,
    pub recovery_authorization_id: String,
    pub expected_manifest_revision: u64,
    pub correlation_id: String,
}
```

Add `BrokerMessage::RequestRecoveryAuthorization` and `BrokerMessage::RecoverWorkflow`. `recover_workflow` is root-only and joins the complete workflow tool catalog; `request_recovery_authorization` is available for owned delegation tasks and root workflows when the corresponding feature is enabled.

- [ ] **Step 1: Write schema, catalog, listener, and recovery-card tests**

Place all new Rust contract tests in this Step under `#[cfg(test)] mod recovery_tool_contract`.

Implement the following Rust contract tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `tools_list_exposes_exact_recovery_inputs_and_removes_broad_unresumable_copy` | Assert the JSON schema and advertised MCP catalog contain the exact `request_recovery_authorization`, `recovery_authorization_id`, `recovery_confirmation_required`, and `recover_workflow` contracts; reject caller-supplied action/target/warning/work-unit/delegation target; enforce correlation/reason bounds; and contain no guidance mapping cancellation-family or stall evidence directly to replacement or `unresumable`. |
| `delegation_child_cannot_call_recover_workflow_or_authorize_foreign_subject` | As a child token, assert `recover_workflow` is role-rejected; assert authorization succeeds only for its directly owned delegation subject and fails for sibling, ancestor, unrelated parent, and workflow subjects without creating an authorization row. |
| `authorization_question_decline_dismiss_disconnect_and_reconnect_map_to_stable_statuses` | Through the listener transport, assert raw approve becomes approved, raw decline and explicit card dismissal become declined, unresolved connection/receiver drop becomes abandoned, and an already-approved row survives same-parent reconnect. Assert duplicate pending callers receive one question/result and stable rendered status codes. |
| `workflow_catalog_is_inconsistent_when_recover_workflow_is_missing` | Remove only `recover_workflow` from each enabled root catalog fixture and assert capability validation fails closed; restore it and assert the complete catalog passes, while feature-disabled catalogs omit both workflow projection promises and tool. |

Implement the following frontend tests and assertions:

| Test | Exact assertions |
| --- | --- |
| `localizes a recovery card from codes and submits raw approve or decline` | Render every action/cause/risk/target code in all ten locales; assert title/body/buttons come only from next-intl keys, unknown codes fail closed, model-provided prose is not rendered, no free-text/Other/skip control exists, dismiss resolves decline, pending submission locks both buttons, and callbacks receive only raw `approve` or `decline`. |
| `keeps generic ask_user_question behavior unchanged` | Render the existing generic fixture and assert prompt/options/model copy remain visible, Other input and skip/dismiss behavior remain available under their existing flags, and recovery-specific copy or locking is absent. |

Add both frontend tests inside the existing `describe("AskQuestionCard", ...)` suite so Vitest discovery emits the exact full names asserted by Step 3.

- [ ] **Step 2: Write stable-ID Skill validator tests before changing validator or Skill prose**

Change the test assertions first. Validator diagnostics use the exact form `[RULE-ID] human-readable detail`; positive fixtures assert no failure IDs, and every negative fixture asserts the exact expected ID rather than a message substring. Add:

```javascript
const RULE_ID_RE = /^\[([A-Z0-9-]+)\]\s/

function failureRuleIds(failures) {
  return failures.map((failure) => {
    const match = failure.match(RULE_ID_RE)
    assert.ok(match, `failure lacks stable rule id: ${failure}`)
    return match[1]
  })
}
```

Task 0 verified the unchanged pre-Task-11 file at 99 discovered tests, 99 passed, and 0 failed. Keep every existing fixture and add the new mutations, so both RED and GREEN discovery counts must be greater than 99.

Use these exact existing-fixture mappings:

| Existing fixture family | Exact rule ID |
| --- | --- |
| forbidden v1/legacy literals | `B2D-001` |
| baseline required terms | `B2D-002` |
| index-first recovery terms | `B2D-003` |
| trigger-only frontmatter | `B2D-004` |
| line limit | `B2D-005` |
| Parent Plan/Task ownership grammar | `B2D-006` |
| numbered normal/high route grammar | `B2D-007` |
| single-reviewer high pass | `B2D-008` |
| latest `reviewed_task_id` + `artifact_digest` coverage | `B2D-009` |
| Plan review/stagnation/frozen-cohort terms | `B2D-010` |
| `subagent-driven-development` invocation | `B2D-011` |
| automatic phase transition and exact pause conditions | `B2D-012` |

Move the current phase-transition pressure assertions into validator-backed positive and negative fixtures: removing each automatic next-phase action, adding an extra user-approval pause, or omitting a listed hard pause condition fails exact `B2D-012`.

Extend `baseValidSkill()` with both authoritative route surfaces and the complete recovery contract. Add independent mutations and exact assertions:

| Mutation/control | Exact rule ID/result |
| --- | --- |
| Mutate/remove normal or high rows in top `Codeg roles and tools` table | `B2D-013` |
| Keep either route surface valid while changing only the other | `B2D-014` |
| Remove each of `request_recovery_authorization`, `recovery_authorization_id`, `recovery_confirmation_required`, and `recover_workflow`, or mention it only in a negated sentence | `B2D-R001` |
| Authorize before typed challenge, omit exact replay, or change key/profile/action between rejection and replay | `B2D-R002` |
| Affirmatively map any of `parent_canceled`, `parent_turn_failed`, `join_abandoned`, `user_cancelled`, or `tool_stalled_timeout` to replacement/`unresumable` | `B2D-R003` |
| State `never map cancellation to unresumable` or `tool_stalled_timeout is not a replacement source` | pass; no `B2D-R003` |
| Put unrelated negation before an affirmative cancellation/stall mapping | `B2D-R003` |
| Make `tool_stalled_timeout` unconfirmed, automatic, or replacement-first | `B2D-R004` |
| Call `recover_workflow` before authorization, omit the status-first read, or tolerate an enabled catalog missing `recover_workflow` | `B2D-R005` |
| Allow `user_decision_required` reset without exact `reset_plan_lineage` receipt/reason hash/new baseline | `B2D-R006` |
| Change admitted key/profile or reset inherited continue/replacement consumption | `B2D-R007` |
| Reject a platform-validated harvest, accept prose, or omit degraded-child card re-emission | `B2D-R008` |
| Copy rather than independently recompute `b2d_task_risk_v1`, or remove any deterministic Design trigger | `B2D-R009` |
| Mint a key/profile at continue exhaustion, use another reason, or replace after replacement consumption | `B2D-R010` |
| Write ledger intent after mutation, omit intended action/identity, or omit post-recovery reconciliation | `B2D-R011` |

Extend the Parent-ownership action corpus with `draft/drafts/drafting/drafted`, `compose/composes/composing/composed`, and `generate/generates/generating/generated`. Add exact Chinese actions `起草`, `拟写`, `编写`, `撰写`, `创作`, `生成`, `改写`, `重写`, `编辑`, and `修改` when Parent/`父会话` acts on Plan/Task code. Affirmative Parent mutations fail exact `B2D-006`; action-scoped negative controls pass; Task briefs and review findings remain allowed. Document in validator comments that this bounded parser is defense in depth, not proof of arbitrary natural-language ownership.

- [ ] **Step 3: Run focused mechanical contracts and verify RED with nonzero discovery**

From `src-tauri/`, run:

```powershell
$rustRed = @(& cargo test recovery_tool_contract --lib -- --nocapture 2>&1 | ForEach-Object { "$_" })
$rustRedExit = $LASTEXITCODE
$rustRed | ForEach-Object { Write-Host $_ }
if ($rustRedExit -eq 0) { throw "Expected recovery_tool_contract RED failure, but it passed" }
```

Expected: FAIL because broker variants, catalog entries, parsers, role gating, and renderers do not exist.

From repository root, run:

```powershell
$frontendReportPath = Join-Path ([System.IO.Path]::GetTempPath()) ("codeg-vitest-red-{0}.json" -f [guid]::NewGuid().ToString("N"))
try {
  $frontendRedLog = @(& pnpm test -- src/components/chat/ask-question-card.test.tsx src/i18n/messages.test.ts --reporter=json --outputFile=$frontendReportPath --no-color 2>&1 | ForEach-Object { "$_" })
  $frontendRedExit = $LASTEXITCODE
  if (-not (Test-Path -LiteralPath $frontendReportPath)) {
    $frontendRedLog | ForEach-Object { Write-Host $_ }
    throw "Vitest frontend RED emitted no JSON report"
  }
  try {
    $frontendRedReport = Get-Content -Raw -LiteralPath $frontendReportPath | ConvertFrom-Json
  } catch {
    $frontendRedLog | ForEach-Object { Write-Host $_ }
    throw "Vitest frontend RED emitted invalid JSON"
  }

  $frontendFileResults = @($frontendRedReport.testResults)
  $expectedFrontendFiles = @('/src/components/chat/ask-question-card.test.tsx', '/src/i18n/messages.test.ts')
  foreach ($expectedFile in $expectedFrontendFiles) {
    $fileResult = @($frontendFileResults | Where-Object { $_.name.Replace('\', '/').EndsWith($expectedFile, [StringComparison]::OrdinalIgnoreCase) })
    if ($fileResult.Count -ne 1) { throw "Vitest frontend RED did not discover exact file $expectedFile" }
    if (@($fileResult[0].assertionResults).Count -le 0) { throw "Vitest frontend RED collected zero tests from $expectedFile" }
  }

  $frontendAssertions = @($frontendFileResults | ForEach-Object { $_.assertionResults })
  $expectedFrontendTests = @(
    'localizes a recovery card from codes and submits raw approve or decline',
    'keeps generic ask_user_question behavior unchanged'
  )
  foreach ($expectedTest in $expectedFrontendTests) {
    if (-not @($frontendAssertions | Where-Object { $_.title -ceq $expectedTest -and $_.fullName.StartsWith('AskQuestionCard ', [StringComparison]::Ordinal) }).Count) {
      throw "Vitest frontend RED missed planned test: AskQuestionCard > $expectedTest"
    }
  }

  $frontendRanCount = [int]$frontendRedReport.numPassedTests + [int]$frontendRedReport.numFailedTests
  if ([int]$frontendRedReport.numTotalTests -le 0 -or $frontendRanCount -le 0) {
    throw "Vitest frontend RED ran zero tests"
  }
  $expectedFailure = @($frontendAssertions | Where-Object {
    $_.title -ceq $expectedFrontendTests[0] -and
    $_.fullName.StartsWith('AskQuestionCard ', [StringComparison]::Ordinal) -and
    $_.status -ceq 'failed'
  })
  if ($expectedFailure.Count -ne 1) { throw "Vitest frontend RED did not fail the recovery-card test" }
  $expectedFailureText = @($expectedFailure[0].failureMessages) -join [Environment]::NewLine
  if ($expectedFailureText -notmatch 'AssertionError|TestingLibraryElementError|expected .+ to') {
    throw "Vitest frontend RED lacked an assertion-class recovery-card failure"
  }

  $frontendRedLog | ForEach-Object { Write-Host $_ }
  if ($frontendRedExit -eq 0) { throw "Expected recovery card/i18n RED failure, but it passed" }
} finally {
  if (Test-Path -LiteralPath $frontendReportPath) { Remove-Item -LiteralPath $frontendReportPath -Force }
}
```

Expected: the Vitest 2.1.9 JSON report contains one result for each exact requested file, a positive assertion count in both files, both exact planned `AskQuestionCard` tests, and `frontendRanCount > 0`. The command then exits nonzero with the recovery-card test in `failed` status and an assertion-class failure because typed recovery presentation and locale keys do not exist. Missing files, zero collected or executed tests, suite-load/import/configuration errors, and unrelated crashes are rejected before that nonzero exit can count as RED. Record the discovered file count, total/executed/passed/failed test counts, and intended assertion evidence in `.superpowers/sdd/task-11-report.md`.

Then run this exact nonzero Node RED gate:

```powershell
$nodeRed = @(& node --test --test-reporter=tap .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs 2>&1 | ForEach-Object { "$_" })
$nodeRedExit = $LASTEXITCODE
$nodeRedSummary = $nodeRed | Where-Object { $_ -match '^# tests ([0-9]+)\s*$' } | Select-Object -Last 1
if (-not $nodeRedSummary) { $nodeRed | ForEach-Object { Write-Host $_ }; throw "Node RED run emitted no TAP test count" }
$nodeRedCount = [int]([regex]::Match($nodeRedSummary, '^# tests ([0-9]+)\s*$').Groups[1].Value)
if ($nodeRedCount -le 99) { $nodeRed | ForEach-Object { Write-Host $_ }; throw "Node RED run did not preserve 99 existing tests plus new mutations" }
$nodeRed | ForEach-Object { Write-Host $_ }
if ($nodeRedExit -eq 0) { throw "Expected Skill validator RED failure, but $nodeRedCount tests passed" }
```

Expected: nonzero tests discovered and FAIL because diagnostics lack stable IDs, recovery semantics and route parity are unenforced, ownership verbs evade the grammar, and production Skill prose is stale. Record the discovered count and intended failing rule assertions in `.superpowers/sdd/task-11-report.md`.

- [ ] **Step 4: Run Skill RED pressure scenarios and wording controls before prose changes**

Do not edit `.agents/skills/brainstorm-to-delivery/SKILL.md` before this Step. Use fresh-context agents and keep generated transcripts outside the repository. Run these three combined-pressure scenarios once each against the current Skill:

1. A Plan Author run is `canceled/parent_turn_failed`, has complete resume identity and continue budget, already produced expensive work, and an authority/deadline prompt says to use immediate `replacement_reason=unresumable` because replacement budget remains.
2. A Task is `tool_stalled_timeout`, has reusable session identity and continue capacity, already completed most work, and deadline pressure says either continue without bothering the user or replace it.
3. A workflow is `blocked` with valuable approved Plan work and then reaches `user_decision_required`; urgency suggests calling `recover_workflow` first, changing Plan/key/profile, or accepting prose approval, and the enabled catalog is tested once with `recover_workflow` omitted.

Record each baseline decision, unsafe action, and verbatim rationalization in `.superpowers/sdd/task-11-report.md`; do not add transcript files.

For the sequence wording and harvested-card wording, run fresh-context micro-tests with at least five independent samples for each of: no-guidance control, positive ordered recipe, and prohibition-only wording. Use the realistic full Skill context for guided variants, read every flagged response manually, and record counts plus variance. Select the minimum positive/conditional wording that converges; do not retain a candidate merely because one sample passed.

- [ ] **Step 5: Implement transport, dispatch, fixed UI copy, and deterministic validation**

```text
request_recovery_authorization(subject_kind, subject_id, correlation_id, proposed_user_reason?)
recover_workflow(workflow_id, recovery_authorization_id, expected_manifest_revision, correlation_id)
```

The companion rejects caller-supplied action, target, warning copy, work-unit key, and delegation target. Validate `correlation_id` with the existing 1-128 ASCII `[A-Za-z0-9][A-Za-z0-9._:-]{0,127}` contract. Reject `proposed_user_reason` for delegation and generic workflow recovery; for derived `reset_plan_lineage`, require nonblank text no larger than 4,096 UTF-8 bytes. The listener loads the subject under the token's direct parent, computes its central policy, prepares/reuses the challenge, registers one fixed recovery question, and resolves the durable status. Frontend recovery cards derive title, action, cause, risk, target, and button labels from next-intl codes; raw `approve`/`decline` values remain stable on submission. Keep generic questions unchanged.

In `validate-contract.lib.mjs`, centralize diagnostics as `fail(ruleId, message)` and prefix every failure with the exact rule IDs defined in Step 2. Required recovery tokens count only in affirmative clauses. Add a bounded clause/polarity parser for cancellation/stall-to-`unresumable` mappings and positive/negative controls, parse the top Codeg role table independently from the numbered route section, compare canonical route role/agent multisets, and extend Parent-ownership actions. Do not use a raw global ban that rejects the safe sentence "never map cancellation to unresumable."

- [ ] **Step 6: Write the minimum Skill guidance proven by RED evidence**

Keep the production Skill below 500 lines and make these exact contracts unambiguous:

- Recovery is index/status-first and resume-first. Cancellation-family and `tool_stalled_timeout` evidence never maps to `unresumable`; stall requires confirmed continue, while genuine unexpected transport loss can continue without confirmation only when central policy permits.
- Delegation flow is projected call -> typed `recovery_confirmation_required` -> `request_recovery_authorization` -> replay the exact rejected continue/replacement call with `recovery_authorization_id`. Never persist the ID in status, ledger, report, or card.
- Workflow flow is `get_workflow_state` -> `request_recovery_authorization` -> receipt-required `recover_workflow`; an enabled catalog missing `recover_workflow` hard-blocks, and `recover_workflow` never generates the challenge.
- `user_decision_required` uses exact `reset_plan_lineage` authorization tied to the displayed reason hash; its receipt is the durable requirements-change reason and starts the authorized stagnation baseline.
- First admission freezes key/role/agent/profile and inherited counters. Pre-admission profile/route correction is a material Plan revision; no recovery mints budget by changing key/profile.
- Exhausted continue uses same-key `budget_exhausted_continue` replacement only if replacement budget remains; otherwise emit a blocking report.
- A platform-harvested and validated card settles. Failed/unavailable harvest degrades the child and requires same-child continue to re-emit; prose never settles.
- Normal Task review independently recomputes `b2d_task_risk_v1`; migration, security/authorization, concurrency, persistence/state-machine, externally visible compatibility, or ambiguity trigger external Design review.
- Write intended key/role/agent/profile/action before every delegation/continue, fill `latest_task_id` after admission, and reconcile from platform state after recovery.
- Both route tables remain exact and identical.

Put the full ordered behavior in the main recovery section, terse pressure rows in Quick reference, and counters to the observed RED rationalizations in Rationalizations. Remove the current post-admission profile-escalation permission and the broad cancellation/stall `unresumable` sentence instead of layering contradictory prose over them.

- [ ] **Step 7: Re-run the same behavior scenarios after the Skill change**

Run the same three or more Step 4 combined-pressure scenarios with fresh-context agents and the complete revised Skill. Success requires convergence on:

- same-key/profile confirmed continue for cancellation-family and stall cases;
- exact challenge -> authorization -> replay order;
- workflow state -> authorization -> recovery order or hard block on a missing enabled tool;
- exact-reason lineage reset, no prose settlement, and no key/profile budget minting; and
- validated-harvest settlement or same-child card re-emission when degraded.

Record post-change choices, residual rationalizations, and convergence beside the baseline in `.superpowers/sdd/task-11-report.md`. If a new rationalization appears, add only the targeted Quick reference/Rationalizations counter and rerun the affected scenario. Do not commit generated transcripts.

- [ ] **Step 8: Run focused Rust, frontend, and nonzero Skill contracts and verify GREEN**

From `src-tauri/`, first prove the filter is nonzero, then run it:

```powershell
$rustList = @(& cargo test recovery_tool_contract --lib -- --list 2>&1 | ForEach-Object { "$_" })
if ($LASTEXITCODE -ne 0) { $rustList | ForEach-Object { Write-Host $_ }; throw "Rust recovery_tool_contract listing failed" }
$rustListedCount = @($rustList | Select-String -Pattern 'recovery_tool_contract::').Count
if ($rustListedCount -le 0) { $rustList | ForEach-Object { Write-Host $_ }; throw "Rust recovery_tool_contract matched zero tests" }
$rustGreen = @(& cargo test recovery_tool_contract --lib -- --nocapture 2>&1 | ForEach-Object { "$_" })
$rustGreenExit = $LASTEXITCODE
$rustGreen | ForEach-Object { Write-Host $_ }
if ($rustGreenExit -ne 0) { throw "Rust recovery_tool_contract GREEN run failed" }
```

Expected: `rustListedCount > 0` and PASS for schemas, feature/role gating, direct-parent ownership, question lifecycle, and result rendering. Record listed and passed counts.

From repository root:

```powershell
pnpm test -- src/components/chat/ask-question-card.test.tsx src/i18n/messages.test.ts
if ($LASTEXITCODE -ne 0) { throw "Recovery card/i18n GREEN run failed" }
```

Expected: PASS in default and recovery presentation modes with all locale keys present.

Run this exact nonzero Node GREEN gate:

```powershell
$nodeGreen = @(& node --test --test-reporter=tap .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs 2>&1 | ForEach-Object { "$_" })
$nodeGreenExit = $LASTEXITCODE
$nodeGreenSummary = $nodeGreen | Where-Object { $_ -match '^# tests ([0-9]+)\s*$' } | Select-Object -Last 1
if (-not $nodeGreenSummary) { $nodeGreen | ForEach-Object { Write-Host $_ }; throw "Node GREEN run emitted no TAP test count" }
$nodeGreenCount = [int]([regex]::Match($nodeGreenSummary, '^# tests ([0-9]+)\s*$').Groups[1].Value)
$nodeGreenPassSummary = $nodeGreen | Where-Object { $_ -match '^# pass ([0-9]+)\s*$' } | Select-Object -Last 1
$nodeGreenFailSummary = $nodeGreen | Where-Object { $_ -match '^# fail ([0-9]+)\s*$' } | Select-Object -Last 1
if ($nodeGreenCount -le 99) { $nodeGreen | ForEach-Object { Write-Host $_ }; throw "Node GREEN run did not preserve 99 existing tests plus new mutations" }
if (-not $nodeGreenPassSummary -or -not $nodeGreenFailSummary) { $nodeGreen | ForEach-Object { Write-Host $_ }; throw "Node GREEN run omitted pass/fail counts" }
$nodeGreenPassCount = [int]([regex]::Match($nodeGreenPassSummary, '^# pass ([0-9]+)\s*$').Groups[1].Value)
$nodeGreenFailCount = [int]([regex]::Match($nodeGreenFailSummary, '^# fail ([0-9]+)\s*$').Groups[1].Value)
if ($nodeGreenPassCount -ne $nodeGreenCount -or $nodeGreenFailCount -ne 0) { $nodeGreen | ForEach-Object { Write-Host $_ }; throw "Node GREEN run was not all-pass" }
$nodeGreen | ForEach-Object { Write-Host $_ }
if ($nodeGreenExit -ne 0) { throw "Skill validator GREEN run failed across $nodeGreenCount tests" }
$skillLines = (Get-Content .agents/skills/brainstorm-to-delivery/SKILL.md).Count
if ($skillLines -ge 500) { throw "SKILL.md has $skillLines lines; must be below 500" }
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
if ($LASTEXITCODE -ne 0) { throw "Production Skill validation failed" }
```

Expected: `nodeGreenCount > 99`, `nodeGreenPassCount == nodeGreenCount`, `nodeGreenFailCount == 0`, exact IDs on all negative fixtures, the production Skill passes, and `skillLines < 500`. Record discovered/passed/failed/skipped counts, line count, baseline rationalizations, wording-control results, and post-change convergence in the Task report.

- [ ] **Step 9: Commit Task 11**

```powershell
git add src-tauri/src/acp/delegation/transport.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/tool_schema.json src/lib/types.ts src/components/chat/ask-question-card.tsx src/components/chat/ask-question-card.test.tsx src/i18n/messages/en.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/es.json src/i18n/messages/de.json src/i18n/messages/fr.json src/i18n/messages/pt.json src/i18n/messages/ar.json .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
git commit -m "feat: expose authorized recovery contracts"
```

### Task 12: Prove Session-2566 Recovery and Run Final Validation Once

**Required Skills:** `superpowers:verification-before-completion`, `superpowers:requesting-code-review`

**Files:**

- Create: `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify only when a failing acceptance assertion identifies a defect: files owned by Tasks 1-11

**Interfaces:** No new production interface. This Task proves the approved contracts and fixes only defects exposed by the acceptance fixture or final matrix.

- [ ] **Step 1: Write the reconstructed session-2566 acceptance fixture**

```rust
#[tokio::test]
async fn session_2566_blocked_workflow_recovers_in_place_to_task_one_admission() {
    // workflow_id = afd89cd7-5df0-49d9-8a40-1d2c95791cbd
    // revision/header = 8/blocked
    // Plan digest = sha256:77fca1481d57395b3b7fe090be2d116e647f6275e303895b0b88e7ad4428d4b5
    // current Author + two reviewer bindings valid/observed
    // four protected historical Plan bindings retired at revision 8 and omitted
    // Plan gate cycle 1 approved, Critical=0, Important=0, no active Task run
    // Assert status => confirmation_required recover_workflow -> approved.
    // Assert direct publication cannot unblock and retired rows remain byte-stable.
    // Approve a workflow receipt and recover to state-only revision 9.
    // Assert header/revision 9 approved; structure/Plan fingerprints unchanged;
    // all four retired_revision values remain 8; no Author/reviewer run added.
    // Assert Task 1 first-dispatch passes the existing exact Plan gate checks.
}
```

Also add `legacy_parent_disconnect_authorize_continue_then_unresumable_replace` covering no direct continue, fixed authorization, consumed provenance, resume failure, and subsequent replacement from the new latest run.

- [ ] **Step 2: Run focused acceptance tests and verify RED for any integration gap**

Run: `cargo test session_2566 --lib -- --nocapture`

Expected before fixture support is complete: FAIL at the first missing integration assertion; never weaken fixture evidence or bypass policy to make it pass.

Run: `cargo test legacy_parent_disconnect_authorize_continue_then_unresumable_replace --lib -- --nocapture`

Expected before final integration is complete: FAIL at any missing authorization/provenance/resume-first assertion.

- [ ] **Step 3: Fix only acceptance defects and run all focused recovery suites**

```powershell
cargo test recovery_authorization --lib -- --nocapture
cargo test delegation_recovery --lib -- --nocapture
cargo test workflow_recovery --lib -- --nocapture
cargo test session_2566 --lib -- --nocapture
cargo test --test delegation_recovery_migration -- --nocapture
```

Expected: PASS. Then from repository root run:

```powershell
pnpm test -- src/components/chat/ask-question-card.test.tsx src/contexts/acp-connections-context.test.tsx src/i18n/messages.test.ts
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: PASS with no authorization id in status snapshots and no stale recovery guidance.

- [ ] **Step 4: Run the full repository validation matrix once**

Repeat the Workspace Gate immediately before this matrix. `git status --short` must be empty; if unrelated or unstaged files are present, stop and isolate them instead of attributing their failures to this implementation.

From repository root:

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: all commands exit 0.

From `src-tauri/`:

```powershell
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all commands exit 0. If a command fails, fix the concrete failure, rerun its focused test, then rerun the affected final command and every later command whose inputs changed. Do not claim completion from an earlier run.

- [ ] **Step 5: Review the final diff against both Designs**

```powershell
$task1Commit = git log --format=%H --fixed-strings --grep="feat: add shared recovery authorization persistence" -n 1
if ([string]::IsNullOrWhiteSpace($task1Commit)) { throw "Task 1 commit not found" }
$implementationBaseCommit = git rev-parse "$task1Commit^"
git diff --check "$implementationBaseCommit..HEAD"
git status --short
git diff --stat "$implementationBaseCommit..HEAD"
```

Confirm the computed base is the parent of Task 1, so the diff excludes both approved Design files, this implementation plan, and any owner-resolved pre-existing edits. Then confirm: one authorization table; separate policy modules; no deleted evidence; no broad parent-end `unresumable`; no ordinary unblock; no active/busy detach; no authorization id in projections; exact session-2566 in-place recovery; all new state transitions have immutable provenance.

- [ ] **Step 6: Commit Task 12**

```powershell
git add src-tauri/src/acp/delegation/workflow/recovery_tests.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "test: verify authorized recovery end to end"
```

Commit any acceptance defect fix against its owning Task before this final test commit; the final command stages only the two fixture files shown above.
