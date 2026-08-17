# Brainstorm-to-Delivery Durable Orchestration Binding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bind every admitted brainstorm-to-delivery Task route to immutable generic delegation identity, reconcile that identity from a complete parent-scoped snapshot, and fail closed before routed execution or recovery when durable truth disagrees with the reviewed Plan and progress.

**Architecture:** Extend `delegation_task_runs` with a nullable all-or-none orchestration binding that is fixed in the reserving transaction and inherited by continuation and replacement. Expose only bounded parent-scoped run identity through a new read-only MCP snapshot tool, then make the Node validator the sole route-fingerprint and execution-admission authority. Keep Rust Simple projection observational: it may surface bounded binding warnings, but it never creates a Gate or owns admission, completion, or recovery decisions.

**Tech Stack:** Rust 2021, serde/serde_json, SeaORM and SQLite, Axum-compatible shared server features, the `codeg-mcp` stdio companion and broker transport, Node.js ESM with `node:test` and `node:crypto`, Markdown Skill contracts, and the existing Simple Plan/progress projection.

## Global Constraints

- Simple remains the only writable brainstorm-to-delivery mode. Do not restore workflow manifests, platform Gates, gate settlement, completion Cards, artifact digests, or platform-owned completion decisions.
- The initial Task Agent generation is exactly `agent_type: "grok"`, `profile_id: null`, `generation: 1`, and `effective_from_task_index: 1`. Do not invent or substitute another Task Agent.
- `orchestration_binding` v1 is optional for standalone/ad-hoc delegation. When present it is exactly `{schema_version, namespace, generation, route_fingerprint}`, rejects unknown or partial fields, and is separate from the ACP launch/config `delegation_task_runs.route_fingerprint`.
- The brainstorm-to-delivery namespace is exactly `brainstorm-to-delivery`. Namespace syntax is `^[a-z][a-z0-9-]{0,63}$`; generation is an integer in `1..=4294967295`; route fingerprints match `^sha256:[0-9a-f]{64}$`.
- Add exactly four nullable columns to `delegation_task_runs`: `orchestration_schema_version`, `orchestration_namespace`, `orchestration_generation`, and `orchestration_route_fingerprint`. Do not backfill historical rows.
- Name the all-or-none insert trigger `trg_dtr_orchestration_binding_shape`, name the immutability trigger `trg_dtr_orchestration_binding_immutable`, and create `idx_dtr_parent_orchestration_created_task` on `(parent_conversation_id, orchestration_namespace, created_at, task_id)`.
- Persist a binding in the same reserving transaction. Never include any of the four columns in lifecycle/status update models.
- Continuation and replacement inherit their source binding. Return `orchestration_binding_lineage_mismatch` for a well-formed explicit mismatch or bound/unbound conversion before child, resume, authorization, eligibility, or budget side effects. Return `orchestration_binding_invalid` for malformed input.
- Preserve the current seven-string request fingerprint for unbound requests. Bound requests use the Design's exact v2 string-array shape and the inherited effective binding.
- `get_delegation_orchestration_bindings` is read-only. Parent identity comes only from the MCP token; no input may accept a parent or conversation ID.
- Snapshot defaults and bounds are exact: default `limit` 100, allowed `1..=200`, at most 4096 materialized rows, 60-second expiry, 4 MiB evidence file, stable `(created_at, task_id)` order, and a decimal-string unsigned 64-bit `snapshot_revision`.
- The snapshot conflict set is the deduplicated union of requested-namespace rows and every row with a non-null `work_unit_key`, including unbound and foreign-namespace keyed rows. Foreign-namespace unkeyed rows remain excluded.
- Query responses expose only durable run identity, actual `agent_type`/`profile_id`, lineage, status, and binding. Never return prompt, task preview, output, result, termination details, card summaries, completion evidence, or profile configuration.
- The Node validator owns RFC 8785 route canonicalization and SHA-256 derivation. The parent and Plan Author copy its exact output and never independently hash.
- Keep the published high-Task vector exactly `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a` in cross-language regression coverage.
- Lost acknowledgement adoption requires one eligible unresolved dispatch intent and one globally unambiguous durable row under operation-specific `first`, `continue`, or `replacement` lineage. A deleted mirror without an unresolved intent remains blocking.
- A Task Agent boundary change uses one nullable top-level `pending_route_change` progress intent with requested Agent/profile, next generation, effective Task boundary, and the exact affected pending suffix. It is recorded only after fresh full admission proves no affected durable row, survives interruption, and is cleared only after revised Plan approval and another full admission.
- Rust Simple projection may emit only warning-level binding diagnostics: `simple_orchestration_binding_missing`, `simple_orchestration_binding_mismatch`, and `simple_orchestration_binding_orphan`. These warnings never authorize, reject, or complete a route.
- Requirement, scope, architecture, and user-data decisions remain user-owned. Review findings do not authorize agents to infer a material change.
- Preserve legacy five-part reviewer readability, routing-block-free legacy Simple behavior, archived manifest projection, generic recovery limits, and unbound standalone delegation behavior.
- Keep `SKILL.md` imperative and below 500 lines. Keep the Plan at or below 2 MiB, the routing block at or below 256 KiB, progress at or below 512 KiB, and its structured block at or below 64 KiB.
- Preserve the Grok `tools/list` JSONL assertion literal `7_680` and its user-facing `7680` wording. The new tool and nested binding schema must fit without weakening that budget.
- Follow RED-GREEN-REFACTOR. Every production behavior change starts with a focused test observed failing for the intended reason.
- Every filtered test command must execute at least one test. Record the executed count in the Task report; a zero-test success is not evidence.
- Every Rust compile, test, or lint command in this Plan uses `--no-default-features --features server,test-utils`. No command enables the default `tauri-runtime`.
- Execute Tasks serially. Each Task consumes the committed output of every prior Task, owns only its listed files, ends with one focused commit, and writes the exact untracked report path listed for it.
- Inspect repository status before every producer edit. Preserve unrelated changes and never stage or commit `.superpowers/sdd/**` reports.

## File Map

- `src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs`: nullable columns, shape/immutability triggers, lookup index, no-backfill migration tests, and rollback.
- `src-tauri/src/db/migration/mod.rs`: registers the new migration after `m20260811_000001_simple_workflows`.
- `src-tauri/src/db/entities/delegation_task_run.rs`: SeaORM mirror of the four insert-fixed nullable columns.
- `src-tauri/src/acp/delegation/types.rs`: strict binding value object, delegate/continue fields, query/page DTOs, and stable transport error variants.
- `src-tauri/src/acp/delegation/store.rs`: store-level lineage mismatch error carried out of reserving transactions.
- `src-tauri/src/acp/delegation/run_store.rs`: binding persistence, inherited effective binding, v1/v2 request fingerprints, mutation revision fencing, and conflict-set materialization.
- `src-tauri/src/acp/delegation/broker.rs`: pre-side-effect binding checks and exact first/continue/replacement request behavior.
- `src-tauri/src/acp/delegation/listener.rs`: strict argument parsing, token-derived parent query handling, and stable error mapping.
- `src-tauri/src/acp/delegation/transport.rs`: token-only broker request variant for binding snapshot pages.
- `src-tauri/src/acp/delegation/orchestration_binding_query.rs`: process-local revision tracker, 60-second snapshot cache, cursor validation, paging, and stale detection.
- `src-tauri/src/acp/delegation/mod.rs`: registers the focused query module.
- `src-tauri/src/acp/delegation/tool_schema.json`: optional binding schemas and the read-only query tool schema.
- `src-tauri/src/acp/delegation/companion.rs`: root-only catalog exposure, query round trip/rendering, schema tests, redaction assertions, and fixed Grok JSONL budget regression.
- `src-tauri/tests/fixtures/orchestration_binding_v1.json`: the one shared binding-grammar corpus loaded by Rust value tests, MCP schema/listener tests, and Node validator tests.
- `src-tauri/src/acp/connection.rs`: compatibility literals for reserving inserts and direct delegation requests.
- `src-tauri/src/acp/lifecycle.rs`: compatibility literals for direct delegation requests.
- `src-tauri/src/acp/delegation/attention.rs`: compatibility reserving-insert literal.
- `src-tauri/src/acp/delegation/workflow/admission.rs`: compatibility reserving-insert fixtures.
- `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`: compatibility reserving/continuation admission fixtures.
- `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`: compatibility reserving and delegation request fixtures.
- `src-tauri/src/acp/delegation/workflow/store.rs`: compatibility reserving-insert fixtures.
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`: bounded CLI mode parsing, evidence file loading, structured output, and exit behavior.
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`: RFC 8785 route binding derivation, static contracts, durable page validation, bidirectional reconciliation, and lost-acknowledgement actions.
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`: canonical vectors, CLI mode matrix, durable negatives, adoption fixtures, bootstrap flow, compatibility, and root-cause regression.
- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`: bounded optional progress route fingerprint and per-run binding parsing.
- `src-tauri/src/acp/delegation/workflow/project.rs`: Task 1 legacy `delegation_task_run::Model` literal compatibility, then Task 6 warning-only Plan/progress/durable binding comparison.
- `.agents/skills/brainstorm-to-delivery/SKILL.md`: durable query, validator-copied binding, dispatch-intent, recovery, and parent-ownership operating rules.
- `src-tauri/tests/delegation_session_reuse_integration.rs`: repository-level bound route, inheritance, no-substitution, document independence, and final-review scenarios.
- `src-tauri/tests/completion_protocol_v2.rs`: compatibility literals for reserving and delegation requests across historical completion fixtures.
- `src-tauri/tests/completion_transport_parity.rs`: compatibility reserving-insert literal for transport parity.

## Canonical Plan Routing Contract

The following is the only machine-readable route source in this Plan. Fingerprints are deliberately absent: Task 4 makes the validator derive them from accepted routing data.

