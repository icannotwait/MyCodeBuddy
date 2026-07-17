//! ACP launch terminal context: shell snapshot finalization and declaration env.
//!
//! Launch inputs (runtime env + terminal settings) are loaded at every entry
//! point; the immutable shell snapshot is finalized only when a new process
//! will actually be spawned (after connection reuse has been ruled out).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sacp::schema::{ContentBlock, Meta, TextContent};

use crate::acp::connection::{agent_delivers_wire_mcp, locate_codeg_mcp_binary};
use crate::acp::delegation::route::{
    resolve_route, DelegationConnectionOrigin, DelegationRoutePlan, DelegationRoutePolicy,
    RouteCapabilitySnapshot, RouteResolutionError, RouteResolutionInput,
};
use crate::acp::error::AcpError;
use crate::acp::registry;
use crate::acp::terminal_adapter::{adapter_for, AcpTerminalAdapter};
use crate::commands::delegation::DelegationRuntimeSnapshot;
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::SystemTerminalSettings;
use crate::terminal::shell::{
    resolve_terminal_shell, ResolvedShellSnapshot, ResolvedShellSpec, ShellDialect,
};
use sea_orm::DatabaseConnection;

/// Connect-time route preference inputs. Resolved exactly once before process
/// launch; the resulting plan is stored immutable on the connection.
#[derive(Debug, Clone)]
pub struct AcpRouteRequest {
    pub conversation_id: Option<i32>,
    pub draft_override: Option<DelegationRoutePolicy>,
    pub origin: DelegationConnectionOrigin,
}

impl AcpRouteRequest {
    pub fn root(
        conversation_id: Option<i32>,
        draft_override: Option<DelegationRoutePolicy>,
    ) -> Self {
        Self {
            conversation_id,
            draft_override,
            origin: DelegationConnectionOrigin::Root,
        }
    }

    /// Broker-spawned children always force Codeg regardless of global settings.
    pub fn codeg_child() -> Self {
        Self {
            conversation_id: None,
            draft_override: None,
            origin: DelegationConnectionOrigin::CodegChild,
        }
    }
}

/// Inputs loaded before reuse / spawn: agent runtime env + terminal settings +
/// one already-resolved route plan + the exact capability snapshot used.
#[derive(Debug, Clone)]
pub struct AcpLaunchInputs {
    pub runtime_env: BTreeMap<String, String>,
    pub terminal_settings: SystemTerminalSettings,
    pub route_plan: DelegationRoutePlan,
    pub origin: DelegationConnectionOrigin,
    /// Preference used for later comparison re-resolution (not source-of-truth
    /// for the live process — that is `route_plan`).
    pub route_preference: Option<DelegationRoutePolicy>,
    /// Exact capability facts used when resolving `route_plan`. Stored on the
    /// connection so every stale comparison reuses them — never optimistic.
    pub route_capability: RouteCapabilitySnapshot,
}

impl AcpLaunchInputs {
    /// Construct launch inputs with a feature-disabled native plan.
    /// **Tests only** — production call sites must resolve a real route plan.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_placeholder_route(
        runtime_env: BTreeMap<String, String>,
        terminal_settings: SystemTerminalSettings,
    ) -> Self {
        use crate::acp::delegation::route::{
            resolve_route, SuppressionCapability, ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        // Feature-disabled root: native effective, stable empty-capability path.
        let capability = RouteCapabilitySnapshot {
            suppression: SuppressionCapability::supported(ROUTE_ADAPTER_CONTRACT_VERSION),
            agent_mcp_supported: true,
            companion_binary_available: true,
        };
        let route_plan = resolve_route(RouteResolutionInput {
            agent_type: AgentType::ClaudeCode,
            origin: DelegationConnectionOrigin::Root,
            session_override: None,
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: false,
            suppression: capability.suppression.clone(),
            agent_mcp_supported: capability.agent_mcp_supported,
            companion_binary_available: capability.companion_binary_available,
        })
        .expect("feature-disabled native plan must resolve");
        Self {
            runtime_env,
            terminal_settings,
            route_plan,
            origin: DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability: capability,
        }
    }
}

/// Immutable config for a connection that is about to spawn a process.
#[derive(Debug, Clone)]
pub(crate) struct AcpLaunchConfig {
    pub runtime_env: BTreeMap<String, String>,
    pub terminal_shell: ResolvedShellSnapshot,
    pub route_plan: DelegationRoutePlan,
    pub origin: DelegationConnectionOrigin,
    pub route_preference: Option<DelegationRoutePolicy>,
    pub route_capability: RouteCapabilitySnapshot,
}

/// Build the exact capability snapshot for a launch from installed-version and
/// runtime-env facts. Companion/MCP gates use live process facts.
pub fn build_route_capability_snapshot(
    agent_type: AgentType,
    installed_version: Option<&str>,
    runtime_env: &BTreeMap<String, String>,
) -> RouteCapabilitySnapshot {
    let meta = registry::get_agent_meta(agent_type);
    let agent_mcp_supported = meta.supports_mcp && agent_delivers_wire_mcp(agent_type);
    let companion_binary_available = locate_codeg_mcp_binary().is_some();
    RouteCapabilitySnapshot::from_launch_facts(
        agent_type,
        installed_version,
        runtime_env,
        agent_mcp_supported,
        companion_binary_available,
    )
}

