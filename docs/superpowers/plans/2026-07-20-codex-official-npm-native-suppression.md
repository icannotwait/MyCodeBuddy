# Codex Official npm Native Suppression Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make managed Codeg launches suppress Codex native collaboration through the official npm adapter's `CODEX_CONFIG.features.multi_agent=false` contract.

**Architecture:** Keep route application at the existing process boundary in `connection.rs`. For `CodexMultiAgentFalse`, parse and merge the per-launch `CODEX_CONFIG` JSON object, preserving unrelated keys; all other routes remain no-ops for that variable. Malformed managed-route input returns the existing typed `NativeSuppressionInvalid` error before process launch.

**Tech Stack:** Rust 2021, `serde_json`, existing ACP route types and unit-test helpers.

## Global Constraints

- Continue launching official `@agentclientprotocol/codex-acp@1.1.2`.
- Guarantee suppression only for App Server mode; `CODEX_ACP_USE_CLI=1` remains unsupported by this change.
- Do not edit persistent Codex files such as `~/.codex/config.toml`.
- Native routes must preserve `CODEX_CONFIG` byte-for-byte.
- Grok, CodeBuddy, and Claude suppression behavior must remain unchanged.
- Do not log `CODEX_CONFIG` contents.

---

### Task 1: Route-scoped official Codex configuration

**Files:**
- Modify: `src-tauri/src/acp/connection.rs:607`
- Test: `src-tauri/src/acp/connection.rs:11236`

**Interfaces:**
- Consumes: `apply_route_environment(agent_type, plan, env) -> Result<(), AcpError>` and `NativeSuppressionPlan::CodexMultiAgentFalse`.
- Produces: a private `merge_codex_official_native_suppression(env: &mut BTreeMap<String, String>) -> Result<(), AcpError>` helper used only by the Codex Codeg route branch.

- [ ] **Step 1: Write the failing official-config tests**

Replace the Codex assertions in the existing route-scope test with focused tests that parse the resulting JSON instead of comparing serialization order:

```rust
#[test]
fn codex_codeg_route_sets_official_multi_agent_config() {
    let mut env = BTreeMap::from([("KEEP".into(), "yes".into())]);
    apply_route_environment(AgentType::Codex, &codeg_plan(AgentType::Codex), &mut env)
        .unwrap();

    let config: serde_json::Value =
        serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
    assert_eq!(config["features"]["multi_agent"], false);
    assert_eq!(env.get("KEEP").map(String::as_str), Some("yes"));
    assert!(!env.contains_key("CODEX_ACP_MULTI_AGENT"));
}

#[test]
fn codex_codeg_route_merges_existing_official_config() {
    let original = serde_json::json!({
        "model": "gpt-5.4",
        "features": { "fast_mode": true, "multi_agent": true },
        "nested": { "keep": [1, 2, 3] }
    });
    let mut env = BTreeMap::from([
        ("CODEX_CONFIG".into(), serde_json::to_string(&original).unwrap()),
        ("CODEX_ACP_MULTI_AGENT".into(), "user-value".into()),
    ]);
    apply_route_environment(AgentType::Codex, &codeg_plan(AgentType::Codex), &mut env)
        .unwrap();

    let merged: serde_json::Value =
        serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
    assert_eq!(merged["model"], "gpt-5.4");
    assert_eq!(merged["features"]["fast_mode"], true);
    assert_eq!(merged["features"]["multi_agent"], false);
    assert_eq!(merged["nested"], original["nested"]);
    assert_eq!(
        env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
        Some("user-value")
    );
}

#[test]
fn codex_codeg_route_rejects_malformed_official_config() {
    for raw in ["not-json", "[]", r#"{"features":[]}"#] {
        let mut env = BTreeMap::from([("CODEX_CONFIG".into(), raw.into())]);
        let err = apply_route_environment(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid
            }
        ));
        assert_eq!(env.get("CODEX_CONFIG").map(String::as_str), Some(raw));
    }
}

#[test]
fn codex_native_route_preserves_official_config_byte_for_byte() {
    let raw = " { \"features\" : { \"multi_agent\" : true } } ";
    let mut env = BTreeMap::from([("CODEX_CONFIG".into(), raw.into())]);
    apply_route_environment(AgentType::Codex, &native_plan(AgentType::Codex), &mut env)
        .unwrap();
    assert_eq!(env.get("CODEX_CONFIG").map(String::as_str), Some(raw));
}
```

Keep the existing Grok and Claude assertions, renaming their combined test if necessary so its name matches its remaining scope.

- [ ] **Step 2: Run the focused tests and verify RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils codex_codeg_route_ --lib
cargo test --features test-utils codex_native_route_preserves_official_config_byte_for_byte --lib
```

Expected: the Codeg tests fail because current code writes only `CODEX_ACP_MULTI_AGENT=0`; the Native preservation test may already pass and documents the unchanged baseline.

- [ ] **Step 3: Implement the minimal structured merge**

Add a private typed-error constructor and merge helper near `apply_route_environment`, then call it from the Codex suppression branch:

```rust
fn native_suppression_invalid() -> AcpError {
    AcpError::RouteUnavailable {
        reason: crate::acp::delegation::route::RouteDegradedReason::NativeSuppressionInvalid,
    }
}

fn merge_codex_official_native_suppression(
    env: &mut BTreeMap<String, String>,
) -> Result<(), AcpError> {
    let mut config = match env.get("CODEX_CONFIG") {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|_| native_suppression_invalid())?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    let root = config
        .as_object_mut()
        .ok_or_else(native_suppression_invalid)?;
    let features = root
        .entry("features")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let features = features
        .as_object_mut()
        .ok_or_else(native_suppression_invalid)?;
    features.insert("multi_agent".into(), serde_json::Value::Bool(false));
    env.insert(
        "CODEX_CONFIG".into(),
        serde_json::to_string(&config).map_err(|_| native_suppression_invalid())?,
    );
    Ok(())
}
```

Update comments to describe the official `CODEX_CONFIG` contract. Do not remove or overwrite a user-provided legacy `CODEX_ACP_MULTI_AGENT` key.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils codex_codeg_route_ --lib
cargo test --features test-utils codex_native_route_preserves_official_config_byte_for_byte --lib
cargo test --features test-utils codex_env_and_claude_meta_are_additive_and_route_scoped --lib
```

Expected: all selected tests pass with no warnings.

- [ ] **Step 5: Format and run Rust verification**

Run from `src-tauri/`:

```powershell
cargo fmt --check
cargo test --features test-utils
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: every command exits `0`.

- [ ] **Step 6: Review the final diff and commit**

```powershell
git diff --check
git diff -- src-tauri/src/acp/connection.rs
git status --short
git add -- src-tauri/src/acp/connection.rs
git commit -m "fix(acp): suppress Codex native agents via official config"
```

The commit must contain only `src-tauri/src/acp/connection.rs`; preserve unrelated user files and untracked paths.