<!-- codeg-b2d-routing-v1
{
  "schema_version": 1,
  "risk_policy_version": "b2d_task_risk_v1",
  "task_agent_generations": [
    {
      "generation": 1,
      "agent_type": "grok",
      "profile_id": null,
      "effective_from_task_index": 1
    }
  ],
  "tasks": [
    {
      "index": 1,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "migration_destructive_persistence",
            "evidence": [
              "src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs changes the durable delegation_task_runs schema and database triggers"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "broad_production_surface",
            "score": 1,
            "evidence": [
              "new migration, migration registry, entity, delegation types, run store, and broker are six changed production files"
            ]
          },
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              "SeaORM migration/entity ownership and ACP run-store identity ownership"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "ReservingRunInsert and PersistedRun are shared durable admission interfaces"
            ]
          }
        ],
        "score": 3,
        "reason": "Changes persistent generic run identity and enforces immutable database shape without historical backfill."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 2,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "concurrency_lifecycle",
            "evidence": [
              "first, continuation, and replacement admission ordering before child, resume, authorization, and recovery-budget side effects"
            ]
          },
          {
            "kind": "public_compatibility",
            "evidence": [
              "delegate_to_agent and continue_delegation MCP schemas plus stable orchestration_binding error codes"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "codeg-mcp JSON arguments cross the companion, listener, broker, and child process boundary"
            ]
          },
          {
            "kind": "broad_production_surface",
            "score": 1,
            "evidence": [
              "delegation types, store, run store, broker, listener, tool schema, and companion are seven changed production files"
            ]
          },
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              "delegation transport/admission ownership and ACP connection/lifecycle callers both consume the changed request structs"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "DelegationRequest and ContinueDelegationRequest are constructed outside their delegation types module"
            ]
          }
        ],
        "score": 5,
        "reason": "Changes public delegation inputs and recovery ordering while preserving side-effect-free mismatch rejection."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 3,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "security_trust_boundary",
            "evidence": [
              "get_delegation_orchestration_bindings derives parent identity exclusively from the MCP token"
            ]
          },
          {
            "kind": "concurrency_lifecycle",
            "evidence": [
              "revision-stable 60-second pagination is invalidated by concurrent run inserts and durable status transitions"
            ]
          },
          {
            "kind": "public_compatibility",
            "evidence": [
              "new read-only MCP page, cursor, snapshot, and stable error contract"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "codeg-mcp companion requests cross listener/transport into the Rust broker and database-backed query"
            ]
          },
          {
            "kind": "broad_production_surface",
            "score": 1,
            "evidence": [
              "orchestration query, delegation module/types/transport/run store/listener/schema/companion total eight production files"
            ]
          },
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              "MCP companion/transport ownership and durable run-store/query ownership are independent subsystems"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "query request/page DTOs and BrokerMessage variants are consumed outside orchestration_binding_query.rs"
            ]
          }
        ],
        "score": 5,
        "reason": "Introduces a token-scoped public query over concurrently changing durable identity."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 4,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "validator CLI modes, structured task_bindings output, and RFC 8785 route fingerprint wire identity"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "Node validator output is copied into Rust-backed MCP delegation calls"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "Plan routing and progress orchestration_binding fields share one derived route contract"
            ]
          }
        ],
        "score": 3,
        "reason": "Creates the sole cross-runtime canonical fingerprint and non-authorizing bootstrap outputs."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 5,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "security_trust_boundary",
            "evidence": [
              "complete durable evidence becomes the fail-closed dispatch authorization input"
            ]
          },
          {
            "kind": "public_compatibility",
            "evidence": [
              "B2D-DURABLE-001 through B2D-DURABLE-009 and reconciliation_actions are stable validator outputs"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "Rust durable snapshot pages are consumed by Node admission reconciliation"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "progress dispatch_intent and durable lineage page contracts must agree field-for-field"
            ]
          }
        ],
        "score": 3,
        "reason": "Authorizes execution across a trust boundary and performs exact lost-acknowledgement identity adoption."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 6,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "Simple workflow graph exposes three new serialized projection warning codes"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              "simple_parse.rs document parsing and project.rs graph reconciliation"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "Simple projection warning codes are consumed by desktop and server workflow graph clients"
            ]
          }
        ],
        "score": 2,
        "reason": "Adds externally visible warning-only binding diagnostics while preserving non-authoritative projection."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    },
    {
      "index": 7,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "codeg-b2d-skill-contract-v2 and brainstorm-to-delivery dispatch, recovery, and selection behavior"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "Markdown Skill instructions coordinate Node validation and generic MCP child processes"
            ]
          },
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              ".agents Skill/validator contracts and Rust session-reuse integration scenarios"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "Skill, Plan, progress, validator output, and generic delegation binding contract"
            ]
          }
        ],
        "score": 4,
        "reason": "Activates the public end-to-end workflow only after every durable backend and validator prerequisite is available."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    }
  ]
}
-->

---

### Task 1: Persist immutable optional orchestration bindings

**Dependencies:** The completed 2026-08-16 routing increment supplies canonical Task keys and route metadata. This Task introduces only generic durable identity; no caller can send a binding until Task 2.

**Risk:** `high` because `migration_destructive_persistence` is active for the existing `delegation_task_runs` schema. Broad production surface, multiple ownership modules, and shared interfaces total 3; the hard trigger independently forces high.

**Files:**

- Create: `src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/delegation_task_run.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs` for reserving literals and `None` at all current request-fingerprint call sites until Task 2 exposes the field
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/attention.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` only for the three legacy `delegation_task_run::Model` literal expressions; Task 6 retains ownership of later warning logic
- Create: `src-tauri/tests/fixtures/orchestration_binding_v1.json`
- Modify: `src-tauri/tests/completion_protocol_v2.rs`
- Modify: `src-tauri/tests/completion_transport_parity.rs`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Test: inline `#[cfg(test)]` modules in the migration, `types.rs`, and `run_store.rs`, the shared JSON corpus, and compile coverage for every listed literal owner
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-report.md` (do not stage)

**Interfaces:**

- Consumes: existing `delegation_task_run::Model`, `ReservingRunInsert`, `PersistedRun`, `insert_reserving_txn`, lifecycle update methods, and the seven-string `request_fingerprint` behavior.
- Produces: `OrchestrationBindingV1`, `Option<OrchestrationBindingV1>` on reserving/persisted run values, the four exact database columns/triggers/index, and `request_fingerprint(tool_name: &str, task_text: &str, work_unit_key: Option<&str>, replaces_task_id: Option<&str>, replacement_reason: Option<&str>, target_task_id: Option<&str>, route_fingerprint_hex: &str, orchestration_binding: Option<&OrchestrationBindingV1>) -> String` with backward-compatible unbound bytes.
- Invariant for later Tasks: `DelegationRequest` and `ContinueDelegationRequest` do not yet expose the field; every currently admitted call passes `None` and behaves byte-for-byte as before.

- [ ] **Step 1: Write focused migration and value-object tests**

Add tests named with the prefix `delegation_orchestration_bindings_`. The first test opens through the prior migration, inserts a legacy run, applies the new migration, and proves all four new values remain SQL `NULL`. The remaining tests prove exact column types/nullability, exact `idx_dtr_parent_orchestration_created_task` column order, all-null/all-set acceptance, partial insert rejection by `trg_dtr_orchestration_binding_shape`, post-insert add/change/clear rejection by `trg_dtr_orchestration_binding_immutable`, and a status-only update succeeding without changing the binding.

Create `src-tauri/tests/fixtures/orchestration_binding_v1.json` as the only cross-language binding grammar corpus. Its exact top-level shape is `{ "schema_version": 1, "cases": [{ "name": STRING, "valid": BOOLEAN, "value": JSON }] }`, with no other top-level or case keys and unique names. Valid cases are `minimum` (`namespace: "a"`, generation 1, 64 lowercase zero hex), `maximum` (`namespace: "a123456789012345678901234567890123456789012345678901234567890123"`, generation 4294967295, 64 lowercase `f` hex), and `brainstorm_to_delivery` (the exact workflow namespace and published lowercase Design fingerprint). Invalid cases are named `null`, `non_object`, `missing_schema_version`, `missing_namespace`, `missing_generation`, `missing_route_fingerprint`, `extra_field`, `wrong_schema_version`, `schema_version_string`, `namespace_number`, `generation_string`, `fingerprint_number`, `generation_zero`, `generation_overflow`, `namespace_empty`, `namespace_65_bytes`, `namespace_uppercase`, `namespace_underscore`, `fingerprint_uppercase_hex`, `fingerprint_wrong_length`, and `fingerprint_wrong_prefix`. Give each invalid case exactly the single named defect relative to the valid minimum object, except the four missing-field cases and `extra_field` whose names define their exact structural mutation.

Load that file with `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/orchestration_binding_v1.json"))` in the `OrchestrationBindingV1` tests. Assert corpus schema/name uniqueness before iterating it, then assert every valid value deserializes and passes semantic validation while every invalid value fails. Tasks 2 and 4 must consume this same file; they must not duplicate the grammar vectors in Rust or JavaScript tables.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationBindingV1 {
    pub schema_version: u32,
    pub namespace: String,
    pub generation: u32,
    pub route_fingerprint: String,
}

#[test]
fn delegation_orchestration_bindings_reject_partial_insert_and_every_update() {
    // Execute raw INSERT/UPDATE statements against a fully migrated in-memory DB.
    // Assert the two exact trigger names in each SQLite error.
}
```

- [ ] **Step 2: Run the migration/value tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib delegation_orchestration_bindings_ -- --nocapture
```

Expected: at least one test executes and fails because the four columns, migration registration, triggers, index, and strict Rust value object do not exist.

- [ ] **Step 3: Implement the no-backfill migration and SeaORM identity fields**

Register the new migration immediately after `m20260811_000001_simple_workflows`. Its `up` path executes the following contract in order: add four nullable columns, create the shape trigger, create the immutable-update trigger, then create `idx_dtr_parent_orchestration_created_task` with the exact ordered columns `(parent_conversation_id, orchestration_namespace, created_at, task_id)`. The shape trigger accepts exactly zero or four non-null values. The immutable trigger uses SQLite `IS NOT` comparisons so null-to-value, value-to-null, and value-to-different-value all abort. Its `down` path drops the named index and triggers before dropping the four columns.

```sql
CREATE TRIGGER trg_dtr_orchestration_binding_shape
BEFORE INSERT ON delegation_task_runs
WHEN (NEW.orchestration_schema_version IS NOT NULL) +
     (NEW.orchestration_namespace IS NOT NULL) +
     (NEW.orchestration_generation IS NOT NULL) +
     (NEW.orchestration_route_fingerprint IS NOT NULL) NOT IN (0, 4)
BEGIN
  SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_shape');
END;

CREATE TRIGGER trg_dtr_orchestration_binding_immutable
BEFORE UPDATE OF orchestration_schema_version,
                 orchestration_namespace,
                 orchestration_generation,
                 orchestration_route_fingerprint
ON delegation_task_runs
WHEN OLD.orchestration_schema_version IS NOT NEW.orchestration_schema_version
  OR OLD.orchestration_namespace IS NOT NEW.orchestration_namespace
  OR OLD.orchestration_generation IS NOT NEW.orchestration_generation
  OR OLD.orchestration_route_fingerprint IS NOT NEW.orchestration_route_fingerprint
BEGIN
  SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_immutable');
END;
```

Mirror the four nullable fields in `delegation_task_run::Model`. Do not add defaults, guessed values, or a data update. Every existing legacy literal expression must explicitly initialize all four fields to `None`:

```rust
orchestration_schema_version: None,
orchestration_namespace: None,
orchestration_generation: None,
orchestration_route_fingerprint: None,
```

- [ ] **Step 4: Write focused store and fingerprint tests**

Add `durable_binding_` run-store tests that prove:

- `ReservingRunInsert` writes a valid binding atomically with `status = reserving` and `PersistedRun` reconstructs it;
- an injected/forced insert transaction error leaves no run and no partial columns;
- all existing promote, status, terminal-settle, and runtime-stat update paths leave the four columns byte-for-byte unchanged;
- direct SQL attempts to mutate the binding fail while ordinary lifecycle changes succeed;
- the existing unbound seven-string test vectors retain their exact digests;
- a bound call hashes the exact 12-string v2 array, different generation/fingerprint values separate, and an exact retry matches.

