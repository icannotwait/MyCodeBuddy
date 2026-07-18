# Coordination V1 Status-Wait Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject legacy positive `wait_ms` status calls on `coordination_v1` connections while advertising an accurate, actionable MCP schema and preserving legacy connections unchanged.

**Architecture:** Keep the Broker and listener wait modes intact. Make the embedded MCP schema coordination-aware by default, project it back to the legacy shape when `coordination_v1` is absent, and enforce the same capability-specific rule synchronously in the companion before constructing `BrokerStatusRequest`.

**Tech Stack:** Rust 2021, Tokio, serde/serde_json, MCP JSON-RPC, existing `codeg-mcp` companion unit tests.

## Global Constraints

- Implement `docs/superpowers/specs/2026-07-18-coordination-v1-status-wait-contract-design.md`.
- Apply strict positive-wait rejection only when the immutable companion feature set contains `coordination_v1`.
- Preserve all legacy connection behavior: omitted `wait_ms` is snapshot, `wait_ms: 0` is terminal-only, and positive `wait_ms` is bounded supervised wait.
- Preserve coordination snapshot calls that omit `wait_ms`.
- Preserve coordination `wait_ms: 0` without `return_when` as the existing terminal-only form.
- Preserve canonical Join as `return_when="all_terminal_or_attention"` with explicit `wait_ms: 0`.
- Do not remap positive waits into Join and do not change Broker or listener wait semantics.
- Reject invalid coordination calls synchronously with JSON-RPC `-32602`, before a Broker socket round trip is spawned.
- Use this exact corrective error text: `positive wait_ms is unavailable with coordination_v1; retry with return_when="all_terminal_or_attention" and wait_ms=0`.
- MCP tool tips mean `tools/list` tool and parameter `description` fields only; add no frontend hover UI.
- Keep the all-feature serialized `tools/list` line at or below `GROK_STDIO_SAFE_TOOLS_LIST_BYTES` (7,680 bytes).
- Preserve unrelated worktree changes. At planning time, `src-tauri/resources/THIRD_PARTY_LICENSES.txt` is already modified and must not be staged or reverted.

---

## File Map

- `src-tauri/src/acp/delegation/companion.rs`: capability-specific status argument validation, legacy schema projection, MCP description constants, and companion unit tests.
- `src-tauri/src/acp/delegation/tool_schema.json`: coordination-aware status schema and concise model-facing tool/parameter tips.

No frontend, Broker, listener, transport, database, migration, or locale file changes are required.

---

### Task 1: Reject Positive Legacy Waits on Coordination Connections

**Files:**
- Modify: `src-tauri/src/acp/delegation/companion.rs:504-539`
- Test: `src-tauri/src/acp/delegation/companion.rs:1861-1911`
- Test: `src-tauri/src/acp/delegation/companion.rs:2028-2052`

**Interfaces:**
- Consumes: `CompanionFeatures::coordination_v1`, raw `tools/call` arguments, and existing `parse_return_when` validation.
- Produces: `parse_status_wait_arguments(arguments, coordination_v1) -> Result<(Option<u64>, Option<DelegationReturnWhen>), String>` and stable `COORDINATION_POSITIVE_WAIT_ERROR` guidance.

- [ ] **Step 1: Add dispatch-level contract tests before production changes**

Add these tests beside the existing `get_delegation_status_*` companion tests. They use the real dispatcher so `LineAction::Respond` proves the rejected request never became a spawned Broker round trip.

