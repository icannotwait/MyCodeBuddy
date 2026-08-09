# Task 4 Brief

### Task 4: Add the Grok and Codex Settings Facade and Async Bridge Operations

**Milestone:** M2.

**Files:**

- Create: `src-tauri/src/commands/eui_facade.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Expand: `src-tauri/codeg-eui-core/src/commands.rs`
- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Test: `src-tauri/codeg-eui-core/tests/settings_contract.rs`
- Test: unit tests in `src-tauri/src/commands/eui_facade.rs`

**Interfaces:**

- Consumes: `acp_list_agents_core`, `acp_update_agent_config_and_refresh`, `acp_update_agent_env_and_refresh`, `acp_preflight_core`, `AcpAgentInfo`, `CodexSandboxStructuredConfig`, and `GrokStructuredConfig`.
- Produces: public `EuiAgentSettings`, `EuiAgentSettingsPatch`, `EuiAgentProbe`, `get_eui_agent_settings`, `set_eui_agent_settings`, and `probe_eui_agent`; async C functions `codeg_eui_get_agent_settings`, `codeg_eui_set_agent_settings`, and `codeg_eui_probe_agent`.
- Restricts: only wire values `"codex"` and `"grok"`; every other agent returns `EuiFacadeError::UnsupportedAgent` before file or DB access.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 4 | Grok/Codex settings facade and async operations | ACP facade, DTOs, bridge handlers, tests | `security_trust_boundary`: auth/config writes; `public_compatibility`: new shared facade | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write facade round-trip tests before adding the facade**

Use isolated `CODEX_HOME`, `GROK_HOME` when supported by the existing helpers, a temporary OS home otherwise under a serialized env mutex, and a fresh disk DB. Assert the DTO contains only backend-owned fields:

```rust
#[tokio::test]
async fn codex_settings_round_trip_through_existing_native_files() {
    let fixture = SettingsFixture::new(AgentType::Codex).await;
    let patch = EuiAgentSettingsPatch {
        enabled: Some(true),
        env: Some(BTreeMap::from([("OPENAI_API_KEY".into(), "test-key".into())])),
        model_provider_id: None,
        config_json: None,
        codex_auth_json: Some(r#"{"OPENAI_API_KEY":"test-key"}"#.into()),
        codex_config_toml: Some("model = \"gpt-5\"\napproval_policy = \"never\"\n".into()),
        codex_model_catalog: None,
        codex_sandbox: None,
        grok_config_toml: None,
        grok_structured: None,
    };
    set_eui_agent_settings(fixture.state(), AgentType::Codex, patch).await.unwrap();
    let got = get_eui_agent_settings(fixture.state(), AgentType::Codex).await.unwrap();
    assert_eq!(got.agent_type, AgentType::Codex);
    assert_eq!(got.codex_config_toml.as_deref(),
               Some("model = \"gpt-5\"\napproval_policy = \"never\"\n"));
    assert!(got.grok_config_toml.is_none());
}
```

Add the equivalent Grok raw/structured round-trip and a test proving `ClaudeCode` is rejected before any filesystem path is touched.

- [ ] **Step 2: Run the facade tests to verify RED**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
```

Expected: FAIL because `commands::eui_facade` does not exist.

- [ ] **Step 3: Define the narrow backend-aligned DTOs**

Use serde camelCase only at this public Rust boundary:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiAgentSettings {
    pub agent_type: AgentType,
    pub available: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
    pub env: BTreeMap<String, String>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxSettings>,
    pub grok_config_toml: Option<String>,
    pub grok_settings: Option<GrokSettings>,
    pub model_provider_id: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EuiAgentSettingsPatch {
    pub enabled: Option<bool>,
    pub env: Option<BTreeMap<String, String>>,
    pub model_provider_id: Option<i32>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxStructuredConfig>,
    pub grok_config_toml: Option<String>,
    pub grok_structured: Option<GrokStructuredConfig>,
}
```

Do not expose OpenCode, Cursor, Cline, Hermes, installation/download mutation, Axum parameter structs, or transport-specific status codes.

- [ ] **Step 4: Implement the facade over existing ACP cores**

Keep the existing ACP helpers `pub(crate)`: `eui_facade` is a sibling module in the same `codeg` crate and can call them without exposing a wider API or editing `commands/acp.rs` visibility. `get` calls `acp_list_agents_core`, selects exactly one row, and projects fields. `set` first validates agent-specific field exclusivity, then applies env/preferences and config through the existing refresh helpers. It never writes TOML/JSON directly. `probe` calls `acp_preflight_core(agent, Some(true), db)` and returns `{launchable, installed_version, message}`.

- [ ] **Step 5: Write failing async completion tests**

Inject a `CoreOps` test implementation into the command worker. Prove get/probe run off the UI thread and result JSON arrives through a later frame:

```rust
#[tokio::test]
async fn slow_probe_never_blocks_poll_and_completes_once() {
    let gate = Arc::new(Notify::new());
    let bridge = TestBridge::with_ops(SlowProbeOps::new(gate.clone()));
    let request_id = bridge.enqueue_probe("codex").unwrap();
    assert!(bridge.poll_within(Duration::from_millis(20)).completions.is_empty());
    gate.notify_one();
    let completion = bridge.wait_completion(request_id).await;
    assert_eq!(completion.status, CompletionStatus::Ok);
    assert_eq!(completion.op, Operation::ProbeAgent);
    assert_eq!(bridge.completion_count(request_id), 1);
}
```

- [ ] **Step 6: Implement settings/probe ABI entry points**

Each entry uses the generic validated enqueue helper. `set_agent_settings` parses JSON with `deny_unknown_fields` after the 2 MiB bound and before acceptance. Result payloads are UTF-8 JSON serialized from the facade DTO; errors are diagnostic strings with no secret values. Redact auth/env values from tracing.

- [ ] **Step 7: Run M2 verification**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test settings_contract -- --test-threads=1
cargo check --manifest-path src-tauri/codeg-eui-core/Cargo.toml
```

Expected: Codex and Grok read/write/probe paths round-trip through existing helpers, unsupported agents fail closed, malformed/oversized JSON is rejected before acceptance, slow probe does not block poll, and every accepted settings request completes once.

- [ ] **Step 8: Commit and prepare the Task 4 review package**

```bash
git add --dry-run -- src-tauri/src/commands/eui_facade.rs src-tauri/src/commands/mod.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/tests/settings_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git add -- src-tauri/src/commands/eui_facade.rs src-tauri/src/commands/mod.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/tests/settings_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): expose Grok and Codex settings facade"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/src/commands src-tauri/codeg-eui-core codeg-eui/app/bridge/codeg_eui_bridge.h
```

Expected package: one settings/probe commit, with no new config persistence implementation and no secret-bearing logs. Route it to both high-risk reviewers, then continue directly to Task 5.