Add a separate `durable_binding_lifecycle_identity_` fault-injection matrix. Seed a bound reserving row with non-default `agent_type: "custom:binding-fixture"` and `profile_id: "profile-binding-fixture"`. Exercise reserving promotion, pre-admission terminalization, normal terminal settlement, cancellation/cleanup, runtime-stat writes, and completion/projection updates with each path's existing one-shot transaction fault; add a focused test-only post-write failure hook only where no fault seam exists. After both the forced rollback and a successful retry, raw-select and byte-compare `agent_type`, `profile_id`, and all four orchestration columns to the original insert. Name the tests with that exact prefix and keep the hooks under `#[cfg(any(test, feature = "test-utils"))]`.

Use this branch in the fingerprint implementation; do not alter the unbound array:

```rust
let fields = match orchestration_binding {
    None => vec![
        tool_name.to_owned(),
        task_nfc,
        work_unit_key.unwrap_or("").to_owned(),
        replaces_task_id.unwrap_or("").to_owned(),
        replacement_reason.unwrap_or("").to_owned(),
        target_task_id.unwrap_or("").to_owned(),
        route.to_owned(),
    ],
    Some(binding) => vec![
        "delegation-request-v2".to_owned(),
        tool_name.to_owned(),
        task_nfc,
        work_unit_key.unwrap_or("").to_owned(),
        replaces_task_id.unwrap_or("").to_owned(),
        replacement_reason.unwrap_or("").to_owned(),
        target_task_id.unwrap_or("").to_owned(),
        route.to_owned(),
        binding.schema_version.to_string(),
        binding.namespace.clone(),
        binding.generation.to_string(),
        binding.route_fingerprint.clone(),
    ],
};
```

The bound vector has the v2 domain tag plus the existing seven positions plus four binding strings, for 12 total strings as shown in the approved Design. Name the test so this count cannot silently regress.

- [ ] **Step 5: Run the store tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib
```

Expected: FAIL for the intended missing binding fields/fingerprint branch; this unfiltered RED command avoids a zero-test filtered result if the new API first fails compilation.

- [ ] **Step 6: Persist the binding in the reserving insert and map it back out**

Add one optional binding field to `ReservingRunInsert` and `PersistedRun`. In `insert_reserving_txn`, populate all four ActiveModel fields from one validated `Option`; in `model_to_persisted_run`, accept either all-null or all-set and reject an impossible partial row as unreadable. Do not touch the binding columns in `promote_running`, `settle_pre_admission_failure_if_owned`, `settle_terminal`, cancellation, runtime-stat, completion, or projection updates.

Update every existing compatibility literal rather than assuming Rust supplies an omitted optional field. The revision scan found `ReservingRunInsert` literals in exactly these files: `connection.rs`, `attention.rs`, `broker.rs`, `listener.rs`, `run_store.rs`, `store.rs`, `workflow/admission.rs`, `workflow/completion_evidence.rs`, `workflow/recovery_tests.rs`, `workflow/store.rs`, `completion_protocol_v2.rs`, `completion_transport_parity.rs`, and `delegation_session_reuse_integration.rs`. Add `orchestration_binding: None` to every old literal and reserve non-null values for the new focused tests.

The same scan found `request_fingerprint` calls in exactly `broker.rs`, `run_store.rs`, `store.rs`, and `delegation_session_reuse_integration.rs`. Add a final `None` to every old call. This is only source compatibility for the new function signature; Task 2 replaces broker admission values with the effective request/source binding. Re-run both scans before GREEN and fail the Task report checklist if any literal or call remains outside these owned files.

`PersistedRun` literals occur only in `broker.rs` and `run_store.rs`; add `orchestration_binding: None` to legacy test literals while `model_to_persisted_run` supplies the real mapped value. Include this third scan in the same pre-GREEN checklist.

Run and record a fourth, complete SeaORM Model-literal scan before GREEN:

```bash
rg -n -U 'delegation_task_run::Model\s*\{' src-tauri/src src-tauri/tests
rg -n 'delegation_task_run::\{[^}]*Model|delegation_task_run::Model[[:space:]]+as|type[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*.*delegation_task_run::Model' src-tauri/src src-tauri/tests
```

At this Plan baseline, the qualified scan has six textual matches. Three are actual literal expressions in `workflow/project.rs` (`finished_a`, `finished_b`, and `open_c`); explicitly add the four `None` fields to all three, including the two struct-update expressions rather than relying on inheritance. The other three matches in `listener.rs`, `run_store.rs`, and `workflow/completion_evidence.rs` are function return types whose following brace opens the function body, not Model constructors. The alias/import scan finds no alias of `delegation_task_run::Model` and therefore no unqualified literal owner. Inspect and classify every match in the Task report; if the branch changes before implementation, add every newly discovered literal owner to this Task and its commit before running GREEN.

- [ ] **Step 7: Run Task 1 GREEN and shared-core checks**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib delegation_orchestration_bindings_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib durable_binding_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib durable_binding_lifecycle_identity_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib
cargo check --no-default-features --features server,test-utils --tests
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: each filtered command reports at least one executed test and PASS; the full library passes with every direct SeaORM Model literal naming the four nullable fields; every integration-test target containing a compatibility literal compiles; the shared library, server binary, and MCP companion compile without `tauri-runtime`.

- [ ] **Step 8: Commit Task 1**

```bash
git add -- src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/delegation_task_run.rs src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/attention.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/workflow/recovery_tests.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/tests/fixtures/orchestration_binding_v1.json src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(delegation): persist orchestration bindings"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-report.md` with migration SQL facts, no-backfill evidence, trigger/index results, shared-corpus counts, all four compatibility scans including the classified SeaORM Model scan, unbound and bound fingerprint vectors, lifecycle identity fault/rollback results, exact test counts/outcomes, commit hash, and retained concerns. Do not stage it.

---

### Task 2: Enforce binding transport and lineage admission

**Dependencies:** Task 1 provides the validated binding type, immutable columns, reserving persistence, and fingerprint branch. This Task is the only writer-facing transport/admission switch.

**Risk:** `high` because both `concurrency_lifecycle` and `public_compatibility` hard triggers are active. Cross-process transport, broad production surface, multiple ownership modules, and a shared request interface total 5; either hard trigger independently forces high.

**Files:**

- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- Modify: `src-tauri/tests/completion_protocol_v2.rs`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Test fixture: `src-tauri/tests/fixtures/orchestration_binding_v1.json` from Task 1, loaded unchanged by schema, listener, and semantic validation tests
- Test: inline Rust unit/integration-style tests in the same files plus compile coverage for every request/admission literal owner
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-2-report.md` (do not stage)

**Interfaces:**

- Consumes: `OrchestrationBindingV1`, `ReservingRunInsert::orchestration_binding`, `PersistedRun::orchestration_binding`, and the v1/v2 fingerprint function from Task 1.
- Produces: optional `orchestration_binding` on `DelegationRequest` and `ContinueDelegationRequest`; listener parser `parse_orchestration_binding`; `TaskStoreError::OrchestrationBindingLineageMismatch`; `DelegationError::{OrchestrationBindingInvalid, OrchestrationBindingLineageMismatch}`; exact inheritance for continuation/replacement.
- Ordering guarantee for Task 3: a reserving row's actual Agent/profile and effective binding are final before the query can observe it.

- [ ] **Step 1: Write raw-schema and listener RED tests**

Load every case from `src-tauri/tests/fixtures/orchestration_binding_v1.json`; do not create another transport grammar table. For each case, inject its `value` into raw `delegate_to_agent` and `continue_delegation` arguments and validate the same candidate against both published MCP input schemas. Every shared valid case must pass schema, listener deserialization, and `OrchestrationBindingV1` semantic validation. Every shared invalid case must fail all three with exactly `orchestration_binding_invalid`, and the mock spawner/resumer must record zero child side effects. Test omitted `orchestration_binding` separately as the backward-compatible `None` case; explicit JSON null remains the shared invalid `null` case.

```rust
let input = json!({
    "agent_type": "grok",
    "task": "bound first dispatch",
    "correlation_id": "binding-listener-red",
    "orchestration_binding": {
        "schema_version": 1,
        "namespace": "brainstorm-to-delivery",
        "generation": 1,
        "route_fingerprint": format!("sha256:{}", "a".repeat(64))
    }
});
```

- [ ] **Step 2: Run parser/schema tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture
```

Expected: at least one test executes and fails because the listener currently ignores the field and the MCP schemas do not publish it.

- [ ] **Step 3: Expose strict optional binding inputs and stable errors**

Add the same `$defs`-style object shape to both delegation tool schemas with `additionalProperties: false`, four required fields, exact integer/string limits, and no JSON `null` alternative. Keep the containing request optional so omitted old calls remain valid.

Parse the raw field before depth, recovery, child, or resume work. `parse_orchestration_binding` returns `Ok(None)` only when the property is absent; every present non-object or invalid object maps to `orchestration_binding_invalid`. Validate again at direct broker entry so non-listener callers cannot bypass the contract.

Map the two new `DelegationError` variants in `DelegationTaskReport::from_err` to the exact stable codes, and map the store mismatch variant through `store_err_to_delegation_error` without collapsing it into `not_continuable` or `invalid_replacement`.

- [ ] **Step 4: Write first/continue/replacement lineage RED tests**

Add broker/run-store tests for this complete matrix:

- first dispatch: omitted remains unbound; valid binding persists before spawn; invalid direct-broker binding rejects before depth/spawn;
- continue from bound source: omitted inherits; exact explicit match succeeds; changed one of any four fields rejects;
- continue from unbound source: omitted stays unbound; a supplied binding rejects conversion;
- replacement from bound source: omitted and exact supplied values create a generation-1 replacement with the exact source binding;
- replacement from unbound source: omitted stays unbound; supplied binding rejects conversion;
- every mismatch occurs before child allocation/resume, replacement eligibility changes, recovery authorization consumption, counter preflight/charge, or process spawn;
- inherited binding participates in continue/replacement idempotency even when omitted by the caller;
- different effective bindings cannot alias under one parent tool use ID.

Use test gates/counters already present in broker and run store. Assert the rejected replacement leaves no provisional child conversation and its authorization remains consumable by the subsequent exact call.