```rust
#[tokio::test]
async fn coordination_rejects_positive_legacy_status_wait_without_spawning() {
    let line = json!({
        "jsonrpc": "2.0",
        "id": 24,
        "method": "tools/call",
        "params": {
            "name": "get_delegation_status",
            "arguments": { "task_ids": ["task-a"], "wait_ms": 60_000 }
        }
    })
    .to_string();

    let response = unwrap_respond(dispatch_with_features(COORDINATION, &line).await);
    let error = response.error.expect("positive coordination wait must fail");
    assert_eq!(error.code, -32602);
    assert_eq!(
        error.message,
        "positive wait_ms is unavailable with coordination_v1; retry with \
         return_when=\"all_terminal_or_attention\" and wait_ms=0"
    );
}

#[tokio::test]
async fn coordination_keeps_supported_status_wait_forms() {
    for arguments in [
        json!({ "task_ids": ["task-a"] }),
        json!({ "task_ids": ["task-a"], "wait_ms": 0 }),
        json!({
            "task_ids": ["task-a"],
            "wait_ms": 0,
            "return_when": "all_terminal_or_attention"
        }),
    ] {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 25,
            "method": "tools/call",
            "params": { "name": "get_delegation_status", "arguments": arguments }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(COORDINATION, &line).await,
            LineAction::Spawn(_)
        ));
    }
}

#[tokio::test]
async fn legacy_connection_keeps_positive_status_wait() {
    let line = json!({
        "jsonrpc": "2.0",
        "id": 26,
        "method": "tools/call",
        "params": {
            "name": "get_delegation_status",
            "arguments": { "task_ids": ["task-a"], "wait_ms": 60_000 }
        }
    })
    .to_string();
    assert!(matches!(dispatch_for_test(&line).await, LineAction::Spawn(_)));
}
```

- [ ] **Step 2: Run the rejection test and verify RED**

Run:

```powershell
cargo test --features test-utils coordination_rejects_positive_legacy_status_wait_without_spawning -- --nocapture
```

Expected: FAIL because the current coordination request returns `LineAction::Spawn`, causing `unwrap_respond` to panic with `expected Respond, got Spawn`.

- [ ] **Step 3: Add one argument parser that owns the combined wait contract**

Near `LEGACY_STATUS_DESCRIPTION`, add the stable error constant:

```rust
pub const COORDINATION_POSITIVE_WAIT_ERROR: &str =
    "positive wait_ms is unavailable with coordination_v1; retry with \
     return_when=\"all_terminal_or_attention\" and wait_ms=0";
```

Keep `parse_return_when` responsible for validating the optional Join field and add this helper immediately after it:

```rust
fn parse_status_wait_arguments(
    arguments: &Value,
    coordination_v1: bool,
) -> Result<(Option<u64>, Option<DelegationReturnWhen>), String> {
    let wait_ms = arguments.get("wait_ms").and_then(Value::as_u64);
    let return_when = parse_return_when(arguments, coordination_v1)?;
    if coordination_v1 && return_when.is_none() && wait_ms.is_some_and(|ms| ms > 0) {
        return Err(COORDINATION_POSITIVE_WAIT_ERROR.into());
    }
    Ok((wait_ms, return_when))
}
```

In the `get_delegation_status` dispatch arm, replace the separate `wait_ms` read and `parse_return_when` match with:

```rust
let (wait_ms, return_when) =
    match parse_status_wait_arguments(&arguments, ctx.features.coordination_v1) {
        Ok(values) => values,
        Err(message) => return LineAction::Respond(err(id, -32602, message)),
    };
```

Do not add validation to the listener or Broker; legacy calls must still reach their existing wait modes.

- [ ] **Step 4: Run the Task 1 tests and verify GREEN**

Run:

```powershell
cargo test --features test-utils coordination_rejects_positive_legacy_status_wait_without_spawning -- --nocapture
cargo test --features test-utils coordination_keeps_supported_status_wait_forms -- --nocapture
cargo test --features test-utils legacy_connection_keeps_positive_status_wait -- --nocapture
cargo test --features test-utils join_input_requires_capability_literal_value_and_explicit_zero -- --nocapture
```

Expected: all four commands exit 0. The rejection returns exact `-32602`; the other supported forms still produce `LineAction::Spawn`.

- [ ] **Step 5: Commit the runtime contract**

```powershell
git add -- src-tauri/src/acp/delegation/companion.rs
git commit -m "fix(delegation): reject positive coordination waits"
```

Before committing, confirm `git diff --cached --name-only` lists only `src-tauri/src/acp/delegation/companion.rs`.

---

### Task 2: Align Coordination and Legacy MCP Tips and Schemas