/// Resolve the immutable route plan for a connect request.
///
/// Public test-friendly entry: builds a capability snapshot from empty
/// runtime env and no installed-version (managed host pin). Production
/// launch paths use [`resolve_connect_route_with_capability`] after building
/// real runtime_env facts.
///
/// Persisted conversation override is authoritative when `conversation_id` is
/// present; a mismatching payload override is ignored and logged. Draft roots
/// (no row) use `draft_override`. Children force Codeg via origin. The
/// conversation row must exist and its Agent type must match `agent_type`.
pub async fn resolve_connect_route(
    conn: &DatabaseConnection,
    agent_type: AgentType,
    request: AcpRouteRequest,
    runtime: &DelegationRuntimeSnapshot,
) -> Result<DelegationRoutePlan, AcpError> {
    let capability = build_route_capability_snapshot(agent_type, None, &BTreeMap::new());
    resolve_connect_route_with_capability(conn, agent_type, request, runtime, &capability).await
}

/// Capability-aware connect resolution. Callers that already built
/// `RouteCapabilitySnapshot` from real runtime_env/installed-version must use
/// this so suppression/MCP/companion facts match the launch process.
pub async fn resolve_connect_route_with_capability(
    conn: &DatabaseConnection,
    agent_type: AgentType,
    request: AcpRouteRequest,
    runtime: &DelegationRuntimeSnapshot,
    capability: &RouteCapabilitySnapshot,
) -> Result<DelegationRoutePlan, AcpError> {
    let session_override = match request.origin {
        DelegationConnectionOrigin::CodegChild => None,
        DelegationConnectionOrigin::Root => {
            if let Some(conversation_id) = request.conversation_id {
                let persisted =
                    load_persisted_route_override(conn, conversation_id, agent_type).await?;
                if let Some(payload) = request.draft_override {
                    if Some(payload) != persisted {
                        tracing::warn!(
                            conversation_id,
                            ?payload,
                            ?persisted,
                            "[ACP] ignoring connect payload delegation_route_override; \
                             persisted conversation override is authoritative"
                        );
                    }
                }
                persisted
            } else {
                request.draft_override
            }
        }
    };

    resolve_route(RouteResolutionInput {
        agent_type,
        origin: request.origin,
        session_override,
        global_policy: runtime.route_policy,
        delegation_enabled: runtime.enabled,
        suppression: capability.suppression.clone(),
        agent_mcp_supported: capability.agent_mcp_supported,
        companion_binary_available: capability.companion_binary_available,
    })
    .map_err(route_resolution_to_acp)
}

/// Load a conversation's route override only when the row exists and its
/// stored Agent type equals `expected_agent`.
async fn load_persisted_route_override(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected_agent: AgentType,
) -> Result<Option<DelegationRoutePolicy>, AcpError> {
    match crate::db::service::conversation_service::get_by_id(conn, conversation_id).await {
        Ok(summary) => {
            if summary.agent_type != expected_agent {
                return Err(AcpError::protocol(format!(
                    "conversation {conversation_id} agent type is {:?}, but connect requested {:?}",
                    summary.agent_type, expected_agent
                )));
            }
            Ok(summary.delegation_route_override)
        }
        Err(crate::db::error::DbError::Migration(msg)) if msg.contains("not found") => {
            Err(AcpError::protocol(format!(
                "conversation not found: {conversation_id}"
            )))
        }
        Err(e) => Err(AcpError::protocol(e.to_string())),
    }
}

fn route_resolution_to_acp(err: RouteResolutionError) -> AcpError {
    match err {
        RouteResolutionError::RouteUnavailable { reason } => AcpError::RouteUnavailable { reason },
        RouteResolutionError::MixedCreationSurfaces => {
            AcpError::protocol("managed plan exposes both native creation and codeg delegation")
        }
    }
}

/// Case-aware env key equality: Windows env keys are case-insensitive.
pub(crate) fn env_key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn set_env_value(env: &mut BTreeMap<String, String>, key: &str, value: String) {
    env.retain(|existing, _| !env_key_eq(existing, key));
    env.insert(key.to_string(), value);
}

/// Declare the selected shell on the agent process environment without
/// touching `COMSPEC` (never overwrite cmd.exe's COMSPEC with PowerShell).
pub fn apply_agent_launch_env(env: &mut BTreeMap<String, String>, spec: &ResolvedShellSpec) {
    let executable = spec.executable.to_string_lossy().into_owned();
    set_env_value(env, "SHELL", executable.clone());
    set_env_value(env, "CODEG_TERMINAL_SHELL", executable);
    set_env_value(
        env,
        "CODEG_TERMINAL_DIALECT",
        spec.dialect.as_str().to_string(),
    );
}