- [ ] **Step 5: Run lineage tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
```

Expected: at least one test executes and fails because continuation/replacement currently neither compare nor inherit orchestration identity.

- [ ] **Step 6: Resolve one effective binding before every side effect**

For first dispatch, validate the supplied value at broker entry and pass it into both `request_fingerprint` and `ReservingRunInsert`.

For continuation, load the source for ownership, compute `effective = source` when omitted or require exact equality when supplied, use that effective value in the request fingerprint, and pass the supplied/effective values into `ContinueRunAdmission`. Under the existing writer transaction, reload the source and repeat the comparison before continuability, recovery authorization, budget preflight, and insert; copy the source binding into the new insert.

For replacement, load and compare source identity before provisional child creation. Under `validate_replacement_insert_txn`, repeat equality before recovery eligibility/authorization/budget checks and overwrite the new insert with the source binding. This transaction check is the race backstop even though the database trigger already makes the source immutable.

```rust
fn inherited_binding(
    source: Option<&OrchestrationBindingV1>,
    supplied: Option<&OrchestrationBindingV1>,
) -> Result<Option<OrchestrationBindingV1>, TaskStoreError> {
    match (source, supplied) {
        (Some(source), None) => Ok(Some(source.clone())),
        (Some(source), Some(value)) if source == value => Ok(Some(source.clone())),
        (None, None) => Ok(None),
        _ => Err(TaskStoreError::OrchestrationBindingLineageMismatch),
    }
}
```

Update every existing Rust literal explicitly. The revision's word-boundary scan found `DelegationRequest` or `ContinueDelegationRequest` literals in exactly `connection.rs`, `broker.rs`, `listener.rs`, `run_store.rs`, `workflow/recovery_tests.rs`, `lifecycle.rs`, `completion_protocol_v2.rs`, and `delegation_session_reuse_integration.rs`; add `orchestration_binding: None` to legacy literals and exact values only to focused binding tests. The scan also found `ContinueRunAdmission` literals in exactly `broker.rs`, `run_store.rs`, and `workflow/completion_evidence.rs`; add the new supplied/effective binding fields to each. `types.rs` owns the two request definitions. Re-run both scans before GREEN and record their complete file sets in the Task report.

- [ ] **Step 7: Run Task 2 GREEN and compatibility checks**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib request_fingerprint_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture
cargo test --no-default-features --features server,test-utils --lib
cargo check --no-default-features --features server,test-utils --tests
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: every filter executes at least one test and passes; the unchanged `7_680` assertion still passes after nested binding schema growth; the full library passes; all request/admission literal integration targets compile; old omitted-binding request/fingerprint cases remain unchanged; server and companion compile without desktop defaults.

- [ ] **Step 8: Commit Task 2**

```bash
git add -- src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/workflow/recovery_tests.rs src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(delegation): enforce binding lineage"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-2-report.md` with shared-corpus schema/listener results, both literal-scan file sets, side-effect counters, continuation/replacement inheritance evidence, fingerprint compatibility, printed Grok JSONL byte count with unchanged `7_680`/`7680` contract, exact commands/counts, commit hash, and retained concerns. Do not stage it.

---

### Task 3: Expose complete parent-scoped durable binding snapshots

**Dependencies:** Tasks 1-2 guarantee every observable routed reservation has immutable binding and actual Agent/profile identity before child execution. This Task reads those rows and never changes workflow state.

**Risk:** `high` because `security_trust_boundary`, `concurrency_lifecycle`, and `public_compatibility` hard triggers are active. Cross-process transport, broad production surface, multiple ownership modules, and shared DTO/message interfaces total 5; the hard triggers independently force high.

**Files:**

- Create: `src-tauri/src/acp/delegation/orchestration_binding_query.rs`
- Modify: `src-tauri/src/acp/delegation/mod.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Test: inline tests in the new module and the modified transport/listener/companion files
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-3-report.md` (do not stage)

**Interfaces:**

- Consumes: immutable `PersistedRun` identity and all run insert/status transition entry points from Tasks 1-2.
- Produces: `OrchestrationBindingQueryRequest`, `DelegationOrchestrationBindingRun`, `DelegationOrchestrationBindingPage`, `OrchestrationBindingQueryError`, token-only `BrokerOrchestrationBindingsRequest`, `BrokerMessage::OrchestrationBindings`, private `OrchestrationBindingQueryAuthError`, dedicated `orchestration_binding_query_auth_context(token)`, and `RunStore::get_orchestration_binding_page(parent_id, request)`.
- Produces for Task 5: raw page envelopes with exact cursor echo and revision metadata; these are the only durable evidence accepted by the validator.

- [ ] **Step 1: Write catalog, auth, selection, and paging RED tests**

Add a companion catalog and raw-call matrix that expects `get_delegation_orchestration_bindings` only for a root companion with delegation plus coordination enabled and proves the successful production-shaped case has `workflow_v2: false`. In that same case, assert the entire retired `WORKFLOW_V2_TOOLS` catalog remains absent, calls are rejected as unavailable, and the mutation tools `publish_workflow_manifest`, `settle_workflow_gate`, and `recover_workflow` remain retired without writes. Add listener/run-store tests for:

- successful query authorization with a valid root token whose `coordination_v1` is true and `workflow_v2` is false;
- rejection for an invalid token, delegation-child role, `coordination_v1: false`, and no current parent conversation, with no query or partial page;
- no client-supplied parent identity and cross-parent token isolation;
- input unknown-field rejection, namespace grammar, default/min/max limit, paired snapshot/cursor requirement, UUID snapshot, base64url cursor length 1..=128, and namespace/limit immutability across pages;
- requested-namespace unkeyed rows plus keyed unbound/same-namespace/foreign-namespace rows, union deduplication, foreign unkeyed exclusion, and `(created_at, task_id)` ordering;
- 4096-row success and 4097-row `orchestration_binding_query_too_large` with no page;
- first-page null `request_cursor`, later exact echo, non-final `next_cursor`, final null cursor/`complete: true`, total/page offsets, and identical replay of one page request;
- revision change, 60-second expiry, unknown/restarted snapshot, and mid-scan insert/status transition returning `orchestration_binding_snapshot_stale` with no partial page;
- DB/materialization failure returning `orchestration_binding_query_failed`;
- actual stored Agent/profile and complete lineage serialization;
- serialized response absence of prompt, preview, output, result, termination, card, completion, and profile-config keys.

- [ ] **Step 2: Run query tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_query_ -- --nocapture
```

Expected: at least one raw catalog test executes and fails because the tool and broker query variant are absent.

- [ ] **Step 3: Implement the bounded snapshot cache and revision fence**

Create `OrchestrationBindingSnapshotCache` in the focused module. Store a per-parent unsigned 64-bit revision and server-minted snapshot entries containing parent ID, namespace, limit, revision, timestamps, ordered rows, and stable opaque cursors. Use one process-local async read/write mutation gate: first-page materialization holds the read side from revision read through DB row materialization and cache insertion; every run insert, deletion, or durable status transition holds the write side through commit and revision increment. This prevents a committed row/status from being observed under the prior revision.

```rust
pub struct OrchestrationBindingSnapshotCache {
    mutation_gate: tokio::sync::RwLock<()>,
    state: tokio::sync::Mutex<SnapshotState>,
}

pub const ORCHESTRATION_BINDING_SNAPSHOT_TTL: Duration = Duration::from_secs(60);
pub const ORCHESTRATION_BINDING_MAX_ROWS: u64 = 4096;
pub const ORCHESTRATION_BINDING_DEFAULT_LIMIT: u16 = 100;
pub const ORCHESTRATION_BINDING_MAX_LIMIT: u16 = 200;
```

Generate an unguessable server cursor, encode it with base64url without padding, store rather than decode caller offsets, and bind it to parent, namespace, snapshot ID, limit, and exact page start. Purge expired entries opportunistically. A fresh `RunStore` cannot resolve an old process snapshot and returns stale.

Audit every `delegation_task_runs` mutation in `run_store.rs`: reserving insert, provisional-row deletion, promote to running, pre-admission terminalization, normal terminal settlement, and cancellation/cleanup status changes all pass through the write guard and increment only the affected parent after commit. Tests enumerate each path.

- [ ] **Step 4: Materialize the exact conflict set and DTO**

Query at most 4097 rows using the equivalent predicate below, deduplicate by `task_id`, and reject rather than truncate when the cap is exceeded. Map only the approved fields.

```sql
SELECT task_id, root_task_id, previous_task_id, lineage_root_task_id,
       replaced_task_id, replacement_reason, generation, work_unit_key,
       child_conversation_id, agent_type, profile_id, status,
       orchestration_schema_version, orchestration_namespace,
       orchestration_generation, orchestration_route_fingerprint,
       created_at
FROM delegation_task_runs
WHERE parent_conversation_id = ?1
  AND (orchestration_namespace = ?2 OR work_unit_key IS NOT NULL)
ORDER BY created_at ASC, task_id ASC
LIMIT 4097
```

Serialize `snapshot_revision` as a 1-to-20 digit decimal string and timestamps as UTC RFC 3339. Reconstruct `orchestration_binding: null` only from four null columns; a partial row is a query failure rather than sanitized evidence.

- [ ] **Step 5: Add the token-only MCP transport and stable errors**

The tool schema accepts exactly `namespace`, optional `limit`, and an all-or-none `snapshot_id`/`cursor`; it has `additionalProperties: false`. `BrokerOrchestrationBindingsRequest` contains only `token` plus those query fields. Add a dedicated private listener path with this exact interface:

```rust
async fn orchestration_binding_query_auth_context(
    &self,
    token: &str,
) -> Result<i32, OrchestrationBindingQueryAuthError>
```

It performs, in order: registry lookup of the supplied token; `CompanionRole::Root` enforcement; the production delegation/coordination gate represented in the registry by `entry.coordination_v1`; and `current_conversation_id(&entry.parent_connection_id)` resolution. Its stable auth failures are `invalid_token`, `root_only`, `coordination_unavailable`, and `no_active_conversation`. It does not inspect or change `entry.workflow_v2`. Companion `allows_tool` independently requires `features.delegation && features.coordination_v1 && role == CompanionRole::Root`, so production catalog and call dispatch enforce both advertised feature groups while the broker independently enforces the immutable token's root/coordination facts. Never parse parent identity from arguments, and do not loosen or reuse any workflow mutation/recovery authentication path or change retired feature parsing.

Expose the exact errors:

```text
orchestration_binding_query_invalid
orchestration_binding_query_too_large
orchestration_binding_query_failed
orchestration_binding_snapshot_stale
```

Render success as the page object in `structuredContent` without adding a text copy that could exceed or diverge from the structured evidence.

- [ ] **Step 6: Keep the Grok tools/list budget fixed**

Update the expected Grok root tool list to include the new query and keep the existing budget test name, `println!`, comparison literal `7_680`, and message text `7680` unchanged. If schema growth exceeds the limit, shorten descriptions without dropping fields or weakening the literal.

- [ ] **Step 7: Run Task 3 GREEN, including the retained Design Minor**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_query_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: the query filter runs its complete matrix, including successful read-only access with `workflow_v2: false`, root/coordination/token-derived-parent failures, cross-parent isolation, and the workflow-v2 retirement matrix; the exact budget command reports `running 1 test`, prints `Grok tools/list JSONL bytes: N`, and passes with the unchanged `N <= 7_680` assertion; both binaries compile without `tauri-runtime`.

- [ ] **Step 8: Commit Task 3**

```bash
git add -- src-tauri/src/acp/delegation/orchestration_binding_query.rs src-tauri/src/acp/delegation/mod.rs src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/transport.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/companion.rs
git commit -m "feat(delegation): expose binding snapshots"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-3-report.md` with conflict-set rows, the `workflow_v2: false` read-only auth matrix, root/coordination/token-derived-parent isolation, proof that every workflow-v2 catalog tool and all three mutation tools remain unavailable/retired, cursor/revision/expiry cases, redaction scan, exact error codes, the printed Grok JSONL byte count and unchanged 7680 budget, exact test counts, commit hash, and retained concerns. Do not stage it.

---

### Task 4: Derive canonical route bindings and non-authorizing validator modes

**Dependencies:** Tasks 1-3 define the exact Rust wire shape and durable output. This Task gives Plan bootstrap and static document checks one canonical Node implementation; it does not authorize execution without Task 5 evidence reconciliation.

**Risk:** `high` because the validator CLI/fingerprint is public compatibility surface and the three soft signals score 3.

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Test fixture: `src-tauri/tests/fixtures/orchestration_binding_v1.json` from Task 1, loaded unchanged by Node binding validation
- Test: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-4-report.md` (do not stage)