**Files:**
- Modify: `src-tauri/src/acp/delegation/tool_schema.json:45-68`
- Modify: `src-tauri/src/acp/delegation/companion.rs:89-91`
- Modify: `src-tauri/src/acp/delegation/companion.rs:402-434`
- Test: `src-tauri/src/acp/delegation/companion.rs:2100-2141`
- Test: `src-tauri/src/acp/delegation/companion.rs:2292-2340`
- Test: `src-tauri/src/acp/delegation/companion.rs:2459-2496`

**Interfaces:**
- Consumes: embedded coordination-aware `TOOL_SCHEMA_JSON` and the existing legacy `tools/list` projection.
- Produces: coordination `wait_ms` schema with `maximum: 0`, restored unconstrained legacy `wait_ms`, and capability-accurate descriptions.

- [ ] **Step 1: Strengthen schema-projection tests before changing the schema**

Rename `join_tools_list_hides_return_when_without_coordination` to
`coordination_and_legacy_tools_list_project_wait_contract` and extend it with
these assertions after resolving each status tool:

```rust
let legacy_wait = &status["inputSchema"]["properties"]["wait_ms"];
assert_eq!(legacy_wait["minimum"], 0);
assert!(legacy_wait.get("maximum").is_none());
let legacy_guidance = tool_guidance(status);
assert!(legacy_guidance.contains("positive wait (max 60000 ms)"));
assert!(!legacy_guidance.contains("positive wait_ms is rejected"));
```

For the coordination status tool, add:

```rust
let coordination_wait = &status["inputSchema"]["properties"]["wait_ms"];
assert_eq!(coordination_wait["minimum"], 0);
assert_eq!(coordination_wait["maximum"], 0);
let coordination_guidance = tool_guidance(status);
for required in [
    "omit wait_ms for an immediate snapshot",
    "return_when=all_terminal_or_attention",
    "positive wait_ms is rejected",
    "re-join only still-running required",
] {
    assert!(
        coordination_guidance.contains(required),
        "coordination guidance missing {required:?}"
    );
}
assert!(!coordination_guidance.contains("positive wait (max 60000 ms)"));
```

In `tool_schema_retains_essential_agent_guidance`, replace the legacy positive-wait phrases for `get_delegation_status` with the coordination contract phrases above plus `"all requested tasks are terminal"`, `"attention"`, `"unavailable"`, `"input order"`, `"wake_reason"`, and `"attention_requests"`.

- [ ] **Step 2: Run the schema test and verify RED**

Run:

```powershell
cargo test --features test-utils coordination_and_legacy_tools_list_project_wait_contract -- --nocapture
```

Expected: FAIL because coordination `wait_ms.maximum` is currently absent and its guidance still advertises positive waits.

- [ ] **Step 3: Replace the embedded coordination tips and constrain `wait_ms`**

In `tool_schema.json`, use this concise coordination-aware tool description:

```json
"description": "Get status or results for task_ids. Omit wait_ms for an immediate snapshot. To block, Join with return_when=all_terminal_or_attention and explicit wait_ms=0; positive wait_ms is rejected on coordination_v1. Join returns when all requested tasks are terminal, requested-child attention is required, or the Broker is unavailable. Answer every attention request, then re-Join only still-running required task ids. Returns {\"tasks\":[...]} in input order; Join responses also include wake_reason and attention_requests."
```

Change the coordination `wait_ms` property to:

```json
"wait_ms": {
  "type": "integer",
  "minimum": 0,
  "maximum": 0,
  "description": "Omit wait_ms for an immediate snapshot. Use 0 with return_when=all_terminal_or_attention for canonical Join. Positive wait_ms is rejected on coordination_v1."
}
```

Change the `return_when` description to:

```json
"description": "Canonical Join condition. Requires explicit wait_ms=0; returns for all terminal, attention required, or unavailable."
```

- [ ] **Step 4: Restore the complete legacy parameter schema during projection**

Beside `LEGACY_STATUS_DESCRIPTION`, add the exact legacy parameter guidance:

