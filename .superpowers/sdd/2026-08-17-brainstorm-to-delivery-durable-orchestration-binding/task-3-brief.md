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