**Interfaces:**

- Consumes: current `parseSimpleRouting`, `validateRoutingSnapshot`, `deriveExpectedRoute`, `validateProgressRouting`, Plan/progress size bounds, and Task 1's shared `orchestration_binding_v1.json` grammar corpus.
- Produces: `deriveRouteBindingInput`, `deriveOrchestrationBinding`, `deriveTaskBindings`, structured validator result envelopes, Plan-only `--derive-plan-routing --output-json`, and optional JSON output for combined static Plan/progress mode.
- Produces for Task 5: each normalized Task has `route_fingerprint`, `expected_work_unit_keys`, and an exact `orchestration_binding`; every non-durable mode fixes `admission_authorized: false`.

- [ ] **Step 1: Write canonical fingerprint vector RED tests**

Add independent fixtures for the Design's exact high vector and expected digest, plus normal/high, generation 1/4294967295, maximum Task index, null/profile, custom Agent, escaped controls, and exact Unicode composed/decomposed profile values. Reject lone surrogate/non-I-JSON strings. Prove these mutations change the digest: reviewer order, work-unit key order, risk, Task index, generation, Agent, and profile. Prove irrelevant source-object insertion order does not change it because the positional array is built only after route validation.

Load `src-tauri/tests/fixtures/orchestration_binding_v1.json` with `readFileSync` and assert its schema and unique case names before applying the Node binding parser to every value. Its valid/invalid results must match the same corpus outcomes already enforced by Rust semantic validation, MCP JSON Schema, and listener deserialization. Do not recreate those grammar cases in JavaScript.

```js
const HIGH_ROUTE_INPUT = [
  "codeg-b2d-route-binding-v1",
  1,
  "brainstorm-to-delivery",
  7,
  2,
  "high",
  ["codex", null],
  [
    ["primary", "codex", null],
    ["auxiliary", "grok", null],
  ],
  [
    "task|7|implementer|codex|none",
    "task|7|reviewer|primary|codex|none",
    "task|7|reviewer|auxiliary|grok|none",
  ],
]

const highTask = {
  index: 7,
  task_agent_generation: 2,
  risk: { level: "high" },
}
const highGeneration = {
  generation: 2,
  agent_type: "grok",
  profile_id: null,
  effective_from_task_index: 7,
}
const routeFailures = []
const highExpectedRoute = deriveExpectedRoute(
  highTask,
  highGeneration,
  routeFailures
)
assert.deepEqual(routeFailures, [])
assert.deepEqual(
  deriveRouteBindingInput(highTask, highGeneration, highExpectedRoute),
  HIGH_ROUTE_INPUT
)
assert.equal(
  deriveOrchestrationBinding(
    highTask,
    highGeneration,
    highExpectedRoute
  ).route_fingerprint,
  "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
)
```

- [ ] **Step 2: Write CLI mode and static agreement RED tests**

Test all three non-authorizing modes:

- no arguments validates only `SKILL.md` and keeps readable PASS/FAIL;
- `--plan FILE --plan-rel-path REL_PATH --derive-plan-routing --output-json` requires no progress and returns exact Task bindings with `admission_authorized: false`;
- `--plan FILE --progress FILE --plan-rel-path REL_PATH` validates static agreement, optionally emits the same JSON envelope, and always returns `admission_authorized: false`.

Plan-only mode rejects missing `--output-json`, any progress/durable/admission flag, missing routing, invalid risk/route/key input, and every size violation. Static progress requires Task-level `route_fingerprint` and every admitted/intended run's exact `orchestration_binding` to match the derived Task binding.

Add `production Plan-only derivation emits seven durable bindings` to the Node suite. It reads exactly `docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md`, invokes the same `runValidation` path as `--derive-plan-routing --output-json`, and asserts: one normalized generation `{generation: 1, agent_type: "grok", profile_id: null, effective_from_task_index: 1}`; seven ordered high Tasks; seven non-authorizing ordered `task_bindings`; and for each Task N the exact implementer key `task|N|implementer|codex|none`, primary key `task|N|reviewer|primary|codex|none`, auxiliary key `task|N|reviewer|auxiliary|grok|none`, generation 1 binding, and validator-derived lowercase fingerprint. Run derivation twice and deep-equal all seven fingerprints; record their exact emitted values in the Task report rather than hand-authoring them in this Plan.

The common success envelope is exact:

```json
{
  "schema_version": 1,
  "admission_authorized": false,
  "durable_snapshot": null,
  "task_bindings": [],
  "reconciliation_actions": [],
  "failures": []
}
```

Populate `task_bindings` in Task order with `task_index`, `risk_level`, `task_agent_generation`, exact keys, and binding. Failure objects are `{rule_id, message}`; a failing result has no usable Task bindings or reconciliation actions.

- [ ] **Step 3: Run the Node suite and observe RED**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: FAIL because the current validator exports no fingerprint derivation and accepts only Skill-only or all-three static document arguments.

- [ ] **Step 4: Implement the exact positional route input and RFC 8785 bytes**

Import `createHash` from `node:crypto`. Build the exact positional array only from normalized, already-validated routing data. Reject non-Unicode-scalar JavaScript strings before serialization. Because this closed value contains only arrays, strings, safe integers, and null, use ECMAScript `JSON.stringify` as the RFC 8785 serialization for this restricted domain, encode UTF-8, SHA-256 hash, lowercase hex, and prefix `sha256:`. Do not expose a generic object canonicalizer as route identity.

```js
export function deriveOrchestrationBinding(task, generation, expectedRoute) {
  const input = deriveRouteBindingInput(task, generation, expectedRoute)
  assertRouteInputIsIJson(input)
  const bytes = Buffer.from(JSON.stringify(input), "utf8")
  return {
    schema_version: 1,
    namespace: "brainstorm-to-delivery",
    generation: task.task_agent_generation,
    route_fingerprint: `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
  }
}
```

Reviewer order is primary then auxiliary. Normal routes omit the auxiliary tuple/key. Keep JSON null for profiles and `none` only inside canonical keys.

- [ ] **Step 5: Implement explicit CLI modes and structured output**

Replace pair-stepping argument parsing with a single-pass parser that distinguishes boolean and value flags, rejects duplicates/unknowns, and enforces the mode matrix. Export a pure `runValidation(options)` from the library and keep filesystem bounds in the CLI.

Plan-only calls `parseSimplePlan`, `validateRoutingSnapshot`, and `deriveTaskBindings`. Combined static additionally parses progress and verifies risk/generation/keys/Task fingerprint/per-run binding. Neither code path reads durable evidence or can set authorization true.

The parent and Plan Author consume the exact JSON result; do not add instructions that invite them to hash, normalize, or reconstruct the binding.

- [ ] **Step 6: Run Task 4 GREEN and production Plan-only derivation fixtures**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs --plan docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md --plan-rel-path docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md --derive-plan-routing --output-json
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: all Node tests pass, Skill-only production validation passes, the direct production-Plan command emits seven ordered non-authorizing bindings after validating Grok/null generation 1 and seven high routes, the high vector equals the exact published digest, and formatting has no diff. Record explicit assertions that derivation/static JSON never authorizes.

- [ ] **Step 7: Commit Task 4**

```bash
git add -- .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
git commit -m "feat(validator): derive route bindings"
```

- [ ] **Step 8: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-4-report.md` with shared-corpus Node results, canonical bytes/digests, Unicode cases, CLI mode outputs, the exact seven production-Plan fingerprints/keys in order, static agreement failures, exact commands/test counts, commit hash, and the explicit fact that no mode in this Task authorizes admission. Do not stage it.

---

### Task 5: Reconcile durable evidence and lost acknowledgements

**Dependencies:** Task 3 emits complete raw pages; Task 4 derives the only accepted Task bindings. This Task combines them and is the only validator path allowed to authorize routed execution.