```rust
pub const LEGACY_WAIT_MS_DESCRIPTION: &str = "Omit wait_ms for an immediate snapshot. A positive wait (max 60000 ms) returns on terminal, stalled, waiting_input, or its deadline. wait_ms=0 waits only for a terminal result without a timeout.";
```

Inside the legacy `get_delegation_status` projection, after `props.remove("return_when")`, restore the property shape:

```rust
if let Some(wait_ms) = props.get_mut("wait_ms").and_then(Value::as_object_mut) {
    wait_ms.remove("maximum");
    wait_ms.insert(
        "description".into(),
        Value::String(LEGACY_WAIT_MS_DESCRIPTION.into()),
    );
}
```

The legacy description intentionally remains independent of the embedded
coordination description. Do not leak `coordination_v1`, rejection guidance,
or `return_when` into legacy `tools/list`.

- [ ] **Step 5: Run schema, tips, and byte-budget tests and verify GREEN**

Run:

```powershell
cargo test --features test-utils coordination_and_legacy_tools_list_project_wait_contract -- --nocapture
cargo test --features test-utils tool_schema_retains_essential_agent_guidance -- --nocapture
cargo test --features test-utils all_feature_tools_list_stays_within_grok_stdio_budget -- --nocapture
cargo test --features test-utils join_input_requires_capability_literal_value_and_explicit_zero -- --nocapture
```

Expected: all commands exit 0. Coordination has `maximum: 0`, legacy has no
maximum, both tip sets match their runtime semantics, and the serialized line
remains at or below 7,680 bytes.

- [ ] **Step 6: Commit the schema and tips contract**

```powershell
git add -- src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/tool_schema.json
git commit -m "feat(delegation): clarify coordination wait tips"
```

Before committing, confirm `git diff --cached --name-only` lists only those two files.

---

### Task 3: Verify the Companion End to End

**Files:**
- Verify: `src-tauri/src/acp/delegation/companion.rs`
- Verify: `src-tauri/src/acp/delegation/tool_schema.json`

**Interfaces:**
- Consumes: completed Task 1 and Task 2 behavior.
- Produces: fresh evidence that the full companion test surface, no-default-feature binary, and lint gates accept the change.

- [ ] **Step 1: Run the full companion unit-test module**

```powershell
cargo test --features test-utils acp::delegation::companion::tests -- --nocapture
```

Expected: all companion tests pass with zero failures.

- [ ] **Step 2: Re-run the two behavioral regressions that distinguish Join from supervised wait**

```powershell
cargo test --features test-utils supervised_wait_wakes_on_requested_active_timestamp_change -- --nocapture
cargo test --features test-utils join_returns_for_attention_before_or_after_wait_but_not_for_noise -- --nocapture
```

Expected: both pass. This proves the implementation gated access to legacy
supervised waits without changing either Broker behavior.

- [ ] **Step 3: Check and lint the `codeg-mcp` binary configuration**

```powershell
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: both commands exit 0 with no warnings promoted to errors.

- [ ] **Step 4: Run the repository-required Rust test gate**

```powershell
cargo test --features test-utils
```

Expected: the full Rust test suite exits 0. If an unrelated existing failure
appears, record its exact test name and reproduce it from unchanged `HEAD`
before attributing it to this change.

- [ ] **Step 5: Audit the final diff and worktree**

```powershell
git diff --check HEAD~2..HEAD
git diff --stat HEAD~2..HEAD
git status --short
```

Expected: the two implementation commits change only `companion.rs` and
`tool_schema.json`; the pre-existing `THIRD_PARTY_LICENSES.txt` modification
remains unstaged and untouched.

- [ ] **Step 6: Request code review**

Use the `requesting-code-review` skill. Ask the reviewer to verify:

```text
1. coordination_v1 rejects only positive wait_ms without return_when;
2. supported snapshot, terminal-only, and canonical Join forms remain valid;
3. legacy connections retain positive supervised waits;
4. tools/list schema and tips match runtime behavior;
5. no invalid call reaches the Broker and the Grok byte budget remains green.
```

Address any finding through a new RED/GREEN test cycle and re-run the relevant
Task 3 gates before completion.