/// Merge adapter contributions with Codeg's authoritative terminal snapshot
/// into ACP `_meta`.
///
/// Adapter keys are inserted only when absent (`or_insert`) so pre-existing
/// Claude (or other) metadata is preserved. Codeg's `codeg.dev/terminal`
/// entry is always written last and overwrites any adapter collision.
pub fn terminal_metadata(
    mut existing: Meta,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
) -> Result<Meta, AcpError> {
    for (key, value) in adapter.agent_metadata(spec)? {
        existing.entry(key).or_insert(value);
    }
    // Platform: prefer the well-known host triples; otherwise emit the raw
    // `std::env::consts::OS` string rather than guessing a family name.
    let platform = match std::env::consts::OS {
        "windows" | "macos" | "linux" => std::env::consts::OS,
        other => other,
    };
    existing.insert(
        "codeg.dev/terminal".into(),
        serde_json::json!({
            "shell": spec.executable.to_string_lossy(),
            "dialect": spec.dialect.as_str(),
            "platform": platform,
            "commandMode": "selected-shell-for-command-lines",
        }),
    );
    Ok(existing)
}

/// Connection-scoped first-prompt injector: appends a versioned
/// `<codeg_terminal_context>` text block **once** per spawned
/// `AgentConnection` process lifetime (wire path only).
///
/// Created in `run_connection` beside the prompt ledger; shared across
/// fork restarts of the conversation loop. Browser/transport reattachment
/// reuses the live `AgentConnection` and never constructs another injector.
pub struct TerminalPromptContext {
    spec: ResolvedShellSpec,
    pending: AtomicBool,
}

impl TerminalPromptContext {
    pub fn new(spec: ResolvedShellSpec) -> Self {
        Self {
            spec,
            pending: AtomicBool::new(true),
        }
    }

    /// Append the terminal context block on the first call only.
    /// Subsequent prompts (including post-fork) are left unchanged.
    pub fn append_once(&self, blocks: &mut Vec<ContentBlock>) {
        if self.pending.swap(false, Ordering::AcqRel) {
            blocks.push(ContentBlock::Text(TextContent::new(
                render_terminal_prompt_context(&self.spec),
            )));
        }
    }
}

/// Human dialect label used inside the Generate-syntax instruction line.
fn dialect_instruction_label(dialect: ShellDialect) -> &'static str {
    match dialect {
        ShellDialect::Cmd => "CMD",
        ShellDialect::PowerShell => "PowerShell",
        ShellDialect::Posix => "POSIX",
        // Custom uses a fixed instruction (not this label).
        ShellDialect::Custom => "custom",
    }
}

/// Render the versioned wire-only terminal context envelope from a shell snapshot.
pub fn render_terminal_prompt_context(spec: &ResolvedShellSpec) -> String {
    let generate_line = match spec.dialect {
        ShellDialect::Custom => {
            "Generate shell command lines using the selected custom shell's syntax.".to_string()
        }
        other => format!(
            "Generate shell command lines using {} syntax.",
            dialect_instruction_label(other)
        ),
    };
    format!(
        "<codeg_terminal_context version=\"1\">\n\
Selected shell: {}\n\
Dialect: {}\n\
{}\n\
ACP command+args requests may still execute directly.\n\
This context is authoritative for the current connection and supersedes\n\
earlier terminal context records.\n\
</codeg_terminal_context>",
        spec.display_name,
        spec.dialect.as_str(),
        generate_line,
    )
}

/// True when `key` is a terminal-declaration field that must not participate
/// in the agent-config fingerprint (tracked separately as a shell selection key).
pub(crate) fn is_terminal_declaration_env_key(key: &str) -> bool {
    env_key_eq(key, "SHELL")
        || env_key_eq(key, "CODEG_TERMINAL_SHELL")
        || env_key_eq(key, "CODEG_TERMINAL_DIALECT")
}