**Risk:** `high` because durable evidence crosses a trust boundary, the stable output is public compatibility surface, and soft signals score 3.

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Test: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-5-report.md` (do not stage)

**Interfaces:**

- Consumes: Task 3 page DTO JSON and Task 4 normalized Task bindings/result envelope.
- Produces: `MAX_DURABLE_EVIDENCE_BYTES = 4 * 1024 * 1024`, `parseDurableBindingEvidence`, `reconcileDurableBindings`, `--document-admission`, full `--admission`, exact `B2D-DURABLE-001` through `B2D-DURABLE-009`, deterministic `adopt_lost_acknowledgement` actions, and strict `pending_route_change` validation.
- Produces for Tasks 6-7: exact progress run lineage/binding/intent fields, the pending route-change coordination contract, and the fail-closed workflow admission contract.

- [ ] **Step 1: Write strict page-envelope RED tests**

Build raw multi-page fixtures and reject every malformed evidence condition with `B2D-DURABLE-001`: over 4 MiB, unknown fields, wrong schema/namespace, mixed snapshot ID/revision/timestamps/totals, invalid decimal revision, expired timestamps, total over 4096, first offset not zero, gaps/overlaps/reordering, duplicate Task IDs, first non-null request cursor, altered/swapped request cursor, non-final null outgoing cursor, duplicate final page, incomplete final page, total/run-count mismatch, and trailing page after completion.

Validate exact row fields/types/status/binding and bounded canonical Agent/profile/key identity. Map malformed binding/namespace/fingerprint to `B2D-DURABLE-002`. Preserve raw page envelopes; never flatten away cursor evidence before checks.

- [ ] **Step 2: Write bidirectional reconciliation and root-cause RED tests**

Cover all nine durable rule families with exact negative fixtures:

| Rule | Focused cases |
| --- | --- |
| `B2D-DURABLE-001` | page shape, bounds, freshness, cursor chain |
| `B2D-DURABLE-002` | binding grammar, namespace, canonical fingerprint |
| `B2D-DURABLE-003` | admitted progress Task ID with no durable row |
| `B2D-DURABLE-004` | bound/recognized durable row missing from progress or Plan |
| `B2D-DURABLE-005` | Task/key/role/slot/child/root/previous/lineage/generic generation/replacement/current status/actual Agent/profile disagreement |
| `B2D-DURABLE-006` | durable generation/fingerprint differs from Task 4 derivation |
| `B2D-DURABLE-007` | recognized routed Task row is unbound |
| `B2D-DURABLE-008` | generation change touches any reserving/running/completed/failed/canceled durable row |
| `B2D-DURABLE-009` | absent, ambiguous, repeated, cross-matched, or lineage-invalid acknowledgement adoption |

The root-cause regression rewrites the Plan high Task generation/route, rewrites progress generation/fingerprint and every mirrored binding, retains the admitted durable Codex implementer at the original generation/fingerprint, and asserts a `B2D-DURABLE-*` failure despite its generation-invariant implementer key still matching.

Also prove a wrong-namespace recognized key blocks after deleting its progress mirror, while a foreign unkeyed row is not present in valid query evidence. Wrong durable actual Agent/profile fails first, continuation, and replacement fixtures even when key and binding look correct.

Add status-only controls: `reserving -> running`, `reserving -> completed/failed/canceled`, and `running -> completed/failed/canceled` with every identity/binding field equal return only the exact non-authorizing `B2D-DURABLE-005` refresh prefix. After the fixture updates only progress state and supplies a newly minted complete snapshot, full admission passes. Terminal-to-different-terminal, status regression, `unknown`, altered identity/binding, or a status advance mixed with any other failure never enters the refresh path.

Add exact pending route-change fixtures. Routed progress has a required nullable top-level field; its non-null shape is:

```json
{
  "pending_route_change": {
    "requested_agent_type": "gemini",
    "requested_profile_id": null,
    "next_generation": 2,
    "effective_from_task_index": 5,
    "affected_task_indices": [5, 6, 7]
  }
}
```

Reject missing/extra object keys, invalid or unavailable-shape Agent/profile values, non-contiguous/overflow `next_generation`, a boundary unequal to the first affected index, duplicate/gapped/non-suffix affected indices, any affected non-pending Task or nonempty `runs`, any active Task, any incomplete prior Task, and any selected durable row for an affected Task under every durable status. `pending_route_change: null` is the settled/default state; routing-block-free legacy progress may omit the additive field.

Test both fully synchronized document states while the intent is pending: before Author revision, current Plan/progress remain mutually synchronized on the old generation; after Author revision and parent resynchronization, the Plan contains exactly the requested next generation and only the affected suffix uses its exact derived route fields. For interruption recovery only, combined static validation may recognize one exact half-applied state in which the Plan has the complete approved-shape generation/suffix rewrite and progress still has the complete old suffix. It reconstructs and validates the old suffix from the prior generation, validates the new Plan against the intent, returns `admission_authorized: false` with the new exact `task_bindings`, and authorizes no call; partial progress resync, prior Task changes, identity changes, or any other mismatch fail. Full admission never accepts the half-applied state. The parent applies the static output to the entire affected suffix, reruns ordinary combined validation, then obtains fresh evidence and full admission.

- [ ] **Step 3: Write operation-specific adoption RED tests**

Extend progress run validation with exact durable fields and this exact intent shape:

```json
{
  "kind": "first",
  "continuation_target_task_id": null,
  "replacement_target_task_id": null,
  "replacement_reason": null,
  "expected_root_task_id": null,
  "expected_lineage_root_task_id": null,
  "expected_generic_generation": 1,
  "expected_child_conversation_id": null,
  "adopted_after_lost_acknowledgement": false
}
```

An eligible run has `state: "reserving"` and null returned Task/child/root/previous/lineage/generic/replacement fields. Test one exact case each:

- `first`: generation 1, root and lineage root equal new Task ID, null previous/replacement;
- `continue`: previous equals target; root, lineage, child, Agent/profile/key/binding inherit target; generation is target plus one; null replacement;
- `replacement`: replaced ID/reason equal intent; generic generation 1; root equals new Task ID; null previous; lineage/Agent/profile/key/binding inherit source; child differs from replaced child.

Each exact case returns exit zero, `admission_authorized: false`, no Task bindings, and exactly one action containing every Design field plus zero-based `progress_run_index`. Zero/two candidates, two eligible intents, cross-match, wrong source/status/Agent/profile/binding/lineage, prior adoption flag, and deleted mirror without any intent all block with `B2D-DURABLE-009` or the applicable identity rule.

- [ ] **Step 4: Run the durable suite and observe RED**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: FAIL because durable evidence modes, strict page parsing, full lineage fields, and reconciliation actions do not exist.

- [ ] **Step 5: Implement strict evidence parsing before identity maps**

Use `readUtf8FileBounded` with the exact 4 MiB cap. Reject additional properties at wrapper, page, run, binding, dispatch-intent, and action-source layers. Verify identical metadata and cursor chaining before concatenating rows. Parse timestamps as real UTC instants and reject evidence expired at validation time.

Normalize only progress `cancelled` to durable `canceled`; allow progress `stalled` to match only durable `running`; never authorize `unknown`. Validate the actual durable Agent/profile directly rather than decoding them from keys or bindings.

Parse `pending_route_change` before identity maps. Require the exact object above, canonical Agent/profile syntax, the next contiguous generation, a complete pending Task suffix, completed prior prefix, null active Task, and empty affected run lists. Use `B2D-DURABLE-008` when any affected durable row exists; use the existing routing/progress structural rule family for malformed intent or Plan/progress generation disagreement. The validator verifies identity shape and durable absence; live availability confirmation remains a Skill action in Task 7.

In combined static mode, add only the bounded transition exception described in Step 2. It must prove every affected progress entry still equals the complete route derivable from the immediately prior generation and that the Plan's next generation, boundary, Agent/profile, Task references, risks, routes, and keys exactly match the intent. Its output is the ordinary non-authorizing static envelope with the new Plan-derived bindings and no reconciliation action. After any affected entry is partially changed, neither old-state nor new-state validation matches, so the result fails closed.

- [ ] **Step 6: Implement one adoption pass, then exact two-way reconciliation**

Before normal maps, enumerate eligible unresolved intents and otherwise-unmatched durable candidates. Require exactly one global intent/candidate pair and exact operation-specific lineage. Return the deterministic action without authorization so the parent must apply it to the unchanged progress snapshot, persist, fetch a fresh complete snapshot, and rerun from the beginning.

With no adoption action, map both sides by Task ID and enforce:

1. every admitted progress run has exactly one durable row;
2. every requested-namespace row and recognized Task-key row has exactly one Plan Task and progress mirror;
3. all identity, lineage, status, Agent/profile, key role/slot, and replacement fields agree;
4. durable/progress binding equals Task 4 derivation;
5. unbound or foreign-namespace recognized routed rows block;
6. generation changes touch only a never-admitted pending suffix.

When `pending_route_change` is non-null, additionally require either the fully synchronized pre-revision state or the fully synchronized post-revision state described in Step 2. Preserve every earlier generation, Task route, run, and binding byte-for-byte. Full admission may validate document/review coordination while the intent is pending, but Task 7 must prohibit Task dispatch until Plan approval settles the intent to null and a new full admission passes.

If every non-status field and binding already matches and durable lifecycle legitimately advanced from progress `reserving`/`running` to `running` or one terminal status, return non-authorizing `B2D-DURABLE-005` with the exact message prefix `status-only refresh required:` followed by Task ID, progress state, and durable status. Emit no Task bindings or reconciliation actions. Do not classify terminal-to-different-terminal, durable regression, or `unknown` as refreshable. The parent updates only state from that validated row, obtains a fresh query, and reruns; any simultaneous non-status failure blocks the refresh loop.

- [ ] **Step 7: Implement document and full admission CLI modes**

`--document-admission --durable-evidence FILE --output-json` accepts no Plan/progress flags, validates Skill plus complete evidence, and authorizes only unbound Design/Plan work when the snapshot has no requested-namespace row and no recognized Task-key row.

`--plan FILE --progress FILE --plan-rel-path REL_PATH --admission --durable-evidence FILE --output-json` requires every flag and returns `admission_authorized: true` only after static and durable checks both pass with no action. Admission failure exits nonzero with no usable Task bindings/actions. Adoption exits zero but remains non-authorizing.

Add an end-to-end bootstrap test: start with a progress shell containing no Tasks, write a Plan fixture, run Plan-only derivation, initialize progress only from an independent rerun's exact output, pass combined static validation, pass full empty-snapshot admission, and only then admit Plan review. Prove combined validation before initialization fails, so the sequence has no circular dependency.

Add route-change interruption tests for each recoverable checkpoint: intent recorded with old synchronized Plan/progress; Plan revised but progress not yet resynchronized; progress resynchronized but combined/full validation not yet complete; Plan review pending; Plan approved but intent not yet cleared. Recovery always starts by fetching a fresh complete snapshot. A synchronized state runs full admission directly; the exact Plan-ahead/progress-old checkpoint runs Plan-only plus the non-authorizing combined-static transition check, changes the entire affected suffix from that exact output, reruns ordinary combined validation, discards the earlier snapshot, and then obtains fresh evidence for full admission. It never dispatches a Task while the intent remains non-null. Also prove that partial resync, clearing before approval, losing the intent mid-change, or finding any affected durable row blocks rather than guessing state.

- [ ] **Step 8: Run Task 5 GREEN and CLI compatibility**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: all page, reconciliation, adoption, root-cause, pending route-change/interruption, legal-boundary, bootstrap, legacy, and current Skill-only tests pass; the production no-argument check stays readable; formatting has no diff.

- [ ] **Step 9: Commit Task 5**

```bash
git add -- .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
git commit -m "feat(validator): reconcile durable admission"
```

- [ ] **Step 10: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-5-report.md` with each rule family's fixtures, cursor tamper cases, root-cause failure, exact pending route-change shape and all interruption checkpoints, legal suffix control, three adoption operations/actions, deleted-mirror control, CLI authorization matrix, exact commands/test counts, commit hash, and retained concerns. Do not stage it.

---

### Task 6: Project binding drift as bounded warnings only

**Dependencies:** Tasks 1-2 put immutable bindings on durable rows; Task 4 defines progress binding mirrors. Task 5 remains the only authority, so this Task must never share its admission result with projection.

**Risk:** `high` because the three new warning codes are externally serialized behavior. The two soft signals total 2; the public compatibility hard trigger forces high.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/simple_parse.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Test: inline tests in both files
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-6-report.md` (do not stage)

**Interfaces:**

- Consumes: Task 1 entity fields and Task 4 progress fields `route_fingerprint` plus per-run `orchestration_binding`.
- Produces: bounded parsed binding mirrors and warning codes `simple_orchestration_binding_missing`, `simple_orchestration_binding_mismatch`, and `simple_orchestration_binding_orphan`.
- Preserves for Task 7: `gates: []`, `workflow_id: None`, `manifest_revision: None`, existing node identities/edges, legacy aggregate projection, and archived manifest behavior.

- [ ] **Step 1: Write parser/projection RED tests**

Add routed progress fixtures with Task-level `route_fingerprint`, full run lineage, dispatch intent, and per-run binding. Assert:

- routed progress fields parse within existing 512 KiB/64 KiB bounds;
- routed expected run with a missing progress or durable binding emits `simple_orchestration_binding_missing`;
- progress Task/run binding versus durable binding/generation/fingerprint disagreement emits `simple_orchestration_binding_mismatch`;
- a durable bound or recognized Task-key row with no progress mirror/Plan Task emits `simple_orchestration_binding_orphan`;
- warning codes deduplicate and stay within the existing 64-warning cap;
- warnings set only sync/warning state and never add a Gate or change completion/admission decisions;
- a routing-block-free legacy fixture with unbound rows gains none of the three new warnings;
- valid routed bindings keep current high producer/primary/auxiliary nodes and edges unchanged;
- archived manifest projection remains byte-for-byte unchanged.

- [ ] **Step 2: Run projection tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib simple_orchestration_binding_ -- --nocapture
```

