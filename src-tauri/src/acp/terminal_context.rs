//! ACP launch terminal context: shell snapshot finalization and declaration env.
//!
//! Launch inputs (runtime env + terminal settings) are loaded at every entry
//! point; the immutable shell snapshot is finalized only when a new process
//! will actually be spawned (after connection reuse has been ruled out).

use std::collections::BTreeMap;
use std::path::Path;

use crate::acp::error::AcpError;
use crate::acp::terminal_adapter::{adapter_for, AcpTerminalAdapter};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::SystemTerminalSettings;
use crate::terminal::shell::{
    resolve_terminal_shell, ResolvedShellSnapshot, ResolvedShellSpec, ShellDialect,
};

/// Inputs loaded before reuse / spawn: agent runtime env + persisted terminal settings.
#[derive(Debug, Clone)]
pub struct AcpLaunchInputs {
    pub runtime_env: BTreeMap<String, String>,
    pub terminal_settings: SystemTerminalSettings,
}

/// Immutable config for a connection that is about to spawn a process.
#[derive(Debug, Clone)]
pub(crate) struct AcpLaunchConfig {
    pub runtime_env: BTreeMap<String, String>,
    pub terminal_shell: ResolvedShellSnapshot,
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

fn dialect_as_str(dialect: ShellDialect) -> &'static str {
    match dialect {
        ShellDialect::Cmd => "cmd",
        ShellDialect::PowerShell => "powershell",
        ShellDialect::Posix => "posix",
        ShellDialect::Custom => "custom",
    }
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
        dialect_as_str(spec.dialect).to_string(),
    );
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

/// Load agent runtime env + system terminal settings for an ACP entry point.
pub(crate) async fn build_acp_launch_inputs(
    db: &AppDatabase,
    agent_type: AgentType,
    session_id: Option<&str>,
    data_dir: &Path,
) -> Result<AcpLaunchInputs, AcpError> {
    let runtime_env =
        crate::commands::acp::build_session_runtime_env(db, agent_type, session_id, data_dir)
            .await?;
    let terminal_settings =
        crate::commands::system_settings::load_system_terminal_settings(&db.conn)
            .await
            .map_err(|error| AcpError::protocol(error.to_string()))?;

    Ok(AcpLaunchInputs {
        runtime_env,
        terminal_settings,
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::error::AcpError;
    use crate::db::service::app_metadata_service;
    use crate::db::test_helpers;
    use crate::terminal::shell::test_support::pwsh_spec as test_pwsh_spec;
    use crate::terminal::shell::ShellResolveError;
    use std::path::PathBuf;

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

        let inputs = AcpLaunchInputs {
            runtime_env: BTreeMap::from([(
                "COMSPEC".into(),
                r"C:\Windows\System32\cmd.exe".into(),
            )]),
            terminal_settings: SystemTerminalSettings {
                default_shell: Some(path.to_string_lossy().into_owned()),
            },
        };
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
        let inputs = AcpLaunchInputs {
            runtime_env: BTreeMap::from([("COMSPEC".into(), original_comspec.clone())]),
            terminal_settings: SystemTerminalSettings {
                default_shell: None,
            },
        };
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
                let inputs = AcpLaunchInputs {
                    runtime_env: BTreeMap::from([("COMSPEC".into(), original_comspec.clone())]),
                    terminal_settings: SystemTerminalSettings {
                        default_shell: Some(path.to_string_lossy().into_owned()),
                    },
                };
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

        let inputs = build_acp_launch_inputs(&db, AgentType::Grok, None, Path::new("."))
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

        let inputs = build_acp_launch_inputs(&db, AgentType::Grok, None, Path::new("."))
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