/// Build terminal base env for ACP `terminal/create`: only git credential
/// helper keys from the agent runtime, plus OfficeCLI PATH. Never forwards
/// model/API secrets.
pub(crate) fn build_terminal_base_env(
    runtime_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    runtime_env
        .iter()
        .filter(|(k, _)| k.starts_with("GIT_CONFIG_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Load agent runtime env + system terminal settings + one resolved route plan.
///
/// Builds `runtime_env` **before** route resolution so capability facts
/// (host contract, custom CODEX_PATH, MCP/companion) match the process that
/// will actually launch.
pub(crate) async fn build_acp_launch_inputs(
    db: &AppDatabase,
    agent_type: AgentType,
    session_id: Option<&str>,
    data_dir: &Path,
    route_request: AcpRouteRequest,
    runtime: &DelegationRuntimeSnapshot,
) -> Result<AcpLaunchInputs, AcpError> {
    let runtime_env =
        crate::commands::acp::build_session_runtime_env(db, agent_type, session_id, data_dir)
            .await?;

    let installed_version = crate::db::service::agent_setting_service::get_by_agent_type(
        &db.conn,
        agent_type,
    )
    .await
    .ok()
    .flatten()
    .and_then(|s| s.installed_version);

    let route_capability = build_route_capability_snapshot(
        agent_type,
        installed_version.as_deref(),
        &runtime_env,
    );

    let route_preference = match route_request.origin {
        DelegationConnectionOrigin::CodegChild => None,
        DelegationConnectionOrigin::Root => {
            if let Some(conversation_id) = route_request.conversation_id {
                load_persisted_route_override(&db.conn, conversation_id, agent_type).await?
            } else {
                route_request.draft_override
            }
        }
    };
    let origin = route_request.origin;
    let route_plan = resolve_connect_route_with_capability(
        &db.conn,
        agent_type,
        route_request,
        runtime,
        &route_capability,
    )
    .await?;

    let terminal_settings =
        crate::commands::system_settings::load_system_terminal_settings(&db.conn)
            .await
            .map_err(|error| AcpError::protocol(error.to_string()))?;

    Ok(AcpLaunchInputs {
        runtime_env,
        terminal_settings,
        route_plan,
        origin,
        route_preference,
        route_capability,
    })
}

/// Finalize launch inputs into an immutable shell snapshot + declared env.
///
/// Call only after the manager has decided a new process is required (no
/// reusable live connection). Automatic reconnects that reuse must never
/// reach this path.
pub(crate) fn finalize_acp_launch_config(
    inputs: AcpLaunchInputs,
    agent_type: AgentType,
) -> Result<AcpLaunchConfig, AcpError> {
    finalize_acp_launch_config_with_adapter(inputs, adapter_for(agent_type))
}

fn finalize_acp_launch_config_with_adapter(
    mut inputs: AcpLaunchInputs,
    adapter: &dyn AcpTerminalAdapter,
) -> Result<AcpLaunchConfig, AcpError> {
    let terminal_shell = resolve_terminal_shell(&inputs.terminal_settings)?;
    adapter.validate_shell(&terminal_shell.spec)?;
    for (key, value) in adapter.agent_launch_env(&terminal_shell.spec)? {
        if key.eq_ignore_ascii_case("COMSPEC") {
            return Err(AcpError::protocol(
                "terminal adapters cannot override COMSPEC",
            ));
        }
        if !inputs
            .runtime_env
            .keys()
            .any(|existing| env_key_eq(existing, &key))
        {
            inputs.runtime_env.insert(key, value);
        }
    }
    // Codeg's declarations are authoritative over adapter-provided additions.
    apply_agent_launch_env(&mut inputs.runtime_env, &terminal_shell.spec);
    Ok(AcpLaunchConfig {
        runtime_env: inputs.runtime_env,
        terminal_shell,
        route_plan: inputs.route_plan,
        origin: inputs.origin,
        route_preference: inputs.route_preference,
        route_capability: inputs.route_capability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::error::AcpError;
    use crate::db::service::app_metadata_service;
    use crate::db::test_helpers;
    use crate::terminal::shell::test_support::{
        posix_spec as test_posix_spec, pwsh_spec as test_pwsh_spec,
    };
    use crate::terminal::shell::{ShellCommandStrategy, ShellResolveError, ShellSource};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn prompt_context_is_versioned_and_derived_from_snapshot() {
        let context = render_terminal_prompt_context(&test_pwsh_spec());
        assert_eq!(
            context,
            "<codeg_terminal_context version=\"1\">\n\
Selected shell: PowerShell 7\n\
Dialect: powershell\n\
Generate shell command lines using PowerShell syntax.\n\
ACP command+args requests may still execute directly.\n\
This context is authoritative for the current connection and supersedes\n\
earlier terminal context records.\n\
</codeg_terminal_context>"
        );
    }

    #[test]
    fn prompt_context_maps_cmd_and_posix_dialect_labels() {
        let mut cmd = test_posix_spec();
        cmd.dialect = ShellDialect::Cmd;
        cmd.display_name = "cmd.exe".into();
        cmd.command_strategy = ShellCommandStrategy::Cmd;
        let rendered = render_terminal_prompt_context(&cmd);
        assert!(rendered.contains("Dialect: cmd\n"));
        assert!(rendered.contains("Generate shell command lines using CMD syntax.\n"));

        let posix = test_posix_spec();
        let rendered = render_terminal_prompt_context(&posix);
        assert!(rendered.contains("Dialect: posix\n"));
        assert!(rendered.contains("Generate shell command lines using POSIX syntax.\n"));
    }

    #[test]
    fn prompt_context_custom_dialect_uses_fixed_instruction() {
        let custom = ResolvedShellSpec {
            executable: PathBuf::from("/usr/bin/fish"),
            dialect: ShellDialect::Custom,
            display_name: "Fish".into(),
            source: ShellSource::Explicit,
            command_strategy: ShellCommandStrategy::GenericDashC,
        };
        let rendered = render_terminal_prompt_context(&custom);
        assert!(rendered.contains("Selected shell: Fish\n"));
        assert!(rendered.contains("Dialect: custom\n"));
        assert!(rendered.contains(
            "Generate shell command lines using the selected custom shell's syntax.\n"
        ));
        assert!(!rendered.contains("using custom syntax"));
    }

    #[test]
    fn injector_appends_context_only_once() {
        let injector = TerminalPromptContext::new(test_pwsh_spec());
        let mut first = vec![ContentBlock::Text(TextContent::new("hello"))];
        injector.append_once(&mut first);
        let mut second = vec![ContentBlock::Text(TextContent::new("again"))];
        injector.append_once(&mut second);
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 1);
        match &first[1] {
            ContentBlock::Text(t) => {
                assert!(t.text.contains("<codeg_terminal_context version=\"1\">"));
                assert!(t.text.contains("Dialect: powershell"));
            }
            other => panic!("expected text context block, got {other:?}"),
        }
    }

    #[test]
    fn terminal_prompt_context_shared_across_fork_does_not_reset() {
        // Fork restarts the conversation loop but reuses the same Arc injector
        // created in run_connection — second append must be a no-op.
        let injector = Arc::new(TerminalPromptContext::new(test_pwsh_spec()));
        let mut first = vec![ContentBlock::Text(TextContent::new("hello"))];
        injector.append_once(&mut first);
        let mut post_fork = vec![ContentBlock::Text(TextContent::new("after fork"))];
        injector.append_once(&mut post_fork);
        assert_eq!(first.len(), 2);
        assert_eq!(post_fork.len(), 1);
        match &post_fork[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "after fork"),
            other => panic!("expected original user text only, got {other:?}"),
        }
    }

    #[test]
    fn new_connection_gets_fresh_injector() {
        // A newly spawned / manual-reconnect process constructs a new injector.
        let first_conn = TerminalPromptContext::new(test_pwsh_spec());
        let mut a = vec![ContentBlock::Text(TextContent::new("a"))];
        first_conn.append_once(&mut a);
        assert_eq!(a.len(), 2);

        let second_conn = TerminalPromptContext::new(test_pwsh_spec());
        let mut b = vec![ContentBlock::Text(TextContent::new("b"))];
        second_conn.append_once(&mut b);
        assert_eq!(b.len(), 2, "fresh injector must still be pending");
    }

    #[test]
    fn terminal_metadata_declares_codeg_namespace() {
        let spec = test_pwsh_spec();
        let meta = terminal_metadata(Meta::default(), &spec, adapter_for(AgentType::Codex))
            .expect("metadata");
        let term = meta.get("codeg.dev/terminal").expect("namespace");
        assert_eq!(term["dialect"], "powershell");
        assert_eq!(term["shell"], spec.executable.to_string_lossy().as_ref());
        assert_eq!(term["platform"], std::env::consts::OS);
        assert_eq!(term["commandMode"], "selected-shell-for-command-lines");
    }

    #[test]
    fn terminal_metadata_preserves_existing_and_overwrites_adapter_collision() {
        let mut existing = Meta::default();
        existing.insert(
            "claudeCode".into(),
            serde_json::json!({"emitRawSDKMessages": true}),
        );

        struct CollidingAdapter;
        impl AcpTerminalAdapter for CollidingAdapter {
            fn agent_metadata(
                &self,
                _shell: &ResolvedShellSpec,
            ) -> Result<Meta, AcpError> {
                let mut m = Meta::default();
                m.insert(
                    "codeg.dev/terminal".into(),
                    serde_json::json!({"dialect": "cmd"}),
                );
                m.insert("adapterKey".into(), serde_json::json!(1));
                Ok(m)
            }
        }

        let meta = terminal_metadata(existing, &test_posix_spec(), &CollidingAdapter).unwrap();
        assert_eq!(meta["claudeCode"]["emitRawSDKMessages"], true);
        assert_eq!(meta["adapterKey"], 1);
        assert_eq!(meta["codeg.dev/terminal"]["dialect"], "posix");
        assert_eq!(meta["codeg.dev/terminal"]["shell"], "/bin/sh");
    }

    #[test]
    fn launch_env_declares_selected_shell_without_touching_comspec() {
        let spec = test_pwsh_spec();
        let mut env = BTreeMap::from([("COMSPEC".into(), r"C:\Windows\System32\cmd.exe".into())]);
        apply_agent_launch_env(&mut env, &spec);
        assert_eq!(
            env.get("SHELL"),
            Some(&spec.executable.to_string_lossy().into_owned())
        );
        assert_eq!(env.get("CODEG_TERMINAL_SHELL"), env.get("SHELL"));
        assert_eq!(
            env.get("CODEG_TERMINAL_DIALECT"),
            Some(&"powershell".to_string())
        );
        assert_eq!(
            env.get("COMSPEC"),
            Some(&r"C:\Windows\System32\cmd.exe".to_string())
        );
    }

    #[test]
    fn set_env_value_replaces_case_variants_on_windows() {
        let mut env = BTreeMap::from([("Shell".into(), "old".into())]);
        set_env_value(&mut env, "SHELL", "new".into());
        if cfg!(windows) {
            assert_eq!(env.len(), 1);
            assert_eq!(env.get("SHELL"), Some(&"new".to_string()));
            assert!(!env.keys().any(|k| k == "Shell"));
        } else {
            // Case-sensitive: both keys may coexist; authoritative insert is SHELL.
            assert_eq!(env.get("SHELL"), Some(&"new".to_string()));
        }
    }

    struct FakeAdapter {
        env: BTreeMap<String, String>,
    }

    impl AcpTerminalAdapter for FakeAdapter {
        fn agent_launch_env(
            &self,
            _shell: &ResolvedShellSpec,
        ) -> Result<BTreeMap<String, String>, AcpError> {
            Ok(self.env.clone())
        }
    }

    #[test]
    fn finalize_prefers_codeg_shell_declarations_over_adapter_collisions() {
        let dir = tempfile::tempdir().unwrap();
        // Use a PowerShell basename so the authoritative dialect is "powershell",
        // while the adapter tries to inject "cmd" / a bogus path.
        let shell_name = if cfg!(windows) {
            "pwsh.exe"
        } else {
            "pwsh"
        };
        let path = make_usable_shell(dir.path(), shell_name);

        let mut adapter_env = BTreeMap::new();
        adapter_env.insert("SHELL".into(), "/bogus/shell".into());
        adapter_env.insert("CODEG_TERMINAL_DIALECT".into(), "cmd".into());
        adapter_env.insert("ADAPTER_EXTRA".into(), "1".into());

        let inputs = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::from([(
                "COMSPEC".into(),
                r"C:\Windows\System32\cmd.exe".into(),
            )]),
            SystemTerminalSettings {
                default_shell: Some(path.to_string_lossy().into_owned()),
            },
        );
        let adapter = FakeAdapter { env: adapter_env };
        let config = finalize_acp_launch_config_with_adapter(inputs, &adapter)
            .expect("pwsh temp shell must resolve");

        let expected_shell = config
            .terminal_shell
            .spec
            .executable
            .to_string_lossy()
            .into_owned();
        assert_eq!(config.runtime_env.get("SHELL"), Some(&expected_shell));
        assert_eq!(
            config.runtime_env.get("CODEG_TERMINAL_SHELL"),
            Some(&expected_shell)
        );
        assert_eq!(
            config.runtime_env.get("CODEG_TERMINAL_DIALECT"),
            Some(&"powershell".to_string())
        );
        assert_eq!(
            config.runtime_env.get("ADAPTER_EXTRA"),
            Some(&"1".to_string())
        );
        assert_eq!(
            config.runtime_env.get("COMSPEC"),
            Some(&r"C:\Windows\System32\cmd.exe".to_string())
        );
        assert_ne!(
            config.runtime_env.get("SHELL").map(String::as_str),
            Some("/bogus/shell")
        );
        assert_ne!(
            config
                .runtime_env
                .get("CODEG_TERMINAL_DIALECT")
                .map(String::as_str),
            Some("cmd")
        );
    }

    #[test]
    fn finalize_rejects_adapter_comspec_without_mutating_original() {
        let mut adapter_env = BTreeMap::new();
        adapter_env.insert("COMSPEC".into(), r"C:\evil\cmd.exe".into());
        adapter_env.insert("ADAPTER_EXTRA".into(), "1".into());

        let original_comspec = r"C:\Windows\System32\cmd.exe".to_string();
        let inputs = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::from([("COMSPEC".into(), original_comspec.clone())]),
            SystemTerminalSettings {
                default_shell: None,
            },
        );
        let adapter = FakeAdapter { env: adapter_env };

        // Even when resolve succeeds, COMSPEC addition must error. When resolve
        // fails first, we still assert the error path via a direct merge that
        // would hit COMSPEC — use a resolvable explicit shell temp file when
        // system is unavailable.
        let result = finalize_acp_launch_config_with_adapter(inputs, &adapter);
        match result {
            Err(AcpError::Protocol(msg)) => {
                assert!(
                    msg.contains("COMSPEC"),
                    "expected COMSPEC rejection message, got {msg}"
                );
            }
            Err(AcpError::TerminalShellUnavailable { .. }) => {
                // Fall through: host system shell unavailable; exercise via
                // explicit usable shell path instead.
                let dir = tempfile::tempdir().unwrap();
                let shell_name = if cfg!(windows) {
                    "pwsh.exe"
                } else {
                    "bash"
                };
                let path = make_usable_shell(dir.path(), shell_name);
                let inputs = AcpLaunchInputs::with_placeholder_route(
                    BTreeMap::from([("COMSPEC".into(), original_comspec.clone())]),
                    SystemTerminalSettings {
                        default_shell: Some(path.to_string_lossy().into_owned()),
                    },
                );
                let mut adapter_env = BTreeMap::new();
                adapter_env.insert("COMSPEC".into(), r"C:\evil\cmd.exe".into());
                let err =
                    finalize_acp_launch_config_with_adapter(inputs, &FakeAdapter { env: adapter_env })
                        .unwrap_err();
                match err {
                    AcpError::Protocol(msg) => assert!(msg.contains("COMSPEC")),
                    other => panic!("expected Protocol COMSPEC error, got {other}"),
                }
            }
            Ok(cfg) => panic!(
                "expected COMSPEC rejection, got ok with shell {:?}",
                cfg.terminal_shell.spec.executable
            ),
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    fn make_usable_shell(dir: &std::path::Path, basename: &str) -> PathBuf {
        let path = dir.join(basename);
        std::fs::write(&path, b"").expect("write temp shell");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn persisted_override_beats_connect_payload_but_draft_uses_payload() {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource,
        };
        use crate::commands::delegation::DelegationRuntimeSnapshot;
        use crate::db::test_helpers::seed_folder;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/route-resolve").await;
        let conversation_id = crate::commands::conversations::create_conversation_core(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            Some(DelegationRoutePolicy::Native),
        )
        .await
        .unwrap();
        let runtime = DelegationRuntimeSnapshot {
            enabled: true,
            route_policy: DelegationRoutePolicy::Codeg,
            stalled_after_seconds: 300,
        };

        let persisted = resolve_connect_route(
            &db.conn,
            AgentType::Codex,
            AcpRouteRequest::root(
                Some(conversation_id),
                Some(DelegationRoutePolicy::Codeg),
            ),
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(persisted.effective, DelegationRoutePolicy::Native);
        assert_eq!(persisted.source, DelegationRouteSource::SessionOverride);

        let draft = resolve_connect_route(
            &db.conn,
            AgentType::Codex,
            AcpRouteRequest::root(None, Some(DelegationRoutePolicy::Native)),
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(draft.effective, DelegationRoutePolicy::Native);
    }

    #[tokio::test]
    async fn resolve_connect_route_rejects_mismatched_conversation_agent_type() {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource,
        };
        use crate::db::test_helpers;
        use crate::commands::delegation::DelegationRuntimeSnapshot;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/route-mismatch").await;
        let conversation_id = crate::commands::conversations::create_conversation_core(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        let runtime = DelegationRuntimeSnapshot {
            enabled: true,
            route_policy: DelegationRoutePolicy::Codeg,
            stalled_after_seconds: 300,
        };

        let err = resolve_connect_route(
            &db.conn,
            AgentType::Grok,
            AcpRouteRequest::root(Some(conversation_id), None),
            &runtime,
        )
        .await
        .expect_err("mismatched agent type must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("agent type") || msg.contains("Grok") || msg.contains("Codex"),
            "expected agent mismatch detail, got {msg}"
        );
        // Matching agent still works (public resolve uses managed host pin).
        let ok = resolve_connect_route(
            &db.conn,
            AgentType::Codex,
            AcpRouteRequest::root(Some(conversation_id), None),
            &runtime,
        )
        .await
        .expect("matching agent resolves");
        assert_eq!(ok.source, DelegationRouteSource::GlobalDefault);
    }

    #[tokio::test]
    async fn codex_built_in_host_contract_resolves_child_without_route_unavailable() {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource, PINNED_CODEX_CLI_VERSION,
        };
        use crate::commands::delegation::DelegationRuntimeSnapshot;
        use crate::db::test_helpers;

        let db = test_helpers::fresh_in_memory_db().await;
        let runtime = DelegationRuntimeSnapshot {
            enabled: true,
            route_policy: DelegationRoutePolicy::Codeg,
            stalled_after_seconds: 300,
        };
        // Empty env + ACP adapter installed id: managed host pin 0.144.1, not 1.1.2.
        let capability =
            build_route_capability_snapshot(AgentType::Codex, Some("1.1.2"), &BTreeMap::new());
        assert!(
            capability.suppression.failure.is_none(),
            "ACP adapter version must not make host contract unsupported"
        );
        let _ = PINNED_CODEX_CLI_VERSION;

        let child = resolve_connect_route_with_capability(
            &db.conn,
            AgentType::Codex,
            AcpRouteRequest::codeg_child(),
            &runtime,
            &capability,
        )
        .await
        .expect("codex child with host contract must not RouteUnavailable");
        assert_eq!(child.effective, DelegationRoutePolicy::Codeg);
        assert_eq!(child.source, DelegationRouteSource::ForcedChild);
    }

    #[tokio::test]
    async fn build_acp_launch_inputs_uses_runtime_env_for_capability_and_conversation_id() {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource, RouteDegradedReason,
        };
        use crate::commands::delegation::DelegationRuntimeSnapshot;
        use crate::db::service::agent_setting_service;
        use crate::db::test_helpers;
        use sea_orm::{ActiveModelTrait, Set};

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/launch-cap").await;
        let conversation_id = crate::commands::conversations::create_conversation_core(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            Some(DelegationRoutePolicy::Native),
        )
        .await
        .unwrap();

        // Ensure agent setting row exists, then inject custom CODEX_PATH.
        agent_setting_service::ensure_defaults(
            &db.conn,
            &[agent_setting_service::AgentDefaultInput {
                agent_type: AgentType::Codex,
                registry_id: "codex-acp".into(),
                default_sort_order: 0,
            }],
        )
        .await
        .unwrap();
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert(
            crate::acp::codex_cli::CODEX_PATH_ENV.to_string(),
            "C:\\custom\\codex.exe".into(),
        );
        let env_json = serde_json::to_string(&env).unwrap();
        let model = agent_setting_service::get_by_agent_type(&db.conn, AgentType::Codex)
            .await
            .unwrap()
            .expect("codex setting row");
        let mut active: crate::db::entities::agent_setting::ActiveModel = model.into();
        active.env_json = Set(Some(env_json));
        active.enabled = Set(true);
        active.update(&db.conn).await.unwrap();

        let runtime = DelegationRuntimeSnapshot {
            enabled: true,
            route_policy: DelegationRoutePolicy::Codeg,
            stalled_after_seconds: 300,
        };
        let inputs = build_acp_launch_inputs(
            &db,
            AgentType::Codex,
            None,
            Path::new("."),
            AcpRouteRequest::root(Some(conversation_id), None),
            &runtime,
        )
        .await
        .expect("launch inputs");

        assert_eq!(inputs.route_preference, Some(DelegationRoutePolicy::Native));
        assert_eq!(
            inputs.route_capability.suppression.failure,
            Some(RouteDegradedReason::NativeSuppressionUnsupported)
        );
        // Persisted Native override → native effective even with custom path.
        assert_eq!(inputs.route_plan.effective, DelegationRoutePolicy::Native);
        assert_eq!(
            inputs.route_plan.source,
            DelegationRouteSource::SessionOverride
        );
        assert_eq!(inputs.origin, DelegationConnectionOrigin::Root);
    }

    #[tokio::test]
    async fn finalize_rejects_unavailable_shell_from_db_settings() {
        let db = test_helpers::fresh_in_memory_db().await;
        let settings = SystemTerminalSettings {
            default_shell: Some("missing-shell".into()),
        };
        let raw = serde_json::to_string(&settings).unwrap();
        app_metadata_service::upsert_value(
            &db.conn,
            crate::commands::system_settings::SYSTEM_TERMINAL_SETTINGS_KEY,
            &raw,
        )
        .await
        .unwrap();

        let runtime = crate::commands::delegation::DelegationRuntimeSnapshot::default();
        let inputs = build_acp_launch_inputs(
            &db,
            AgentType::Grok,
            None,
            Path::new("."),
            AcpRouteRequest::root(None, None),
            &runtime,
        )
            .await
            .expect("launch inputs load");
        let err = finalize_acp_launch_config(inputs, AgentType::Grok).unwrap_err();
        assert!(
            matches!(
                err,
                AcpError::TerminalShellUnavailable {
                    ref display_name,
                    ref executable
                } if executable == "missing-shell" || display_name.contains("missing")
            ),
            "got {err:?}"
        );
        assert_eq!(err.code(), Some("terminal_shell_unavailable"));
    }

    #[tokio::test]
    async fn finalize_rejects_unsupported_shell_from_db_settings() {
        let db = test_helpers::fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let basename = if cfg!(windows) {
            "weirdthing.exe"
        } else {
            "weirdthing"
        };
        let path = make_usable_shell(dir.path(), basename);
        let settings = SystemTerminalSettings {
            default_shell: Some(path.to_string_lossy().into_owned()),
        };
        let raw = serde_json::to_string(&settings).unwrap();
        app_metadata_service::upsert_value(
            &db.conn,
            crate::commands::system_settings::SYSTEM_TERMINAL_SETTINGS_KEY,
            &raw,
        )
        .await
        .unwrap();

        let runtime = crate::commands::delegation::DelegationRuntimeSnapshot::default();
        let inputs = build_acp_launch_inputs(
            &db,
            AgentType::Grok,
            None,
            Path::new("."),
            AcpRouteRequest::root(None, None),
            &runtime,
        )
            .await
            .expect("launch inputs load");
        let err = finalize_acp_launch_config(inputs, AgentType::Grok).unwrap_err();
        assert!(
            matches!(err, AcpError::TerminalShellUnsupported { .. }),
            "got {err:?}"
        );
        assert_eq!(err.code(), Some("terminal_shell_unsupported"));
    }

    #[test]
    fn shell_resolve_error_maps_without_env_leak() {
        let err: AcpError = ShellResolveError::Unavailable {
            display_name: "pwsh".into(),
            executable: "missing".into(),
        }
        .into();
        let msg = err.to_string();
        assert!(!msg.contains("OPENAI"));
        assert!(!msg.contains("API_KEY"));
        assert!(matches!(
            err,
            AcpError::TerminalShellUnavailable { .. }
        ));
    }

    #[test]
    fn terminal_base_env_excludes_api_secrets() {
        let mut runtime = BTreeMap::new();
        runtime.insert("OPENAI_API_KEY".into(), "secret".into());
        runtime.insert("GIT_CONFIG_COUNT".into(), "1".into());
        runtime.insert("GIT_CONFIG_KEY_0".into(), "credential.helper".into());
        let base = build_terminal_base_env(&runtime);
        assert!(!base.contains_key("OPENAI_API_KEY"));
        assert_eq!(base.get("GIT_CONFIG_COUNT"), Some(&"1".to_string()));
        assert_eq!(
            base.get("GIT_CONFIG_KEY_0"),
            Some(&"credential.helper".to_string())
        );
    }

    #[test]
    fn fingerprint_ignores_terminal_declaration_keys() {
        let agent = AgentType::Grok;
        let mut env = BTreeMap::new();
        env.insert("OPENAI_API_KEY".into(), "k".into());
        let before = crate::commands::acp::fingerprint_config(agent, &env);
        apply_agent_launch_env(&mut env, &test_pwsh_spec());
        let after = crate::commands::acp::fingerprint_config(agent, &env);
        assert_eq!(
            before, after,
            "SHELL/CODEG_TERMINAL_* must not change agent-config fingerprint"
        );
    }
}