Expected: at least one test executes and fails because Simple parsing/projecting currently has no orchestration binding fields or warning codes.

- [ ] **Step 3: Parse additive binding mirrors without creating authority**

Add a small `SimpleOrchestrationBinding` serde shape and optional Task/run fields. Keep parsing bounded and tolerant at the document level: malformed optional binding content produces warning-only invalid/mismatch state rather than a workflow Gate. Do not add route fingerprint to the Plan routing JSON or compute SHA-256 in Rust projection.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleOrchestrationBinding {
    pub schema_version: u32,
    pub namespace: String,
    pub generation: u32,
    pub route_fingerprint: String,
}
```

Task-level progress provides the expected generation/fingerprint mirror; per-run progress and durable rows provide the observed pair.

- [ ] **Step 4: Reconcile warnings on routed Tasks only**

For each routed Task, compare expected canonical keys and progress Task binding to every mirrored/admitted durable row:

- missing means an expected routed progress/durable side has no complete binding;
- mismatch means both exist but schema, namespace, generation, fingerprint, Task index, key, Agent/profile, or mirrored Task binding disagrees;
- orphan means a durable requested-namespace/recognized Task-key row cannot be matched to one Plan Task and one progress Task ID mirror.

Attach the warning to affected route nodes and the graph-level bounded warning list. Do not alter the current node status derivation beyond its existing warning-driven out-of-sync presentation. Do not create Gate data, completion evidence, Cards, or mutation paths.

- [ ] **Step 5: Run Task 6 GREEN and projection regressions**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib simple_orchestration_binding_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib simple_projection_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib workflow::project::tests -- --nocapture
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server
```

Expected: every filter executes at least one test and passes; routed/legacy/archived graph regressions pass; warning fixtures retain empty Gates and no platform completion/admission state.

- [ ] **Step 6: Commit Task 6**

```bash
git add -- src-tauri/src/acp/delegation/workflow/simple_parse.rs src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): warn on binding drift"
```

- [ ] **Step 7: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-6-report.md` with each warning fixture, bounded/deduplicated results, explicit empty-Gate/non-authority assertions, legacy/archived outcomes, exact test counts, commit hash, and retained concerns. Do not stage it.

---

### Task 7: Activate durable orchestration in the Skill and workflow scenarios

**Dependencies:** Tasks 1-3 provide writable binding and complete query surfaces; Tasks 4-5 provide canonical derivation and admission; Task 6 provides warning-only display. This emitter/operational switch is deliberately last.

**Risk:** `high` because the Skill contract and dispatch/recovery behavior are public compatibility surfaces and the soft score is 4.

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Test: Node validator suite and Rust `skill_forward_` integration scenarios
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-7-report.md` (do not stage)

**Interfaces:**

- Consumes: every prior Task interface, `writing-plans`, `subagent-driven-development`, generic delegation/recovery rails, and existing canonical work-unit keys.
- Produces: revised exact `codeg-b2d-skill-contract-v2`, imperative durable operating policy, initial bootstrap/admission sequence, validator-copied Task bindings, operation-specific dispatch intents, pending route-change coordination, status-only refresh recovery, per-call document/final admission cadence, and full workflow scenario coverage.
- Final ownership: the parent remains coordinator/progress editor only; independent producers own Design, Plan, Skill/validator/tests, and Task implementation.

Before editing the Skill, read and follow `/Users/pengchao/.codex/skills/.system/skill-creator/SKILL.md` and `/Users/pengchao/.codex/plugins/cache/gf-team/superpowers/6.2.0/skills/writing-skills/SKILL.md`. Record those reads in the Task report.

- [ ] **Step 1: Write Skill contract/prose contradiction RED tests**

Extend the exact deep-equal Skill contract with these values:

```json
{
  "interfaces": {
    "binding_query": "get_delegation_orchestration_bindings"
  },
  "routing": {
    "binding_schema_version": 1,
    "binding_namespace": "brainstorm-to-delivery",
    "binding_source": "validator_output"
  },
  "progress": {
    "dispatch_intent": "operation_specific_before_call",
    "pending_route_change": "record_before_plan_revision_clear_after_approval"
  },
  "recovery": {
    "durable_reconciliation": "fresh_complete_parent_scoped_snapshot",
    "lost_acknowledgement": "one_exact_unresolved_intent",
    "status_refresh": "state_only_then_fresh_full_admission"
  },
  "document_work": {
    "admission_cadence": "fresh_applicable_mode_before_every_dispatch_or_continuation"
  }
}
```

These properties are additive to the existing v2 object; do not replace existing phase, route, workspace, budget, or final-review values. Replace `plan_setup_order` with this exact Design sequence:

```json
[
  "create-progress-shell",
  "dispatch-plan-author",
  "derive-plan-routing",
  "initialize-progress-from-validator",
  "validate-static-documents",
  "validate-durable-admission",
  "review-plan",
  "register-simple-workflow"
]
```

Add negative prose fixtures for query fallback, parent hashing, Plan Author hashing, post-call intent recording, adopting a row without intent, reusing stale/mixed pages, authorizing from static validation, binding a document/final-review run, changing an admitted Task generation, omitting the pending route-change intent, clearing it before Plan approval, dispatching a Task while it remains pending, treating a status-only refresh as a permanent blocker, rewriting identity/binding during status refresh, reusing one admission across document/final continuations, and treating Rust warnings as a Gate.

- [ ] **Step 2: Write bound workflow scenario RED tests**

Enhance the eleven approved `skill_forward_` scenarios so every routed Task route carries one shared Task binding while Design/Plan/final-review work remains unbound:

1. default normal Grok implementer plus Codex primary;
2. selected non-Grok normal, and high still forces Codex plus selected auxiliary;
3. Task Agent Codex uses three keys/children with one binding and both re-review after a fix;
4. admitted high Codex implementer blocks coordinated Plan/progress generation rewrite before reviewers;
5. boundary change records the exact pending route-change intent only after fresh full admission, affects only never-admitted pending Tasks, confirms availability, survives every interruption checkpoint, reruns derivation/static/full review, clears only after approval, and blocks/defers an active change;
6. deleted mirrors, fabricated identities, wrong actual routes, wrong namespace, unbound routed rows, stale/tampered pages, unavailable query, compaction, continuation/replacement, and exhausted rails fail closed or preserve source binding; a legitimate newer durable status updates only progress state, requeries, and then passes full admission;
7. Design Reviewer/Fixer and Plan Author/Reviewer remain separate and unbound, and every dispatch or continuation obtains a fresh complete snapshot plus document admission before routed documents exist or full admission after Plan/progress synchronization;
8. final findings return to the owning bound Task producer and reopen Task/final review; the same final reviewer continuation after each producer fix requires another fresh complete snapshot and full admission;
9. routing-block-free legacy Simple remains readable; adding routing over admitted unbound history blocks;
10. projection stays warning-only and creates no manifest/Gate/Card/completion decision;
11. first/continue/replacement lost acknowledgements adopt only exact unresolved intents; deleting the intent/mirror remains blocking.

Add a cross-language Rust helper assertion for the exact published high vector digest. It may independently verify the test vector, but production parent/Plan Author code must not hash.

- [ ] **Step 3: Run Skill/scenario tests and observe RED**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: FAIL because current Skill prose and v2 contract do not mention the durable query, binding source, or intent/adoption rules.

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --test delegation_session_reuse_integration skill_forward_ -- --nocapture
```

Expected: at least one existing `skill_forward_` test executes and the strengthened binding assertions fail before the Skill/scenario implementation is updated.

- [ ] **Step 4: Rewrite the Skill as an imperative fail-closed operating sequence**

Keep exactly one v2 contract block and fewer than 500 lines. Preserve the nine phases, but make each phase enforce the approved sequence:

- establish truth: inspect live schemas/Agent discovery, require the query tool, fetch every page into one OS-temporary raw evidence file, reject/restart stale scans, remove the file after validation;
- resolve Task Agent: generation 1 is Grok/null when omitted; for a boundary change first fetch complete evidence and pass full admission, prove every affected Task is pending with no durable row, write the exact `pending_route_change` object, confirm requested Agent/profile availability, continue the same Plan Author to append only the next generation and rewrite only that suffix, run Author and parent Plan-only derivation, resynchronize only affected pending entries, run combined static plus fresh full admission, continue full Plan re-review, then after approval fetch/validate again and clear the intent; never dispatch a Task while the object is non-null;
- Design work: record an unbound intent, fetch a fresh snapshot, and run the applicable mode before every reviewer/Fixer dispatch or continuation; use document admission only before a routed Plan and synchronized progress exist, then use full admission for all later Design decisions; user-owned material changes pause;
- Plan work: create only a bounded progress shell, pass document admission, dispatch the same independent Plan Author using `writing-plans`, run Plan-only derivation twice, initialize route fields only from the parent's exact rerun, run static then full admission, then review; before every later Author or reviewer continuation fetch a new complete snapshot and run full admission against synchronized Plan/progress; register Simple only after approval;
- progress: before each call append a new reserving run with exact validator-copied binding, full null durable identity, and operation-specific intent; after acknowledgement fill every returned/durable field without deleting intent history;
- Task execution: before each individual first/continue/replacement action fetch a fresh complete snapshot and run full admission; pass the exact emitted binding; admit high primary intent/call/ack first, refresh and validate, then auxiliary intent/call/ack, after which children may run concurrently;
- recovery: after compaction or lost response, query first, apply only one exact validator adoption action to still-matching progress, mark adoption true, persist, requery, and revalidate; only when every failure is `B2D-DURABLE-005` with exact prefix `status-only refresh required:` and the validator reports no identity/binding failure, update each named progress run's `state` from its matched durable `status`, persist, discard the snapshot, fetch every page again, and rerun full admission; never rewrite Task ID, child, lineage, Agent/profile, key, generation, or binding, and never idempotently replay an unresolved call before reconciliation;
- route-change recovery: if `pending_route_change` exists, start by fetching a fresh complete snapshot; run full admission directly for a synchronized pre- or post-revision state; when Plan is exactly one intent-approved generation ahead and the whole progress suffix is still old, run Plan-only derivation plus Task 5's non-authorizing combined-static transition check, replace the entire affected suffix from that exact output, rerun ordinary combined validation, discard the earlier snapshot, and fetch full evidence again before full admission; resume complete Plan review and clear only after approval plus another full admission; missing/altered intent, partial resync, or an affected durable row blocks;
- final review: run full admission before the unbound fresh final reviewer and before every continuation of that reviewer after an owning producer fix; bound final fixes return to the owning Task producer and reopen route review.

Encode the boundary-change sequence in the Skill as these exact ordered mutations:

1. Fetch every page of a fresh snapshot and pass full admission against the unchanged Plan/progress.
2. Prove every affected Task is pending with an empty run list and no selected durable row in any status.
3. Persist `pending_route_change` with the requested Agent/profile, next generation, first affected index, and complete affected suffix.
4. Confirm that exact Agent/profile is currently available; keep the intent and block on unavailability rather than substituting.
5. Continue the same Plan Author to append the generation and rewrite only that suffix.
6. Run Author then parent Plan-only derivation, resynchronize only those entries from the parent's exact output, pass combined static validation and a newly queried full admission, then continue every Plan Reviewer for complete re-review.
7. After approval, fetch a new complete snapshot, pass full admission, clear `pending_route_change` to null, persist, and fetch/pass full admission again before the next Task.
8. At any interruption, retain the intent, re-read Plan/progress/reviews, fetch a new complete snapshot, and identify the checkpoint. Resume synchronized states with full admission; resume the one Plan-ahead/progress-old state only through Plan-only derivation and the non-authorizing combined-static transition check, then replace the complete suffix and requery for full admission. Never infer, partially patch, or erase a half-applied change.

Encode fresh document/final cadence per call, not per phase: initial Design and Plan authoring use document admission only while no routed Plan plus synchronized progress exists; every Design Fixer/Reviewer continuation, Plan Author/Reviewer continuation, initial final review, and final-review continuation after a producer fix fetches a new complete snapshot and uses full admission once those routed documents exist. Any intervening delegation action invalidates the prior snapshot.

State explicitly that query unavailability, DB failure, incomplete/stale/mixed/truncated/oversized evidence, any identity/binding/non-refreshable durable mismatch, wrong namespace, unbound routed row, deleted mirror, ambiguous adoption, unavailable Agent/profile, or exhausted rail blocks without Plan/progress fallback or Agent substitution. A validator-confirmed status-only lifecycle advance follows the narrow state-only refresh loop above and is not treated as permanent identity failure.

- [ ] **Step 5: Keep validator and Skill exact and contradiction-resistant**

Update `REQUIRED_SKILL_CONTRACT`, test fixture contract, ordered directives, positive-action vocabulary, and negative patterns to deep-equal the Skill block and reject every contradictory fixture. Keep the validator as the only hash source. Do not introduce workflow-v2 mutation identifiers, Gate settlement, Cards, digest publication, or a Final Fixer.

Update the Skill progress example with top-level `pending_route_change: null`, Task-level `route_fingerprint`, full run lineage, `dispatch_intent`, and `orchestration_binding` from the Design. Include the exact non-null route-change object from Task 5 next to the example and state its eight-step mutation/recovery sequence. State that a definitive pre-reservation failure can close its dispatch intent with null durable identity, while only unknown acknowledgement stays `reserving` and adoption-eligible.

- [ ] **Step 6: Update Rust integration scenarios and compatibility assertions**

Thread `OrchestrationBindingV1` through the existing integration request helpers. Assert Task runs persist/inherit the shared route binding, document/final work omits it, distinct keys never share a child, actual Agent/profile remains the routed value, and source binding survives continuation/replacement. Keep the two unexpected continuation/one logical replacement limits.

Retain legacy unbound request vectors and routing-block-free Simple cases. Add the coordinated Plan/progress rewrite durable-row control and the exact high-vector Rust digest assertion.

Add integration assertions for all route-change interruption checkpoints, the state-only durable status refresh/requery loop, fresh applicable admission before every Design/Plan continuation, the document-to-full mode switch after synchronization, and fresh full admission before every continued final review after a producer fix. Assert no such unbound work receives an orchestration binding.

- [ ] **Step 7: Run Task 7 GREEN and production checks**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
wc -l .agents/skills/brainstorm-to-delivery/SKILL.md
```

Expected: validator tests and production Skill-only check pass; pending route-change, status refresh, and per-continuation admission directives are contradiction-resistant; formatting has no diff; Skill line count is below 500.

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --test delegation_session_reuse_integration skill_forward_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_query_ -- --nocapture
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: every filter executes at least one test and passes; all eleven scenarios bind routed Tasks, keep document/final work unbound, recover pending route changes/status advances, and never reuse stale admission across continuations; server and companion compile without desktop defaults.

- [ ] **Step 8: Commit Task 7**

```bash
git add -- .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(skill): require durable routed admission"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-7-report.md` with skill reads, exact contract result, line count, pending route-change checkpoint results, status-only refresh loop, per-call document/final admission cadence, workflow scenarios, cross-language vector, compatibility cases, exact commands/test counts, commit hash, and retained concerns. Do not stage it.

---

## Design Coverage Matrix

| Approved Design requirement | Owning Task(s) |
| --- | --- |
| Optional strict binding and four immutable nullable columns with no backfill | Tasks 1-2 |
| One shared binding grammar corpus across Rust, MCP schema/listener, and Node | Tasks 1, 2, 4 |
| Same-transaction reservation, insert-fixed Agent/profile, unbound/v2 fingerprints | Tasks 1-2 |
| Continue/replacement inheritance and pre-side-effect mismatch errors | Task 2 |
| Parent-token query, conflict set, bounded stable snapshot, cursors, redaction | Task 3 |
| Fixed Grok `tools/list` 7680-byte regression | Task 3 |
| RFC 8785 derivation, published vector, Plan-only/static non-authorizing modes | Task 4 |
| 4 MiB page validation, full admission, document admission, exact rule IDs | Task 5 |
| First/continue/replacement lost-ack adoption and deleted-mirror blocking | Task 5 |
| Pending route-change intent, never-admitted suffix proof, interruption recovery, and approval settlement | Tasks 5, 7 |
| Warning-only Simple missing/mismatch/orphan projection and legacy/archive compatibility | Task 6 |
| Query-to-exhaustion Skill behavior, validator-copied bindings, intents, status-only refresh, and boundary changes | Task 7 |
| Fresh applicable admission before every Design/Plan/final dispatch or continuation | Task 7 |
| Independent document/Task/final roles, serial routes, high fan-out, owner fixes | Task 7 |
| Complete Testing workflow scenarios and root-cause rewrite regression | Tasks 1-7, consolidated in Task 7 |
| Compatibility and rollout: unbound old clients, no guessed backfill, legacy Simple, archived manifests | Tasks 1, 2, 6, 7 |
| Success Criteria: durable immutable route proof without manifest/Gate/Card/completion authority | Tasks 1-7 and final verification |

## Final Verification and Review

After Task 7's Codex primary and Grok auxiliary reviewers approve the same latest commit:

- [ ] Re-read the approved Design, this Plan, all seven Task reports, each focused commit, and the complete branch diff. Confirm Tasks 1-5 from the 2026-08-16 Plan were not reimplemented or regressed.
- [ ] Confirm every Design Testing, Compatibility, and Success Criteria row maps to the matrix above and to passing evidence in a Task report.
- [ ] Confirm the Plan still contains one authoritative routing block, seven contiguous Task headings, generation 1 Grok/null on every Task, and seven high routes with Codex implementer/primary plus Grok auxiliary.
- [ ] Run all Node contract checks from the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs --plan docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md --plan-rel-path docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md --derive-plan-routing --output-json
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected for the direct production-Plan command: seven ordered non-authorizing bindings, one validated Grok/null generation 1 effective from Task 1, seven high routes, exact three-key high route identity for each Task index, and the same seven deterministic fingerprints recorded by Task 4.

- [ ] Run complete server/test-utils Rust verification from `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib
cargo test --no-default-features --features server,test-utils --test delegation_session_reuse_integration
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
cargo clippy --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp -- -D warnings
```

- [ ] Re-run the retained fixed-budget test from `src-tauri/` and record the printed byte count:

```bash
cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture
```

Expected: exactly one test executes, the assertion literal remains `7_680`, and the line remains within the user-facing 7680-byte budget.

- [ ] Run `git status --short --branch`; only intentionally untracked `.superpowers/sdd/**` reports may remain outside committed Task changes. Preserve every unrelated user file.
- [ ] Dispatch a fresh independent Codex final reviewer over the complete Design/Plan/diff/reports. It must inspect migration immutability, side-effect ordering, snapshot trust isolation/concurrency, cursor/evidence bounds, canonical vectors, root-cause rewrite protection, lost-ack ambiguity, projection non-authority, Skill parent-ownership rules, legacy behavior, and the fixed Grok schema budget.
- [ ] Return every Critical/Important final finding to the producer of its owning Task. After a fix, rerun that Task's Codex primary and Grok auxiliary reviews, all covering commands, and the same final-review work unit. Retain a Minor only with a concrete reason in the final ledger.
- [ ] Complete delivery only when covering verification, full durable reconciliation, all Task reviews, and final review approve one identical repository state. Leave merge, push, PR creation, and deployment to a separate explicit request.

## Recovery and Rollback Boundaries

- If Task 1 cannot enforce all-or-none/immutable columns without altering legacy rows, stop before exposing the binding input. Do not backfill or reuse ACP `route_fingerprint`.
- If Task 2 cannot reject mismatch before every child/recovery side effect, keep bindings internal and do not ship the MCP fields.
- If Task 3 cannot return one complete token-scoped conflict set within row/time/schema bounds, keep the revised Skill disabled; never fall back to `get_delegation_status` or mutable documents.
- If Task 4 canonical bytes differ from the published vector, stop before Task 5. Do not approximate with delimiter joins, locale sorting, or a second parent hash.
- If Task 5 evidence is stale, mixed, incomplete, oversized, ambiguous, or inconsistent, return the exact failure and no authorization. Restart a stale scan at page one; never merge snapshots.
- If a pending route-change intent is missing, altered, partially applied, cleared before approval, or contradicted by any durable affected row, stop the change. Recover only by fresh evidence, Plan-only derivation, exact suffix resynchronization, full admission, and complete Plan re-review.
- If a committed run lacks one exact unresolved intent, keep the missing/deleted progress mirror blocking. Do not synthesize an intent, replay the call, or adopt by best effort.
- If Task 6 cannot prove warning-only behavior, omit the new projection warning behavior until fixed; do not create a Gate, Card, or platform decision as a substitute.
- If Task 7 Skill/validator integration fails, revert only the uncommitted Task 7 emitter changes. Tasks 1-6 remain backward-compatible because binding is optional, the query is read-only, static modes are non-authorizing, and projection is warning-only.
- A selected Agent/profile becoming unavailable never permits silent Grok fallback or an active-Task handoff. Preserve the binding and ask the user at the approved boundary.
- No frontend migration, manifest conversion, historical binding guess, platform completion state, remote delivery, or data rewrite is part of this increment.
